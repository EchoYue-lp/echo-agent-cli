//! Plugin management API routes.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/plugins | List installed plugins |
//! | POST | /api/plugins/install | Install a plugin |
//! | POST | /api/plugins/uninstall | Uninstall a plugin |
//! | POST | /api/plugins/:name/enable | Enable a plugin |
//! | POST | /api/plugins/:name/disable | Disable a plugin |
//! | GET | /api/plugins/:name | Get plugin details |
//! | POST | /api/plugins/reload | Reload all plugins |

use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use echo_agent::plugin::{InstallSource, PluginRegistry, PluginScope};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::state::AppState;

// ── Request / Response types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    /// Local path or git URL.
    pub source: String,
    /// Installation scope: "user" (default), "project", or "local".
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "user".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    pub name: String,
    #[serde(default)]
    pub keep_data: bool,
}

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

// ── Helpers ─────────────────────────────────────────────────────────────

fn build_registry(_state: &AppState) -> PluginRegistry {
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

fn entry_to_info(entry: &echo_agent::plugin::PluginEntry) -> PluginInfo {
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

// ── Routes ──────────────────────────────────────────────────────────────

/// GET /api/plugins — List all installed plugins.
pub async fn list_plugins(State(state): State<Arc<AppState>>) -> Response {
    let mut registry = build_registry(&state);
    if let Err(e) = registry.scan_all() {
        return Json(serde_json::json!({
            "error": format!("Failed to scan plugins: {e}"),
        }))
        .into_response();
    }

    let plugins: Vec<PluginInfo> = registry.list().into_iter().map(entry_to_info).collect();
    Json(plugins).into_response()
}

/// POST /api/plugins/install — Install a plugin from local path or git URL.
pub async fn install_plugin(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InstallRequest>,
) -> Response {
    let mut registry = build_registry(&state);

    let scope = PluginScope::from_arg(&req.scope).unwrap_or(PluginScope::User);
    let source = InstallSource::parse(&req.source);

    match registry.install(&source, scope) {
        Ok(id) => {
            let info = registry.get(&id).map(entry_to_info);
            Json(serde_json::json!({
                "success": true,
                "plugin_id": id,
                "info": info,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e,
        }))
        .into_response(),
    }
}

/// POST /api/plugins/uninstall — Uninstall a plugin.
pub async fn uninstall_plugin(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UninstallRequest>,
) -> Response {
    let mut registry = build_registry(&state);
    if let Err(e) = registry.scan_all() {
        return Json(serde_json::json!({
            "success": false,
            "error": format!("Scan failed: {e}"),
        }))
        .into_response();
    }

    match registry.uninstall(&req.name, req.keep_data) {
        Ok(()) => Json(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{}' uninstalled", req.name),
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e,
        }))
        .into_response(),
    }
}

/// POST /api/plugins/:name/enable — Enable a plugin.
pub async fn enable_plugin(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let mut registry = build_registry(&state);
    if let Err(e) = registry.scan_all() {
        return Json(serde_json::json!({
            "success": false,
            "error": format!("Scan failed: {e}"),
        }))
        .into_response();
    }

    match registry.enable(&name) {
        Ok(()) => Json(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{name}' enabled"),
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e,
        }))
        .into_response(),
    }
}

/// POST /api/plugins/:name/disable — Disable a plugin.
pub async fn disable_plugin(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let mut registry = build_registry(&state);
    if let Err(e) = registry.scan_all() {
        return Json(serde_json::json!({
            "success": false,
            "error": format!("Scan failed: {e}"),
        }))
        .into_response();
    }

    match registry.disable(&name) {
        Ok(()) => Json(serde_json::json!({
            "success": true,
            "message": format!("Plugin '{name}' disabled"),
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e,
        }))
        .into_response(),
    }
}

/// GET /api/plugins/:name — Get plugin details.
pub async fn get_plugin(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let mut registry = build_registry(&state);
    if let Err(e) = registry.scan_all() {
        return Json(serde_json::json!({
            "error": format!("Scan failed: {e}"),
        }))
        .into_response();
    }

    match registry.get(&name) {
        Some(entry) => {
            let mut info = entry_to_info(entry);
            // Always return consistent {info, resolved} shape
            let resolved = registry.resolve_components(&name).ok().map(|r| {
                let mut extra = serde_json::Map::new();
                extra.insert("skill_dirs".into(), serde_json::json!(r.skill_dirs.len()));
                extra.insert("agent_files".into(), serde_json::json!(r.agent_files.len()));
                extra.insert("has_hooks".into(), serde_json::json!(r.hooks_file.is_some()));
                extra.insert("has_mcp".into(), serde_json::json!(r.mcp_config_file.is_some()));
                extra.insert("has_lsp".into(), serde_json::json!(r.lsp_config_file.is_some()));
                extra
            });
            Json(serde_json::json!({
                "info": info,
                "resolved": resolved,
            }))
            .into_response()
        }
        None => Json(serde_json::json!({
            "error": format!("Plugin '{name}' not found"),
        }))
        .into_response(),
    }
}

/// POST /api/plugins/reload — Reload all plugins.
pub async fn reload_plugins(State(state): State<Arc<AppState>>) -> Response {
    let mut registry = build_registry(&state);
    match registry.scan_all() {
        Ok(count) => {
            let enabled = registry.list_enabled().len();
            Json(serde_json::json!({
                "success": true,
                "total": count,
                "enabled": enabled,
                "message": format!("Loaded {count} plugins ({enabled} enabled)"),
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Reload failed: {e}"),
        }))
        .into_response(),
    }
}
