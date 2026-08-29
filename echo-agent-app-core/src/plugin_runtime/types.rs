// Process-level plugin runtime with atomic live component replacement.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use echo_agent::lsp::{LspConfig, LspManager, LspServerStatus};
use echo_agent::plugin::{
    AGENT_PLUGIN_SCHEMA_V1, InstallSource, PluginEntry, PluginIntegrator, PluginLifecycle,
    PluginLifecycleManager, PluginPreparationDiagnostic, PluginRegistry, PluginScope,
    PluginWiringResult, PreparedPluginSet, WiredPluginComponents,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::agent_handle::AgentHandle;
use crate::agent_pool::{AgentPluginGeneration, AgentPool};
use crate::mcp_config_runtime::{
    McpNameOwnershipGuard, McpNameOwnershipRegistry, PluginMcpOwnershipToken,
};
pub use crate::plugin_components::{PluginOutputStyle, PluginThemeDefinition};
use crate::plugin_components::{
    PreparedApplicationComponents, prepare_application_components, register_plugin_agents,
    validate_application_component_files,
};
use crate::scheduler::{CronTask, SchedulerRunner};

pub(crate) const OUTPUT_STYLE_PROJECTION: &str = "eko:plugin-output-style";

#[derive(Clone)]
pub struct PluginLspRuntime {
    pub manager: Arc<RwLock<LspManager>>,
    binding: Arc<RwLock<PluginLspBinding>>,
}

#[derive(Clone)]
struct PluginLspBinding {
    base_config: LspConfig,
    project_root: PathBuf,
}

impl PluginLspRuntime {
    pub fn new(
        manager: Arc<RwLock<LspManager>>,
        base_config: LspConfig,
        project_root: PathBuf,
    ) -> Self {
        Self {
            manager,
            binding: Arc::new(RwLock::new(PluginLspBinding {
                base_config,
                project_root,
            })),
        }
    }

    /// Build the non-plugin LSP configuration for one workspace generation.
    pub fn config_for_workspace(project_root: &Path) -> LspConfig {
        let mut config = LspConfig::discover(project_root);
        let global_lsp = crate::data_root::user_data_path(".lsp.yaml");
        if global_lsp.is_file() {
            match LspConfig::from_file(&global_lsp) {
                Ok(global) => config.merge(global),
                Err(error) => tracing::warn!(
                    path = %global_lsp.display(),
                    %error,
                    "Failed to load global LSP config"
                ),
            }
        }

        let mut directory = Some(project_root);
        let project_lsp = loop {
            let Some(candidate_root) = directory else {
                break None;
            };
            let candidate = candidate_root.join(".lsp.yaml");
            if candidate.is_file() {
                break Some(candidate);
            }
            directory = candidate_root.parent();
        };
        if let Some(project_lsp) = project_lsp {
            match LspConfig::from_file(&project_lsp) {
                Ok(project) => config.merge(project),
                Err(error) => tracing::warn!(
                    path = %project_lsp.display(),
                    %error,
                    "Failed to load project LSP config"
                ),
            }
        }
        config
    }

    async fn binding(&self) -> PluginLspBinding {
        self.binding.read().await.clone()
    }

    async fn publish_binding(&self, binding: PluginLspBinding) {
        *self.binding.write().await = binding;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReloadSummary {
    pub total: usize,
    pub enabled: usize,
    pub skills_loaded: usize,
    pub hooks_registered: usize,
    pub mcp_connected: usize,
    pub agents_loaded: usize,
    pub lsp_languages_loaded: usize,
    pub monitors_loaded: usize,
    pub themes_loaded: usize,
    pub output_styles_loaded: usize,
    pub errors: Vec<String>,
}

impl ReloadSummary {
    fn from_components(
        total: usize,
        enabled: usize,
        wiring: Option<&PluginWiringResult>,
        generation: Option<&PreparedPluginSet>,
        application: &PreparedApplicationComponents,
    ) -> Self {
        Self {
            total,
            enabled,
            skills_loaded: wiring.map_or(0, |receipt| receipt.skills_loaded.len()),
            hooks_registered: wiring.map_or(0, |receipt| receipt.hooks_registered.len()),
            mcp_connected: wiring.map_or(0, |receipt| receipt.mcp_connected.len()),
            agents_loaded: application.agents.len(),
            lsp_languages_loaded: application
                .lsp_configs
                .iter()
                .map(|(_, config)| config.servers.len())
                .sum(),
            monitors_loaded: application.monitors.len(),
            themes_loaded: application.themes.len(),
            output_styles_loaded: application.output_styles.len(),
            errors: generation
                .into_iter()
                .flat_map(PreparedPluginSet::diagnostics)
                .filter(|diagnostic| {
                    diagnostic.severity() == echo_agent::plugin::PluginDiagnosticSeverity::Error
                })
                .map(ToString::to_string)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginScaffoldResult {
    pub path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginValidationReport {
    pub valid: bool,
    pub name: Option<String>,
    pub components: Vec<String>,
    pub errors: Vec<String>,
}

/// EKO projection of portable framework components plus product-only UI and
/// background-service components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EkoPluginCapability {
    Skill,
    Hook,
    McpServer,
    LspServer,
    Agent,
    Tool,
    Monitor,
    Theme,
    OutputStyle,
}

impl EkoPluginCapability {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Skill => "Skills",
            Self::Hook => "Hooks",
            Self::McpServer => "MCP Servers",
            Self::LspServer => "LSP Servers",
            Self::Agent => "Agents",
            Self::Tool => "Tools",
            Self::Monitor => "Monitors",
            Self::Theme => "Themes",
            Self::OutputStyle => "Output Styles",
        }
    }
}

impl From<echo_agent::plugin::PluginCapability> for EkoPluginCapability {
    fn from(capability: echo_agent::plugin::PluginCapability) -> Self {
        match capability {
            echo_agent::plugin::PluginCapability::Skill => Self::Skill,
            echo_agent::plugin::PluginCapability::Hook => Self::Hook,
            echo_agent::plugin::PluginCapability::McpServer => Self::McpServer,
            echo_agent::plugin::PluginCapability::LspServer => Self::LspServer,
            echo_agent::plugin::PluginCapability::Agent => Self::Agent,
            echo_agent::plugin::PluginCapability::Tool => Self::Tool,
        }
    }
}

pub fn plugin_capabilities(entry: &PluginEntry) -> Vec<EkoPluginCapability> {
    let mut capabilities = entry
        .inferred_capabilities()
        .into_iter()
        .map(EkoPluginCapability::from)
        .collect::<Vec<_>>();
    if let Ok(eko) = crate::plugin_components::resolve_eko_components(&entry.root) {
        if eko.monitors_file.is_some() {
            capabilities.push(EkoPluginCapability::Monitor);
        }
        if !eko.theme_files.is_empty() {
            capabilities.push(EkoPluginCapability::Theme);
        }
        if !eko.output_style_files.is_empty() {
            capabilities.push(EkoPluginCapability::OutputStyle);
        }
    }
    capabilities
}

#[derive(Clone)]
enum RegistrySource {
    Default,
    #[cfg(test)]
    Custom {
        state_file: PathBuf,
        data_dir: PathBuf,
        scopes: Vec<PluginScope>,
    },
}

type PluginMcpOwnership = HashMap<String, HashMap<String, PluginMcpOwnershipToken>>;
type PluginMcpDeclarations = HashMap<String, Vec<String>>;

struct PluginRuntimeState {
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    framework_receipt: Option<PluginWiringResult>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    lifecycle: PluginLifecycleManager,
    cleanup_quarantine: Vec<PluginCleanupQuarantine>,
    active_theme: Option<String>,
    active_output_style: Option<String>,
    generation: u64,
    shut_down: bool,
}

struct PluginCleanupQuarantine {
    root: PathBuf,
    lifecycle: Option<PluginLifecycleManager>,
    lifecycle_plugin_ids: Vec<String>,
    monitors: Vec<CronTask>,
    last_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PluginPreferences {
    #[serde(default)]
    active_theme: Option<String>,
    #[serde(default)]
    active_output_style: Option<String>,
}

struct PluginMutationSupervisor {
    accepting: bool,
    settlements: tokio::task::JoinSet<()>,
    sequence: Arc<tokio::sync::Semaphore>,
}

impl Default for PluginMutationSupervisor {
    fn default() -> Self {
        Self {
            accepting: true,
            settlements: tokio::task::JoinSet::new(),
            sequence: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

pub struct PluginRuntimeService {
    agent_handle: AgentHandle,
    lsp: PluginLspRuntime,
    scheduler: RwLock<Option<Arc<SchedulerRunner>>>,
    mcp_ownership: Arc<McpNameOwnershipRegistry>,
    integrator: PluginIntegrator,
    target_scope: String,
    registry_source: RegistrySource,
    preferences_file: PathBuf,
    state: Mutex<PluginRuntimeState>,
    agent_pool: RwLock<Option<Weak<AgentPool>>>,
    mutation_supervisor: Mutex<PluginMutationSupervisor>,
}
