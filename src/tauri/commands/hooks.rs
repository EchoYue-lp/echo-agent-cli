//! Tauri IPC commands for hook management.
//!
//! These mirror the CLI `/hooks list` and `/hooks reload` slash commands so
//! the GUI Hooks panel is feature-parity with TUI (AGENTS.md: TUI/GUI must be
//! functionally equivalent). Both read from the live agent's `HookRegistry`
//! via the primary agent handle, the same accessor the CLI uses.

use crate::tauri::commands::extensions;
use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::extension_commands::{
    ExtensionCommand, ExtensionCommandReceipt, ExtensionRequestScope, HookCommand,
};

// ── IPC Commands ────────────────────────────────────────────────────────────

/// List every registered hook source on the live agent, with rule counts.
///
/// Mirrors `HookRegistry::list_sources` (returns `Vec<(source_name, rule_count)>`).
/// Returns an empty vector (rather than an error) when no hooks are configured.
#[tauri::command]
pub async fn list_hooks(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    Ok(extensions::dispatch_scoped(
        &state,
        request_scope,
        "tauri-hook-control",
        ExtensionCommand::Hooks(HookCommand::List),
        None,
    )
    .await)
}

#[tauri::command]
pub fn list_hook_events() -> Vec<&'static str> {
    echo_agent::skills::hooks::HookEvent::ALL
        .iter()
        .map(|event| event.as_str())
        .collect()
}

#[tauri::command]
pub async fn test_hook(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    event: String,
    matcher: Option<String>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    Ok(extensions::dispatch_scoped(
        &state,
        request_scope,
        "tauri-hook-control",
        ExtensionCommand::Hooks(HookCommand::Test {
            event,
            matcher: matcher.unwrap_or_else(|| "*".to_string()),
        }),
        None,
    )
    .await)
}

/// Reload user-configured hooks from disk and re-register them on the live
/// agent.
///
/// This reuses the shared `HookConfigLoader::load_merged_from_disk` path that
/// the CLI `/hooks reload` uses, so both surfaces apply the exact same merge
/// semantics (eko.yaml inline + ~/.eko/hooks.yaml + .eko/hooks.yaml).
/// Only user-config hooks are replaced (`clear_user_hooks` +
/// `register_user_hooks`); skill/plugin-sourced hooks are left intact.
#[tauri::command]
pub async fn reload_hooks(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    Ok(extensions::dispatch_scoped(
        &state,
        request_scope,
        "tauri-hook-control",
        ExtensionCommand::Hooks(HookCommand::Reload),
        None,
    )
    .await)
}
