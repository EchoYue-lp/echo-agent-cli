//! Tauri IPC commands for plugin management.
//!
//! All commands delegate to the shared [`PluginRuntimeService`] living on
//! `AppState` (audit P0-4). Previously every command spun up its own
//! `PluginRegistry` via `build_registry()`, completely disconnected from the
//! running agent's `SkillRegistry`/`HookRegistry`/`McpManager`. The shared
//! service keeps one registry and publishes one prepared framework generation
//! on enable/disable/reload so changes take effect against the live agent.

use crate::tauri::commands::extensions;
use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::extension_commands::{
    ExtensionCommand, ExtensionCommandReceipt, ExtensionRequestScope, PluginCommand,
    PluginInstallScope,
};

async fn dispatch_plugin(
    state: &TauriState,
    request_scope: ExtensionRequestScope,
    command: PluginCommand,
) -> Result<ExtensionCommandReceipt, IpcError> {
    Ok(extensions::dispatch_scoped(
        state,
        request_scope,
        "tauri-plugin-control",
        ExtensionCommand::Plugins(command),
        None,
    )
    .await)
}

// ── IPC Commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_plugins(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::List).await
}

#[tauri::command]
pub async fn get_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: String,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Info { name }).await
}

#[tauri::command]
pub async fn install_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    source: String,
    scope: Option<String>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    let install_scope = match scope.as_deref() {
        Some("project" | "p") => PluginInstallScope::Project,
        Some("local" | "l") => PluginInstallScope::Local,
        _ => PluginInstallScope::User,
    };
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Install {
            source,
            scope: install_scope,
        },
    )
    .await
}

#[tauri::command]
pub async fn uninstall_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: String,
    keep_data: Option<bool>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Uninstall {
            name,
            keep_data: keep_data.unwrap_or(false),
        },
    )
    .await
}

#[tauri::command]
pub async fn enable_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: String,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Enable { name }).await
}

#[tauri::command]
pub async fn disable_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: String,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Disable { name }).await
}

#[tauri::command]
pub async fn configure_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: String,
    values: std::collections::HashMap<String, serde_json::Value>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Configure { name, values },
    )
    .await
}

#[tauri::command]
pub async fn reload_plugins(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Reload).await
}

#[tauri::command]
pub async fn list_plugin_themes(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Themes).await
}

#[tauri::command]
pub async fn activate_plugin_theme(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: Option<String>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    let selected = name.filter(|value| !value.trim().is_empty());
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Theme { name: selected },
    )
    .await
}

#[tauri::command]
pub async fn list_plugin_output_styles(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Styles).await
}

#[tauri::command]
pub async fn activate_plugin_output_style(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    name: Option<String>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    let selected = name.filter(|value| !value.trim().is_empty());
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Style { name: selected },
    )
    .await
}

#[tauri::command]
pub async fn scaffold_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    directory: String,
    name: String,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(
        &state,
        request_scope,
        PluginCommand::Scaffold { directory, name },
    )
    .await
}

#[tauri::command]
pub async fn validate_plugin(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    directory: String,
) -> Result<ExtensionCommandReceipt, IpcError> {
    dispatch_plugin(&state, request_scope, PluginCommand::Validate { directory }).await
}
