//! Process-level plugin runtime with atomic live component replacement.

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

impl PluginRuntimeService {
    pub(crate) async fn new(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_for_scope(agent_handle, lsp, mcp_ownership, "global".to_string(), None).await
    }

    pub(crate) async fn new_for_scope(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
        target_scope: String,
        authority_generation: Option<AgentPluginGeneration>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_with_source(
            agent_handle,
            lsp,
            mcp_ownership,
            RegistrySource::Default,
            target_scope,
            authority_generation,
        )
        .await
    }

    async fn new_with_source(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
        registry_source: RegistrySource,
        target_scope: String,
        authority_generation: Option<AgentPluginGeneration>,
    ) -> anyhow::Result<Arc<Self>> {
        let framework_generation = authority_generation
            .as_ref()
            .and_then(AgentPluginGeneration::framework_generation);
        let authority_revision = authority_generation
            .as_ref()
            .map_or(0, AgentPluginGeneration::revision);
        let preferences_file = match &registry_source {
            RegistrySource::Default => crate::data_root::user_data_dir()
                .join("plugins")
                .join("preferences.json"),
            #[cfg(test)]
            RegistrySource::Custom { state_file, .. } => {
                state_file.with_file_name("preferences.json")
            }
        };
        let preferences = load_preferences(&preferences_file);
        let service = Arc::new(Self {
            agent_handle,
            lsp,
            scheduler: RwLock::new(None),
            mcp_ownership,
            integrator: PluginIntegrator::default(),
            target_scope,
            registry_source,
            preferences_file,
            state: Mutex::new(PluginRuntimeState {
                registry: PluginRegistry::new(crate::data_root::user_data_dir(), None),
                framework_components: HashMap::new(),
                framework_generation,
                framework_receipt: None,
                mcp_ownership: HashMap::new(),
                prepared: PreparedApplicationComponents::default(),
                lifecycle: PluginLifecycleManager::new(),
                cleanup_quarantine: Vec::new(),
                active_theme: preferences.active_theme,
                active_output_style: preferences.active_output_style,
                generation: authority_revision,
                shut_down: false,
            }),
            agent_pool: RwLock::new(None),
            mutation_supervisor: Mutex::new(PluginMutationSupervisor::default()),
        });
        if let Some(authority_generation) = authority_generation {
            // A cold workspace primary is created from the global pool's exact
            // committed projection. Retire it before applying the workspace's
            // full User + Project + Local prepared set so global project-only
            // descriptors cannot survive in the new target.
            service
                .agent_handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::agent_pool::remove_agent_plugin_generation(
                            agent,
                            &authority_generation,
                        )
                        .await;
                    })
                })
                .await;
        }
        service.reload().await?;
        Ok(service)
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_for_test_with_ownership(
            agent_handle,
            project_root,
            state_file,
            data_dir,
            McpNameOwnershipRegistry::new(Vec::<String>::new()),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test_with_ownership(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        let manager = Arc::new(RwLock::new(LspManager::new()));
        let target_scope = format!("test:{}", project_root.display());
        let lsp = PluginLspRuntime::new(manager, LspConfig::default(), project_root);
        Self::new_with_source(
            agent_handle,
            lsp,
            mcp_ownership,
            RegistrySource::Custom {
                state_file,
                data_dir,
                scopes: vec![PluginScope::Project, PluginScope::Local],
            },
            target_scope,
            None,
        )
        .await
    }

    async fn run_owned_mutation<T, F, Fut>(self: &Arc<Self>, operation: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let mut supervisor = self.mutation_supervisor.lock().await;
        if !supervisor.accepting {
            return Err(anyhow::anyhow!("plugin runtime is shutting down"));
        }
        let sequence_permit = Arc::clone(&supervisor.sequence)
            .acquire_owned()
            .await
            .map_err(|error| anyhow::anyhow!("plugin mutation sequence is closed: {error}"))?;
        while let Some(result) = supervisor.settlements.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(%error, "completed plugin mutation owner failed");
            }
        }
        let service = Arc::clone(self);
        supervisor.settlements.spawn(async move {
            let _sequence_permit = sequence_permit;
            let result = match tokio::spawn(operation(service)).await {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(
                    "plugin mutation task failed before settlement: {error}"
                )),
            };
            let _ = result_sender.send(result);
        });
        drop(supervisor);
        result_receiver
            .await
            .map_err(|_| anyhow::anyhow!("plugin mutation settlement task stopped unexpectedly"))?
    }

    /// Bind the process pool to the currently committed plugin generation.
    pub async fn bind_agent_pool(self: &Arc<Self>, pool: Weak<AgentPool>) -> anyhow::Result<()> {
        self.run_owned_mutation(
            move |service| async move { service.bind_agent_pool_inner(pool).await },
        )
        .await
    }

    async fn bind_agent_pool_inner(&self, pool: Weak<AgentPool>) -> anyhow::Result<()> {
        let pool_owner = pool
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("AgentPool was released before plugin binding"))?;
        if let Some(existing) = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
            && !Arc::ptr_eq(&existing, &pool_owner)
        {
            return Err(anyhow::anyhow!(
                "plugin runtime is already bound to another live AgentPool"
            ));
        }
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let generation = self
            .capture_agent_generation(
                state.generation,
                &state.prepared,
                state.active_output_style.as_deref(),
            )
            .await
            .with_framework_generation(state.framework_generation.clone());
        let mut publication = pool_owner
            .begin_plugin_publication()
            .await
            .map_err(anyhow::Error::msg)?;
        publication
            .prepare(generation)
            .await
            .map_err(anyhow::Error::msg)?;
        publication.commit().await.map_err(anyhow::Error::msg)?;
        *self.agent_pool.write().await = Some(pool);
        Ok(())
    }

    async fn capture_agent_generation(
        &self,
        revision: u64,
        prepared: &PreparedApplicationComponents,
        active_output_style: Option<&str>,
    ) -> AgentPluginGeneration {
        let descriptors = self
            .agent_handle
            .read(|agent| agent.skill_descriptors())
            .await;
        let output_style = active_output_style_instructions_for(active_output_style, prepared);
        AgentPluginGeneration::new(revision, descriptors, prepared.agents.clone(), output_style)
    }

    /// Atomically load one EKO-owned skill into the primary and pool catalog.
    /// The registry edit happens inside the same mutation owner as plugin
    /// reload/rebind, preventing a plugin generation from overwriting it.
    pub(crate) async fn enable_application_skill(
        self: &Arc<Self>,
        name: String,
        load_root: PathBuf,
        source: String,
    ) -> anyhow::Result<Vec<String>> {
        self.run_owned_mutation(move |service| async move {
            service
                .enable_application_skill_inner(&name, load_root, &source)
                .await
        })
        .await
    }

    async fn enable_application_skill_inner(
        &self,
        name: &str,
        load_root: PathBuf,
        source: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut state = self.state.lock().await;
        if state.shut_down {
            anyhow::bail!("plugin runtime is shut down");
        }
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let primary_had_skill = primary.skill_descriptors().iter().any(|descriptor| {
            descriptor.name == name && descriptor.source.as_deref() == Some(source)
        });
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if primary_had_skill && pool.is_none() {
            return Ok(vec![name.to_string()]);
        }
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let loaded = if primary_had_skill {
            vec![name.to_string()]
        } else {
            load_exact_application_skill(&mut primary, name, load_root, source).await?
        };
        if !loaded.iter().any(|loaded_name| loaded_name == name) {
            anyhow::bail!("Skill '{name}' was not discovered");
        }
        crate::runtime::configure_intent_router(&mut primary);
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            active_output_style_instructions(&state),
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication
                .prepare_application_skill(generation, name, source)
                .await
        {
            if !primary_had_skill {
                primary.unregister_skills_by_source(source).await;
                crate::runtime::configure_intent_router(&mut primary);
            }
            return Err(anyhow::Error::msg(error));
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            if !primary_had_skill {
                primary.unregister_skills_by_source(source).await;
                crate::runtime::configure_intent_router(&mut primary);
            }
            let rollback = publication.rollback().await.err();
            return Err(match rollback {
                Some(rollback) => anyhow::anyhow!(
                    "Skill pool commit failed: {error}; rollback failed: {rollback}"
                ),
                None => anyhow::Error::msg(error),
            });
        }
        state.generation = revision;
        Ok(loaded)
    }

    /// Atomically remove one EKO-owned skill from primary and pool catalogs.
    pub(crate) async fn disable_application_skill(
        self: &Arc<Self>,
        name: String,
        load_root: PathBuf,
        source: String,
    ) -> anyhow::Result<Vec<String>> {
        self.run_owned_mutation(move |service| async move {
            service
                .disable_application_skill_inner(&name, load_root, &source)
                .await
        })
        .await
    }

    async fn disable_application_skill_inner(
        &self,
        name: &str,
        load_root: PathBuf,
        source: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut state = self.state.lock().await;
        if state.shut_down {
            anyhow::bail!("plugin runtime is shut down");
        }
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let primary_had_skill = primary.skill_descriptors().iter().any(|descriptor| {
            descriptor.name == name && descriptor.source.as_deref() == Some(source)
        });
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if !primary_had_skill && pool.is_none() {
            return Ok(Vec::new());
        }
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let removed = if primary_had_skill {
            primary.unregister_skills_by_source(source).await
        } else {
            Vec::new()
        };
        crate::runtime::configure_intent_router(&mut primary);
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            active_output_style_instructions(&state),
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication
                .prepare_application_skill(generation, name, source)
                .await
        {
            let restore_error = if primary_had_skill {
                let restore =
                    load_exact_application_skill(&mut primary, name, load_root.clone(), source)
                        .await
                        .err();
                crate::runtime::configure_intent_router(&mut primary);
                restore
            } else {
                None
            };
            return Err(match restore_error {
                Some(restore) => anyhow::anyhow!(
                    "Skill pool preparation failed: {error}; primary restore failed: {restore}"
                ),
                None => anyhow::Error::msg(error),
            });
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            let restore = if primary_had_skill {
                let restore =
                    load_exact_application_skill(&mut primary, name, load_root, source).await;
                crate::runtime::configure_intent_router(&mut primary);
                Some(restore)
            } else {
                None
            };
            let rollback = publication.rollback().await.err();
            let mut errors = vec![format!("Skill pool commit failed: {error}")];
            if let Some(Err(error)) = restore {
                errors.push(format!("primary restore failed: {error}"));
            }
            errors.extend(rollback.map(|error| format!("pool rollback failed: {error}")));
            return Err(anyhow::anyhow!(errors.join("; ")));
        }
        state.generation = revision;
        Ok(removed)
    }

    async fn drain_owned_mutations(&self) -> anyhow::Result<()> {
        let mut settlements = {
            let mut supervisor = self.mutation_supervisor.lock().await;
            supervisor.accepting = false;
            std::mem::take(&mut supervisor.settlements)
        };
        let mut errors = Vec::new();
        while let Some(settlement) = settlements.join_next().await {
            if let Err(error) = settlement {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "plugin mutation settlement failed: {}",
                errors.join("; ")
            ))
        }
    }

    pub(crate) async fn reload(self: &Arc<Self>) -> anyhow::Result<ReloadSummary> {
        self.run_owned_mutation(|service| async move { service.reload_inner().await })
            .await
    }

    async fn reload_inner(&self) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        self.apply_candidate(&mut state, candidate, &binding).await
    }

    /// Validate a target generation without replacing live plugin resources.
    pub async fn preflight_workspace(&self, project_root: PathBuf) -> anyhow::Result<()> {
        let binding = PluginLspBinding {
            base_config: PluginLspRuntime::config_for_workspace(&project_root),
            project_root,
        };
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let framework_generation =
            require_applicable_generation(self.integrator.prepare(&mut candidate).await)?;
        let prepared = prepare_application_components(&framework_generation, &self.target_scope)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let declarations = plugin_mcp_declarations(&framework_generation)?;
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        self.validate_agent_collisions(&state, &prepared).await?;
        let ownership_guard = self.mcp_ownership.lock().await;
        validate_plugin_mcp_claims(&ownership_guard, &declarations, &state.mcp_ownership)
            .map_err(anyhow::Error::msg)?;
        drop(ownership_guard);
        let mut lsp = self.prepare_lsp(&prepared, &binding).await?;
        lsp.shutdown_all().await;
        Ok(())
    }

    /// Replace project/local plugins and LSP processes for a committed workspace.
    /// A target failure converges to the target's User-only generation instead
    /// of clearing user-scoped plugins or retaining old workspace plugins.
    pub(crate) async fn rebind_workspace(
        self: &Arc<Self>,
        project_root: PathBuf,
    ) -> anyhow::Result<ReloadSummary> {
        self.run_owned_mutation(move |service| async move {
            service.rebind_workspace_inner(project_root).await
        })
        .await
    }

    async fn rebind_workspace_inner(&self, project_root: PathBuf) -> anyhow::Result<ReloadSummary> {
        let previous_binding = self.lsp.binding().await;
        let binding = PluginLspBinding {
            base_config: PluginLspRuntime::config_for_workspace(&project_root),
            project_root,
        };
        let mut candidate = self.registry_for(binding.project_root.clone());
        let scan = self.scan_registry(&mut candidate);
        let mut state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let quarantine_errors = self.retry_cleanup_quarantine(&mut state).await;
        let previous_workspace_plugins = workspace_scope_plugin_ids(&state.registry);
        let result = match scan {
            Ok(()) => self.apply_candidate(&mut state, candidate, &binding).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(summary) => {
                let mut retirement_errors = quarantine_errors;
                let (errors, failed_plugin_ids) =
                    retire_plugin_lifecycles(&mut state.lifecycle, &previous_workspace_plugins);
                retirement_errors.extend(errors);
                if !failed_plugin_ids.is_empty() {
                    state.cleanup_quarantine.push(PluginCleanupQuarantine {
                        root: previous_binding.project_root.clone(),
                        lifecycle: None,
                        lifecycle_plugin_ids: failed_plugin_ids,
                        monitors: Vec::new(),
                        last_errors: retirement_errors.clone(),
                    });
                }
                if retirement_errors.is_empty() {
                    Ok(summary)
                } else {
                    Err(anyhow::anyhow!(
                        "Target plugin generation committed, but previous workspace lifecycle retirement failed: {}",
                        retirement_errors.join("; ")
                    ))
                }
            }
            Err(error) => {
                let primary = error.to_string();
                let mut user_candidate = self.registry_for(binding.project_root.clone());
                let fallback =
                    match self.scan_registry_scopes(&mut user_candidate, &[PluginScope::User]) {
                        Ok(()) => self
                            .apply_candidate(&mut state, user_candidate, &binding)
                            .await
                            .map(|_| ()),
                        Err(error) => Err(error),
                    };
                match fallback {
                    Ok(()) => {
                        let mut retirement_errors = quarantine_errors;
                        let (errors, failed_plugin_ids) = retire_plugin_lifecycles(
                            &mut state.lifecycle,
                            &previous_workspace_plugins,
                        );
                        retirement_errors.extend(errors);
                        if !failed_plugin_ids.is_empty() {
                            state.cleanup_quarantine.push(PluginCleanupQuarantine {
                                root: previous_binding.project_root.clone(),
                                lifecycle: None,
                                lifecycle_plugin_ids: failed_plugin_ids,
                                monitors: Vec::new(),
                                last_errors: retirement_errors.clone(),
                            });
                        }
                        Err(anyhow::anyhow!(append_errors(
                            format!(
                                "Target workspace plugins were rejected; committed User-scope plugin generation instead: {primary}"
                            ),
                            retirement_errors,
                        )))
                    }
                    Err(fallback_error) => {
                        let retirement_errors = self
                            .retire_generation_fail_closed(
                                &mut state,
                                &binding,
                                previous_binding.project_root.clone(),
                            )
                            .await;
                        let mut all_errors = quarantine_errors;
                        all_errors.extend(retirement_errors);
                        Err(anyhow::anyhow!(append_errors(
                            format!(
                                "{primary}; failed to converge target User-scope plugin generation: {fallback_error}; retired all plugin-owned components fail-closed, including degraded User-scope plugins"
                            ),
                            all_errors,
                        )))
                    }
                }
            }
        }
    }

    async fn retire_generation_fail_closed(
        &self,
        state: &mut PluginRuntimeState,
        binding: &PluginLspBinding,
        previous_root: PathBuf,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            match pool.begin_plugin_publication().await {
                Ok(publication) => Some(publication),
                Err(error) => {
                    errors.push(format!(
                        "Failed to close AgentPool admission for fail-closed plugin retirement: {error}"
                    ));
                    return errors;
                }
            }
        } else {
            None
        };
        let previous_prepared = std::mem::take(&mut state.prepared);
        let mut failed_monitors = Vec::new();
        if let Some(scheduler) = self.scheduler.read().await.clone() {
            let monitor_errors =
                remove_plugin_monitors_best_effort(&scheduler, &previous_prepared.monitors).await;
            if !monitor_errors.is_empty() {
                failed_monitors = previous_prepared.monitors.clone();
                errors.extend(monitor_errors);
            }
        }

        let mut previous_lifecycle =
            std::mem::replace(&mut state.lifecycle, PluginLifecycleManager::new());
        let lifecycle_errors = previous_lifecycle.shutdown();
        let quarantine_lifecycle = !lifecycle_errors.is_empty();
        errors.extend(lifecycle_errors);
        if quarantine_lifecycle || !failed_monitors.is_empty() {
            state.cleanup_quarantine.push(PluginCleanupQuarantine {
                root: previous_root,
                lifecycle: Some(previous_lifecycle),
                lifecycle_plugin_ids: Vec::new(),
                monitors: failed_monitors,
                last_errors: errors.clone(),
            });
        }

        state.framework_components.clear();
        state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let mut ownership_guard = self.mcp_ownership.lock().await;
        if let Some(receipt) = previous_framework_receipt.as_ref() {
            self.integrator.rollback(&mut primary, receipt).await;
        }
        unload_application_components(&mut primary, &previous_prepared).await;
        primary
            .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
            .await;
        crate::runtime::configure_intent_router(&mut primary);
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);
        drop(ownership_guard);

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, LspManager::new())
        };
        previous_lsp.shutdown_all().await;
        self.lsp.publish_binding(binding.clone()).await;

        state.registry = self.registry_for(binding.project_root.clone());
        state.active_theme = None;
        state.active_output_style = None;
        match state.generation.checked_add(1) {
            Some(revision) => {
                let generation = AgentPluginGeneration::new(
                    revision,
                    primary.skill_descriptors(),
                    state.prepared.agents.clone(),
                    None,
                );
                if let Some(publication) = pool_publication.as_mut() {
                    match publication.prepare(generation).await {
                        Ok(()) => match publication.commit().await {
                            Ok(()) => state.generation = revision,
                            Err(error) => errors.push(format!(
                                "Failed to commit fail-closed AgentPool generation: {error}"
                            )),
                        },
                        Err(error) => errors.push(format!(
                            "Failed to prepare fail-closed AgentPool generation: {error}"
                        )),
                    }
                } else {
                    state.generation = revision;
                }
            }
            None => errors.push("plugin generation revision exhausted".to_string()),
        }
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: None,
                active_output_style: None,
            },
        ) {
            errors.push(format!(
                "Failed to persist fail-closed plugin preferences: {error}"
            ));
        }
        drop(pool_publication);
        drop(primary);
        errors
    }

    async fn retry_cleanup_quarantine(&self, state: &mut PluginRuntimeState) -> Vec<String> {
        let scheduler = self.scheduler.read().await.clone();
        let quarantined = std::mem::take(&mut state.cleanup_quarantine);
        let mut retry_errors = Vec::new();
        for mut debt in quarantined {
            let mut debt_errors = Vec::new();
            if let Some(lifecycle) = debt.lifecycle.as_mut() {
                debt_errors.extend(lifecycle.shutdown());
            } else {
                for plugin_id in &debt.lifecycle_plugin_ids {
                    if let Err(error) = state.lifecycle.unregister(plugin_id) {
                        debt_errors.push(error);
                    }
                }
            }

            if !debt.monitors.is_empty() {
                match scheduler.as_ref() {
                    Some(scheduler) => {
                        debt_errors.extend(
                            remove_plugin_monitors_best_effort(scheduler, &debt.monitors).await,
                        );
                    }
                    None => debt_errors.push(format!(
                        "Scheduler unavailable while retrying {} plugin monitor cleanup receipt(s)",
                        debt.monitors.len()
                    )),
                }
            }

            if debt_errors.is_empty() {
                continue;
            }
            debt.last_errors = debt_errors
                .iter()
                .map(|error| format!("{}: {error}", debt.root.display()))
                .collect();
            retry_errors.extend(debt.last_errors.clone());
            state.cleanup_quarantine.push(debt);
        }
        retry_errors
    }

    /// Roots with plugin-owned external cleanup that has not yet settled.
    pub async fn cleanup_debt_roots(&self) -> Vec<PathBuf> {
        let mut roots = self
            .state
            .lock()
            .await
            .cleanup_quarantine
            .iter()
            .map(|debt| debt.root.clone())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }

    pub async fn workspace_root(&self) -> PathBuf {
        self.lsp.binding().await.project_root
    }

    /// Opaque identity of the prepared framework generation currently exposed
    /// by this target.
    pub(crate) async fn prepared_generation_identity(&self) -> String {
        let state = self.state.lock().await;
        state
            .framework_generation
            .as_ref()
            .map(|generation| generation.identity().to_string())
            .unwrap_or_else(|| format!("unprepared:{}", state.generation))
    }

    pub(crate) async fn mcp_reconcile_target(
        &self,
    ) -> crate::mcp_config_runtime::McpReconcileTarget {
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        crate::mcp_config_runtime::McpReconcileTarget::new(
            self.agent_handle.clone(),
            Arc::clone(&self.mcp_ownership),
            pool,
        )
    }

    pub async fn lsp_configured_languages(&self) -> Vec<String> {
        let _state = self.state.lock().await;
        let manager = self.lsp.manager.read().await;
        let mut languages = manager
            .configured_languages()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        languages.sort();
        languages
    }

    pub async fn lsp_status(&self) -> Vec<LspServerStatus> {
        let _state = self.state.lock().await;
        let manager = self.lsp.manager.read().await;
        let mut statuses = manager.status_all().await;
        statuses.sort_by(|left, right| left.language.cmp(&right.language));
        statuses
    }

    pub(crate) async fn lsp_start(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            let mut manager = service.lsp.manager.write().await;
            if manager.get_client(&language).is_some() {
                return Err(anyhow::anyhow!(
                    "language server '{language}' is already running"
                ));
            }
            manager
                .start_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    pub(crate) async fn lsp_stop(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            service
                .lsp
                .manager
                .write()
                .await
                .stop_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    pub(crate) async fn lsp_restart(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            let mut manager = service.lsp.manager.write().await;
            manager
                .stop_server(&language)
                .await
                .map_err(anyhow::Error::msg)?;
            manager
                .start_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Rebuild only the LSP manager for this exact target. Config-file changes
    /// must not rescan plugins or republish Skills, MCP, Subagents and monitors.
    pub(crate) async fn reload_lsp_generation(
        self: &Arc<Self>,
        project_root: PathBuf,
    ) -> anyhow::Result<usize> {
        self.run_owned_mutation(move |service| async move {
            let state = service.state.lock().await;
            if state.shut_down {
                return Err(anyhow::anyhow!("plugin runtime is shut down"));
            }
            let current = service.lsp.binding().await;
            if current.project_root != project_root {
                anyhow::bail!(
                    "LSP target root changed from '{}' to '{}'",
                    current.project_root.display(),
                    project_root.display()
                );
            }
            let binding = PluginLspBinding {
                base_config: PluginLspRuntime::config_for_workspace(&project_root),
                project_root,
            };
            let replacement = service.prepare_lsp(&state.prepared, &binding).await?;
            let configured = replacement.configured_languages().len();
            let mut previous = {
                let mut current = service.lsp.manager.write().await;
                std::mem::replace(&mut *current, replacement)
            };
            service.lsp.publish_binding(binding).await;
            previous.shutdown_all().await;
            drop(state);
            Ok(configured)
        })
        .await
    }

    pub async fn bind_scheduler(
        self: &Arc<Self>,
        scheduler: Arc<SchedulerRunner>,
    ) -> anyhow::Result<usize> {
        self.run_owned_mutation(move |service| async move {
            service.bind_scheduler_inner(scheduler).await
        })
        .await
    }

    async fn bind_scheduler_inner(&self, scheduler: Arc<SchedulerRunner>) -> anyhow::Result<usize> {
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let monitors = state.prepared.monitors.clone();
        let mut slot = self.scheduler.write().await;
        if slot.is_some() {
            return Ok(monitors.len());
        }
        replace_plugin_monitors(&scheduler, &[], &monitors).await?;
        *slot = Some(scheduler);
        Ok(monitors.len())
    }

    /// Release all plugin-owned resources. Repeated calls are harmless.
    pub async fn shutdown(self: &Arc<Self>) -> anyhow::Result<()> {
        let settlement_error = self.drain_owned_mutations().await.err();
        let mut state = self.state.lock().await;
        let mut errors = self.retry_cleanup_quarantine(&mut state).await;
        if state.shut_down {
            errors.extend(state.lifecycle.shutdown());
            errors.extend(settlement_error.map(|error| error.to_string()));
            return if errors.is_empty() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Plugin runtime shutdown retry failed: {}",
                    errors.join("; ")
                ))
            };
        }

        errors.extend(state.lifecycle.shutdown());
        errors.extend(settlement_error.map(|error| error.to_string()));
        state.framework_components.clear();
        state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let previous_prepared = std::mem::take(&mut state.prepared);
        if !previous_prepared.monitors.is_empty()
            && let Some(scheduler) = self.scheduler.read().await.clone()
        {
            let monitor_errors =
                remove_plugin_monitors_best_effort(&scheduler, &previous_prepared.monitors).await;
            if !monitor_errors.is_empty() {
                let root = self.lsp.binding().await.project_root;
                state.cleanup_quarantine.push(PluginCleanupQuarantine {
                    root,
                    lifecycle: None,
                    lifecycle_plugin_ids: Vec::new(),
                    monitors: previous_prepared.monitors.clone(),
                    last_errors: monitor_errors.clone(),
                });
                errors.extend(monitor_errors);
            }
        }

        let mut ownership_guard = self.mcp_ownership.lock().await;
        let integrator = self.integrator.clone();
        self.agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    if let Some(receipt) = previous_framework_receipt.as_ref() {
                        integrator.rollback(agent, receipt).await;
                    }
                    unload_application_components(agent, &previous_prepared).await;
                    agent
                        .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                        .await;
                })
            })
            .await;
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, LspManager::new())
        };
        previous_lsp.shutdown_all().await;

        let project_root = self.lsp.binding().await.project_root;
        state.registry = self.registry_for(project_root);
        state.active_theme = None;
        state.active_output_style = None;

        if errors.is_empty() {
            state.shut_down = true;
            Ok(())
        } else {
            // Mutation admission is already closed by drain_owned_mutations,
            // but keep the runtime unsettled so a later shutdown retries the
            // retained lifecycle/monitor cleanup receipts.
            state.shut_down = false;
            Err(anyhow::anyhow!(
                "Plugin runtime shutdown failed: {}",
                errors.join("; ")
            ))
        }
    }

    pub(crate) async fn enable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move { service.enable_inner(&name).await })
            .await
    }

    async fn enable_inner(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .enable(name)
            .map_err(|error| anyhow::anyhow!("Enable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.restore_enabled_state(name, previously_enabled).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn disable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move { service.disable_inner(&name).await })
            .await
    }

    async fn disable_inner(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .disable(name)
            .map_err(|error| anyhow::anyhow!("Disable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => {
                self.fire_plugin_disabled(name).await;
                Ok(summary)
            }
            Err(error) => {
                self.restore_enabled_state(name, previously_enabled).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn install(
        self: &Arc<Self>,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let source = source.clone();
        self.run_owned_mutation(move |service| async move {
            service.install_inner(&source, scope).await
        })
        .await
    }

    async fn install_inner(
        &self,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let plugin_id = candidate
            .install(source, scope)
            .map_err(|error| anyhow::anyhow!("Install plugin failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok((plugin_id, summary)),
            Err(error) => {
                self.rollback_install(&plugin_id).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn uninstall(
        self: &Arc<Self>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.uninstall_inner(&name, keep_data).await
        })
        .await
    }

    async fn uninstall_inner(&self, name: &str, keep_data: bool) -> anyhow::Result<ReloadSummary> {
        let was_enabled = self
            .get(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        let mut summary = if was_enabled {
            self.disable_inner(name).await?
        } else {
            let state = self.state.lock().await;
            ReloadSummary {
                total: state.registry.count(),
                enabled: state.registry.list_enabled().len(),
                skills_loaded: state
                    .framework_components
                    .values()
                    .map(|components| components.skills.len())
                    .sum(),
                hooks_registered: state
                    .framework_components
                    .values()
                    .filter(|components| components.hooks_registered)
                    .count(),
                mcp_connected: state
                    .framework_components
                    .values()
                    .map(|components| components.mcp_servers.len())
                    .sum(),
                agents_loaded: state.prepared.agents.len(),
                lsp_languages_loaded: state
                    .prepared
                    .lsp_configs
                    .iter()
                    .map(|(_, config)| config.servers.len())
                    .sum(),
                monitors_loaded: state.prepared.monitors.len(),
                themes_loaded: state.prepared.themes.len(),
                output_styles_loaded: state.prepared.output_styles.len(),
                errors: Vec::new(),
            }
        };
        let mut state = self.state.lock().await;
        state
            .registry
            .uninstall(name, keep_data)
            .map_err(|error| anyhow::anyhow!("Uninstall plugin '{name}' failed: {error}"))?;
        let lifecycle_error = state.lifecycle.unregister(name).err();
        summary.total = state.registry.count();
        summary.enabled = state.registry.list_enabled().len();
        if !was_enabled {
            self.fire_plugin_disabled(name).await;
        }
        match lifecycle_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(summary),
        }
    }

    pub async fn list(&self) -> Vec<PluginEntry> {
        self.state
            .lock()
            .await
            .registry
            .list()
            .into_iter()
            .cloned()
            .collect()
    }

    #[cfg(test)]
    pub(crate) async fn generation_for_test(&self) -> u64 {
        self.state.lock().await.generation
    }

    pub async fn get(&self, name: &str) -> Option<PluginEntry> {
        self.state.lock().await.registry.get(name).cloned()
    }

    pub(crate) async fn configure(
        self: &Arc<Self>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.configure_inner(&name, values).await
        })
        .await
    }

    async fn configure_inner(
        &self,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previous = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .user_config
            .clone();
        candidate
            .configure(name, values)
            .map_err(|error| anyhow::anyhow!("Configure plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.restore_plugin_config(name, previous).await;
                Err(error)
            }
        }
    }

    /// Register native lifecycle callbacks and synchronize them immediately.
    pub async fn register_lifecycle(
        self: &Arc<Self>,
        name: &str,
        callbacks: Arc<dyn PluginLifecycle>,
    ) -> anyhow::Result<()> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.register_lifecycle_inner(&name, callbacks).await
        })
        .await
    }

    async fn register_lifecycle_inner(
        &self,
        name: &str,
        callbacks: Arc<dyn PluginLifecycle>,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let enabled = state
            .registry
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        state
            .lifecycle
            .register(name, callbacks)
            .map_err(anyhow::Error::msg)?;
        if enabled && let Err(error) = state.lifecycle.activate(name) {
            let cleanup_error = state.lifecycle.unregister(name).err();
            return Err(anyhow::anyhow!(append_errors(
                error,
                cleanup_error.into_iter().collect(),
            )));
        }
        Ok(())
    }

    pub async fn themes(&self) -> Vec<PluginThemeDefinition> {
        self.state.lock().await.prepared.themes.clone()
    }

    pub async fn active_theme(&self) -> Option<String> {
        self.state.lock().await.active_theme.clone()
    }

    pub(crate) async fn activate_theme(
        self: &Arc<Self>,
        name: Option<&str>,
    ) -> anyhow::Result<Option<PluginThemeDefinition>> {
        let name = name.map(str::to_string);
        self.run_owned_mutation(move |service| async move {
            service.activate_theme_inner(name.as_deref()).await
        })
        .await
    }

    async fn activate_theme_inner(
        &self,
        name: Option<&str>,
    ) -> anyhow::Result<Option<PluginThemeDefinition>> {
        let mut state = self.state.lock().await;
        let theme = match name {
            Some(name) => Some(
                state
                    .prepared
                    .themes
                    .iter()
                    .find(|theme| theme.name == name)
                    .ok_or_else(|| anyhow::anyhow!("Theme '{name}' not found"))?
                    .clone(),
            ),
            None => None,
        };
        let selected = name.map(str::to_string);
        persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: selected.clone(),
                active_output_style: state.active_output_style.clone(),
            },
        )?;
        state.active_theme = selected;
        Ok(theme)
    }

    pub async fn output_styles(&self) -> Vec<PluginOutputStyle> {
        self.state.lock().await.prepared.output_styles.clone()
    }

    pub async fn active_output_style(&self) -> Option<String> {
        self.state.lock().await.active_output_style.clone()
    }

    pub(crate) async fn activate_output_style(
        self: &Arc<Self>,
        name: Option<&str>,
    ) -> anyhow::Result<()> {
        let name = name.map(str::to_string);
        self.run_owned_mutation(move |service| async move {
            service.activate_output_style_inner(name.as_deref()).await
        })
        .await
    }

    async fn activate_output_style_inner(&self, name: Option<&str>) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let instructions = match name {
            Some(name) => Some(
                state
                    .prepared
                    .output_styles
                    .iter()
                    .find(|style| style.name == name)
                    .ok_or_else(|| anyhow::anyhow!("Output style '{name}' not found"))?
                    .instructions
                    .clone(),
            ),
            None => None,
        };
        let selected = name.map(str::to_string);
        let previous_selected = state.active_output_style.clone();
        let previous = active_output_style_instructions(&state);
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        primary
            .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions.clone())
            .await;
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            instructions,
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.prepare(generation).await
        {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous.clone())
                .await;
            return Err(anyhow::Error::msg(error));
        }
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: selected.clone(),
            },
        ) {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous.clone())
                .await;
            let rollback = match pool_publication.as_mut() {
                Some(publication) => publication.rollback().await.err(),
                None => None,
            };
            return Err(match rollback {
                Some(rollback) => anyhow::anyhow!(
                    "Output-style persistence failed: {error}; pool rollback failed: {rollback}"
                ),
                None => error,
            });
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous)
                .await;
            let rollback = publication.rollback().await.err();
            let preference_rollback = persist_preferences(
                &self.preferences_file,
                &PluginPreferences {
                    active_theme: state.active_theme.clone(),
                    active_output_style: previous_selected,
                },
            )
            .err();
            let mut errors = vec![format!("Output-style pool commit failed: {error}")];
            errors.extend(rollback.map(|error| format!("pool rollback failed: {error}")));
            errors.extend(
                preference_rollback.map(|error| format!("preference rollback failed: {error}")),
            );
            return Err(anyhow::anyhow!(errors.join("; ")));
        }
        state.generation = revision;
        state.active_output_style = selected;
        Ok(())
    }

    pub fn scaffold(
        directory: impl AsRef<Path>,
        name: &str,
    ) -> anyhow::Result<PluginScaffoldResult> {
        let directory = directory.as_ref();
        let name = name.trim();
        validate_plugin_name(name)?;
        if directory.exists() {
            return Err(anyhow::anyhow!(
                "Plugin scaffold target already exists: {}",
                directory.display()
            ));
        }

        std::fs::create_dir_all(directory).map_err(|error| {
            anyhow::anyhow!(
                "Failed to create plugin directory '{}': {error}",
                directory.display()
            )
        })?;
        let result = write_scaffold(directory, name);
        if let Err(error) = result {
            let cleanup = std::fs::remove_dir_all(directory).err();
            return Err(match cleanup {
                Some(cleanup) => anyhow::anyhow!(
                    "{error}; failed to roll back scaffold '{}': {cleanup}",
                    directory.display()
                ),
                None => error,
            });
        }
        Ok(PluginScaffoldResult {
            path: directory.to_path_buf(),
            name: name.to_string(),
        })
    }

    pub fn validate(directory: impl AsRef<Path>) -> PluginValidationReport {
        let directory = directory.as_ref();
        match PluginRegistry::validate_plugin_dir(directory) {
            Ok((manifest, resolved)) => {
                let defaults = manifest
                    .config
                    .iter()
                    .filter_map(|(name, entry)| {
                        entry.default.clone().map(|value| (name.clone(), value))
                    })
                    .collect::<HashMap<_, _>>();
                let project_dir =
                    std::env::current_dir().unwrap_or_else(|_| directory.to_path_buf());
                let variables = echo_agent::plugin::PluginVariables::new(
                    directory.to_path_buf(),
                    std::env::temp_dir(),
                    project_dir,
                )
                .with_json_user_config(&defaults);
                let errors = validate_application_component_files(
                    &manifest.name,
                    directory,
                    &resolved,
                    &variables,
                );
                let components = component_names(directory, &resolved);
                PluginValidationReport {
                    valid: errors.is_empty(),
                    name: Some(manifest.name),
                    components,
                    errors,
                }
            }
            Err(errors) => PluginValidationReport {
                valid: false,
                name: None,
                components: Vec::new(),
                errors,
            },
        }
    }

    async fn apply_candidate(
        &self,
        state: &mut PluginRuntimeState,
        mut candidate: PluginRegistry,
        binding: &PluginLspBinding,
    ) -> anyhow::Result<ReloadSummary> {
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let candidate_revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let framework_generation =
            require_applicable_generation(self.integrator.prepare(&mut candidate).await)?;
        let candidate_plugins = candidate
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<Vec<_>>();
        let previous_plugins = state
            .registry
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<Vec<_>>();
        let prepared = prepare_application_components(&framework_generation, &self.target_scope)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let candidate_mcp_declarations = plugin_mcp_declarations(&framework_generation)?;
        self.validate_agent_collisions(state, &prepared).await?;
        let mut replacement_lsp = self.prepare_lsp(&prepared, binding).await?;

        // Publication lock order: primary execution, primary agent write,
        // then pool transition/agents. No primary or pooled turn can observe
        // a half-published skill/Subagent/router catalog.
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            match pool.begin_plugin_publication().await {
                Ok(publication) => Some(publication),
                Err(error) => {
                    replacement_lsp.shutdown_all().await;
                    return Err(anyhow::anyhow!(
                        "Failed to close AgentPool plugin publication admission: {error}"
                    ));
                }
            }
        } else {
            None
        };

        let deactivate_errors = state.lifecycle.deactivate_all();
        if !deactivate_errors.is_empty() {
            let mut errors = deactivate_errors;
            errors.extend(
                state
                    .lifecycle
                    .activate_enabled(previous_plugins.iter().map(String::as_str)),
            );
            replacement_lsp.shutdown_all().await;
            return Err(anyhow::anyhow!(
                "Plugin lifecycle deactivation failed: {}",
                errors.join("; ")
            ));
        }

        let scheduler = self.scheduler.read().await.clone();
        if let Some(scheduler) = scheduler.as_ref()
            && let Err(error) =
                replace_plugin_monitors(scheduler, &state.prepared.monitors, &prepared.monitors)
                    .await
        {
            replacement_lsp.shutdown_all().await;
            let mut errors = vec![error.to_string()];
            errors.extend(
                state
                    .lifecycle
                    .activate_enabled(previous_plugins.iter().map(String::as_str)),
            );
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        let previous_registry = std::mem::replace(
            &mut state.registry,
            self.registry_for(binding.project_root.clone()),
        );
        let previous_framework = std::mem::take(&mut state.framework_components);
        let previous_framework_generation = state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let previous_prepared = std::mem::take(&mut state.prepared);
        let apply = self
            .replace_agent_components(
                &mut primary,
                previous_registry,
                previous_framework,
                previous_framework_generation,
                previous_framework_receipt,
                previous_mcp_ownership,
                previous_prepared,
                candidate,
                Some(framework_generation),
                candidate_mcp_declarations,
                prepared,
            )
            .await;

        let applied = match apply {
            Ok(applied) => applied,
            Err(mut failed) => {
                state.registry = failed.registry;
                state.framework_components = failed.framework_components;
                state.framework_generation = failed.framework_generation;
                state.framework_receipt = failed.framework_receipt;
                state.mcp_ownership = failed.mcp_ownership;
                state.prepared = failed.prepared;
                if let Some(scheduler) = scheduler.as_ref()
                    && let Err(error) = replace_plugin_monitors(
                        scheduler,
                        &failed.candidate_monitors,
                        &state.prepared.monitors,
                    )
                    .await
                {
                    failed.error =
                        format!("{}; rollback plugin monitors failed: {error}", failed.error);
                }
                replacement_lsp.shutdown_all().await;
                failed.error = append_errors(
                    failed.error,
                    state
                        .lifecycle
                        .activate_enabled(previous_plugins.iter().map(String::as_str)),
                );
                return Err(anyhow::anyhow!(failed.error));
            }
        };

        let candidate_generation = AgentPluginGeneration::new(
            candidate_revision,
            primary.skill_descriptors(),
            applied.prepared.agents.clone(),
            active_output_style_instructions_for(
                state.active_output_style.as_deref(),
                &applied.prepared,
            ),
        )
        .with_framework_generation(applied.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(pool_error) = publication.prepare(candidate_generation).await
        {
            let candidate_monitors = applied.prepared.monitors.clone();
            let previous_monitors = applied.previous_prepared.monitors.clone();
            let candidate_framework = applied
                .wiring
                .as_ref()
                .map(|receipt| receipt.components_by_plugin.clone())
                .unwrap_or_default();
            let rollback = self
                .replace_agent_components(
                    &mut primary,
                    applied.registry,
                    candidate_framework,
                    applied.framework_generation,
                    applied.wiring,
                    applied.mcp_ownership,
                    applied.prepared,
                    applied.previous_registry,
                    applied.previous_framework_generation,
                    applied.previous_mcp_declarations,
                    applied.previous_prepared,
                )
                .await;
            let mut errors = vec![format!(
                "AgentPool plugin generation publication failed: {pool_error}"
            )];
            match rollback {
                Ok(restored) => {
                    if let Some(scheduler) = scheduler.as_ref()
                        && let Err(error) = replace_plugin_monitors(
                            scheduler,
                            &candidate_monitors,
                            &previous_monitors,
                        )
                        .await
                    {
                        errors.push(format!("rollback plugin monitors failed: {error}"));
                    }
                    state.registry = restored.registry;
                    state.framework_components = restored
                        .wiring
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default();
                    state.framework_generation = restored.framework_generation;
                    state.framework_receipt = restored.wiring;
                    state.mcp_ownership = restored.mcp_ownership;
                    state.prepared = restored.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(previous_plugins.iter().map(String::as_str)),
                    );
                }
                Err(failed) => {
                    errors.push(format!(
                        "rollback agent components failed: {}",
                        failed.error
                    ));
                    state.registry = failed.registry;
                    state.framework_components = failed.framework_components;
                    state.framework_generation = failed.framework_generation;
                    state.framework_receipt = failed.framework_receipt;
                    state.mcp_ownership = failed.mcp_ownership;
                    state.prepared = failed.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(candidate_plugins.iter().map(String::as_str)),
                    );
                }
            }
            replacement_lsp.shutdown_all().await;
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, replacement_lsp)
        };

        let activation_errors = state
            .lifecycle
            .activate_enabled(candidate_plugins.iter().map(String::as_str));
        if !activation_errors.is_empty() {
            let mut errors = vec![format!(
                "Plugin lifecycle activation failed: {}",
                activation_errors.join("; ")
            )];
            errors.extend(state.lifecycle.deactivate_all());

            let candidate_monitors = applied.prepared.monitors.clone();
            let previous_monitors = applied.previous_prepared.monitors.clone();
            let candidate_framework = applied
                .wiring
                .as_ref()
                .map(|receipt| receipt.components_by_plugin.clone())
                .unwrap_or_default();
            let rollback = self
                .replace_agent_components(
                    &mut primary,
                    applied.registry,
                    candidate_framework,
                    applied.framework_generation,
                    applied.wiring,
                    applied.mcp_ownership,
                    applied.prepared,
                    applied.previous_registry,
                    applied.previous_framework_generation,
                    applied.previous_mcp_declarations,
                    applied.previous_prepared,
                )
                .await;
            match rollback {
                Ok(restored) => {
                    if let Some(scheduler) = scheduler.as_ref()
                        && let Err(error) = replace_plugin_monitors(
                            scheduler,
                            &candidate_monitors,
                            &previous_monitors,
                        )
                        .await
                    {
                        errors.push(format!("rollback plugin monitors failed: {error}"));
                    }
                    {
                        let mut current = self.lsp.manager.write().await;
                        let mut candidate_lsp = std::mem::replace(&mut *current, previous_lsp);
                        candidate_lsp.shutdown_all().await;
                    }
                    state.registry = restored.registry;
                    state.framework_components = restored
                        .wiring
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default();
                    state.framework_generation = restored.framework_generation;
                    state.framework_receipt = restored.wiring;
                    state.mcp_ownership = restored.mcp_ownership;
                    state.prepared = restored.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(previous_plugins.iter().map(String::as_str)),
                    );
                }
                Err(failed) => {
                    errors.push(format!(
                        "rollback agent components failed: {}",
                        failed.error
                    ));
                    previous_lsp.shutdown_all().await;
                    state.registry = failed.registry;
                    state.framework_components = failed.framework_components;
                    state.framework_generation = failed.framework_generation;
                    state.framework_receipt = failed.framework_receipt;
                    state.mcp_ownership = failed.mcp_ownership;
                    state.prepared = failed.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(candidate_plugins.iter().map(String::as_str)),
                    );
                }
            }
            if let Some(publication) = pool_publication.as_mut()
                && let Err(error) = publication.rollback().await
            {
                errors.push(error);
            }
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        if let Some(publication) = pool_publication.as_mut() {
            publication.commit().await.map_err(anyhow::Error::msg)?;
        }

        previous_lsp.shutdown_all().await;
        self.lsp.publish_binding(binding.clone()).await;

        let active_style = state.active_output_style.clone();
        let active_theme = state.active_theme.clone();
        state.registry = applied.registry;
        state.framework_components = applied
            .wiring
            .as_ref()
            .map(|receipt| receipt.components_by_plugin.clone())
            .unwrap_or_default();
        state.framework_generation = applied.framework_generation;
        state.framework_receipt = applied.wiring;
        state.mcp_ownership = applied.mcp_ownership;
        state.prepared = applied.prepared;
        state.generation = candidate_revision;
        if let Some(style) = active_style {
            if state
                .prepared
                .output_styles
                .iter()
                .any(|candidate| candidate.name == style)
            {
                let instructions = state
                    .prepared
                    .output_styles
                    .iter()
                    .find(|candidate| candidate.name == style)
                    .map(|candidate| candidate.instructions.clone());
                primary
                    .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions)
                    .await;
            } else {
                state.active_output_style = None;
                primary
                    .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                    .await;
            }
        }

        if let Some(theme) = active_theme
            && !state
                .prepared
                .themes
                .iter()
                .any(|candidate| candidate.name == theme)
        {
            state.active_theme = None;
        }

        let total = state.registry.count();
        let enabled = state.registry.list_enabled().len();
        let mut summary = ReloadSummary::from_components(
            total,
            enabled,
            state.framework_receipt.as_ref(),
            state.framework_generation.as_deref(),
            &state.prepared,
        );
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: state.active_output_style.clone(),
            },
        ) {
            summary.errors.push(error.to_string());
        }
        drop(pool_publication);
        drop(primary);
        self.fire_loaded_events(&candidate_plugins).await;
        tracing::info!(
            total,
            enabled,
            agents = summary.agents_loaded,
            lsp = summary.lsp_languages_loaded,
            monitors = summary.monitors_loaded,
            themes = summary.themes_loaded,
            output_styles = summary.output_styles_loaded,
            "plugin runtime replaced atomically"
        );
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    async fn replace_agent_components(
        &self,
        agent: &mut echo_agent::agent::react::ReactAgent,
        previous_registry: PluginRegistry,
        previous_framework: HashMap<String, WiredPluginComponents>,
        previous_framework_generation: Option<Arc<PreparedPluginSet>>,
        previous_framework_receipt: Option<PluginWiringResult>,
        previous_mcp_ownership: PluginMcpOwnership,
        previous_prepared: PreparedApplicationComponents,
        candidate: PluginRegistry,
        candidate_framework_generation: Option<Arc<PreparedPluginSet>>,
        candidate_mcp_declarations: PluginMcpDeclarations,
        candidate_prepared: PreparedApplicationComponents,
    ) -> Result<AppliedAgentComponents, FailedAgentComponents> {
        let candidate_monitors = candidate_prepared.monitors.clone();
        let previous_mcp_declarations = match previous_framework_generation
            .as_deref()
            .map(plugin_mcp_declarations)
            .transpose()
        {
            Ok(declarations) => declarations.unwrap_or_default(),
            Err(error) => {
                return Err(FailedAgentComponents {
                    error: format!("Failed to inspect prepared plugin MCP receipts: {error}"),
                    registry: previous_registry,
                    framework_components: previous_framework,
                    framework_generation: previous_framework_generation,
                    framework_receipt: previous_framework_receipt,
                    mcp_ownership: previous_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                });
            }
        };
        let mut ownership_guard = self.mcp_ownership.lock().await;
        if let Err(error) = validate_plugin_mcp_claims(
            &ownership_guard,
            &candidate_mcp_declarations,
            &previous_mcp_ownership,
        ) {
            return Err(FailedAgentComponents {
                error,
                registry: previous_registry,
                framework_components: previous_framework,
                framework_generation: previous_framework_generation,
                framework_receipt: previous_framework_receipt,
                mcp_ownership: previous_mcp_ownership,
                prepared: previous_prepared,
                candidate_monitors,
            });
        }

        if let Some(receipt) = previous_framework_receipt.as_ref() {
            self.integrator.rollback(agent, receipt).await;
        }
        unload_application_components(agent, &previous_prepared).await;
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);
        let candidate_mcp_ownership = match claim_plugin_mcp_names(
            &mut ownership_guard,
            &candidate_mcp_declarations,
        ) {
            Ok(ownership) => ownership,
            Err(error) => {
                let restored_mcp_ownership = match claim_plugin_mcp_names(
                    &mut ownership_guard,
                    &previous_mcp_declarations,
                ) {
                    Ok(ownership) => ownership,
                    Err(restore_error) => {
                        return Err(FailedAgentComponents {
                            error: format!(
                                "{error}; rollback MCP ownership failed: {restore_error}"
                            ),
                            registry: previous_registry,
                            framework_components: HashMap::new(),
                            framework_generation: None,
                            framework_receipt: None,
                            mcp_ownership: HashMap::new(),
                            prepared: PreparedApplicationComponents::default(),
                            candidate_monitors,
                        });
                    }
                };
                let restored = match previous_framework_generation.as_deref() {
                    Some(generation) => {
                        match self.integrator.wire_prepared(agent, generation).await {
                            Ok(receipt) => Some(receipt),
                            Err(restore_error) => {
                                return Err(FailedAgentComponents {
                                    error: format!(
                                        "{error}; rollback framework wiring failed: {restore_error}"
                                    ),
                                    registry: previous_registry,
                                    framework_components: HashMap::new(),
                                    framework_generation: None,
                                    framework_receipt: None,
                                    mcp_ownership: restored_mcp_ownership,
                                    prepared: PreparedApplicationComponents::default(),
                                    candidate_monitors,
                                });
                            }
                        }
                    }
                    None => None,
                };
                let restore_agent_error = register_plugin_agents(agent, &previous_prepared.agents)
                    .await
                    .err();
                crate::runtime::configure_intent_router(agent);
                let mut errors = vec![error];
                if let Some(error) = restore_agent_error {
                    errors.push(format!("rollback Subagent wiring failed: {error}"));
                }
                return Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry: previous_registry,
                    framework_components: restored
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default(),
                    framework_generation: previous_framework_generation,
                    framework_receipt: restored,
                    mcp_ownership: restored_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                });
            }
        };

        let wiring = match candidate_framework_generation.as_deref() {
            Some(generation) => self
                .integrator
                .wire_prepared(agent, generation)
                .await
                .map(Some)
                .map_err(|error| error.to_string()),
            None => Ok(None),
        };
        let candidate_outcome = match wiring {
            Ok(wiring) => match register_plugin_agents(agent, &candidate_prepared.agents).await {
                Ok(_) => {
                    crate::runtime::configure_intent_router(agent);
                    Ok((candidate, wiring))
                }
                Err(error) => {
                    if let Some(receipt) = wiring.as_ref() {
                        self.integrator.rollback(agent, receipt).await;
                    }
                    unload_application_components(agent, &candidate_prepared).await;
                    Err((
                        format!("Plugin Subagent registration failed: {error}"),
                        candidate,
                    ))
                }
            },
            Err(error) => Err((format!("Plugin wiring failed: {error}"), candidate)),
        };

        match candidate_outcome {
            Ok((registry, wiring)) => Ok(AppliedAgentComponents {
                registry,
                wiring,
                framework_generation: candidate_framework_generation,
                mcp_ownership: candidate_mcp_ownership,
                prepared: candidate_prepared,
                previous_registry,
                previous_framework_generation,
                previous_mcp_declarations,
                previous_prepared,
            }),
            Err((error, _candidate_registry)) => {
                release_plugin_mcp_claims(&mut ownership_guard, &candidate_mcp_ownership);
                let restored_mcp_ownership = match claim_plugin_mcp_names(
                    &mut ownership_guard,
                    &previous_mcp_declarations,
                ) {
                    Ok(ownership) => ownership,
                    Err(restore_error) => {
                        return Err(FailedAgentComponents {
                            error: format!(
                                "{error}; rollback MCP ownership failed: {restore_error}"
                            ),
                            registry: previous_registry,
                            framework_components: HashMap::new(),
                            framework_generation: None,
                            framework_receipt: None,
                            mcp_ownership: HashMap::new(),
                            prepared: PreparedApplicationComponents::default(),
                            candidate_monitors,
                        });
                    }
                };
                let restored = match previous_framework_generation.as_deref() {
                    Some(generation) => self
                        .integrator
                        .wire_prepared(agent, generation)
                        .await
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                };
                let restore_agent_error = register_plugin_agents(agent, &previous_prepared.agents)
                    .await
                    .err();
                crate::runtime::configure_intent_router(agent);
                let registry = previous_registry;
                let mut errors = vec![error];
                let restored = match restored {
                    Ok(restored) => restored,
                    Err(error) => {
                        errors.push(format!("rollback framework wiring failed: {error}"));
                        None
                    }
                };
                if let Some(error) = restore_agent_error {
                    errors.push(format!("rollback Subagent wiring failed: {error}"));
                }
                Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry,
                    framework_components: restored
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default(),
                    framework_generation: previous_framework_generation,
                    framework_receipt: restored,
                    mcp_ownership: restored_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                })
            }
        }
    }

    async fn validate_agent_collisions(
        &self,
        state: &PluginRuntimeState,
        prepared: &PreparedApplicationComponents,
    ) -> anyhow::Result<()> {
        let existing = self
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await
            .agent_names()
            .await
            .into_iter()
            .collect::<HashSet<_>>();
        let previous = state
            .prepared
            .agents
            .iter()
            .map(agent_name)
            .collect::<HashSet<_>>();
        let collisions = prepared
            .agents
            .iter()
            .map(agent_name)
            .filter(|name| existing.contains(name) && !previous.contains(name))
            .collect::<Vec<_>>();
        if collisions.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Plugin Subagent names collide with existing runtime roles: {}",
                collisions.join(", ")
            ))
        }
    }

    async fn prepare_lsp(
        &self,
        prepared: &PreparedApplicationComponents,
        binding: &PluginLspBinding,
    ) -> anyhow::Result<LspManager> {
        let mut config = binding.base_config.clone();
        let current_binding = self.lsp.binding().await;
        let mut required = if current_binding.project_root == binding.project_root {
            self.lsp
                .manager
                .read()
                .await
                .running_servers()
                .into_iter()
                .map(str::to_string)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        for (_, plugin_config) in &prepared.lsp_configs {
            required.extend(plugin_config.servers.keys().cloned());
            config.merge(plugin_config.clone());
        }
        let mut manager = LspManager::new();
        manager.load_config(&config);
        manager.set_project_root(&binding.project_root);
        let languages = manager
            .configured_languages()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        for language in languages {
            if let Err(error) = manager.start_server(&language).await {
                if required.contains(&language) {
                    manager.shutdown_all().await;
                    return Err(anyhow::anyhow!(
                        "Plugin LSP server '{language}' failed to start: {error}"
                    ));
                }
                tracing::warn!(%language, %error, "base LSP server unavailable during plugin reload");
            }
        }
        Ok(manager)
    }

    pub(crate) async fn project_root(&self) -> PathBuf {
        self.lsp.binding().await.project_root
    }

    fn registry_for(&self, project_root: PathBuf) -> PluginRegistry {
        match &self.registry_source {
            RegistrySource::Default => {
                PluginRegistry::new(crate::data_root::user_data_dir(), Some(project_root))
            }
            #[cfg(test)]
            RegistrySource::Custom {
                state_file,
                data_dir,
                ..
            } => {
                PluginRegistry::with_paths(state_file.clone(), data_dir.clone(), Some(project_root))
            }
        }
    }

    fn scan_registry(&self, registry: &mut PluginRegistry) -> anyhow::Result<()> {
        match &self.registry_source {
            RegistrySource::Default => registry.scan_all().map(|_| ()),
            #[cfg(test)]
            RegistrySource::Custom { scopes, .. } => registry.scan_scopes(scopes).map(|_| ()),
        }
        .map_err(|error| anyhow::anyhow!("Plugin scan failed: {error}"))
    }

    fn scan_registry_scopes(
        &self,
        registry: &mut PluginRegistry,
        requested: &[PluginScope],
    ) -> anyhow::Result<()> {
        let scopes = match &self.registry_source {
            RegistrySource::Default => requested.to_vec(),
            #[cfg(test)]
            RegistrySource::Custom { scopes, .. } => requested
                .iter()
                .filter(|scope| scopes.contains(scope))
                .copied()
                .collect(),
        };
        registry
            .scan_scopes(&scopes)
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("Plugin scan failed: {error}"))
    }

    async fn restore_enabled_state(&self, name: &str, enabled: bool) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok() {
            let result = if enabled {
                registry.enable(name)
            } else {
                registry.disable(name)
            };
            if let Err(error) = result {
                tracing::error!(plugin = %name, %error, "failed to roll back plugin enabled state");
            }
        }
    }

    async fn restore_plugin_config(&self, name: &str, values: HashMap<String, serde_json::Value>) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok()
            && let Err(error) = registry.configure(name, values)
        {
            tracing::error!(plugin = %name, %error, "failed to roll back plugin configuration");
        }
    }

    async fn rollback_install(&self, name: &str) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok()
            && let Err(error) = registry.uninstall(name, false)
        {
            tracing::error!(plugin = %name, %error, "failed to roll back plugin install");
        }
    }

    async fn fire_loaded_events(&self, names: &[String]) {
        let (hook_registry, session_id, agent_name) = self
            .agent_handle
            .read(|agent| {
                (
                    agent.hook_registry().clone(),
                    agent
                        .config()
                        .get_session_id()
                        .unwrap_or_default()
                        .to_string(),
                    agent.config().get_agent_name().to_string(),
                )
            })
            .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginLoaded,
            names,
            &session_id,
            &agent_name,
        )
        .await;
    }

    async fn fire_plugin_disabled(&self, name: &str) {
        let (hook_registry, session_id, agent_name) = self
            .agent_handle
            .read(|agent| {
                (
                    agent.hook_registry().clone(),
                    agent
                        .config()
                        .get_session_id()
                        .unwrap_or_default()
                        .to_string(),
                    agent.config().get_agent_name().to_string(),
                )
            })
            .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginDisabled,
            &[name.to_string()],
            &session_id,
            &agent_name,
        )
        .await;
    }
}

fn load_preferences(path: &Path) -> PluginPreferences {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|error| {
            tracing::warn!(%error, "Ignoring invalid plugin preferences");
            PluginPreferences::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PluginPreferences::default(),
        Err(error) => {
            tracing::warn!(%error, "Failed to read plugin preferences");
            PluginPreferences::default()
        }
    }
}

fn append_errors(mut primary: String, errors: Vec<String>) -> String {
    if !errors.is_empty() {
        primary.push_str("; ");
        primary.push_str(&errors.join("; "));
    }
    primary
}

fn persist_preferences(path: &Path, preferences: &PluginPreferences) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(preferences)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

struct AppliedAgentComponents {
    registry: PluginRegistry,
    wiring: Option<PluginWiringResult>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    previous_registry: PluginRegistry,
    previous_framework_generation: Option<Arc<PreparedPluginSet>>,
    previous_mcp_declarations: PluginMcpDeclarations,
    previous_prepared: PreparedApplicationComponents,
}

struct FailedAgentComponents {
    error: String,
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    framework_receipt: Option<PluginWiringResult>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    candidate_monitors: Vec<CronTask>,
}

fn active_output_style_instructions(state: &PluginRuntimeState) -> Option<String> {
    active_output_style_instructions_for(state.active_output_style.as_deref(), &state.prepared)
}

fn active_output_style_instructions_for(
    selected: Option<&str>,
    prepared: &PreparedApplicationComponents,
) -> Option<String> {
    selected.and_then(|name| {
        prepared
            .output_styles
            .iter()
            .find(|style| style.name == name)
            .map(|style| style.instructions.clone())
    })
}

async fn load_exact_application_skill(
    agent: &mut echo_agent::agent::react::ReactAgent,
    requested: &str,
    load_root: PathBuf,
    requested_source: &str,
) -> anyhow::Result<Vec<String>> {
    let loaded = agent.load_skills_from_dir(load_root).await?;
    for name in &loaded {
        let source = if name == requested {
            requested_source.to_string()
        } else {
            format!("eko:discarded-sibling-skill:{name}")
        };
        agent
            .tag_skills_source(std::slice::from_ref(name), &source)
            .await;
        if name != requested {
            agent.unregister_skills_by_source(&source).await;
        }
    }
    Ok(loaded
        .into_iter()
        .filter(|name| name == requested)
        .collect())
}

fn agent_name(agent: &crate::plugin_components::PreparedPluginAgent) -> String {
    agent.name().to_string()
}

fn plugin_mcp_declarations(
    generation: &PreparedPluginSet,
) -> anyhow::Result<PluginMcpDeclarations> {
    let mut declarations = HashMap::new();
    let mut declared_by = HashMap::<String, String>::new();

    for plugin in generation.plugins() {
        let plugin_id = plugin.id();
        let Some(config) = plugin.mcp() else {
            continue;
        };
        let mut names = config.mcp_servers.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for name in &names {
            if let Some(previous) = declared_by.insert(name.clone(), plugin_id.to_string()) {
                return Err(anyhow::anyhow!(
                    "Plugin MCP server name '{name}' is declared by both '{previous}' and '{plugin_id}'"
                ));
            }
        }
        declarations.insert(plugin_id.to_string(), names);
    }
    Ok(declarations)
}

fn require_applicable_generation(
    generation: Arc<PreparedPluginSet>,
) -> anyhow::Result<Arc<PreparedPluginSet>> {
    if generation.is_applicable() {
        return Ok(generation);
    }
    Err(anyhow::Error::new(PluginPreparationRejected {
        generation: generation.generation(),
        diagnostics: generation.diagnostics().to_vec(),
    }))
}

#[derive(Debug)]
struct PluginPreparationRejected {
    generation: u64,
    diagnostics: Vec<PluginPreparationDiagnostic>,
}

impl std::fmt::Display for PluginPreparationRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let diagnostics = self
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        write!(
            formatter,
            "prepared plugin generation {} is not applicable: {}",
            self.generation, diagnostics
        )
    }
}

impl std::error::Error for PluginPreparationRejected {}

fn validate_plugin_mcp_claims(
    guard: &McpNameOwnershipGuard,
    declarations: &PluginMcpDeclarations,
    previous: &PluginMcpOwnership,
) -> Result<(), String> {
    for (plugin_id, names) in declarations {
        for name in names {
            let previous_token = previous
                .get(plugin_id)
                .and_then(|tokens| tokens.get(name))
                .copied();
            guard.validate_plugin_claim(plugin_id, name, previous_token)?;
        }
    }
    Ok(())
}

fn release_plugin_mcp_claims(guard: &mut McpNameOwnershipGuard, ownership: &PluginMcpOwnership) {
    for (plugin_id, tokens) in ownership {
        for (name, token) in tokens {
            guard.release_plugin(plugin_id, name, *token);
        }
    }
}

fn claim_plugin_mcp_names(
    guard: &mut McpNameOwnershipGuard,
    declarations: &PluginMcpDeclarations,
) -> Result<PluginMcpOwnership, String> {
    let mut claimed: PluginMcpOwnership = HashMap::new();
    for (plugin_id, names) in declarations {
        for name in names {
            match guard.claim_plugin(plugin_id, name) {
                Ok(token) => {
                    claimed
                        .entry(plugin_id.clone())
                        .or_default()
                        .insert(name.clone(), token);
                }
                Err(error) => {
                    release_plugin_mcp_claims(guard, &claimed);
                    return Err(error);
                }
            }
        }
    }
    Ok(claimed)
}

#[cfg(test)]
fn exact_plugin_framework_receipts(
    framework: &HashMap<String, WiredPluginComponents>,
    ownership: &PluginMcpOwnership,
    guard: &McpNameOwnershipGuard,
) -> HashMap<String, WiredPluginComponents> {
    framework
        .iter()
        .map(|(plugin_id, components)| {
            let mut exact = components.clone();
            exact.mcp_servers.retain(|name| {
                ownership
                    .get(plugin_id)
                    .and_then(|tokens| tokens.get(name))
                    .is_some_and(|token| guard.owns_plugin(plugin_id, name, *token))
            });
            (plugin_id.clone(), exact)
        })
        .collect()
}

fn workspace_scope_plugin_ids(registry: &PluginRegistry) -> Vec<String> {
    let mut plugin_ids = registry
        .list()
        .into_iter()
        .filter(|entry| matches!(entry.scope, PluginScope::Project | PluginScope::Local))
        .map(|entry| entry.manifest.name.clone())
        .collect::<Vec<_>>();
    plugin_ids.sort();
    plugin_ids.dedup();

    plugin_ids
}

fn retire_plugin_lifecycles(
    lifecycle: &mut PluginLifecycleManager,
    plugin_ids: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut failed_plugin_ids = Vec::new();
    for plugin_id in plugin_ids {
        if let Err(error) = lifecycle.unregister(plugin_id) {
            errors.push(error);
            failed_plugin_ids.push(plugin_id.clone());
        }
    }
    (errors, failed_plugin_ids)
}

async fn unload_application_components(
    agent: &mut echo_agent::agent::react::ReactAgent,
    application: &PreparedApplicationComponents,
) {
    for plugin_agent in &application.agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
}

async fn replace_plugin_monitors(
    scheduler: &Arc<SchedulerRunner>,
    previous: &[CronTask],
    candidate: &[CronTask],
) -> anyhow::Result<()> {
    let mut removed = Vec::new();
    for task in previous {
        match scheduler.remove_task_exact(&task.id).await {
            Ok(true) => removed.push(task.clone()),
            Ok(false) => {}
            Err(error) => {
                let rollback_errors = rollback_plugin_monitors(scheduler, &[], &removed).await;
                return Err(monitor_replacement_error(
                    format!("Failed to remove plugin monitor '{}': {error}", task.name),
                    rollback_errors,
                ));
            }
        }
    }
    let mut added: Vec<CronTask> = Vec::new();
    for task in candidate {
        if let Err(error) = scheduler.add_task(task.clone()).await {
            let rollback_errors = rollback_plugin_monitors(scheduler, &added, &removed).await;
            return Err(monitor_replacement_error(
                format!("Failed to register plugin monitor '{}': {error}", task.name),
                rollback_errors,
            ));
        }
        added.push(task.clone());
    }
    Ok(())
}

async fn remove_plugin_monitors_best_effort(
    scheduler: &Arc<SchedulerRunner>,
    monitors: &[CronTask],
) -> Vec<String> {
    let mut errors = Vec::new();
    for monitor in monitors {
        if let Err(error) = scheduler.remove_task_exact(&monitor.id).await {
            errors.push(format!(
                "Failed to remove plugin monitor '{}': {error}",
                monitor.name
            ));
        }
    }
    errors
}

async fn rollback_plugin_monitors(
    scheduler: &Arc<SchedulerRunner>,
    added: &[CronTask],
    removed: &[CronTask],
) -> Vec<String> {
    let mut errors = Vec::new();
    for task in added.iter().rev() {
        if let Err(error) = scheduler.remove_task_exact(&task.id).await {
            errors.push(format!(
                "failed to remove candidate monitor '{}': {error}",
                task.name
            ));
        }
    }
    for task in removed {
        if let Err(error) = scheduler.add_task(task.clone()).await {
            errors.push(format!(
                "failed to restore previous monitor '{}': {error}",
                task.name
            ));
        }
    }
    errors
}

fn monitor_replacement_error(message: String, rollback_errors: Vec<String>) -> anyhow::Error {
    if rollback_errors.is_empty() {
        anyhow::anyhow!(message)
    } else {
        anyhow::anyhow!(
            "{message}; monitor rollback failed: {}",
            rollback_errors.join("; ")
        )
    }
}

async fn fire_plugin_events(
    hook_registry: &Arc<RwLock<echo_agent::skills::hooks::HookRegistry>>,
    event: echo_agent::skills::hooks::HookEvent,
    plugin_names: &[String],
    session_id: &str,
    agent_name: &str,
) {
    for plugin_name in plugin_names {
        let context = echo_agent::skills::hooks::HookContext::for_lifecycle(
            event,
            plugin_name,
            session_id,
            agent_name,
        );
        let _ = hook_registry
            .read()
            .await
            .run_lifecycle_hooks(&context)
            .await;
    }
}

fn validate_plugin_name(name: &str) -> anyhow::Result<()> {
    let length = name.chars().count();
    if !(1..=64).contains(&length)
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '.'
        })
        || name.contains("--")
        || name.contains("..")
    {
        return Err(anyhow::anyhow!(
            "Plugin name must follow the Agent Plugins lowercase name format"
        ));
    }
    Ok(())
}

fn write_scaffold(directory: &Path, name: &str) -> anyhow::Result<()> {
    let children = [
        "skills/example",
        "agents",
        "hooks",
        "themes",
        "output-styles",
        "scripts",
    ];
    for child in children {
        std::fs::create_dir_all(directory.join(child))?;
    }
    let manifest = serde_json::json!({
        "$schema": AGENT_PLUGIN_SCHEMA_V1,
        "name": name,
        "version": "0.1.0",
        "description": "EKO plugin",
        "license": "MIT",
        "displayName": name,
        "defaultEnabled": true
    });
    std::fs::write(
        directory.join("plugin.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    std::fs::write(
        directory.join("skills/example/SKILL.md"),
        format!(
            "---\nname: {name}-example\ndescription: Example skill\n---\nUse this skill for {name} tasks.\n"
        ),
    )?;
    std::fs::write(
        directory.join("agents/example.md"),
        format!(
            "---\nname: {name}-specialist\ndescription: Example plugin Subagent\nreadonly: true\n---\nHandle the assigned task carefully and return evidence.\n"
        ),
    )?;
    std::fs::write(directory.join("hooks/hooks.yaml"), "{}\n")?;
    std::fs::write(
        directory.join("mcp.json"),
        "{\n  \"$schema\": \"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json\",\n  \"mcpServers\": {}\n}\n",
    )?;
    std::fs::write(directory.join("lsp.yaml"), "languages: {}\n")?;
    std::fs::write(directory.join("monitors.yaml"), "monitors: []\n")?;
    std::fs::write(
        directory.join("themes/example.json"),
        format!(
            "{{\n  \"name\": \"{name}-dark\",\n  \"display_name\": \"{name} Dark\",\n  \"dark\": true,\n  \"colors\": {{\"accent\": \"#5b8def\"}}\n}}\n"
        ),
    )?;
    std::fs::write(
        directory.join(format!("output-styles/{name}-concise.md")),
        format!(
            "---\nname: {name}-concise\ndescription: Concise answers\n---\nAnswer directly, preserve important evidence, and avoid repetition.\n"
        ),
    )?;
    std::fs::write(
        directory.join("README.md"),
        format!("# {name}\n\nEKO plugin package.\n"),
    )?;
    Ok(())
}

fn component_names(root: &Path, resolved: &echo_agent::plugin::ResolvedComponents) -> Vec<String> {
    let mut names = Vec::new();
    if !resolved.skill_dirs.is_empty() {
        names.push("skills".to_string());
    }
    if !resolved.agent_files.is_empty() {
        names.push("agents".to_string());
    }
    if resolved.hooks_file.is_some() {
        names.push("hooks".to_string());
    }
    if resolved.mcp_config_file.is_some() {
        names.push("mcp_servers".to_string());
    }
    if resolved.lsp_config_file.is_some() {
        names.push("lsp_servers".to_string());
    }
    if let Ok(eko) = crate::plugin_components::resolve_eko_components(root) {
        if eko.monitors_file.is_some() {
            names.push("monitors".to_string());
        }
        if !eko.theme_files.is_empty() {
            names.push("themes".to_string());
        }
        if !eko.output_style_files.is_empty() {
            names.push("output_styles".to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::intent::IntentClassifier;
    use echo_agent::testing::MockLlmClient;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct LifecycleCounts {
        init: AtomicUsize,
        activate: AtomicUsize,
        deactivate: AtomicUsize,
        shutdown: AtomicUsize,
        fail_next_activation: AtomicBool,
        shutdown_failures_remaining: AtomicUsize,
    }

    struct TestLifecycle(Arc<LifecycleCounts>);

    impl PluginLifecycle for TestLifecycle {
        fn init(&self) -> Result<(), String> {
            self.0.init.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn activate(&self) -> Result<(), String> {
            self.0.activate.fetch_add(1, Ordering::SeqCst);
            if self.0.fail_next_activation.swap(false, Ordering::SeqCst) {
                Err("injected activation failure".to_string())
            } else {
                Ok(())
            }
        }

        fn deactivate(&self) -> Result<(), String> {
            self.0.deactivate.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn shutdown(&self) -> Result<(), String> {
            self.0.shutdown.fetch_add(1, Ordering::SeqCst);
            if self
                .0
                .shutdown_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                Err("injected shutdown failure".to_string())
            } else {
                Ok(())
            }
        }
    }

    fn write_fixture(root: &Path) -> Result<PathBuf, String> {
        write_fixture_at(
            root.join(".echo-agent/plugins/runtime-fixture"),
            "runtime-fixture",
        )
    }

    fn write_fixture_at(plugin: PathBuf, name: &str) -> Result<PathBuf, String> {
        PluginRuntimeService::scaffold(&plugin, name).map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("skills/example/SKILL.md"),
            format!(
                "---\nname: {name}-example\ndescription: Example skill\ntriggers:\n  - route {name} work\n---\nUse this skill for {name} tasks.\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("monitors.yaml"),
            "monitors:\n  - name: daily-review\n    cron: \"0 0 * * * *\"\n    prompt: Review pending work\n",
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        write_fake_lsp(&plugin, name)?;
        Ok(plugin)
    }

    #[cfg(unix)]
    fn write_fake_lsp(plugin: &Path, plugin_name: &str) -> Result<(), String> {
        let server = plugin.join("fake-lsp.sh");
        std::fs::write(
            &server,
            r#"#!/bin/sh
while IFS= read -r raw_line; do
  line=$(printf '%s' "$raw_line" | tr -d '\r')
  case "$line" in
    Content-Length:*) length=${line#Content-Length: } ;;
    "")
      request=$(dd bs=1 count="$length" 2>/dev/null)
      id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      if [ -n "$id" ]; then
        response=$(printf '{"jsonrpc":"2.0","id":%s,"result":{"capabilities":{}}}' "$id")
        printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
      fi
      ;;
  esac
done
"#,
        )
        .map_err(|error| error.to_string())?;
        let mut permissions = std::fs::metadata(&server)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&server, permissions).map_err(|error| error.to_string())?;

        let language = if plugin_name == "runtime-fixture" {
            "fixture".to_string()
        } else {
            format!("{plugin_name}-fixture")
        };
        let lsp = serde_yaml::to_string(&echo_agent::lsp::LspConfigFile {
            languages: HashMap::from([(
                language.clone(),
                echo_agent::lsp::LspServerConfig {
                    language,
                    command: server.display().to_string(),
                    args: Vec::new(),
                    extensions: vec![".fixture".to_string()],
                    env: HashMap::new(),
                    initialization_options: None,
                    max_restarts: 0,
                },
            )]),
        })
        .map_err(|error| error.to_string())?;
        std::fs::write(plugin.join("lsp.yaml"), lsp).map_err(|error| error.to_string())
    }

    async fn service(root: &Path) -> Result<Arc<PluginRuntimeService>, String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("plugin runtime integration test")
            .enable_tools()
            .enable_subagent()
            .register_agent_dispatch_tool()
            .working_dir(root)
            .build()
            .map_err(|error| error.to_string())?;
        PluginRuntimeService::new_for_test(
            AgentHandle::new(agent),
            root.to_path_buf(),
            root.join("registry.json"),
            root.join("plugin-data"),
        )
        .await
        .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn constructor_rejects_an_invalid_initial_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        std::fs::write(plugin.join("agents/example.md"), "not valid frontmatter")
            .map_err(|error| error.to_string())?;
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("constructor rejection test")
            .working_dir(temp.path())
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;

        let result = PluginRuntimeService::new_for_test(
            agent,
            temp.path().to_path_buf(),
            temp.path().join("registry.json"),
            temp.path().join("plugin-data"),
        )
        .await;

        assert!(result.is_err());
        Ok(())
    }

    async fn bind_test_pool(runtime: &Arc<PluginRuntimeService>) -> Result<Arc<AgentPool>, String> {
        let pool = Arc::new(
            AgentPool::new_for_test(runtime.agent_handle.clone(), None, None, 8, false).await,
        );
        runtime
            .bind_agent_pool(Arc::downgrade(&pool))
            .await
            .map_err(|error| error.to_string())?;
        Ok(pool)
    }

    fn write_application_skill(root: &Path, name: &str) -> Result<PathBuf, String> {
        let skill_root = root.join(name);
        std::fs::create_dir_all(&skill_root).map_err(|error| error.to_string())?;
        std::fs::write(
            skill_root.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Application skill replay fixture\ntriggers:\n  - use {name}\n---\nUse this skill for replay tests.\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        Ok(root.to_path_buf())
    }

    async fn agent_has_application_skill(
        handle: &AgentHandle,
        name: &str,
        source: &str,
        expected: bool,
    ) -> Result<(), String> {
        let matches = handle
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .into_iter()
                    .filter(|descriptor| {
                        descriptor.name == name && descriptor.source.as_deref() == Some(source)
                    })
                    .count()
            })
            .await;
        let exact = if expected { matches == 1 } else { matches == 0 };
        if exact {
            Ok(())
        } else {
            Err(format!(
                "application skill projection mismatch for '{name}' from '{source}': count={matches}, expected_present={expected}"
            ))
        }
    }

    async fn agent_has_plugin_generation(
        handle: &AgentHandle,
        plugin: &str,
        expected: bool,
    ) -> Result<(), String> {
        let skill = format!("{plugin}-example");
        let subagent = format!("{plugin}-specialist");
        let has_skill = handle
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .iter()
                    .any(|descriptor| descriptor.name == skill)
            })
            .await;
        let registry = handle.read(|agent| agent.subagent_registry().clone()).await;
        let has_subagent = registry.contains(&subagent).await;
        let classifier = handle.write(crate::runtime::configure_intent_router).await;
        let routed_skill = match classifier
            .classify(&format!("route {plugin} work"), &[])
            .await
        {
            echo_agent::intent::Intent::SkillRequired { skill_name, .. } => Some(skill_name),
            _ => None,
        };
        let has_route = routed_skill.as_deref() == Some(skill.as_str());
        if has_skill != expected || has_subagent != expected || has_route != expected {
            return Err(format!(
                "agent plugin generation mismatch for {plugin}: skill={has_skill}, subagent={has_subagent}, route={routed_skill:?}, expected={expected}"
            ));
        }
        Ok(())
    }

    async fn agent_has_output_style(handle: &AgentHandle, expected: bool) -> Result<(), String> {
        let messages = handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        let present = messages.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        });
        if present == expected {
            Ok(())
        } else {
            Err(format!(
                "output style projection expected={expected}, actual={present}"
            ))
        }
    }

    async fn default_service(root: &Path) -> Result<Arc<PluginRuntimeService>, String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("default plugin runtime integration test")
            .enable_tools()
            .enable_subagent()
            .register_agent_dispatch_tool()
            .working_dir(root)
            .build()
            .map_err(|error| error.to_string())?;
        let manager = Arc::new(RwLock::new(LspManager::new()));
        let lsp = PluginLspRuntime::new(
            manager,
            PluginLspRuntime::config_for_workspace(root),
            root.to_path_buf(),
        );
        PluginRuntimeService::new(
            AgentHandle::new(agent),
            lsp,
            McpNameOwnershipRegistry::new(Vec::<String>::new()),
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn wait_until_mutation_holds_state(runtime: &PluginRuntimeService) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if runtime.state.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "plugin mutation did not acquire runtime state".to_string())
    }

    #[tokio::test]
    async fn user_first_plugin_claim_is_rejected_without_a_receipt() -> Result<(), String> {
        let ownership = McpNameOwnershipRegistry::new(["shared".to_string()]);
        let mut guard = ownership.lock().await;

        let error = guard
            .claim_plugin("fixture", "shared")
            .err()
            .ok_or_else(|| "plugin unexpectedly claimed a user MCP name".to_string())?;

        assert!(error.contains("user configuration"));
        Ok(())
    }

    #[tokio::test]
    async fn plugin_first_user_takeover_invalidates_plugin_shutdown_receipt() -> Result<(), String>
    {
        let ownership = McpNameOwnershipRegistry::new(Vec::<String>::new());
        let token = {
            let mut guard = ownership.lock().await;
            guard.claim_plugin("fixture", "shared")?
        };
        let plugin_receipts = HashMap::from([(
            "fixture".to_string(),
            WiredPluginComponents {
                mcp_servers: vec!["shared".to_string()],
                ..Default::default()
            },
        )]);
        let plugin_ownership = HashMap::from([(
            "fixture".to_string(),
            HashMap::from([("shared".to_string(), token)]),
        )]);

        ownership.claim_user_names(["shared".to_string()]).await;
        let guard = ownership.lock().await;
        let shutdown_receipts =
            exact_plugin_framework_receipts(&plugin_receipts, &plugin_ownership, &guard);

        assert!(
            shutdown_receipts
                .get("fixture")
                .is_some_and(|receipt| receipt.mcp_servers.is_empty())
        );
        assert!(!guard.owns_plugin("fixture", "shared", token));
        Ok(())
    }

    #[tokio::test]
    async fn real_plugin_load_disable_and_unload_are_live() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let summary = runtime.reload().await.map_err(|error| error.to_string())?;
        assert_eq!(summary.total, 1);
        assert_eq!(summary.agents_loaded, 1);
        #[cfg(unix)]
        assert_eq!(summary.lsp_languages_loaded, 1);
        assert_eq!(summary.monitors_loaded, 1);
        assert_eq!(summary.themes_loaded, 1);
        assert_eq!(summary.output_styles_loaded, 1);
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert_eq!(runtime.themes().await.len(), 1);
        assert_eq!(runtime.output_styles().await.len(), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );

        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        assert_eq!(
            runtime
                .bind_scheduler(scheduler.clone())
                .await
                .map_err(|error| error.to_string())?,
            1
        );
        assert_eq!(scheduler.list_tasks().await.len(), 1);

        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        let projected = runtime
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(projected.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        }));

        runtime
            .disable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(runtime.themes().await.is_empty());
        assert!(runtime.output_styles().await.is_empty());
        assert!(scheduler.list_tasks().await.is_empty());
        #[cfg(unix)]
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        let projected = runtime
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(projected.iter().all(|message| {
            message
                .content
                .as_text_ref()
                .is_none_or(|content| !content.contains("Answer directly"))
        }));

        runtime
            .enable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert_eq!(scheduler.list_tasks().await.len(), 1);
        runtime
            .uninstall("runtime-fixture", false)
            .await
            .map_err(|error| error.to_string())?;
        assert!(!plugin.exists());
        assert!(runtime.list().await.is_empty());
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(scheduler.list_tasks().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn plugin_generation_reaches_primary_existing_and_future_pool_agents()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("existing-plugin-consumer")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", false).await?;

        let _plugin = write_fixture(temporary.path())?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", true).await?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        agent_has_output_style(&runtime.agent_handle, true).await?;
        agent_has_output_style(&existing, true).await?;

        let future_lease = pool
            .acquire("future-plugin-consumer")
            .await
            .map_err(|error| error.to_string())?;
        let future = future_lease.agent();
        drop(future_lease);
        agent_has_plugin_generation(&future, "runtime-fixture", true).await?;
        agent_has_output_style(&future, true).await?;
        let committed_revision = pool.plugin_generation_revision_for_test().await;

        runtime
            .disable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&future, "runtime-fixture", false).await?;
        let after_remove_lease = pool
            .acquire("after-plugin-remove")
            .await
            .map_err(|error| error.to_string())?;
        let after_remove = after_remove_lease.agent();
        drop(after_remove_lease);
        agent_has_plugin_generation(&after_remove, "runtime-fixture", false).await?;
        assert!(pool.plugin_generation_revision_for_test().await > committed_revision);
        Ok(())
    }

    #[tokio::test]
    async fn application_skill_replay_repairs_pool_split_and_future_generation()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("application-skill-existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let name = "replay-skill";
        let source = "eko:user-skill:replay-skill";
        let skill_root = write_application_skill(temporary.path(), name)?;

        runtime
            .enable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&runtime.agent_handle, name, source, true).await?;
        agent_has_application_skill(&existing, name, source, true).await?;

        let tampered_source = source.to_string();
        existing
            .write_async(|agent| {
                Box::pin(async move {
                    agent.unregister_skills_by_source(&tampered_source).await;
                    crate::runtime::configure_intent_router(agent);
                })
            })
            .await;
        agent_has_application_skill(&runtime.agent_handle, name, source, true).await?;
        agent_has_application_skill(&existing, name, source, false).await?;

        let before_enable_repair = pool.plugin_generation_revision_for_test().await;
        runtime
            .enable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&existing, name, source, true).await?;
        assert!(pool.plugin_generation_revision_for_test().await > before_enable_repair);
        let future_enabled_lease = pool
            .acquire("application-skill-future-enabled")
            .await
            .map_err(|error| error.to_string())?;
        let future_enabled = future_enabled_lease.agent();
        drop(future_enabled_lease);
        agent_has_application_skill(&future_enabled, name, source, true).await?;

        let descriptor = runtime
            .agent_handle
            .read(|agent| {
                agent.skill_descriptors().into_iter().find(|descriptor| {
                    descriptor.name == name && descriptor.source.as_deref() == Some(source)
                })
            })
            .await
            .ok_or_else(|| "primary application skill descriptor is missing".to_string())?;
        runtime
            .disable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&runtime.agent_handle, name, source, false).await?;
        agent_has_application_skill(&existing, name, source, false).await?;
        agent_has_application_skill(&future_enabled, name, source, false).await?;

        existing
            .write(|agent| {
                agent.skill_registry_mut().register_descriptor(descriptor);
                crate::runtime::configure_intent_router(agent);
            })
            .await;
        agent_has_application_skill(&runtime.agent_handle, name, source, false).await?;
        agent_has_application_skill(&existing, name, source, true).await?;

        let before_disable_repair = pool.plugin_generation_revision_for_test().await;
        runtime
            .disable_application_skill(name.to_string(), skill_root, source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&existing, name, source, false).await?;
        assert!(pool.plugin_generation_revision_for_test().await > before_disable_repair);
        let future_disabled_lease = pool
            .acquire("application-skill-future-disabled")
            .await
            .map_err(|error| error.to_string())?;
        let future_disabled = future_disabled_lease.agent();
        drop(future_disabled_lease);
        agent_has_application_skill(&future_disabled, name, source, false).await?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_plugin_activation_restores_primary_and_pool_generation() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let _first = write_fixture(temporary.path())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("rollback-existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let previous_revision = pool.plugin_generation_revision_for_test().await;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let _second = write_fixture_at(
            temporary
                .path()
                .join(".echo-agent/plugins/rollback-candidate"),
            "rollback-candidate",
        )?;
        lifecycle.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "injected activation failure unexpectedly committed".to_string())?;
        if !error.to_string().contains("injected activation failure") {
            return Err(format!(
                "plugin activation failed for an unexpected reason: {error}"
            ));
        }
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&runtime.agent_handle, "rollback-candidate", false).await?;
        agent_has_plugin_generation(&existing, "rollback-candidate", false).await?;
        assert_eq!(
            pool.plugin_generation_revision_for_test().await,
            previous_revision
        );

        let future_lease = pool
            .acquire("rollback-future")
            .await
            .map_err(|error| error.to_string())?;
        let future = future_lease.agent();
        drop(future_lease);
        agent_has_plugin_generation(&future, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&future, "rollback-candidate", false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn active_primary_execution_blocks_plugin_generation_publication() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let previous_revision = pool.plugin_generation_revision_for_test().await;
        let primary_execution = runtime
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let active_execution = primary_execution.lock_owned().await;
        let _plugin = write_fixture(temporary.path())?;

        let reload_runtime = Arc::clone(&runtime);
        let mut reload = tokio::spawn(async move { reload_runtime.reload().await });
        tokio::task::yield_now().await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reload)
                .await
                .is_err(),
            "plugin publication escaped an already-active primary execution"
        );
        assert_eq!(
            pool.plugin_generation_revision_for_test().await,
            previous_revision
        );
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;

        drop(active_execution);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        assert!(pool.plugin_generation_revision_for_test().await > previous_revision);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lsp_readers_wait_for_plugin_generation_settlement() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let _plugin = write_fixture(temporary.path())?;
        let ownership = runtime.mcp_ownership.lock().await;

        let reload_runtime = Arc::clone(&runtime);
        let reload = tokio::spawn(async move { reload_runtime.reload().await });
        wait_until_mutation_holds_state(&runtime).await?;

        let reader_runtime = Arc::clone(&runtime);
        let mut reader =
            tokio::spawn(async move { reader_runtime.lsp_configured_languages().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reader)
                .await
                .is_err(),
            "LSP reader observed a plugin generation before settlement"
        );

        drop(ownership);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let languages = reader.await.map_err(|error| error.to_string())?;
        assert!(!languages.is_empty());
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn aborted_reload_waiter_does_not_cancel_owned_component_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        std::fs::write(
            plugin.join("themes/second.json"),
            "{\n  \"name\": \"runtime-fixture-second\",\n  \"dark\": false,\n  \"colors\": {}\n}\n",
        )
        .map_err(|error| error.to_string())?;

        let ownership = runtime.mcp_ownership.lock().await;
        let runtime_for_waiter = Arc::clone(&runtime);
        let waiter = tokio::spawn(async move { runtime_for_waiter.reload().await });
        wait_until_mutation_holds_state(&runtime).await?;
        waiter.abort();
        let waiter_error = waiter
            .await
            .err()
            .ok_or_else(|| "aborted plugin reload waiter unexpectedly completed".to_string())?;
        assert!(waiter_error.is_cancelled());
        drop(ownership);

        let themes = runtime.themes().await;
        assert!(
            themes
                .iter()
                .any(|theme| theme.name == "runtime-fixture-second")
        );
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn owned_plugin_mutations_execute_in_admission_order() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let order = Arc::new(Mutex::new(Vec::new()));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();

        let first_runtime = Arc::clone(&runtime);
        let first_order = Arc::clone(&order);
        let first = tokio::spawn(async move {
            first_runtime
                .run_owned_mutation(move |_| async move {
                    first_order.lock().await.push(1_u8);
                    first_started_tx
                        .send(())
                        .map_err(|_| anyhow::anyhow!("first mutation start waiter closed"))?;
                    release_first_rx
                        .await
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    Ok(())
                })
                .await
        });
        first_started_rx.await.map_err(|error| error.to_string())?;

        let second_runtime = Arc::clone(&runtime);
        let second_order = Arc::clone(&order);
        let second = tokio::spawn(async move {
            second_runtime
                .run_owned_mutation(move |_| async move {
                    second_order.lock().await.push(2_u8);
                    Ok(())
                })
                .await
        });
        release_first_tx
            .send(())
            .map_err(|_| "first plugin mutation stopped before release".to_string())?;
        first
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        second
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(*order.lock().await, vec![1, 2]);
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn aborted_rebind_waiter_does_not_cancel_owned_workspace_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        write_fixture_at(
            second.join(".echo-agent/plugins/second-fixture"),
            "second-fixture",
        )?;
        let runtime = service(&first).await?;

        let ownership = runtime.mcp_ownership.lock().await;
        let runtime_for_waiter = Arc::clone(&runtime);
        let second_for_waiter = second.clone();
        let waiter =
            tokio::spawn(
                async move { runtime_for_waiter.rebind_workspace(second_for_waiter).await },
            );
        wait_until_mutation_holds_state(&runtime).await?;
        waiter.abort();
        let waiter_error = waiter
            .await
            .err()
            .ok_or_else(|| "aborted plugin rebind waiter unexpectedly completed".to_string())?;
        assert!(waiter_error.is_cancelled());
        drop(ownership);

        let entries = runtime.list().await;
        assert_eq!(runtime.workspace_root().await, second);
        assert!(
            entries
                .iter()
                .any(|entry| entry.manifest.name == "second-fixture")
        );
        assert!(
            entries
                .iter()
                .all(|entry| entry.manifest.name != "runtime-fixture")
        );
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn workspace_rebind_replaces_project_plugins_and_lsp_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let runtime = service(&first).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);

        let summary = runtime
            .rebind_workspace(second.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(summary.total, 0);
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert_eq!(lifecycle.deactivate.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_project_lifecycle_cleanup_is_quarantined_and_retried() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let runtime = service(&first).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        lifecycle
            .shutdown_failures_remaining
            .store(1, Ordering::SeqCst);
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;

        let first_error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "lifecycle cleanup failure was not reported".to_string())?;
        assert!(
            first_error
                .to_string()
                .contains("lifecycle retirement failed")
        );
        assert_eq!(runtime.workspace_root().await, second);
        assert_eq!(runtime.cleanup_debt_roots().await, vec![first.clone()]);

        runtime
            .rebind_workspace(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(runtime.cleanup_debt_roots().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 2);
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn shutdown_unwires_plugin_receipts_and_is_idempotent() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);

        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert_eq!(lifecycle.deactivate.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        assert!(runtime.reload().await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_target_preserves_user_scope_receipt_and_agent() -> Result<(), String> {
        const CHILD_BASE: &str = "EKO_PLUGIN_USER_SCOPE_TEST_BASE";
        let child_base = std::env::var_os(CHILD_BASE).map(PathBuf::from);
        if child_base.is_none() {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("malformed_target_preserves_user_scope_receipt_and_agent")
            .arg("--test-threads=1")
            .env(CHILD_BASE, temp.path().join("plugin-base"))
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated User-scope plugin test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let child_base = child_base.ok_or_else(|| "missing child plugin base".to_string())?;
        crate::data_root::configure(child_base.clone()).map_err(|current| {
            format!(
                "plugin base was initialized before isolated test: {}",
                current.display()
            )
        })?;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture_at(child_base.join("plugins/user-fixture"), "user-fixture")?;
        let malformed = write_fixture(&second)?;
        std::fs::write(
            malformed.join("hooks/hooks.yaml"),
            "PreToolUse: [not-a-hook-rule]\n",
        )
        .map_err(|error| error.to_string())?;
        let runtime = default_service(&first).await?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("user-fixture-specialist").await);
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.contains_key("user-fixture"));
            assert!(
                state
                    .prepared
                    .agents
                    .iter()
                    .any(|agent| agent.name() == "user-fixture-specialist")
            );
        }

        let error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "malformed target plugin unexpectedly committed".to_string())?;
        assert!(error.to_string().contains("User-scope plugin generation"));
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(registry.contains("user-fixture-specialist").await);
        let entries = runtime.list().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.first().map(|entry| entry.scope),
            Some(PluginScope::User)
        );
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.contains_key("user-fixture"));
            assert!(
                state
                    .prepared
                    .agents
                    .iter()
                    .any(|agent| agent.name() == "user-fixture-specialist")
            );
        }
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn double_rebind_failure_retires_old_generation_at_target_root() -> Result<(), String> {
        const CHILD_BASE: &str = "EKO_PLUGIN_FAIL_CLOSED_TEST_BASE";
        let child_base = std::env::var_os(CHILD_BASE).map(PathBuf::from);
        if child_base.is_none() {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("double_rebind_failure_retires_old_generation_at_target_root")
            .arg("--test-threads=1")
            .env(CHILD_BASE, temp.path().join("plugin-base"))
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated fail-closed plugin test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let child_base = child_base.ok_or_else(|| "missing child plugin base".to_string())?;
        crate::data_root::configure(child_base.clone()).map_err(|current| {
            format!(
                "plugin base was initialized before isolated test: {}",
                current.display()
            )
        })?;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let target_plugin = write_fixture_at(
            second.join(".echo-agent/plugins/target-fixture"),
            "target-fixture",
        )?;
        let user_plugin = child_base.join("plugins/user-fixture");
        PluginRuntimeService::scaffold(&user_plugin, "user-fixture")
            .map_err(|error| error.to_string())?;

        let runtime = default_service(&first).await?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert!(registry.contains("user-fixture-specialist").await);
        let lifecycle = Arc::new(LifecycleCounts::default());
        lifecycle
            .shutdown_failures_remaining
            .store(1, Ordering::SeqCst);
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        runtime
            .bind_scheduler(scheduler.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(scheduler.list_tasks().await.len(), 1);

        std::fs::write(
            target_plugin.join("monitors.yaml"),
            "monitors: [not-a-monitor-definition]\n",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            user_plugin.join("hooks/hooks.yaml"),
            "PreToolUse: [not-a-hook-rule]\n",
        )
        .map_err(|error| error.to_string())?;

        let error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "double-failure rebind unexpectedly succeeded".to_string())?;
        assert!(
            error
                .to_string()
                .contains("retired all plugin-owned components")
        );
        assert!(error.to_string().contains("degraded User-scope plugins"));
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(!registry.contains("target-fixture-specialist").await);
        assert!(!registry.contains("user-fixture-specialist").await);
        assert!(scheduler.list_tasks().await.is_empty());
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        assert!(runtime.list().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.cleanup_debt_roots().await, vec![first.clone()]);
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.is_empty());
            assert!(state.mcp_ownership.is_empty());
            assert!(state.prepared.agents.is_empty());
            assert!(state.prepared.monitors.is_empty());
        }
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        assert!(runtime.cleanup_debt_roots().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn failed_real_reload_restores_previous_live_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        runtime
            .bind_scheduler(scheduler.clone())
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            plugin.join("hooks/hooks.yaml"),
            "PreToolUse: [this is not a hook rule]\n",
        )
        .map_err(|error| error.to_string())?;
        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "malformed hook reload unexpectedly succeeded".to_string())?;
        let rejection = error
            .downcast_ref::<PluginPreparationRejected>()
            .ok_or_else(|| format!("reload did not preserve prepared diagnostics: {error}"))?;
        assert!(rejection.diagnostics.iter().any(|diagnostic| {
            diagnostic.plugin_id() == Some("runtime-fixture")
                && diagnostic.component() == "hooks"
                && diagnostic.severity() == echo_agent::plugin::PluginDiagnosticSeverity::Error
                && diagnostic
                    .path()
                    .is_some_and(|path| path.ends_with("hooks/hooks.yaml"))
        }));
        assert!(
            registry.contains("runtime-fixture-specialist").await,
            "reload rollback did not restore the previous Subagent: {error}"
        );
        assert_eq!(runtime.themes().await.len(), 1);
        assert_eq!(runtime.output_styles().await.len(), 1);
        assert_eq!(
            runtime.active_output_style().await.as_deref(),
            Some("runtime-fixture-concise")
        );
        assert_eq!(scheduler.list_tasks().await.len(), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_lifecycle_brackets_reload_configure_and_unregisters_on_uninstall()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let manifest_path = plugin.join("plugin.json");
        let manifest_text =
            std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?;
        let mut manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).map_err(|error| error.to_string())?;
        let manifest_object = manifest
            .as_object_mut()
            .ok_or_else(|| "fixture plugin manifest is not an object".to_string())?;
        manifest_object.insert(
            "config".to_string(),
            serde_json::json!({
                "label": {
                    "type": "string",
                    "title": "Label",
                    "default": "initial"
                }
            }),
        );
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .map_err(|error| error.to_string())?;

        runtime.reload().await.map_err(|error| error.to_string())?;
        runtime
            .configure(
                "runtime-fixture",
                HashMap::from([("label".to_string(), serde_json::json!("updated"))]),
            )
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 2);

        runtime
            .uninstall("runtime-fixture", false)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.shutdown.load(Ordering::SeqCst), 1);

        write_fixture(temp.path())?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::new(LifecycleCounts::default()))),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_native_lifecycle_registration_shuts_down_and_can_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        counts.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .err()
            .ok_or_else(|| "failing lifecycle registration unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("injected activation failure"));
        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 1);
        assert_eq!(counts.shutdown.load(Ordering::SeqCst), 1);

        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::new(LifecycleCounts::default()))),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn activation_failure_restores_previous_components_and_lifecycle() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            plugin.join("themes/example.json"),
            "{\n  \"name\": \"runtime-fixture-dark\",\n  \"dark\": true,\n  \"colors\": {\"accent\": \"#000000\"}\n}\n",
        )
        .map_err(|error| error.to_string())?;
        counts.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "lifecycle activation failure unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("injected activation failure"));
        let themes = runtime.themes().await;
        let accent = themes
            .first()
            .and_then(|theme| theme.colors.get("accent"))
            .map(String::as_str);
        assert_eq!(accent, Some("#5b8def"));
        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduler_binding_uses_the_same_lock_order_as_reload() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );

        let state_guard = runtime.state.lock().await;
        let (started, bind_started) = tokio::sync::oneshot::channel();
        let runtime_for_bind = runtime.clone();
        let bind = tokio::spawn(async move {
            let _ = started.send(());
            runtime_for_bind
                .bind_scheduler(scheduler)
                .await
                .map_err(|error| error.to_string())
        });
        bind_started
            .await
            .map_err(|_| "scheduler bind task stopped before starting".to_string())?;
        tokio::task::yield_now().await;

        let scheduler_guard = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            runtime.scheduler.read(),
        )
        .await
        .map_err(|_| "scheduler lock was acquired before plugin state lock".to_string())?;
        drop(scheduler_guard);
        drop(state_guard);

        let monitor_count = tokio::time::timeout(std::time::Duration::from_secs(2), bind)
            .await
            .map_err(|_| "scheduler binding deadlocked".to_string())?
            .map_err(|error| error.to_string())??;
        assert_eq!(monitor_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn theme_and_output_style_preferences_survive_runtime_restart() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temporary.path())?;
        let runtime = service(temporary.path()).await?;
        runtime
            .activate_theme(Some("runtime-fixture-dark"))
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        drop(runtime);

        let restored = service(temporary.path()).await?;

        assert_eq!(
            restored.active_theme().await.as_deref(),
            Some("runtime-fixture-dark")
        );
        assert_eq!(
            restored.active_output_style().await.as_deref(),
            Some("runtime-fixture-concise")
        );
        let messages = restored
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(messages.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        }));
        Ok(())
    }

    #[test]
    fn scaffold_and_validate_cover_application_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = temp.path().join("scaffolded");
        PluginRuntimeService::scaffold(&plugin, "scaffolded").map_err(|error| error.to_string())?;
        for expected_file in [
            "plugin.json",
            "skills/example/SKILL.md",
            "agents/example.md",
            "hooks/hooks.yaml",
            "mcp.json",
            "lsp.yaml",
            "monitors.yaml",
            "themes/example.json",
            "output-styles/scaffolded-concise.md",
            "README.md",
        ] {
            assert!(
                plugin.join(expected_file).is_file(),
                "missing scaffold file {expected_file}"
            );
        }
        assert!(plugin.join("scripts").is_dir());
        assert!(!plugin.join(".echo-plugin").exists());
        assert!(!plugin.join(".mcp.json").exists());
        let report = PluginRuntimeService::validate(&plugin);
        assert!(report.valid, "{}", report.errors.join("; "));
        for expected in [
            "skills",
            "agents",
            "hooks",
            "mcp_servers",
            "lsp_servers",
            "monitors",
            "themes",
            "output_styles",
        ] {
            assert!(
                report
                    .components
                    .iter()
                    .any(|component| component == expected)
            );
        }
        Ok(())
    }

    #[test]
    fn validate_rejects_malformed_runtime_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = temp.path().join("invalid-components");
        PluginRuntimeService::scaffold(&plugin, "invalid-components")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("hooks/hooks.yaml"),
            "PreToolUse:\n  - matcher: '*'\n    hooks:\n      - type: command\n        command: ''\n",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("mcp.json"),
            r#"{"$schema":"invalid","mcpServers":{}}"#,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("skills/example/SKILL.md"),
            "This file has no frontmatter.\n",
        )
        .map_err(|error| error.to_string())?;

        let report = PluginRuntimeService::validate(&plugin);

        assert!(!report.valid);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("empty command"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("MCP config"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("must begin with YAML frontmatter"))
        );
        Ok(())
    }
}
