//! Process-level plugin runtime with atomic live component replacement.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::lsp::{LspConfig, LspManager};
use echo_agent::mcp::McpConfigFile;
use echo_agent::plugin::{
    AGENT_PLUGIN_SCHEMA_V1, InstallSource, PluginEntry, PluginIntegrator, PluginLifecycle,
    PluginLifecycleManager, PluginRegistry, PluginScope, PluginWiringResult, WiredPluginComponents,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::agent_handle::AgentHandle;
use crate::mcp_config_runtime::{
    McpNameOwnershipGuard, McpNameOwnershipRegistry, PluginMcpOwnershipToken,
};
pub use crate::plugin_components::{PluginOutputStyle, PluginThemeDefinition};
use crate::plugin_components::{
    PreparedApplicationComponents, prepare_application_components, register_plugin_agents,
    validate_application_component_files,
};
use crate::scheduler::{CronTask, SchedulerRunner};

const OUTPUT_STYLE_PROJECTION: &str = "eko:plugin-output-style";

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
        let global_lsp = echo_agent::paths::user_data_path(".lsp.yaml");
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
        wiring: &PluginWiringResult,
        application: &PreparedApplicationComponents,
    ) -> Self {
        Self {
            total,
            enabled,
            skills_loaded: wiring.skills_loaded.len(),
            hooks_registered: wiring.hooks_registered.len(),
            mcp_connected: wiring.mcp_connected.len(),
            agents_loaded: application.agents.len(),
            lsp_languages_loaded: application
                .lsp_configs
                .iter()
                .map(|(_, config)| config.servers.len())
                .sum(),
            monitors_loaded: application.monitors.len(),
            themes_loaded: application.themes.len(),
            output_styles_loaded: application.output_styles.len(),
            errors: wiring.warnings.clone(),
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

/// Capabilities visible to EKO, combining framework and application-owned
/// components from the same fixed package layout.
pub fn plugin_capabilities(entry: &PluginEntry) -> Vec<echo_agent::plugin::PluginCapability> {
    let mut capabilities = entry.inferred_capabilities();
    if let Ok(eko) = crate::plugin_components::resolve_eko_components(&entry.root) {
        if eko.monitors_file.is_some() {
            capabilities.push(echo_agent::plugin::PluginCapability::Monitor);
        }
        if !eko.theme_files.is_empty() {
            capabilities.push(echo_agent::plugin::PluginCapability::Theme);
        }
        if !eko.output_style_files.is_empty() {
            capabilities.push(echo_agent::plugin::PluginCapability::OutputStyle);
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
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    lifecycle: PluginLifecycleManager,
    cleanup_quarantine: Vec<PluginCleanupQuarantine>,
    active_theme: Option<String>,
    active_output_style: Option<String>,
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
    registry_source: RegistrySource,
    preferences_file: PathBuf,
    state: Mutex<PluginRuntimeState>,
    mutation_supervisor: Mutex<PluginMutationSupervisor>,
}

impl PluginRuntimeService {
    pub(crate) async fn new(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
    ) -> Arc<Self> {
        Self::new_with_source(agent_handle, lsp, mcp_ownership, RegistrySource::Default).await
    }

    async fn new_with_source(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
        registry_source: RegistrySource,
    ) -> Arc<Self> {
        let preferences_file = match &registry_source {
            RegistrySource::Default => echo_agent::plugin::plugin_data_base_dir()
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
            registry_source,
            preferences_file,
            state: Mutex::new(PluginRuntimeState {
                registry: PluginRegistry::new(None),
                framework_components: HashMap::new(),
                mcp_ownership: HashMap::new(),
                prepared: PreparedApplicationComponents::default(),
                lifecycle: PluginLifecycleManager::new(),
                cleanup_quarantine: Vec::new(),
                active_theme: preferences.active_theme,
                active_output_style: preferences.active_output_style,
                shut_down: false,
            }),
            mutation_supervisor: Mutex::new(PluginMutationSupervisor::default()),
        });
        if let Err(error) = service.reload().await {
            tracing::warn!(%error, "initial plugin load failed; previous runtime kept");
        }
        service
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
    ) -> Arc<Self> {
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
    ) -> Arc<Self> {
        let manager = Arc::new(RwLock::new(LspManager::new()));
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

    pub async fn reload(self: &Arc<Self>) -> anyhow::Result<ReloadSummary> {
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
        candidate
            .resolve_enabled_dependencies()
            .map_err(|error| anyhow::anyhow!("Plugin dependency validation failed: {error}"))?;
        let prepared = prepare_application_components(&mut candidate)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let declarations = plugin_mcp_declarations(&mut candidate)?;
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
    pub async fn rebind_workspace(
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

        let previous_framework = std::mem::take(&mut state.framework_components);
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let mut ownership_guard = self.mcp_ownership.lock().await;
        let exact_framework = exact_plugin_framework_receipts(
            &previous_framework,
            &previous_mcp_ownership,
            &ownership_guard,
        );
        self.agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    unload_agent_components(agent, &exact_framework, &previous_prepared).await;
                    agent
                        .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                        .await;
                })
            })
            .await;
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
        let previous_framework = std::mem::take(&mut state.framework_components);
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
        let exact_framework = exact_plugin_framework_receipts(
            &previous_framework,
            &previous_mcp_ownership,
            &ownership_guard,
        );
        self.agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    // Pass only plugin-owned receipts. Global user MCP
                    // reconciliation stays with the independent MCP owner.
                    unload_agent_components(agent, &exact_framework, &previous_prepared).await;
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

    pub async fn enable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
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

    pub async fn disable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
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

    pub async fn install(
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

    pub async fn uninstall(
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

    pub async fn get(&self, name: &str) -> Option<PluginEntry> {
        self.state.lock().await.registry.get(name).cloned()
    }

    pub async fn configure(
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

    pub async fn activate_theme(
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

    pub async fn activate_output_style(self: &Arc<Self>, name: Option<&str>) -> anyhow::Result<()> {
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
        persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: selected.clone(),
            },
        )?;
        self.agent_handle
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions)
                        .await;
                })
            })
            .await;
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
                    &manifest.name,
                    directory.to_path_buf(),
                    project_dir,
                )
                .with_plugin_data(std::env::temp_dir())
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
        candidate
            .resolve_enabled_dependencies()
            .map_err(|error| anyhow::anyhow!("Plugin dependency validation failed: {error}"))?;
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
        let prepared = prepare_application_components(&mut candidate)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let candidate_mcp_declarations = plugin_mcp_declarations(&mut candidate)?;
        self.validate_agent_collisions(state, &prepared).await?;
        let mut replacement_lsp = self.prepare_lsp(&prepared, binding).await?;

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
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let previous_prepared = std::mem::take(&mut state.prepared);
        let apply = self
            .replace_agent_components(
                previous_registry,
                previous_framework,
                previous_mcp_ownership,
                previous_prepared,
                candidate,
                candidate_mcp_declarations,
                prepared,
            )
            .await;

        let applied = match apply {
            Ok(applied) => applied,
            Err(mut failed) => {
                state.registry = failed.registry;
                state.framework_components = failed.framework_components;
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
            let rollback = self
                .replace_agent_components(
                    applied.registry,
                    applied.wiring.components_by_plugin,
                    applied.mcp_ownership,
                    applied.prepared,
                    applied.previous_registry,
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
                    state.framework_components = restored.wiring.components_by_plugin;
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
                    state.mcp_ownership = failed.mcp_ownership;
                    state.prepared = failed.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(candidate_plugins.iter().map(String::as_str)),
                    );
                }
            }
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        previous_lsp.shutdown_all().await;
        self.lsp.publish_binding(binding.clone()).await;

        let active_style = state.active_output_style.clone();
        let active_theme = state.active_theme.clone();
        state.registry = applied.registry;
        state.framework_components = applied.wiring.components_by_plugin.clone();
        state.mcp_ownership = applied.mcp_ownership;
        state.prepared = applied.prepared;
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
                self.agent_handle
                    .read_async(|agent| {
                        Box::pin(async move {
                            agent
                                .replace_system_context_projection(
                                    OUTPUT_STYLE_PROJECTION,
                                    instructions,
                                )
                                .await;
                        })
                    })
                    .await;
            } else {
                state.active_output_style = None;
                self.agent_handle
                    .read_async(|agent| {
                        Box::pin(async move {
                            agent
                                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                                .await;
                        })
                    })
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
        let mut summary =
            ReloadSummary::from_components(total, enabled, &applied.wiring, &state.prepared);
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: state.active_output_style.clone(),
            },
        ) {
            summary.errors.push(error.to_string());
        }
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
    async fn replace_agent_components(
        &self,
        mut previous_registry: PluginRegistry,
        previous_framework: HashMap<String, WiredPluginComponents>,
        previous_mcp_ownership: PluginMcpOwnership,
        previous_prepared: PreparedApplicationComponents,
        mut candidate: PluginRegistry,
        candidate_mcp_declarations: PluginMcpDeclarations,
        candidate_prepared: PreparedApplicationComponents,
    ) -> Result<AppliedAgentComponents, FailedAgentComponents> {
        let candidate_monitors = candidate_prepared.monitors.clone();
        let previous_mcp_declarations = match plugin_mcp_declarations(&mut previous_registry) {
            Ok(declarations) => declarations,
            Err(error) => {
                return Err(FailedAgentComponents {
                    error: format!("Failed to inspect previous plugin MCP receipts: {error}"),
                    registry: previous_registry,
                    framework_components: previous_framework,
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
                mcp_ownership: previous_mcp_ownership,
                prepared: previous_prepared,
                candidate_monitors,
            });
        }

        let exact_previous = exact_plugin_framework_receipts(
            &previous_framework,
            &previous_mcp_ownership,
            &ownership_guard,
        );
        let previous_prepared_for_unload = previous_prepared.clone();
        self.agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    unload_agent_components(agent, &exact_previous, &previous_prepared_for_unload)
                        .await;
                })
            })
            .await;
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);
        let candidate_mcp_ownership =
            match claim_plugin_mcp_names(&mut ownership_guard, &candidate_mcp_declarations) {
                Ok(ownership) => ownership,
                Err(error) => {
                    return Err(FailedAgentComponents {
                        error,
                        registry: previous_registry,
                        framework_components: HashMap::new(),
                        mcp_ownership: HashMap::new(),
                        prepared: PreparedApplicationComponents::default(),
                        candidate_monitors,
                    });
                }
            };

        let candidate_prepared_for_wiring = candidate_prepared.clone();
        let candidate_outcome = self
            .agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    let wiring = PluginIntegrator::new()
                        .wire_all(agent, &mut candidate)
                        .await;
                    if !wiring.errors.is_empty() {
                        return Err((
                            format!("Plugin wiring failed: {}", wiring.errors.join("; ")),
                            candidate,
                            wiring,
                        ));
                    }
                    if let Err(error) =
                        register_plugin_agents(agent, &candidate_prepared_for_wiring.agents).await
                    {
                        unload_agent_components(
                            agent,
                            &wiring.components_by_plugin,
                            &candidate_prepared_for_wiring,
                        )
                        .await;
                        return Err((
                            format!("Plugin Subagent registration failed: {error}"),
                            candidate,
                            wiring,
                        ));
                    }
                    Ok((candidate, wiring))
                })
            })
            .await;

        match candidate_outcome {
            Ok((registry, wiring)) => {
                let candidate_mcp_ownership = retain_connected_plugin_mcp_claims(
                    &mut ownership_guard,
                    &wiring,
                    candidate_mcp_ownership,
                );
                Ok(AppliedAgentComponents {
                    registry,
                    wiring,
                    mcp_ownership: candidate_mcp_ownership,
                    prepared: candidate_prepared,
                    previous_registry,
                    previous_mcp_declarations,
                    previous_prepared,
                })
            }
            Err((error, _candidate_registry, _candidate_wiring)) => {
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
                            mcp_ownership: HashMap::new(),
                            prepared: PreparedApplicationComponents::default(),
                            candidate_monitors,
                        });
                    }
                };
                let previous_prepared_for_restore = previous_prepared.clone();
                let restored = self
                    .agent_handle
                    .write_async(|agent| {
                        Box::pin(async move {
                            let restored = PluginIntegrator::new()
                                .wire_all(agent, &mut previous_registry)
                                .await;
                            let restore_agent_error = if restored.errors.is_empty() {
                                register_plugin_agents(agent, &previous_prepared_for_restore.agents)
                                    .await
                                    .err()
                            } else {
                                None
                            };
                            (previous_registry, restored, restore_agent_error)
                        })
                    })
                    .await;
                let (registry, restored, restore_agent_error) = restored;
                let mut errors = vec![error];
                if !restored.errors.is_empty() {
                    errors.push(format!(
                        "rollback framework wiring failed: {}",
                        restored.errors.join("; ")
                    ));
                }
                if let Some(error) = restore_agent_error {
                    errors.push(format!("rollback Subagent wiring failed: {error}"));
                }
                let restored_mcp_ownership = retain_connected_plugin_mcp_claims(
                    &mut ownership_guard,
                    &restored,
                    restored_mcp_ownership,
                );
                Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry,
                    framework_components: restored.components_by_plugin,
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

    async fn project_root(&self) -> PathBuf {
        self.lsp.binding().await.project_root
    }

    fn registry_for(&self, project_root: PathBuf) -> PluginRegistry {
        match &self.registry_source {
            RegistrySource::Default => PluginRegistry::new(Some(project_root)),
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
    wiring: PluginWiringResult,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    previous_registry: PluginRegistry,
    previous_mcp_declarations: PluginMcpDeclarations,
    previous_prepared: PreparedApplicationComponents,
}

struct FailedAgentComponents {
    error: String,
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    candidate_monitors: Vec<CronTask>,
}

fn agent_name(agent: &crate::plugin_components::PreparedPluginAgent) -> String {
    agent.name().to_string()
}

fn plugin_mcp_declarations(registry: &mut PluginRegistry) -> anyhow::Result<PluginMcpDeclarations> {
    let plugin_ids = registry
        .list_enabled()
        .into_iter()
        .map(|entry| entry.manifest.name.clone())
        .collect::<Vec<_>>();
    let mut declarations = HashMap::new();
    let mut declared_by = HashMap::<String, String>::new();

    for plugin_id in plugin_ids {
        let variables = registry
            .variables_for(&plugin_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        let resolved = registry
            .resolve_components(&plugin_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(config_file) = resolved.mcp_config_file else {
            continue;
        };
        let content = std::fs::read_to_string(&config_file).map_err(|error| {
            anyhow::anyhow!(
                "Plugin '{plugin_id}' MCP config {}: {error}",
                config_file.display()
            )
        })?;
        let config = McpConfigFile::parse(&variables.substitute(&content)).map_err(|error| {
            anyhow::anyhow!(
                "Plugin '{plugin_id}' MCP config {}: {error}",
                config_file.display()
            )
        })?;
        let mut names = config.mcp_servers.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for name in &names {
            if let Some(previous) = declared_by.insert(name.clone(), plugin_id.clone()) {
                return Err(anyhow::anyhow!(
                    "Plugin MCP server name '{name}' is declared by both '{previous}' and '{plugin_id}'"
                ));
            }
        }
        declarations.insert(plugin_id, names);
    }
    Ok(declarations)
}

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

fn retain_connected_plugin_mcp_claims(
    guard: &mut McpNameOwnershipGuard,
    wiring: &PluginWiringResult,
    mut ownership: PluginMcpOwnership,
) -> PluginMcpOwnership {
    for (plugin_id, tokens) in &mut ownership {
        let connected = wiring
            .components_by_plugin
            .get(plugin_id)
            .map(|components| components.mcp_servers.as_slice())
            .unwrap_or_default();
        let released = tokens
            .iter()
            .filter(|(name, _)| !connected.contains(name))
            .map(|(name, token)| (name.clone(), *token))
            .collect::<Vec<_>>();
        for (name, token) in released {
            guard.release_plugin(plugin_id, &name, token);
            tokens.remove(&name);
        }
    }
    ownership.retain(|_, tokens| !tokens.is_empty());
    ownership
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

async fn unload_agent_components(
    agent: &mut echo_agent::agent::react::ReactAgent,
    framework: &HashMap<String, WiredPluginComponents>,
    application: &PreparedApplicationComponents,
) {
    for plugin_agent in &application.agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
    echo_agent::plugin::PluginIntegrator::unwire(agent, framework).await;
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
            plugin.join("monitors.yaml"),
            "monitors:\n  - name: daily-review\n    cron: \"0 0 * * * *\"\n    prompt: Review pending work\n",
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        write_fake_lsp(&plugin)?;
        Ok(plugin)
    }

    #[cfg(unix)]
    fn write_fake_lsp(plugin: &Path) -> Result<(), String> {
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

        let lsp = serde_yaml::to_string(&serde_json::json!({
            "languages": {
                "fixture": {
                    "language": "fixture",
                    "command": server,
                    "args": [],
                    "extensions": [".fixture"],
                    "env": {},
                    "max_restarts": 0
                }
            }
        }))
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
        Ok(PluginRuntimeService::new_for_test(
            AgentHandle::new(agent),
            root.to_path_buf(),
            root.join("registry.json"),
            root.join("plugin-data"),
        )
        .await)
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
        Ok(PluginRuntimeService::new(
            AgentHandle::new(agent),
            lsp,
            McpNameOwnershipRegistry::new(Vec::<String>::new()),
        )
        .await)
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
        echo_agent::plugin::set_plugin_data_base_dir(child_base.clone()).map_err(|current| {
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
        echo_agent::plugin::set_plugin_data_base_dir(child_base.clone()).map_err(|current| {
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
        assert!(error.to_string().contains("hooks YAML parse"));
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
