//! Tauri IPC commands for plugin management.
//!
//! All commands delegate to the shared [`PluginRuntimeService`] living on
//! `AppState` (audit P0-4). Previously every command spun up its own
//! `PluginRegistry` via `build_registry()`, completely disconnected from the
//! running agent's `SkillRegistry`/`HookRegistry`/`McpManager`. The shared
//! service keeps one registry and runs the framework `wire_all` on
//! enable/disable/reload so changes actually take effect against the live
//! agent.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::plugin::{InstallSource, PluginEntry, PluginScope, PluginUserConfigEntry};
use echo_agent_app_core::plugin_runtime::PluginRuntimeService;
use serde::{Deserialize, Serialize};

// ── PluginInfo structure (matches previous server API) ──────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub license: Option<String>,
    pub scope: String,
    pub enabled: bool,
    pub path: String,
    pub capabilities: Vec<String>,
    pub keywords: Vec<String>,
    pub dependencies: Vec<DependencyInfo>,
    pub config: std::collections::HashMap<String, PluginUserConfigEntry>,
    pub config_values: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub name: String,
    pub version: Option<String>,
}

fn entry_to_info(entry: &PluginEntry) -> PluginInfo {
    let caps = echo_agent_app_core::plugin_runtime::plugin_capabilities(entry)
        .into_iter()
        .map(|c| c.display_name().to_string())
        .collect();

    PluginInfo {
        name: entry.manifest.name.clone(),
        display_name: entry.manifest.display_name().to_string(),
        version: entry.manifest.version_label().to_string(),
        description: entry.manifest.description.clone(),
        author: entry
            .manifest
            .author
            .as_ref()
            .and_then(|author| author.name.clone()),
        license: entry.manifest.license.clone(),
        scope: entry.scope.to_string(),
        enabled: entry.enabled,
        path: entry.root.display().to_string(),
        capabilities: caps,
        keywords: entry.manifest.keywords.clone(),
        dependencies: entry
            .manifest
            .dependencies
            .iter()
            .map(|d| DependencyInfo {
                name: d.name().to_string(),
                version: d.version_constraint().map(|s| s.to_string()),
            })
            .collect(),
        config: entry.manifest.config.clone(),
        config_values: entry.user_config.clone(),
    }
}

/// Resolve the shared plugin runtime from Tauri state.
///
/// Returns an error when running in a headless/IM-channel mode where the
/// service is not constructed (no primary agent is exposed for IPC). Plugin
/// IPC only exists in GUI/Tauri mode, so this is a hard configuration error
/// rather than a recoverable condition.
async fn require_service(
    state: &TauriState,
) -> Result<std::sync::Arc<PluginRuntimeService>, IpcError> {
    state
        .app_state
        .current_plugin_runtime_owned()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))
}

// ── IPC Commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_plugins(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    let plugins: Vec<PluginInfo> = service.list().await.iter().map(entry_to_info).collect();
    Ok(serde_json::to_value(plugins).unwrap_or_default())
}

#[tauri::command]
pub async fn get_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    match service.get(&name).await {
        Some(entry) => {
            let info = entry_to_info(&entry);
            Ok(serde_json::to_value(info).unwrap_or_default())
        }
        None => Err(IpcError::NotFound(format!("Plugin '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn install_plugin(
    state: tauri::State<'_, TauriState>,
    source: String,
    scope: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let scope = scope
        .and_then(|s| PluginScope::from_arg(&s))
        .unwrap_or(PluginScope::User);
    let source = InstallSource::parse(&source);

    // This is a local personal assistant: a user-selected extension path is
    // trusted. Keep validation to the source existing; the framework validates
    // the plugin manifest before copying it.
    if let InstallSource::Local(ref src_path) = source {
        src_path
            .canonicalize()
            .map_err(|_| IpcError::NotFound(format!("插件源目录不存在: {}", src_path.display())))?;
    }

    match state.app_state.install_plugin_owned(&source, scope).await {
        Ok((id, summary)) => {
            // Reuse the shared registry snapshot (already refreshed by reload
            // inside install) for the response info.
            let service = require_service(&state).await?;
            let info = service.get(&id).await.map(|e| entry_to_info(&e));
            let wiring_ok = summary.errors.is_empty();
            Ok(serde_json::json!({
                "success": true,
                "plugin_id": id,
                "info": info,
                "wiring_ok": wiring_ok,
                "errors": summary.errors,
            }))
        }
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn uninstall_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
    keep_data: Option<bool>,
) -> Result<serde_json::Value, IpcError> {
    match state
        .app_state
        .uninstall_plugin_owned(&name, keep_data.unwrap_or(false))
        .await
    {
        Ok(summary) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' uninstalled", name),
            "wiring_ok": summary.errors.is_empty(),
            "errors": summary.errors,
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn enable_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    match state.app_state.set_plugin_enabled_owned(&name, true).await {
        Ok(summary) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' enabled", name),
            "wiring_ok": summary.errors.is_empty(),
            "errors": summary.errors,
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn disable_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    match state.app_state.set_plugin_enabled_owned(&name, false).await {
        Ok(summary) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' disabled", name),
            "wiring_ok": summary.errors.is_empty(),
            "errors": summary.errors,
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn configure_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
    values: std::collections::HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value, IpcError> {
    let summary = state
        .app_state
        .configure_plugin_owned(&name, values)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(serde_json::json!({
        "success": summary.errors.is_empty(),
        "message": format!("Plugin '{}' configured", name),
        "errors": summary.errors,
    }))
}

#[tauri::command]
pub async fn reload_plugins(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    match state.app_state.reload_plugins_owned().await {
        Ok(summary) => {
            let success = summary.errors.is_empty();
            let error = (!success).then(|| summary.errors.join("; "));
            Ok(serde_json::json!({
                "success": success,
                "total": summary.total,
                "enabled": summary.enabled,
                "skills_loaded": summary.skills_loaded,
                "hooks_registered": summary.hooks_registered,
                "mcp_connected": summary.mcp_connected,
                "agents_loaded": summary.agents_loaded,
                "lsp_languages_loaded": summary.lsp_languages_loaded,
                "monitors_loaded": summary.monitors_loaded,
                "themes_loaded": summary.themes_loaded,
                "output_styles_loaded": summary.output_styles_loaded,
                "errors": summary.errors,
                "message": format!("Reloaded {} plugins", summary.total),
                "error": error,
            }))
        }
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn list_plugin_themes(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    let themes = service.themes().await;
    let active = service.active_theme().await;
    Ok(serde_json::json!({ "themes": themes, "active": active }))
}

#[tauri::command]
pub async fn activate_plugin_theme(
    state: tauri::State<'_, TauriState>,
    name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    let selected = name.filter(|value| !value.trim().is_empty());
    let theme = service
        .activate_theme(selected.as_deref())
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({
        "success": true,
        "active": selected,
        "theme": theme,
    }))
}

#[tauri::command]
pub async fn list_plugin_output_styles(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    let styles = service.output_styles().await;
    let active = service.active_output_style().await;
    Ok(serde_json::json!({ "styles": styles, "active": active }))
}

#[tauri::command]
pub async fn activate_plugin_output_style(
    state: tauri::State<'_, TauriState>,
    name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state).await?;
    let selected = name.filter(|value| !value.trim().is_empty());
    service
        .activate_output_style(selected.as_deref())
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({ "success": true, "active": selected }))
}

#[tauri::command]
pub async fn scaffold_plugin(
    directory: String,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let result = PluginRuntimeService::scaffold(&directory, &name)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({
        "success": true,
        "path": result.path,
        "name": result.name,
        "message": format!("Plugin '{}' scaffolded", result.name),
    }))
}

#[tauri::command]
pub async fn validate_plugin(directory: String) -> Result<serde_json::Value, IpcError> {
    serde_json::to_value(PluginRuntimeService::validate(directory))
        .map_err(|error| IpcError::Internal(format!("Failed to serialize validation: {error}")))
}
