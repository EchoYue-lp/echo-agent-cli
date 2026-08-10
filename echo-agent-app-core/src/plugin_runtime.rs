//! PluginRuntimeService — process-level shared plugin runtime.
//!
//! Audit P0-4: the previous Tauri plugin commands each called `build_registry()`
//! to spin up a brand-new `PluginRegistry` on every IPC call, completely
//! disconnected from the running agent's `SkillRegistry` / `HookRegistry` /
//! `McpManager`. `enable`/`disable` only flipped a flag on disk; `reload` only
//! recomputed counts — none of them ever touched the live agent.
//!
//! This service fixes that by holding one shared `PluginRegistry` plus a
//! reference to the running primary agent (`AgentHandle`). Enable/disable and
//! reload now run the framework `PluginIntegrator::wire_all` against the live
//! agent so newly enabled skills/hooks/MCP servers actually get wired in.
//!
//! ## Industry reference
//!
//! Codex and Claude Code both implement enable/disable as "re-discovery +
//! registry rebuild" rather than live hot-plug of a running agent (the agent's
//! subsystems don't support atomic removal of a single registered component).
//! EKO follows the same model: every state change re-runs `wire_all`, which is
//! additive. Disabling a plugin therefore does NOT unload the components it
//! already registered (skills/hooks are still in the agent's registries). True
//! unload is a P1 follow-up tracked in `docs/MASTER-PLAN.md`.
//!
//! ## Threading model
//!
//! The shared registry is guarded by a `tokio::sync::Mutex` because `wire_all`
//! is async and we hold the registry across an `.await` (the agent write lock
//! is also async). Using `parking_lot::Mutex` here would deadlock-prone or
//! require holding a sync guard across an await (not `Send`).

use std::sync::Arc;

use echo_agent::plugin::{
    InstallSource, PluginEntry, PluginIntegrator, PluginRegistry, PluginScope, PluginWiringResult,
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
pub struct PluginRuntimeService {
    agent_handle: AgentHandle,
    registry: Mutex<PluginRegistry>,
}

impl PluginRuntimeService {
    /// Construct the service, deriving `project_root` from the agent's current
    /// `working_dir` (falls back to process cwd).
    ///
    /// Performs an initial `scan_all()` so that `list()`/`get()` work
    /// immediately without requiring a separate reload call. Initial wiring
    /// into the agent is NOT done here — bootstrap (`runtime::load_plugins`)
    /// already wired plugins once during agent construction, and re-running
    /// `wire_all` would double-register skills/hooks. The first `reload()`
    /// call from a user action is what re-wires.
    pub async fn new(agent_handle: AgentHandle) -> Arc<Self> {
        let project_root = agent_handle
            .read(|agent| agent.working_dir())
            .await
            .or_else(|| std::env::current_dir().ok());

        let mut registry = PluginRegistry::new(project_root);
        if let Err(error) = registry.scan_all() {
            tracing::warn!(%error, "PluginRuntimeService: initial scan_all failed");
        }

        Arc::new(Self {
            agent_handle,
            registry: Mutex::new(registry),
        })
    }

    /// Re-scan all plugin scopes and re-run `wire_all` against the live agent.
    ///
    /// This is the rebuild model: discovery + wiring are redone from scratch
    /// against the running agent (not a hot-plug). See module docs for the
    /// known limitation around disable.
    pub async fn reload(&self) -> anyhow::Result<ReloadSummary> {
        // Re-scan with the agent's CURRENT working_dir as project root so a
        // workspace switch is reflected without restarting the service. We
        // reconstruct the registry in place: scan_all already clears plugins,
        // but it doesn't re-resolve project_root, so we rebuild it.
        let project_root = self
            .agent_handle
            .read(|agent| agent.working_dir())
            .await
            .or_else(|| std::env::current_dir().ok());

        let mut registry = PluginRegistry::new(project_root);
        registry
            .scan_all()
            .map_err(|e| anyhow::anyhow!("Plugin scan failed: {e}"))?;

        let total = registry.count();
        let enabled = registry.list_enabled().len();

        // Run wire_all against the live agent inside the write lock. The
        // registry is moved into the closure (it must be `'static` + `Send`),
        // wired by reference, and returned back out so we can publish it to
        // the shared slot. Returning `(PluginWiringResult, PluginRegistry)`
        // avoids a "use of moved value" after the closure.
        let (wiring, registry) = self
            .agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    let wiring = PluginIntegrator::new().wire_all(agent, &mut registry).await;
                    (wiring, registry)
                })
            })
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

        // Publish the freshly-wired registry back to the shared slot.
        *self.registry.lock().await = registry;

        Ok(summary)
    }

    /// Enable a plugin and re-wire the agent.
    ///
    /// The plugin becomes eligible for `wire_all`; reload attaches its
    /// skills/hooks/MCP into the running agent.
    pub async fn enable(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut registry = self.registry.lock().await;
            registry
                .enable(name)
                .map_err(|e| anyhow::anyhow!("Enable plugin '{name}' failed: {e}"))?;
            // save_state is already invoked inside registry.enable; no extra
            // persistence needed here.
        }
        self.reload().await.map(|_| ())
    }

    /// Disable a plugin and re-wire the agent.
    ///
    /// **Known limitation (P1):** the framework `wire_all` is additive — it
    /// registers components but cannot unregister them. After disable, the
    /// re-run `wire_all` only wires the still-enabled subset, but the disabled
    /// plugin's previously-registered skills/hooks remain in the agent's
    /// registries (MCP servers are idempotent — re-running `load_mcp_from_file`
    /// with the smaller set won't disconnect the dropped ones either). True
    /// unload requires framework support and is tracked in MASTER-PLAN.
    pub async fn disable(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut registry = self.registry.lock().await;
            registry
                .disable(name)
                .map_err(|e| anyhow::anyhow!("Disable plugin '{name}' failed: {e}"))?;
        }
        self.reload().await.map(|_| ())
    }

    /// Install a plugin from a source into a scope, then reload to wire it.
    ///
    /// `Local` source confinement to allowed roots (workspace / home) is the
    /// caller's responsibility (preserved in the Tauri command) — this method
    /// trusts the caller. Git-source SSRF is handled inside the framework
    /// registry.
    pub async fn install(
        &self,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<String> {
        let plugin_id = {
            let mut registry = self.registry.lock().await;
            registry
                .install(source, scope)
                .map_err(|e| anyhow::anyhow!("Install plugin failed: {e}"))?
        };
        self.reload().await.map(|_| ())?;
        Ok(plugin_id)
    }

    /// Uninstall a plugin, then reload (which re-wires the remaining set).
    ///
    /// Same additive-wire limitation as `disable`: already-registered
    /// components from the uninstalled plugin are not unloaded from the agent
    /// (P1).
    pub async fn uninstall(&self, name: &str, keep_data: bool) -> anyhow::Result<()> {
        {
            let mut registry = self.registry.lock().await;
            registry
                .uninstall(name, keep_data)
                .map_err(|e| anyhow::anyhow!("Uninstall plugin '{name}' failed: {e}"))?;
        }
        self.reload().await.map(|_| ())
    }

    /// Snapshot of all installed plugins (sorted by name by the framework).
    pub async fn list(&self) -> Vec<PluginEntry> {
        self.registry
            .lock()
            .await
            .list()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Lookup a single plugin by id.
    pub async fn get(&self, name: &str) -> Option<PluginEntry> {
        self.registry.lock().await.get(name).cloned()
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
