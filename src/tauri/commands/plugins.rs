//! Tauri IPC commands for plugin management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::plugin::{PluginEntry, PluginRegistry};
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

fn build_registry() -> PluginRegistry {
    // Detect project root from cwd
    let project_root = std::env::current_dir().ok().and_then(|cwd| {
        let mut dir = cwd.as_path();
        loop {
            if dir.join(".echo-agent").exists() || dir.join(".git").exists() {
                return Some(dir.to_path_buf());
            }
            dir = dir.parent()?;
        }
    });
    PluginRegistry::new(project_root)
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

// ── IPC Commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_plugins(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Failed to scan plugins: {e}")));
    }

    let plugins: Vec<PluginInfo> = registry.list().into_iter().map(entry_to_info).collect();
    Ok(serde_json::to_value(plugins).unwrap_or_default())
}

#[tauri::command]
pub async fn get_plugin(
    _state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Failed to scan plugins: {e}")));
    }

    match registry.get(&name) {
        Some(entry) => {
            let info = entry_to_info(entry);
            Ok(serde_json::to_value(info).unwrap_or_default())
        }
        None => Err(IpcError::NotFound(format!("Plugin '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn install_plugin(
    _state: tauri::State<'_, TauriState>,
    source: String,
    scope: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    let scope = scope
        .and_then(|s| echo_agent::plugin::PluginScope::from_arg(&s))
        .unwrap_or(echo_agent::plugin::PluginScope::User);
    let source = echo_agent::plugin::InstallSource::parse(&source);

    match registry.install(&source, scope) {
        Ok(id) => {
            let info = registry.get(&id).map(entry_to_info);
            Ok(serde_json::json!({
                "success": true,
                "plugin_id": id,
                "info": info,
            }))
        }
        Err(e) => Err(IpcError::Internal(e)),
    }
}

#[tauri::command]
pub async fn uninstall_plugin(
    _state: tauri::State<'_, TauriState>,
    name: String,
    keep_data: Option<bool>,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Scan failed: {e}")));
    }

    match registry.uninstall(&name, keep_data.unwrap_or(false)) {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' uninstalled", name),
        })),
        Err(e) => Err(IpcError::Internal(e)),
    }
}

#[tauri::command]
pub async fn enable_plugin(
    _state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Scan failed: {e}")));
    }

    match registry.enable(&name) {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' enabled", name),
        })),
        Err(e) => Err(IpcError::Internal(e)),
    }
}

#[tauri::command]
pub async fn disable_plugin(
    _state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Scan failed: {e}")));
    }

    match registry.disable(&name) {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' disabled", name),
        })),
        Err(e) => Err(IpcError::Internal(e)),
    }
}

#[tauri::command]
pub async fn reload_plugins(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mut registry = build_registry();
    if let Err(e) = registry.scan_all() {
        return Err(IpcError::Internal(format!("Scan failed: {e}")));
    }

    let plugins = registry.list();
    Ok(serde_json::json!({
        "success": true,
        "total": plugins.len(),
        "enabled": plugins.iter().filter(|e| e.enabled).count(),
        "message": format!("Reloaded {} plugins", plugins.len()),
    }))
}
