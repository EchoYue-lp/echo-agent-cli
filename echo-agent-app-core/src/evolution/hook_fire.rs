//! Evolution hook fire helpers.
//!
//! Provides a best-effort helper to fire evolution lifecycle hook events
//! (`RulePromoted`, `SkillMergeApplied`, etc.) from CLI/Tauri command handlers
//! that hold an `AgentHandle` but are not inside the ReactAgent's own methods.
//!
//! These events notify registered hooks (e.g. user-configured hooks in
//! `hooks.toml`) that an evolution mutation has occurred. The fire is
//! best-effort: errors are logged and never propagated to the caller.

use echo_agent::agent::AgentHandle;
use echo_core::hooks::{HookContext, HookEvent};

/// Fire an evolution lifecycle hook event.
///
/// Reads the agent's `HookRegistry` via `AgentHandle` and runs matching
/// lifecycle hooks. Best-effort: errors are logged, never returned.
///
/// # Arguments
/// * `agent` — The agent handle to read the hook registry from.
/// * `event` — The lifecycle event to fire (e.g. `HookEvent::RulePromoted`).
/// * `matcher` — A skill name or key for the event (used by hook matchers).
pub async fn fire_evolution_hook(agent: &AgentHandle, event: HookEvent, matcher: &str) {
    // Read the shared hook registry (Arc clone — cheap).
    let registry_arc = agent.read(|a| a.hook_registry().clone()).await;

    // Read session_id and agent_name from config via public getters.
    let session_id = agent
        .read(|a| a.config().get_session_id().unwrap_or("").to_string())
        .await;
    let agent_name = agent
        .read(|a| a.config().get_agent_name().to_string())
        .await;

    let ctx = HookContext::for_lifecycle(event, matcher, &session_id, &agent_name);

    let registry = registry_arc.read().await;
    let _ = registry.run_lifecycle_hooks(&ctx).await;
}
