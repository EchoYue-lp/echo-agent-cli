//! Tauri IPC commands for workspace management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::workspace::migration::LegacyMigrator;
use echo_agent_app_core::workspace::registry::WorkspaceRegistry;
use echo_agent_app_core::workspace::{Workspace, WorkspaceId, WorkspaceKind};
use std::sync::Arc;

fn same_workspace_root(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn create_or_open_workspace(
    registry: &WorkspaceRegistry,
    name: &str,
    kind: WorkspaceKind,
    root: Option<&str>,
) -> Result<(Workspace, bool), IpcError> {
    let id = WorkspaceId::from_name(name);
    let requested_root = root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| registry.default_root(name));
    if let Ok(existing) = registry.open(&id) {
        if root.is_none() || same_workspace_root(&existing.root, &requested_root) {
            return Ok((existing, false));
        }
        return Err(IpcError::Validation(format!(
            "Workspace '{}' already exists at a different path: {}",
            id,
            existing.root.display()
        )));
    }

    let workspace = if root.is_some() {
        registry.create_at(name, kind, requested_root)
    } else {
        registry.create(name, kind)
    }
    .map_err(|error| IpcError::Internal(format!("Failed to create workspace: {error}")))?;
    Ok((workspace, true))
}

async fn switch_opened_workspace(
    app_state: &Arc<echo_agent_app_core::AppState>,
    workspace: Workspace,
) -> Result<serde_json::Value, IpcError> {
    let transition = app_state
        .switch_workspace(workspace.clone())
        .await
        .map_err(|error| IpcError::Internal(format!("Failed to switch workspace: {error}")))?;
    let conversation_count = match app_state.conversation_store().await {
        Some(store) => {
            let filter = echo_agent::memory::ConversationFilter::default();
            match store.list_conversations(filter).await {
                Ok(conversations) => conversations.len(),
                Err(error) => {
                    tracing::error!(%error, "failed to list conversations after workspace switch");
                    0
                }
            }
        }
        None => {
            tracing::error!("conversation store is unavailable after workspace switch");
            0
        }
    };
    tracing::info!(
        workspace = %workspace.id,
        conversation_count,
        "Switched workspace via IPC"
    );
    Ok(serde_json::json!({
        "success": true,
        "workspace": workspace,
        "transition": transition,
        "debug_conversation_count": conversation_count,
    }))
}

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

    if let Some(ref root_str) = root {
        crate::tauri::path_validator::validate_workspace_root(root_str)
            .map_err(IpcError::Validation)?;
    }
    let (workspace, created) = create_or_open_workspace(
        &state.app_state.workspace.registry,
        &name,
        ws_kind,
        root.as_deref(),
    )?;
    tracing::info!(workspace = %workspace.id, root = %workspace.root.display(), created, "Created or opened workspace via IPC");
    Ok(serde_json::json!({
        "success": true,
        "workspace": workspace,
        "created": created,
    }))
}

#[tauri::command]
pub async fn create_and_switch_workspace(
    state: tauri::State<'_, TauriState>,
    name: String,
    kind: Option<String>,
    root: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Some(ref root_str) = root {
        crate::tauri::path_validator::validate_workspace_root(root_str)
            .map_err(IpcError::Validation)?;
    }
    let workspace_kind = kind
        .as_deref()
        .map(WorkspaceKind::from_str_loose)
        .unwrap_or_default();
    let (workspace, created) = create_or_open_workspace(
        &state.app_state.workspace.registry,
        &name,
        workspace_kind,
        root.as_deref(),
    )?;
    match switch_opened_workspace(&state.app_state, workspace.clone()).await {
        Ok(mut response) => {
            if let Some(object) = response.as_object_mut() {
                object.insert("created".to_string(), serde_json::Value::Bool(created));
                object.insert("switched".to_string(), serde_json::Value::Bool(true));
            }
            Ok(response)
        }
        Err(error) => Ok(serde_json::json!({
            "success": false,
            "created": created,
            "switched": false,
            "workspace": workspace,
            "error": error.to_string(),
        })),
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
pub async fn exit_workspace(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let transition = state
        .app_state
        .exit_workspace()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({ "success": true, "transition": transition }))
}

#[tauri::command]
pub async fn delete_workspace(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let ws_id = echo_agent_app_core::workspace::WorkspaceId::from_raw(id.clone());

    state
        .app_state
        .ensure_workspace_idle_for_delete(&ws_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;

    if let Some(ref current) = state.app_state.current_workspace().await
        && current.id == ws_id
    {
        state
            .app_state
            .exit_workspace()
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
    }

    state
        .app_state
        .evict_workspace_for_delete(&ws_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;

    state.browser_runtime.remove_workspace(ws_id.as_str()).await;
    state
        .app_state
        .purge_workspace_projections_for_delete(&ws_id)
        .map_err(|error| IpcError::Internal(error.to_string()))?;

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
        Ok(workspace) => switch_opened_workspace(&state.app_state, workspace).await,
        Err(e) => Err(IpcError::NotFound(format!("Workspace not found: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir()
                .join(format!("eko-workspace-command-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("failed to clean workspace command test directory: {error}");
            }
        }
    }

    #[test]
    fn create_or_open_reuses_same_workspace_root() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDirectory::new()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("registry"))?;
        let project = temp.path().join("project");
        registry.create_at("Lp-agent", WorkspaceKind::General, project.clone())?;

        let project_text = project.to_string_lossy().to_string();
        let (workspace, created) = create_or_open_workspace(
            &registry,
            "Lp-agent",
            WorkspaceKind::Code { repo_url: None },
            Some(&project_text),
        )?;

        assert!(!created);
        assert_eq!(workspace.root, project);
        Ok(())
    }

    #[test]
    fn create_or_open_without_root_reuses_custom_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDirectory::new()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("registry"))?;
        let project = temp.path().join("custom-project");
        registry.create_at("Lp-agent", WorkspaceKind::General, project.clone())?;

        let (workspace, created) =
            create_or_open_workspace(&registry, "Lp-agent", WorkspaceKind::General, None)?;

        assert!(!created);
        assert_eq!(workspace.root, project);
        Ok(())
    }

    #[test]
    fn create_or_open_rejects_same_id_at_another_root() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDirectory::new()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("registry"))?;
        registry.create_at(
            "Lp-agent",
            WorkspaceKind::General,
            temp.path().join("project-a"),
        )?;

        let other = temp.path().join("project-b").to_string_lossy().to_string();
        let error =
            create_or_open_workspace(&registry, "Lp-agent", WorkspaceKind::General, Some(&other))
                .err()
                .ok_or("expected workspace path conflict")?;

        assert!(matches!(error, IpcError::Validation(_)));
        Ok(())
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
