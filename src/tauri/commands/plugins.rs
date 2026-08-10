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
use echo_agent::plugin::{InstallSource, PluginEntry, PluginScope};
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
    pub config_keys: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub name: String,
    pub version: Option<String>,
}

fn entry_to_info(entry: &PluginEntry) -> PluginInfo {
    let caps = entry
        .manifest
        .inferred_capabilities()
        .into_iter()
        .map(|c| c.display_name().to_string())
        .collect();

    PluginInfo {
        name: entry.manifest.name.clone(),
        display_name: entry.manifest.display_name().to_string(),
        version: entry.manifest.version.clone(),
        description: entry.manifest.description.clone(),
        author: entry.manifest.author.as_ref().map(|a| a.name.clone()),
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
        config_keys: entry.manifest.config.keys().cloned().collect(),
    }
}

/// Resolve the shared plugin runtime from Tauri state.
///
/// Returns an error when running in a headless/IM-channel mode where the
/// service is not constructed (no primary agent is exposed for IPC). Plugin
/// IPC only exists in GUI/Tauri mode, so this is a hard configuration error
/// rather than a recoverable condition.
fn require_service(state: &TauriState) -> Result<std::sync::Arc<PluginRuntimeService>, IpcError> {
    state
        .app_state
        .plugin_runtime
        .clone()
        .ok_or_else(|| IpcError::Internal("Plugin runtime service is not initialized".to_string()))
}

// ── IPC Commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_plugins(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state)?;
    let plugins: Vec<PluginInfo> = service
        .list()
        .await
        .iter()
        .map(entry_to_info)
        .collect();
    Ok(serde_json::to_value(plugins).unwrap_or_default())
}

#[tauri::command]
pub async fn get_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state)?;
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
    let service = require_service(&state)?;
    let scope = scope
        .and_then(|s| PluginScope::from_arg(&s))
        .unwrap_or(PluginScope::User);
    let source = InstallSource::parse(&source);

    // P1-6: confine a Local install source to an allowed root. A `Local(path)`
    // source copies that directory into a plugin scope dir; without confinement
    // a compromised page could aggregate any on-disk directory (e.g. copy
    // `~/.ssh` into a plugin and read it back). Git sources are SSRF-checked in
    // the registry. Allow only paths under the current workspace root or home.
    if let InstallSource::Local(ref src_path) = source {
        let canonical = src_path
            .canonicalize()
            .map_err(|_| IpcError::NotFound(format!("插件源目录不存在: {}", src_path.display())))?;
        let allowed = super::panels::allowed_skill_roots(&state).await;
        if !allowed.iter().any(|root| canonical.starts_with(root)) {
            return Err(IpcError::Validation(format!(
                "插件源目录不在允许范围内（须位于当前工作区或用户主目录下）: {}",
                src_path.display()
            )));
        }
    }

    match service.install(&source, scope).await {
        Ok(id) => {
            // Reuse the shared registry snapshot (already refreshed by reload
            // inside install) for the response info.
            let info = service.get(&id).await.map(|e| entry_to_info(&e));
            Ok(serde_json::json!({
                "success": true,
                "plugin_id": id,
                "info": info,
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
    let service = require_service(&state)?;
    match service.uninstall(&name, keep_data.unwrap_or(false)).await {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' uninstalled", name),
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn enable_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state)?;
    match service.enable(&name).await {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' enabled", name),
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn disable_plugin(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state)?;
    match service.disable(&name).await {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' disabled", name),
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}

#[tauri::command]
pub async fn reload_plugins(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let service = require_service(&state)?;
    match service.reload().await {
        Ok(summary) => Ok(serde_json::json!({
            "success": true,
            "total": summary.total,
            "enabled": summary.enabled,
            "skills_loaded": summary.skills_loaded,
            "hooks_registered": summary.hooks_registered,
            "mcp_connected": summary.mcp_connected,
            "errors": summary.errors,
            "message": format!("Reloaded {} plugins", summary.total),
        })),
        Err(e) => Err(IpcError::Internal(e.to_string())),
    }
}
