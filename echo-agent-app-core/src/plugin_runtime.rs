//! Process-level plugin runtime with atomic live component replacement.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::lsp::{LspConfig, LspManager};
use echo_agent::plugin::{
    InstallSource, PluginEntry, PluginIntegrator, PluginRegistry, PluginScope, PluginWiringResult,
    WiredPluginComponents,
};
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};

use crate::agent_handle::AgentHandle;
use crate::plugin_components::{
    PluginOutputStyle, PluginThemeDefinition, PreparedApplicationComponents,
    prepare_application_components, register_plugin_agents, validate_application_component_files,
};
use crate::scheduler::{CronTask, SchedulerRunner};

const OUTPUT_STYLE_PROJECTION: &str = "eko:plugin-output-style";

#[derive(Clone)]
pub struct PluginLspRuntime {
    pub manager: Arc<RwLock<LspManager>>,
    pub base_config: LspConfig,
    pub project_root: PathBuf,
}

impl PluginLspRuntime {
    pub fn new(
        manager: Arc<RwLock<LspManager>>,
        base_config: LspConfig,
        project_root: PathBuf,
    ) -> Self {
        Self {
            manager,
            base_config,
            project_root,
        }
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
            errors: Vec::new(),
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

struct PluginRuntimeState {
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    prepared: PreparedApplicationComponents,
    active_output_style: Option<String>,
}

pub struct PluginRuntimeService {
    agent_handle: AgentHandle,
    lsp: PluginLspRuntime,
    scheduler: RwLock<Option<Arc<SchedulerRunner>>>,
    registry_source: RegistrySource,
    state: Mutex<PluginRuntimeState>,
}

impl PluginRuntimeService {
    pub async fn new(agent_handle: AgentHandle, lsp: PluginLspRuntime) -> Arc<Self> {
        Self::new_with_source(agent_handle, lsp, RegistrySource::Default).await
    }

    async fn new_with_source(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        registry_source: RegistrySource,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            agent_handle,
            lsp,
            scheduler: RwLock::new(None),
            registry_source,
            state: Mutex::new(PluginRuntimeState {
                registry: PluginRegistry::new(None),
                framework_components: HashMap::new(),
                prepared: PreparedApplicationComponents::default(),
                active_output_style: None,
            }),
        });
        if let Err(error) = service.reload().await {
            tracing::warn!(%error, "initial plugin load failed; previous runtime kept");
        }
        service
    }

    #[cfg(test)]
    async fn new_for_test(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
    ) -> Arc<Self> {
        let manager = Arc::new(RwLock::new(LspManager::new()));
        let lsp = PluginLspRuntime::new(manager, LspConfig::default(), project_root);
        Self::new_with_source(
            agent_handle,
            lsp,
            RegistrySource::Custom {
                state_file,
                data_dir,
                scopes: vec![PluginScope::Project, PluginScope::Local],
            },
        )
        .await
    }

    pub async fn reload(&self) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let project_root = self.project_root().await;
        let mut candidate = self.registry_for(project_root);
        self.scan_registry(&mut candidate)?;
        self.apply_candidate(&mut state, candidate).await
    }

    pub async fn bind_scheduler(&self, scheduler: Arc<SchedulerRunner>) -> anyhow::Result<usize> {
        let state = self.state.lock().await;
        let monitors = state.prepared.monitors.clone();
        let mut slot = self.scheduler.write().await;
        if slot.is_some() {
            return Ok(monitors.len());
        }
        replace_plugin_monitors(&scheduler, &[], &monitors).await?;
        *slot = Some(scheduler);
        Ok(monitors.len())
    }

    pub async fn enable(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let project_root = self.project_root().await;
        let mut candidate = self.registry_for(project_root);
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .enable(name)
            .map_err(|error| anyhow::anyhow!("Enable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate).await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.restore_enabled_state(name, previously_enabled).await;
                Err(error)
            }
        }
    }

    pub async fn disable(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let project_root = self.project_root().await;
        let mut candidate = self.registry_for(project_root);
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .disable(name)
            .map_err(|error| anyhow::anyhow!("Disable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate).await {
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
        &self,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let mut state = self.state.lock().await;
        let project_root = self.project_root().await;
        let mut candidate = self.registry_for(project_root);
        self.scan_registry(&mut candidate)?;
        let plugin_id = candidate
            .install(source, scope)
            .map_err(|error| anyhow::anyhow!("Install plugin failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate).await {
            Ok(summary) => Ok((plugin_id, summary)),
            Err(error) => {
                self.rollback_install(&plugin_id).await;
                Err(error)
            }
        }
    }

    pub async fn uninstall(&self, name: &str, keep_data: bool) -> anyhow::Result<ReloadSummary> {
        let was_enabled = self
            .get(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        let mut summary = if was_enabled {
            self.disable(name).await?
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
        summary.total = state.registry.count();
        summary.enabled = state.registry.list_enabled().len();
        if !was_enabled {
            self.fire_plugin_disabled(name).await;
        }
        Ok(summary)
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

    pub async fn themes(&self) -> Vec<PluginThemeDefinition> {
        self.state.lock().await.prepared.themes.clone()
    }

    pub async fn output_styles(&self) -> Vec<PluginOutputStyle> {
        self.state.lock().await.prepared.output_styles.clone()
    }

    pub async fn active_output_style(&self) -> Option<String> {
        self.state.lock().await.active_output_style.clone()
    }

    pub async fn activate_output_style(&self, name: Option<&str>) -> anyhow::Result<()> {
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
        self.agent_handle
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions)
                        .await;
                })
            })
            .await;
        state.active_output_style = name.map(str::to_string);
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
                let errors = validate_application_component_files(&manifest.name, &resolved);
                PluginValidationReport {
                    valid: errors.is_empty(),
                    name: Some(manifest.name),
                    components: component_names(&resolved),
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
    ) -> anyhow::Result<ReloadSummary> {
        candidate
            .resolve_enabled_dependencies()
            .map_err(|error| anyhow::anyhow!("Plugin dependency validation failed: {error}"))?;
        let prepared = prepare_application_components(&mut candidate)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        self.validate_agent_collisions(state, &prepared).await?;
        let mut replacement_lsp = self.prepare_lsp(&prepared).await?;

        let scheduler = self.scheduler.read().await.clone();
        if let Some(scheduler) = scheduler.as_ref()
            && let Err(error) =
                replace_plugin_monitors(scheduler, &state.prepared.monitors, &prepared.monitors)
                    .await
        {
            replacement_lsp.shutdown_all().await;
            return Err(error);
        }

        let previous_registry = std::mem::replace(
            &mut state.registry,
            self.registry_for(self.lsp.project_root.clone()),
        );
        let previous_framework = std::mem::take(&mut state.framework_components);
        let previous_prepared = std::mem::take(&mut state.prepared);
        let apply = self
            .replace_agent_components(
                previous_registry,
                previous_framework,
                previous_prepared,
                candidate,
                prepared,
            )
            .await;

        let applied = match apply {
            Ok(applied) => applied,
            Err(mut failed) => {
                state.registry = failed.registry;
                state.framework_components = failed.framework_components;
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
                return Err(anyhow::anyhow!(failed.error));
            }
        };

        {
            let mut current = self.lsp.manager.write().await;
            current.shutdown_all().await;
            *current = replacement_lsp;
        }

        let active_style = state.active_output_style.clone();
        state.registry = applied.registry;
        state.framework_components = applied.wiring.components_by_plugin.clone();
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

        let total = state.registry.count();
        let enabled = state.registry.list_enabled().len();
        let summary =
            ReloadSummary::from_components(total, enabled, &applied.wiring, &state.prepared);
        let loaded_plugins = state
            .registry
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<Vec<_>>();
        self.fire_loaded_events(&loaded_plugins).await;
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

    async fn replace_agent_components(
        &self,
        mut previous_registry: PluginRegistry,
        previous_framework: HashMap<String, WiredPluginComponents>,
        previous_prepared: PreparedApplicationComponents,
        mut candidate: PluginRegistry,
        candidate_prepared: PreparedApplicationComponents,
    ) -> Result<AppliedAgentComponents, FailedAgentComponents> {
        let candidate_monitors = candidate_prepared.monitors.clone();
        let outcome = self
            .agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    unload_agent_components(agent, &previous_framework, &previous_prepared).await;
                    let wiring = PluginIntegrator::new()
                        .wire_all(agent, &mut candidate)
                        .await;
                    if !wiring.errors.is_empty() {
                        let error = format!("Plugin wiring failed: {}", wiring.errors.join("; "));
                        unload_agent_components(
                            agent,
                            &wiring.components_by_plugin,
                            &candidate_prepared,
                        )
                        .await;
                        let restored = PluginIntegrator::new()
                            .wire_all(agent, &mut previous_registry)
                            .await;
                        let restore_error =
                            register_plugin_agents(agent, &previous_prepared.agents)
                                .await
                                .err();
                        return Err((
                            error,
                            previous_registry,
                            restored,
                            previous_prepared,
                            restore_error,
                        ));
                    }
                    if let Err(error) =
                        register_plugin_agents(agent, &candidate_prepared.agents).await
                    {
                        unload_agent_components(
                            agent,
                            &wiring.components_by_plugin,
                            &candidate_prepared,
                        )
                        .await;
                        let restored = PluginIntegrator::new()
                            .wire_all(agent, &mut previous_registry)
                            .await;
                        let restore_error =
                            register_plugin_agents(agent, &previous_prepared.agents)
                                .await
                                .err();
                        return Err((
                            format!("Plugin Subagent registration failed: {error}"),
                            previous_registry,
                            restored,
                            previous_prepared,
                            restore_error,
                        ));
                    }
                    Ok((candidate, wiring, candidate_prepared))
                })
            })
            .await;

        match outcome {
            Ok((registry, wiring, prepared)) => Ok(AppliedAgentComponents {
                registry,
                wiring,
                prepared,
            }),
            Err((error, registry, restored, prepared, restore_agent_error)) => {
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
                Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry,
                    framework_components: restored.components_by_plugin,
                    prepared,
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
    ) -> anyhow::Result<LspManager> {
        let mut config = self.lsp.base_config.clone();
        let mut required = self
            .lsp
            .manager
            .read()
            .await
            .running_servers()
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        for (_, plugin_config) in &prepared.lsp_configs {
            required.extend(plugin_config.servers.keys().cloned());
            config.merge(plugin_config.clone());
        }
        let mut manager = LspManager::new();
        manager.load_config(&config);
        manager.set_project_root(&self.lsp.project_root);
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
        self.agent_handle
            .read(|agent| agent.working_dir())
            .await
            .unwrap_or_else(|| self.lsp.project_root.clone())
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

struct AppliedAgentComponents {
    registry: PluginRegistry,
    wiring: PluginWiringResult,
    prepared: PreparedApplicationComponents,
}

struct FailedAgentComponents {
    error: String,
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    prepared: PreparedApplicationComponents,
    candidate_monitors: Vec<CronTask>,
}

fn agent_name(agent: &crate::plugin_components::PreparedPluginAgent) -> String {
    agent.name().to_string()
}

async fn unload_agent_components(
    agent: &mut echo_agent::agent::react::ReactAgent,
    framework: &HashMap<String, WiredPluginComponents>,
    application: &PreparedApplicationComponents,
) {
    for plugin_agent in &application.agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
    for (plugin_name, components) in framework {
        let source_tag = format!("plugin:{plugin_name}");
        let _ = agent.unregister_skills_by_source(&source_tag).await;
        if components.hooks_registered {
            let source = echo_agent::skills::hooks::HookSource::Plugin(plugin_name.clone());
            agent.hook_registry().write().await.unregister(&source);
        }
        for server_name in &components.mcp_servers {
            let _ = agent.disconnect_mcp(server_name).await;
        }
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
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err(anyhow::anyhow!(
            "Plugin name must be lowercase kebab-case without leading or trailing hyphens"
        ));
    }
    Ok(())
}

fn write_scaffold(directory: &Path, name: &str) -> anyhow::Result<()> {
    for child in [
        ".echo-plugin",
        "skills/example",
        "agents",
        "hooks",
        "monitors",
        "themes",
        "output-styles",
    ] {
        std::fs::create_dir_all(directory.join(child))?;
    }
    let manifest = format!(
        "name: {name}\ndisplay_name: {name}\nversion: \"0.1.0\"\ndescription: \"EKO plugin\"\nlicense: MIT\ndefault_enabled: true\ncomponents:\n  skills: ./skills\n  agents: ./agents\n  hooks: ./hooks/hooks.yaml\n  mcp_servers: ./.mcp.json\n  lsp_servers: ./.lsp.yaml\n  monitors: ./monitors/monitors.yaml\n  themes: ./themes\n  output_styles: ./output-styles\n"
    );
    std::fs::write(directory.join(".echo-plugin/manifest.yaml"), manifest)?;
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
    std::fs::write(directory.join(".mcp.json"), "{\"mcpServers\": {}}\n")?;
    std::fs::write(directory.join(".lsp.yaml"), "languages: {}\n")?;
    std::fs::write(directory.join("monitors/monitors.yaml"), "monitors: []\n")?;
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
    Ok(())
}

fn component_names(resolved: &echo_agent::plugin::ResolvedComponents) -> Vec<String> {
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
    if resolved.monitors_file.is_some() {
        names.push("monitors".to_string());
    }
    if !resolved.theme_files.is_empty() {
        names.push("themes".to_string());
    }
    if !resolved.output_style_files.is_empty() {
        names.push("output_styles".to_string());
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

    fn write_fixture(root: &Path) -> Result<PathBuf, String> {
        let plugin = root.join(".echo-agent/plugins/runtime-fixture");
        PluginRuntimeService::scaffold(&plugin, "runtime-fixture")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("monitors/monitors.yaml"),
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
        std::fs::write(plugin.join(".lsp.yaml"), lsp).map_err(|error| error.to_string())
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
        let scheduler = Arc::new(SchedulerRunner::new(
            cron_store,
            echo_agent::agent::CancellationToken::new(),
            fire_fn,
        ));
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
        let scheduler = Arc::new(SchedulerRunner::new(
            cron_store,
            echo_agent::agent::CancellationToken::new(),
            fire_fn,
        ));
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
        assert!(registry.contains("runtime-fixture-specialist").await);
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
    async fn scheduler_binding_uses_the_same_lock_order_as_reload() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(SchedulerRunner::new(
            cron_store,
            echo_agent::agent::CancellationToken::new(),
            fire_fn,
        ));

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

    #[test]
    fn scaffold_and_validate_cover_application_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = temp.path().join("scaffolded");
        PluginRuntimeService::scaffold(&plugin, "scaffolded").map_err(|error| error.to_string())?;
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
}
