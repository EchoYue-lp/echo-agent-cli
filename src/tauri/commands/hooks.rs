//! Tauri IPC commands for hook management.
//!
//! These mirror the CLI `/hooks list` and `/hooks reload` slash commands so
//! the GUI Hooks panel is feature-parity with TUI (AGENTS.md: TUI/GUI must be
//! functionally equivalent). Both read from the live agent's `HookRegistry`
//! via the primary agent handle, the same accessor the CLI uses.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::hook_config_loader::HookConfigLoader;
use serde::{Deserialize, Serialize};

// ── DTOs ────────────────────────────────────────────────────────────────────

/// A single registered hook source (e.g. `user_config`, `skill:foo`,
/// `plugin:bar`) plus the number of rules it contributes.
#[derive(Debug, Serialize, Deserialize)]
pub struct HookSourceInfo {
    pub source: String,
    pub rule_count: usize,
}

/// Summary of a reload: where hooks were merged from and the total rule count.
#[derive(Debug, Serialize, Deserialize)]
pub struct HooksReloadSummary {
    pub success: bool,
    pub rule_count: usize,
    /// Display strings of the files the merged definition was loaded from.
    pub loaded_from: Vec<String>,
    pub message: String,
}

// ── IPC Commands ────────────────────────────────────────────────────────────

/// List every registered hook source on the live agent, with rule counts.
///
/// Mirrors `HookRegistry::list_sources` (returns `Vec<(source_name, rule_count)>`).
/// Returns an empty vector (rather than an error) when no hooks are configured.
#[tauri::command]
pub async fn list_hooks(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<HookSourceInfo>, IpcError> {
    let agent_handle = state.app_state.connection.primary_agent();
    let sources: Vec<(String, usize)> = agent_handle
        .read_async(|a| {
            Box::pin(async move {
                let registry = a.hook_registry().read().await;
                registry.list_sources()
            })
        })
        .await;
    let infos = sources
        .into_iter()
        .map(|(source, rule_count)| HookSourceInfo { source, rule_count })
        .collect();
    Ok(infos)
}

/// Reload user-configured hooks from disk and re-register them on the live
/// agent.
///
/// This reuses the shared `HookConfigLoader::load_merged_from_disk` path that
/// the CLI `/hooks reload` uses, so both surfaces apply the exact same merge
/// semantics (echo-agent.yaml inline + ~/.eko/hooks.yaml + .eko/hooks.yaml).
/// Only user-config hooks are replaced (`clear_user_hooks` +
/// `register_user_hooks`); skill/plugin-sourced hooks are left intact.
#[tauri::command]
pub async fn reload_hooks(
    state: tauri::State<'_, TauriState>,
) -> Result<HooksReloadSummary, IpcError> {
    // Load the merged definition outside the agent write lock — disk I/O should
    // never block other agent readers/writers.
    let load_result = HookConfigLoader::load_merged_from_disk();
    let rule_count: usize = load_result.definition.rules.values().map(|v| v.len()).sum();

    if load_result.definition.is_empty() {
        return Ok(HooksReloadSummary {
            success: true,
            rule_count: 0,
            loaded_from: load_result
                .loaded_from
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            message: "No hooks found in config sources".to_string(),
        });
    }

    let hooks_def = load_result.definition;
    let loaded_from: Vec<String> = load_result
        .loaded_from
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    // Register into the agent's hook registry. clear_user_hooks +
    // register_user_hooks both need `&mut`, so this runs under write_async.
    let agent_handle = state.app_state.connection.primary_agent();
    agent_handle
        .write_async(|a| {
            Box::pin(async move {
                let mut registry = a.hook_registry().write().await;
                registry.clear_user_hooks();
                registry.register_user_hooks(hooks_def);
            })
        })
        .await;

    Ok(HooksReloadSummary {
        success: true,
        rule_count,
        loaded_from,
        message: format!("Reloaded {} hook rules", rule_count),
    })
}
