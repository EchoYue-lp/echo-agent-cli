//! PluginRuntimeService — process-level shared plugin runtime.
//!
//! Audit P0-4: the previous Tauri plugin commands each called `build_registry()`
//! to spin up a brand-new `PluginRegistry` on every IPC call, completely
//! disconnected from the running agent's `SkillRegistry` / `HookRegistry` /
//! `McpManager`. `enable`/`disable` only flipped a flag on disk; `reload` only
//! recomputed counts — none of them ever touched the live agent.
//!
//! This service owns discovery and live component state for every interaction
//! surface. Each mutation is serialized and rebuilt as one operation: scan the
//! new registry, unload the previous skills/hooks/MCP servers, then wire the
//! enabled set. Reload therefore cannot accumulate duplicate or stale entries.
//!
//! ## Threading model
//!
//! One async mutex protects registry state and serializes reload, install,
//! uninstall, enable, and disable. This prevents a slower rebuild from
//! publishing stale state over a newer operation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use echo_agent::plugin::{
    InstallSource, PluginEntry, PluginIntegrator, PluginRegistry, PluginScope, PluginWiringResult,
    WiredPluginComponents,
};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::agent_handle::AgentHandle;

/// Summary returned by [`PluginRuntimeService::reload`].
///
/// `total`/`enabled` describe the discovered plugin set; the `*_loaded`/
/// `*_registered`/`*_connected` fields come from the framework wiring result
/// and report what was actually attached to the running agent. `errors` lists
/// per-plugin wiring failures (resolution errors, missing files, etc.) without
/// aborting the whole reload.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadSummary {
    pub total: usize,
    pub enabled: usize,
    pub skills_loaded: usize,
    pub hooks_registered: usize,
    pub mcp_connected: usize,
    pub errors: Vec<String>,
}

impl ReloadSummary {
    fn from_wiring(total: usize, enabled: usize, wiring: &PluginWiringResult) -> Self {
        Self {
            total,
            enabled,
            skills_loaded: wiring.skills_loaded.len(),
            hooks_registered: wiring.hooks_registered.len(),
            mcp_connected: wiring.mcp_connected.len(),
            errors: wiring.errors.clone(),
        }
    }
}

/// Process-level shared plugin runtime.
///
/// Holds the running primary agent's handle together with the shared
/// `PluginRegistry`. The `project_root` passed to the registry is derived from
/// the agent's `working_dir` so `Project`/`Local` scoped plugins resolve
/// against the active workspace (matching the bootstrap-time `load_plugins`
/// behavior in `runtime.rs`).
struct PluginRuntimeState {
    registry: PluginRegistry,
    components_by_plugin: HashMap<String, WiredPluginComponents>,
}

pub struct PluginRuntimeService {
    agent_handle: AgentHandle,
    state: Mutex<PluginRuntimeState>,
}

impl PluginRuntimeService {
    /// Construct the service, deriving `project_root` from the agent's current
    /// `working_dir` (falls back to process cwd).
    ///
    /// The initial load goes through the same rebuild path as every later
    /// reload, so bootstrap cannot double-register components.
    pub async fn new(agent_handle: AgentHandle) -> Arc<Self> {
        let service = Arc::new(Self {
            agent_handle,
            state: Mutex::new(PluginRuntimeState {
                registry: PluginRegistry::new(None),
                components_by_plugin: HashMap::new(),
            }),
        });
        if let Err(error) = service.reload().await {
            tracing::warn!(%error, "PluginRuntimeService: initial plugin load failed");
        }
        service
    }

    /// Re-scan all plugin scopes and re-run `wire_all` against the live agent.
    ///
    pub async fn reload(&self) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        self.reload_locked(&mut state).await
    }

    async fn reload_locked(&self, state: &mut PluginRuntimeState) -> anyhow::Result<ReloadSummary> {
        let previously_enabled = state
            .registry
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<HashSet<_>>();
        // Re-scan with the agent's CURRENT working_dir as project root so a
        // workspace switch is reflected without restarting the service.
        let project_root = self
            .agent_handle
            .read(|agent| agent.working_dir())
            .await
            .or_else(|| std::env::current_dir().ok());

        let mut registry = PluginRegistry::new(project_root);
        registry
            .scan_all()
            .map_err(|e| anyhow::anyhow!("Plugin scan failed: {e}"))?;
        registry
            .resolve_enabled_dependencies()
            .map_err(|error| anyhow::anyhow!("Plugin dependency validation failed: {error}"))?;

        let total = registry.count();
        let enabled = registry.list_enabled().len();
        let enabled_names = registry
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<HashSet<_>>();
        let mut externally_disabled = previously_enabled
            .difference(&enabled_names)
            .cloned()
            .collect::<Vec<_>>();
        externally_disabled.sort();
        let previous_components = std::mem::take(&mut state.components_by_plugin);

        let (wiring, registry, hook_registry, session_id, agent_name) = self
            .agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    unload_components(agent, previous_components).await;
                    let wiring = PluginIntegrator::new().wire_all(agent, &mut registry).await;
                    let hook_registry = agent.hook_registry().clone();
                    let session_id = agent
                        .config()
                        .get_session_id()
                        .unwrap_or_default()
                        .to_string();
                    let agent_name = agent.config().get_agent_name().to_string();
                    (wiring, registry, hook_registry, session_id, agent_name)
                })
            })
            .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginDisabled,
            &externally_disabled,
            &session_id,
            &agent_name,
        )
        .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginLoaded,
            &wiring.plugins_loaded,
            &session_id,
            &agent_name,
        )
        .await;

        tracing::info!(
            total,
            enabled,
            skills = wiring.skills_loaded.len(),
            hooks = wiring.hooks_registered.len(),
            mcp = wiring.mcp_connected.len(),
            errors = wiring.errors.len(),
            "PluginRuntimeService: reload wired plugins into live agent"
        );
        for err in &wiring.errors {
            tracing::warn!(error = %err, "Plugin reload wiring error");
        }

        let summary = ReloadSummary::from_wiring(total, enabled, &wiring);
        state.components_by_plugin = wiring.components_by_plugin.clone();
        state.registry = registry;

        Ok(summary)
    }

    /// Enable a plugin and re-wire the agent.
    ///
    /// The plugin becomes eligible for `wire_all`; reload attaches its
    /// skills/hooks/MCP into the running agent.
    pub async fn enable(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        state
            .registry
            .enable(name)
            .map_err(|e| anyhow::anyhow!("Enable plugin '{name}' failed: {e}"))?;
        self.reload_locked(&mut state).await
    }

    /// Disable a plugin and rebuild the live enabled set.
    pub async fn disable(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        state
            .registry
            .disable(name)
            .map_err(|e| anyhow::anyhow!("Disable plugin '{name}' failed: {e}"))?;
        let summary = self.reload_locked(&mut state).await?;
        self.fire_plugin_disabled(name).await;
        Ok(summary)
    }

    /// Install a plugin from a source into a scope, then reload to wire it.
    ///
    pub async fn install(
        &self,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let mut state = self.state.lock().await;
        let plugin_id = state
            .registry
            .install(source, scope)
            .map_err(|e| anyhow::anyhow!("Install plugin failed: {e}"))?;
        let summary = self.reload_locked(&mut state).await?;
        Ok((plugin_id, summary))
    }

    /// Uninstall a plugin and rebuild the remaining live set.
    pub async fn uninstall(&self, name: &str, keep_data: bool) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        state
            .registry
            .uninstall(name, keep_data)
            .map_err(|e| anyhow::anyhow!("Uninstall plugin '{name}' failed: {e}"))?;
        let summary = self.reload_locked(&mut state).await?;
        self.fire_plugin_disabled(name).await;
        Ok(summary)
    }

    /// Snapshot of all installed plugins (sorted by name by the framework).
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

    /// Lookup a single plugin by id.
    pub async fn get(&self, name: &str) -> Option<PluginEntry> {
        self.state.lock().await.registry.get(name).cloned()
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

async fn fire_plugin_events(
    hook_registry: &std::sync::Arc<tokio::sync::RwLock<echo_agent::skills::hooks::HookRegistry>>,
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

async fn unload_components(
    agent: &mut echo_agent::agent::react::ReactAgent,
    components_by_plugin: HashMap<String, WiredPluginComponents>,
) {
    for (plugin_name, components) in components_by_plugin {
        let source_tag = format!("plugin:{plugin_name}");
        let removed_skills = agent.unregister_skills_by_source(&source_tag).await.len();
        let removed_hooks = if components.hooks_registered {
            let hook_source = echo_agent::skills::hooks::HookSource::Plugin(plugin_name.clone());
            agent.hook_registry().write().await.unregister(&hook_source)
        } else {
            false
        };
        let mut removed_mcp = 0usize;
        for server_name in components.mcp_servers {
            if agent.disconnect_mcp(&server_name).await {
                removed_mcp = removed_mcp.saturating_add(1);
            }
        }
        tracing::info!(
            plugin = %plugin_name,
            removed_skills,
            removed_hooks,
            removed_mcp,
            "Unloaded plugin components from live agent"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ReloadSummary::from_wiring` must faithfully mirror the wiring result
    /// counts. Uses a synthesized `PluginWiringResult` to avoid needing a live
    /// agent + real plugins on disk.
    #[test]
    fn reload_summary_reflects_wiring_counts() {
        let wiring = PluginWiringResult {
            skills_loaded: vec!["a".into(), "b".into()],
            hooks_registered: vec!["p1".into()],
            errors: vec!["boom".into()],
            ..Default::default()
        };
        let summary = ReloadSummary::from_wiring(5, 3, &wiring);
        assert_eq!(summary.total, 5);
        assert_eq!(summary.enabled, 3);
        assert_eq!(summary.skills_loaded, 2);
        assert_eq!(summary.hooks_registered, 1);
        assert_eq!(summary.mcp_connected, 0);
        assert_eq!(summary.errors, vec!["boom".to_string()]);
    }
}
