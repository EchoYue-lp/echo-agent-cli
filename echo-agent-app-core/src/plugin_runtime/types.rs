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

/// One exact plugin MCP name claim.
///
/// The token is the only authority that may release the name. A retried cleanup
/// receipt therefore settles as a hand-over (a no-op) when user configuration or
/// another plugin token has taken the name over, and can never disconnect a name
/// this receipt never owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginMcpNameClaim {
    plugin_id: String,
    name: String,
    token: PluginMcpOwnershipToken,
}

impl PluginMcpNameClaim {
    fn new(plugin_id: String, name: String, token: PluginMcpOwnershipToken) -> Self {
        Self {
            plugin_id,
            name,
            token,
        }
    }

    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn token(&self) -> PluginMcpOwnershipToken {
        self.token
    }
}

/// Original resource authorities retained when a receipt leaves its runtime.
/// This deliberately holds no service Arc: pending debt cannot keep a service
/// and its own receipt ledger alive through a permanent ownership cycle.
struct CleanupAuthority {
    agent_handle: AgentHandle,
    mcp_ownership: Arc<McpNameOwnershipRegistry>,
    scheduler: Option<Arc<SchedulerRunner>>,
    registry_source: RegistrySource,
}

/// What a cleanup receipt still owes. Every variant preserves exact withdrawal
/// authority, and settling an already settled obligation is harmless.
enum CleanupSettlement {
    /// An exported receipt always keeps its issuing target and registry. Project
    /// roots are lookup keys only: token counters are local to a registry, and a
    /// replacement runtime must never settle an old receipt using its own Agent.
    OwnedAuthority {
        authority: Arc<CleanupAuthority>,
        receipt: Box<CleanupReceipt>,
    },
    /// Exact MCP names whose owning components were already withdrawn from the
    /// Agent. Only these tokens may be released; no other owner is touched.
    McpNames { claims: Vec<PluginMcpNameClaim> },
    /// Framework components that may still be wired. The exact wiring receipt
    /// must settle before the listed ownership tokens may be released, so a name
    /// is never handed to a new owner while its old client is still connected.
    Framework {
        // Boxed so the rarely-owned wiring receipt does not inflate every
        // settlement variant to its size.
        wiring: Box<PluginWiringResult>,
        claims: Vec<PluginMcpNameClaim>,
    },
    /// Lifecycle registrations still held by the live lifecycle manager.
    LiveLifecycle { plugin_ids: Vec<String> },
    /// A lifecycle manager detached from the runtime by fail-closed retirement.
    OwnedLifecycle {
        manager: Arc<Mutex<PluginLifecycleManager>>,
    },
    /// The runtime-wide lifecycle shutdown still owes a retry.
    LifecycleShutdown,
    /// Plugin monitors that could not be removed from the scheduler.
    Monitors { tasks: Vec<CronTask> },
    /// A plugin entry whose uninstall did not complete.
    PluginUninstall { plugin_id: String, keep_data: bool },
}

/// A retryable, first-class plugin cleanup obligation.
///
/// Receipts survive failed installs/uninstalls, rejected generations, caller
/// cancellation and bootstrap failure. They are retried by every owned mutation
/// and only disappear once their exact resource withdrawal has settled; success
/// is never reported while one is still outstanding.
pub(crate) struct CleanupReceipt {
    root: PathBuf,
    cause: String,
    attempts: u32,
    last_errors: Vec<String>,
    settlement: CleanupSettlement,
}

impl CleanupReceipt {
    fn bind_authority(self, authority: &Arc<CleanupAuthority>) -> Self {
        if matches!(&self.settlement, CleanupSettlement::OwnedAuthority { .. }) {
            return self;
        }
        Self {
            root: self.root.clone(),
            cause: self.cause.clone(),
            attempts: self.attempts,
            last_errors: self.last_errors.clone(),
            settlement: CleanupSettlement::OwnedAuthority {
                authority: Arc::clone(authority),
                receipt: Box::new(self),
            },
        }
    }

    fn mcp_names(root: PathBuf, cause: String, claims: Vec<PluginMcpNameClaim>) -> Self {
        Self::new(root, cause, CleanupSettlement::McpNames { claims })
    }

    fn framework(
        root: PathBuf,
        cause: String,
        wiring: PluginWiringResult,
        claims: Vec<PluginMcpNameClaim>,
    ) -> Self {
        Self::new(
            root,
            cause,
            CleanupSettlement::Framework {
                wiring: Box::new(wiring),
                claims,
            },
        )
    }

    fn live_lifecycle(root: PathBuf, cause: String, plugin_ids: Vec<String>) -> Self {
        Self::new(root, cause, CleanupSettlement::LiveLifecycle { plugin_ids })
    }

    fn owned_lifecycle(root: PathBuf, cause: String, manager: PluginLifecycleManager) -> Self {
        Self::new(
            root,
            cause,
            CleanupSettlement::OwnedLifecycle {
                manager: Arc::new(Mutex::new(manager)),
            },
        )
    }

    fn lifecycle_shutdown(root: PathBuf, cause: String) -> Self {
        Self::new(root, cause, CleanupSettlement::LifecycleShutdown)
    }

    fn monitors(root: PathBuf, cause: String, tasks: Vec<CronTask>) -> Self {
        Self::new(root, cause, CleanupSettlement::Monitors { tasks })
    }

    fn plugin_uninstall(root: PathBuf, cause: String, plugin_id: String, keep_data: bool) -> Self {
        Self::new(
            root,
            cause,
            CleanupSettlement::PluginUninstall {
                plugin_id,
                keep_data,
            },
        )
    }

    fn new(root: PathBuf, cause: String, settlement: CleanupSettlement) -> Self {
        Self {
            root,
            cause,
            attempts: 0,
            last_errors: Vec::new(),
            settlement,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn mcp_claims(&self) -> &[PluginMcpNameClaim] {
        match &self.settlement {
            CleanupSettlement::OwnedAuthority { receipt, .. } => receipt.mcp_claims(),
            CleanupSettlement::McpNames { claims }
            | CleanupSettlement::Framework { claims, .. } => claims,
            CleanupSettlement::LiveLifecycle { .. }
            | CleanupSettlement::OwnedLifecycle { .. }
            | CleanupSettlement::LifecycleShutdown
            | CleanupSettlement::Monitors { .. }
            | CleanupSettlement::PluginUninstall { .. } => &[],
        }
    }

    pub(crate) fn plugin_id(&self) -> Option<&str> {
        match &self.settlement {
            CleanupSettlement::OwnedAuthority { receipt, .. } => receipt.plugin_id(),
            CleanupSettlement::LiveLifecycle { plugin_ids } => {
                plugin_ids.first().map(String::as_str)
            }
            CleanupSettlement::PluginUninstall { plugin_id, .. } => Some(plugin_id.as_str()),
            CleanupSettlement::McpNames { claims }
            | CleanupSettlement::Framework { claims, .. } => {
                claims.first().map(PluginMcpNameClaim::plugin_id)
            }
            CleanupSettlement::OwnedLifecycle { .. }
            | CleanupSettlement::LifecycleShutdown
            | CleanupSettlement::Monitors { .. } => None,
        }
    }

    /// Human-readable identity used in errors and debt reports.
    pub(crate) fn describe(&self) -> String {
        let claims = self
            .mcp_claims()
            .iter()
            .map(|claim| format!("{}:{}", claim.plugin_id(), claim.name()))
            .collect::<Vec<_>>()
            .join(",");
        let kind = match &self.settlement {
            CleanupSettlement::OwnedAuthority { receipt, .. } => {
                format!("original-authority({})", receipt.describe())
            }
            CleanupSettlement::McpNames { .. } => format!("mcp-ownership({claims})"),
            CleanupSettlement::Framework { .. } => {
                format!("framework-wiring+ownership({claims})")
            }
            CleanupSettlement::LiveLifecycle { plugin_ids } => {
                format!("lifecycle({})", plugin_ids.join(","))
            }
            CleanupSettlement::OwnedLifecycle { .. } => "lifecycle-manager".to_string(),
            CleanupSettlement::LifecycleShutdown => "lifecycle-shutdown".to_string(),
            CleanupSettlement::Monitors { tasks } => format!("monitors({})", tasks.len()),
            CleanupSettlement::PluginUninstall { plugin_id, .. } => {
                format!("uninstall({plugin_id})")
            }
        };
        let plugin = self
            .plugin_id()
            .map(|plugin_id| format!("plugin={plugin_id} "))
            .unwrap_or_default();
        format!(
            "cleanup receipt {kind} for {plugin}{} from '{}' (attempts={})",
            self.root.display(),
            self.cause,
            self.attempts
        )
    }
}

impl Clone for CleanupReceipt {
    fn clone(&self) -> Self {
        let settlement = match &self.settlement {
            CleanupSettlement::OwnedAuthority { authority, receipt } => {
                CleanupSettlement::OwnedAuthority {
                    authority: Arc::clone(authority),
                    receipt: receipt.clone(),
                }
            }
            CleanupSettlement::McpNames { claims } => CleanupSettlement::McpNames {
                claims: claims.clone(),
            },
            CleanupSettlement::Framework { wiring, claims } => CleanupSettlement::Framework {
                wiring: wiring.clone(),
                claims: claims.clone(),
            },
            CleanupSettlement::LiveLifecycle { plugin_ids } => CleanupSettlement::LiveLifecycle {
                plugin_ids: plugin_ids.clone(),
            },
            CleanupSettlement::OwnedLifecycle { manager } => CleanupSettlement::OwnedLifecycle {
                manager: Arc::clone(manager),
            },
            CleanupSettlement::LifecycleShutdown => CleanupSettlement::LifecycleShutdown,
            CleanupSettlement::Monitors { tasks } => CleanupSettlement::Monitors {
                tasks: tasks.clone(),
            },
            CleanupSettlement::PluginUninstall {
                plugin_id,
                keep_data,
            } => CleanupSettlement::PluginUninstall {
                plugin_id: plugin_id.clone(),
                keep_data: *keep_data,
            },
        };
        Self {
            root: self.root.clone(),
            cause: self.cause.clone(),
            attempts: self.attempts,
            last_errors: self.last_errors.clone(),
            settlement,
        }
    }
}

impl std::fmt::Debug for CleanupReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CleanupReceipt")
            .field("root", &self.root)
            .field("cause", &self.cause)
            .field("attempts", &self.attempts)
            .field("last_errors", &self.last_errors)
            .field("describe", &self.describe())
            .finish()
    }
}

/// Failure of a plugin runtime bootstrap that still owns cleanup receipts.
///
/// The failed service is gone, so these receipts cannot be settled by it any
/// more. They therefore travel as [`RetainedCleanupReceipts`], a collection that
/// cannot leave scope silently: production callers hand it to the process-level
/// retry owner with [`RetainedCleanupReceipts::park`], and dropping it parks the
/// receipts anyway.
#[derive(Debug)]
pub(crate) struct PluginBootstrapFailure {
    error: anyhow::Error,
    receipts: RetainedCleanupReceipts,
}

impl PluginBootstrapFailure {
    pub(crate) fn new(error: anyhow::Error, receipts: Vec<CleanupReceipt>) -> Self {
        Self {
            error,
            receipts: RetainedCleanupReceipts::new(receipts),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn error(&self) -> &anyhow::Error {
        &self.error
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn receipts(&self) -> &RetainedCleanupReceipts {
        &self.receipts
    }

    /// Split the failure into its error and the retained receipts. Callers must
    /// hand the receipts to a live retry owner; dropping them parks them in the
    /// process-level ledger instead of forgetting them.
    pub(crate) fn into_parts(self) -> (anyhow::Error, RetainedCleanupReceipts) {
        (self.error, self.receipts)
    }

    /// Convenience mapping for the `cfg(test)` constructors, which still return
    /// `anyhow::Result`. Receipts are parked, never dropped.
    #[cfg(test)]
    pub(crate) fn into_error(self) -> anyhow::Error {
        let (error, receipts) = self.into_parts();
        receipts.park();
        error
    }
}

impl std::fmt::Display for PluginBootstrapFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "plugin runtime bootstrap failed: {} ({} cleanup receipt(s) retained)",
            self.error,
            self.receipts.len()
        )
    }
}

impl std::error::Error for PluginBootstrapFailure {}

struct PluginRuntimeState {
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    framework_receipt: Option<PluginWiringResult>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    lifecycle: PluginLifecycleManager,
    cleanup_receipts: Vec<CleanupReceipt>,
    active_theme: Option<String>,
    active_output_style: Option<String>,
    generation: u64,
    shut_down: bool,
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
