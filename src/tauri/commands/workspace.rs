//! Tauri IPC commands for workspace management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::workspace::WorkspaceKind;
use echo_agent_app_core::workspace::migration::LegacyMigrator;

#[tauri::command]
pub async fn list_workspaces(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let workspaces = state
        .app_state
        .workspace
        .registry
        .list()
        .map_err(|e| IpcError::Internal(format!("Failed to list workspaces: {e}")))?;

    Ok(serde_json::json!({
        "workspaces": workspaces,
        "count": workspaces.len(),
    }))
}

#[tauri::command]
pub async fn create_workspace(
    state: tauri::State<'_, TauriState>,
    name: String,
    kind: Option<String>,
    root: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let ws_kind = kind
        .as_deref()
        .map(WorkspaceKind::from_str_loose)
        .unwrap_or_default();

    let result = if let Some(ref root_str) = root {
        crate::tauri::path_validator::validate_workspace_root(root_str)
            .map_err(IpcError::Validation)?;
        let root_path = std::path::PathBuf::from(root_str);
        state
            .app_state
            .workspace
            .registry
            .create_at(&name, ws_kind, root_path)
    } else {
        state.app_state.workspace.registry.create(&name, ws_kind)
    };

    match result {
        Ok(ws) => {
            tracing::info!(workspace = %ws.id, root = %ws.root.display(), "Created workspace via IPC");
            Ok(serde_json::json!({
                "success": true,
                "workspace": ws,
            }))
        }
        Err(e) => Err(IpcError::Internal(format!(
            "Failed to create workspace: {e}"
        ))),
    }
}

#[tauri::command]
pub async fn get_default_root(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let root = state.app_state.workspace.registry.default_root(&name);
    Ok(serde_json::json!({
        "default_root": root.to_string_lossy(),
    }))
}

#[tauri::command]
pub async fn get_current_workspace(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    match state.app_state.current_workspace().await {
        Some(ws) => Ok(serde_json::json!({
            "workspace": ws,
            "active": true,
        })),
        None => Ok(serde_json::json!({
            "workspace": null,
            "active": false,
        })),
    }
}

#[tauri::command]
pub async fn get_workspace(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id);
    match state.app_state.workspace.registry.open(&ws_id) {
        Ok(ws) => Ok(serde_json::json!({ "workspace": ws })),
        Err(e) => Err(IpcError::NotFound(format!("Workspace not found: {e}"))),
    }
}

#[tauri::command]
pub async fn delete_workspace(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id.clone());

    if let Some(ref current) = state.app_state.current_workspace().await
        && current.id == ws_id
    {
        state.app_state.exit_workspace().await;
    }

    match state.app_state.workspace.registry.delete(&ws_id) {
        Ok(()) => {
            tracing::info!(workspace = %id, "Deleted workspace via IPC");
            Ok(serde_json::json!({
                "success": true,
                "message": format!("Workspace '{}' deleted", id),
            }))
        }
        Err(e) => Err(IpcError::Internal(format!(
            "Failed to delete workspace: {e}"
        ))),
    }
}

#[tauri::command]
pub async fn switch_workspace(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id.clone());
    match state.app_state.workspace.registry.open(&ws_id) {
        Ok(ws) => match state.app_state.switch_workspace(ws.clone()).await {
            Ok(()) => {
                tracing::info!(workspace = %id, "Switched workspace via IPC");

                // Immediately verify conversation store by listing conversations
                let conv_count = {
                    let store_guard = state.app_state.storage.conversation_store.read().await;
                    if let Some(store) = store_guard.as_ref() {
                        let filter = echo_agent::memory::ConversationFilter::default();
                        match store.list_conversations(filter).await {
                            Ok(list) => list.len(),
                            Err(e) => {
                                tracing::error!(
                                    "[switch_workspace] list_conversations failed: {e}"
                                );
                                0
                            }
                        }
                    } else {
                        tracing::error!(
                            "[switch_workspace] conversation_store is None after switch!"
                        );
                        0
                    }
                };
                tracing::info!(
                    "[switch_workspace] workspace '{}' has {} conversations",
                    id,
                    conv_count
                );

                Ok(serde_json::json!({
                    "success": true,
                    "workspace": ws,
                    "debug_conversation_count": conv_count,
                }))
            }
            Err(e) => Err(IpcError::Internal(format!(
                "Failed to switch workspace: {e}"
            ))),
        },
        Err(e) => Err(IpcError::NotFound(format!("Workspace not found: {e}"))),
    }
}

#[tauri::command]
pub async fn link_project(
    state: tauri::State<'_, TauriState>,
    id: String,
    path: String,
) -> Result<serde_json::Value, IpcError> {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id);
    let project_path = std::path::PathBuf::from(&path);

    match state
        .app_state
        .workspace
        .registry
        .link_project(&ws_id, project_path)
    {
        Ok(ws) => {
            tracing::info!(
                workspace = %ws.id,
                project = %path,
                "Linked project via IPC"
            );
            Ok(serde_json::json!({
                "success": true,
                "workspace": ws,
            }))
        }
        Err(e) => Err(IpcError::Internal(format!("Failed to link project: {e}"))),
    }
}

#[tauri::command]
pub async fn audit_migration() -> Result<serde_json::Value, IpcError> {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        return Ok(serde_json::json!({
            "has_legacy_data": false,
            "message": "No legacy data found to migrate.",
        }));
    }

    match migrator.audit() {
        Ok(plan) => Ok(serde_json::json!({
            "has_legacy_data": true,
            "plan": plan,
        })),
        Err(e) => Err(IpcError::Internal(format!(
            "Failed to audit legacy data: {e}"
        ))),
    }
}

#[tauri::command]
pub async fn execute_migration(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        return Err(IpcError::Validation(
            "No legacy data found to migrate.".to_string(),
        ));
    }

    let plan = migrator
        .audit()
        .map_err(|e| IpcError::Internal(format!("Failed to audit: {e}")))?;

    match migrator.execute(&plan, &state.app_state.workspace.registry) {
        Ok(report) => {
            tracing::info!(
                workspaces = report.workspaces_created.len(),
                sessions = report.sessions_migrated,
                "Migration completed via IPC"
            );
            Ok(serde_json::json!({
                "success": true,
                "report": report,
            }))
        }
        Err(e) => Err(IpcError::Internal(format!("Migration failed: {e}"))),
    }
}
