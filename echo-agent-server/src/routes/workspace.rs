//! Workspace REST API routes.
//!
//! Provides CRUD and management endpoints for workspaces:
//!
//! | Method | Path                          | Description               |
//! |--------|-------------------------------|---------------------------|
//! | GET    | /api/workspaces               | List all workspaces       |
//! | POST   | /api/workspaces               | Create a workspace        |
//! | GET    | /api/workspaces/current       | Get current workspace     |
//! | GET    | /api/workspaces/:id           | Get workspace by ID       |
//! | DELETE | /api/workspaces/:id           | Delete a workspace        |
//! | POST   | /api/workspaces/:id/switch    | Switch to workspace       |
//! | POST   | /api/workspaces/:id/link      | Link project directory    |
//! | POST   | /api/workspaces/migrate/audit | Audit legacy data         |
//! | POST   | /api/workspaces/migrate       | Execute legacy migration  |

use axum::{
    Json, debug_handler,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::state::AppState;
use echo_agent_app_core::workspace::WorkspaceKind;
use echo_agent_app_core::workspace::migration::LegacyMigrator;

// ── Security helpers ──────────────────────────────────────────────────

/// Validate that a workspace root path is within a reasonable base directory.
///
/// Rejects paths outside the user's home directory (e.g., /etc, /root, /var)
/// to prevent arbitrary filesystem writes via workspace creation.
fn validate_workspace_root(root: &str) -> Result<(), String> {
    if root.trim().is_empty() {
        return Err("Workspace root path cannot be empty".to_string());
    }

    let root_path = std::path::PathBuf::from(root);

    // Determine allowed base: user home directory
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .map_err(|_| "Cannot determine home directory".to_string())?;

    // Resolve the root path (or its nearest existing ancestor)
    let mut check = root_path.as_path();
    while !check.exists() {
        match check.parent() {
            Some(p) if p != check => check = p,
            _ => break,
        }
    }

    let canonical_check = check
        .canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root path: {}", e))?;
    let canonical_home = home
        .canonicalize()
        .map_err(|e| format!("Cannot resolve home directory: {}", e))?;

    if !canonical_check.starts_with(&canonical_home) {
        return Err(format!(
            "Workspace root must be within the home directory ({}). Got: {}",
            canonical_home.display(),
            canonical_check.display()
        ));
    }

    Ok(())
}

// ── Request types ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    /// 可选：自定义工作区根目录。不指定时使用默认路径。
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinkProjectRequest {
    pub path: String,
}

// ── GET /api/workspaces — list all workspaces ────────────────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn list_workspaces(State(state): State<Arc<AppState>>) -> Response {
    match state.workspace.registry.list() {
        Ok(workspaces) => Json(serde_json::json!({
            "workspaces": workspaces,
            "count": workspaces.len(),
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to list workspaces: {e}")
        }))
        .into_response(),
    }
}

// ── POST /api/workspaces — create workspace ──────────────────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Response {
    let kind = req
        .kind
        .as_deref()
        .map(WorkspaceKind::from_str_loose)
        .unwrap_or_default();

    let result = if let Some(ref root_str) = req.root {
        // Security: validate that the root path is within a reasonable base
        // (user home directory). Don't allow writing to /etc, /root, /var, etc.
        if let Err(e) = validate_workspace_root(root_str) {
            return Json(serde_json::json!({
                "success": false,
                "error": e,
            }))
            .into_response();
        }

        let root = std::path::PathBuf::from(root_str);
        state.workspace.registry.create_at(&req.name, kind, root)
    } else {
        state.workspace.registry.create(&req.name, kind)
    };

    match result {
        Ok(ws) => {
            tracing::info!(workspace = %ws.id, root = %ws.root.display(), "Created workspace via API");
            Json(serde_json::json!({
                "success": true,
                "workspace": ws,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Failed to create workspace: {e}")
        }))
        .into_response(),
    }
}

// ── GET /api/workspaces/default-root/:name — get default root path ───

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_default_root(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let root = state.workspace.registry.default_root(&name);
    Json(serde_json::json!({
        "default_root": root.to_string_lossy(),
    }))
    .into_response()
}

// ── GET /api/workspaces/current — get current workspace ──────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_current_workspace(State(state): State<Arc<AppState>>) -> Response {
    match state.current_workspace().await {
        Some(ws) => Json(serde_json::json!({
            "workspace": ws,
            "active": true,
        }))
        .into_response(),
        None => Json(serde_json::json!({
            "workspace": null,
            "active": false,
            "message": "No active workspace. Use POST /api/workspaces/:id/switch to activate one."
        }))
        .into_response(),
    }
}

// ── GET /api/workspaces/:id — get workspace by ID ────────────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id);
    match state.workspace.registry.open(&ws_id) {
        Ok(ws) => Json(serde_json::json!({
            "workspace": ws,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "error": format!("Workspace not found: {e}")
        }))
        .into_response(),
    }
}

// ── DELETE /api/workspaces/:id — delete workspace ────────────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id.clone());

    // Check if this is the current workspace
    if let Some(ref current) = state.current_workspace().await {
        if current.id == ws_id {
            // Exit workspace first
            state.exit_workspace().await;
        }
    }

    match state.workspace.registry.delete(&ws_id) {
        Ok(()) => {
            tracing::info!(workspace = %id, "Deleted workspace via API");
            Json(serde_json::json!({
                "success": true,
                "message": format!("Workspace '{id}' deleted")
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Failed to delete workspace: {e}")
        }))
        .into_response(),
    }
}

// ── POST /api/workspaces/:id/switch — switch to workspace ────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn switch_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id.clone());
    match state.workspace.registry.open(&ws_id) {
        Ok(ws) => match state.switch_workspace(ws.clone()).await {
            Ok(()) => {
                tracing::info!(workspace = %id, "Switched workspace via API");
                Json(serde_json::json!({
                    "success": true,
                    "workspace": ws,
                }))
                .into_response()
            }
            Err(e) => Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to switch workspace: {e}")
            }))
            .into_response(),
        },
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Workspace not found: {e}")
        }))
        .into_response(),
    }
}

// ── POST /api/workspaces/:id/link — link project directory ───────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn link_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<LinkProjectRequest>,
) -> Response {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id);
    let project_path = std::path::PathBuf::from(&req.path);

    match state.workspace.registry.link_project(&ws_id, project_path) {
        Ok(ws) => {
            tracing::info!(
                workspace = %ws.id,
                project = %req.path,
                "Linked project via API"
            );
            Json(serde_json::json!({
                "success": true,
                "workspace": ws,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Failed to link project: {e}")
        }))
        .into_response(),
    }
}

// ── POST /api/workspaces/migrate/audit — audit legacy data ───────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn audit_migration(State(_state): State<Arc<AppState>>) -> Response {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        return Json(serde_json::json!({
            "has_legacy_data": false,
            "message": "No legacy data found to migrate."
        }))
        .into_response();
    }

    match migrator.audit() {
        Ok(plan) => Json(serde_json::json!({
            "has_legacy_data": true,
            "plan": plan,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to audit legacy data: {e}")
        }))
        .into_response(),
    }
}

// ── POST /api/workspaces/migrate — execute migration ─────────────────

#[cfg_attr(debug_assertions, debug_handler)]
pub async fn execute_migration(State(state): State<Arc<AppState>>) -> Response {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        return Json(serde_json::json!({
            "success": false,
            "error": "No legacy data found to migrate."
        }))
        .into_response();
    }

    let plan = match migrator.audit() {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to audit: {e}")
            }))
            .into_response();
        }
    };

    match migrator.execute(&plan, &state.workspace.registry) {
        Ok(report) => {
            tracing::info!(
                workspaces = report.workspaces_created.len(),
                sessions = report.sessions_migrated,
                "Migration completed via API"
            );
            Json(serde_json::json!({
                "success": true,
                "report": report,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Migration failed: {e}")
        }))
        .into_response(),
    }
}
