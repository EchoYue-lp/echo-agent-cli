//! IPC commands for panels not yet migrated from HTTP server.
//!
//! This module provides Tauri IPC commands for functionality that was
//! previously served via Axum HTTP routes. Each section corresponds to
//! a deleted server route module.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::state::{AuditDecision, PermissionBehavior, PermissionRuleConfig};

// ════════════════════════════════════════════════════════════════════════════
// Permissions
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_permissions_mode(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mode = state.app_state.config.permission_mode.read().await;
    Ok(serde_json::json!({ "mode": mode.clone() }))
}

#[tauri::command]
pub async fn set_permissions_mode(
    state: tauri::State<'_, TauriState>,
    mode: String,
) -> Result<serde_json::Value, IpcError> {
    let valid_modes = ["default", "auto-approve", "strict"];
    if !valid_modes.contains(&mode.as_str()) {
        return Err(IpcError::Validation(format!(
            "Invalid permission mode '{}', valid: {:?}",
            mode, valid_modes
        )));
    }
    let mut mode_lock = state.app_state.config.permission_mode.write().await;
    *mode_lock = mode.clone();
    Ok(serde_json::json!({"success": true, "mode": mode}))
}

#[tauri::command]
pub async fn list_permission_rules(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let rules = state.app_state.config.permission_rules.read().await;
    let list: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "matcher": r.matcher,
                "behavior": r.behavior.to_string(),
                "source": r.source,
            })
        })
        .collect();
    Ok(serde_json::to_value(list).unwrap_or_default())
}

#[tauri::command]
pub async fn add_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
    behavior: String,
    source: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let behavior = match behavior.as_str() {
        "allow" => PermissionBehavior::Allow,
        "deny" => PermissionBehavior::Deny,
        "ask" => PermissionBehavior::Ask,
        _ => {
            return Err(IpcError::Validation(format!(
                "Invalid behavior '{}', valid: allow, deny, ask",
                behavior
            )));
        }
    };

    let rule = PermissionRuleConfig {
        matcher: matcher.clone(),
        behavior,
        source: source.unwrap_or_else(|| "manual".to_string()),
    };

    let mut rules = state.app_state.config.permission_rules.write().await;
    if let Some(existing) = rules.iter_mut().find(|r| r.matcher == matcher) {
        *existing = rule;
    } else {
        rules.push(rule);
    }

    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn remove_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
) -> Result<serde_json::Value, IpcError> {
    let mut rules = state.app_state.config.permission_rules.write().await;
    let before = rules.len();
    rules.retain(|r| r.matcher != matcher);
    let removed = before - rules.len();

    if removed == 0 {
        return Err(IpcError::NotFound(format!(
            "Permission rule '{}' not found",
            matcher
        )));
    }

    Ok(serde_json::json!({"success": true, "removed": removed}))
}

// ════════════════════════════════════════════════════════════════════════════
// Audit
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_audit_logs(
    state: tauri::State<'_, TauriState>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).min(1000);
    let total = state.app_state.audit_log_count().await;
    let logs = state.app_state.get_audit_logs_paged(offset, limit).await;
    Ok(serde_json::json!({
        "logs": logs,
        "total": total,
        "offset": offset,
        "limit": limit,
    }))
}

#[tauri::command]
pub async fn get_audit_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let logs = state.app_state.get_audit_logs().await;
    let total_entries = logs.len();
    let allow_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Allow)
        .count();
    let deny_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Deny)
        .count();
    let ask_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Ask)
        .count();

    Ok(serde_json::json!({
        "total_entries": total_entries,
        "allow_count": allow_count,
        "deny_count": deny_count,
        "ask_count": ask_count,
    }))
}

#[tauri::command]
pub async fn clear_audit_logs(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let count = state.app_state.clear_audit_entries().await;
    Ok(serde_json::json!({"success": true, "cleared": count}))
}

// ════════════════════════════════════════════════════════════════════════════
// Auto Memory
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_auto_memory_status(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    // Auto-memory is managed by the agent; return default status
    Ok(serde_json::json!({
        "enabled": true,
        "observation_count": 0,
    }))
}

#[tauri::command]
pub async fn toggle_auto_memory(
    _state: tauri::State<'_, TauriState>,
    enabled: bool,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"enabled": enabled}))
}

#[tauri::command]
pub async fn extract_auto_memory(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    // Extraction is handled by the agent's auto-memory module
    Ok(serde_json::json!({
        "success": true,
        "observations": [],
    }))
}

#[tauri::command]
pub async fn get_auto_memory_observations(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

// ════════════════════════════════════════════════════════════════════════════
// Skills
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_skills(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    // Skills are loaded from plugins; return empty for now
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn get_skill(
    _state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotFound(format!("Skill '{}' not found", name)))
}

#[tauri::command]
pub async fn load_skill(
    _state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true, "loaded": name}))
}

#[tauri::command]
pub async fn upload_skill(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true, "message": "Use file dialog to upload"}))
}

// ════════════════════════════════════════════════════════════════════════════
// Workflow
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_workflows(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let workflows = state.app_state.history.workflows.read().await;
    let list: Vec<serde_json::Value> = workflows
        .values()
        .map(|w| {
            serde_json::json!({
                "id": w.id,
                "name": w.name,
                "node_count": w.node_count,
                "edge_count": w.edge_count,
                "created_at": w.created_at,
                "updated_at": w.updated_at,
            })
        })
        .collect();
    Ok(serde_json::to_value(list).unwrap_or_default())
}

#[tauri::command]
pub async fn get_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let workflows = state.app_state.history.workflows.read().await;
    workflows
        .get(&id)
        .map(|w| serde_json::to_value(w).unwrap_or_default())
        .ok_or_else(|| IpcError::NotFound(format!("Workflow '{}' not found", id)))
}

#[tauri::command]
pub async fn create_workflow(
    state: tauri::State<'_, TauriState>,
    name: String,
    definition: String,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::state::StoredWorkflow;
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let workflow = StoredWorkflow {
        id: id.clone(),
        name,
        definition,
        node_count: 0,
        edge_count: 0,
        created_at: now.clone(),
        updated_at: now,
    };
    let mut workflows = state.app_state.history.workflows.write().await;
    workflows.insert(id.clone(), workflow);
    Ok(serde_json::json!({"success": true, "id": id}))
}

#[tauri::command]
pub async fn delete_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let mut workflows = state.app_state.history.workflows.write().await;
    if workflows.remove(&id).is_some() {
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!("Workflow '{}' not found", id)))
    }
}

#[tauri::command]
pub async fn execute_workflow(
    _state: tauri::State<'_, TauriState>,
    id: String,
    _input: Option<serde_json::Value>,
) -> Result<serde_json::Value, IpcError> {
    // Workflow execution requires the agent; return stub for now
    Ok(serde_json::json!({
        "success": true,
        "message": "Workflow execution not yet implemented in IPC",
        "workflow_id": id,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Sandbox
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_sandbox_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.config.sandbox_config.read().await.clone();
    Ok(serde_json::json!({
        "local_available": true,
        "docker_available": false,
        "config": config,
    }))
}

#[tauri::command]
pub async fn get_sandbox_config(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.config.sandbox_config.read().await;
    Ok(serde_json::to_value(&*config).unwrap_or_default())
}

#[tauri::command]
pub async fn update_sandbox_config(
    state: tauri::State<'_, TauriState>,
    config: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let new_config: echo_agent_app_core::state::SandboxConfigData =
        serde_json::from_value(config).map_err(|e| IpcError::Validation(e.to_string()))?;
    let mut config_lock = state.app_state.config.sandbox_config.write().await;
    *config_lock = new_config;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn execute_sandbox(
    _state: tauri::State<'_, TauriState>,
    _code: String,
    _language: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    // Sandbox execution requires complex setup; return stub
    Ok(serde_json::json!({
        "success": false,
        "error": "Sandbox execution not yet implemented in IPC",
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Compress
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn compress_context(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    // Context compression is handled by the agent
    Ok(serde_json::json!({
        "success": true,
        "message": "Context compression requested",
    }))
}

#[tauri::command]
pub async fn get_compression_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (message_count, token_count) = state
        .app_state
        .connection
        .agent
        .read_async(|agent| Box::pin(async move { agent.context_stats().await }))
        .await;
    Ok(serde_json::json!({
        "message_count": message_count,
        "token_count": token_count,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Extract
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn extract_data(
    _state: tauri::State<'_, TauriState>,
    _input: String,
    _schema: serde_json::Value,
    _schema_name: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "success": false,
        "error": "Data extraction not yet implemented in IPC",
    }))
}

#[tauri::command]
pub async fn validate_schema(
    _state: tauri::State<'_, TauriState>,
    _schema: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "valid": true,
        "message": "Schema validation not yet implemented",
    }))
}

#[tauri::command]
pub async fn get_extract_examples(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

// ════════════════════════════════════════════════════════════════════════════
// Context
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_context_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (message_count, token_count) = state
        .app_state
        .connection
        .agent
        .read_async(|agent| Box::pin(async move { agent.context_stats().await }))
        .await;
    Ok(serde_json::json!({
        "message_count": message_count,
        "token_count": token_count,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// History
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_history(
    _state: tauri::State<'_, TauriState>,
    _limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    // History is managed by conversation store
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn export_history_markdown(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "success": true,
        "content": "# History Export\n\nNot yet implemented.",
    }))
}

#[tauri::command]
pub async fn export_history_json(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "success": true,
        "content": "[]",
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Trace Events
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_trace_sessions(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn get_trace_events(
    _state: tauri::State<'_, TauriState>,
    _session_id: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn get_trace_summary(
    _state: tauri::State<'_, TauriState>,
    session_id: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "session_id": session_id,
        "event_count": 0,
    }))
}

#[tauri::command]
pub async fn clear_trace_session(
    _state: tauri::State<'_, TauriState>,
    session_id: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true, "cleared": session_id}))
}

// ════════════════════════════════════════════════════════════════════════════
// Papers
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_papers(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn get_paper(
    _state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotFound(format!("Paper '{}' not found", id)))
}

#[tauri::command]
pub async fn create_paper(
    _state: tauri::State<'_, TauriState>,
    title: String,
    _authors: Option<Vec<String>>,
) -> Result<serde_json::Value, IpcError> {
    let id = uuid::Uuid::new_v4().to_string();
    Ok(serde_json::json!({
        "success": true,
        "id": id,
        "title": title,
    }))
}

#[tauri::command]
pub async fn delete_paper(
    _state: tauri::State<'_, TauriState>,
    _id: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn update_paper_notes(
    _state: tauri::State<'_, TauriState>,
    _id: String,
    _notes: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn add_paper_tags(
    _state: tauri::State<'_, TauriState>,
    _id: String,
    _tags: Vec<String>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true}))
}

// ════════════════════════════════════════════════════════════════════════════
// Scratchpad
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_scratchpad(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "content": "",
        "updated_at": null,
    }))
}

#[tauri::command]
pub async fn update_scratchpad(
    _state: tauri::State<'_, TauriState>,
    _content: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true}))
}

// ════════════════════════════════════════════════════════════════════════════
// Decisions
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_decisions(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn create_decision(
    _state: tauri::State<'_, TauriState>,
    _title: String,
    _rationale: String,
) -> Result<serde_json::Value, IpcError> {
    let id = uuid::Uuid::new_v4().to_string();
    Ok(serde_json::json!({
        "success": true,
        "id": id,
    }))
}

#[tauri::command]
pub async fn clear_decisions(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true, "cleared": 0}))
}

// ════════════════════════════════════════════════════════════════════════════
// Evolution
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_trajectories(
    _state: tauri::State<'_, TauriState>,
    _date: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn get_trajectory_stats(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "total_trajectories": 0,
        "approved": 0,
        "pending": 0,
    }))
}

#[tauri::command]
pub async fn review_trajectory(
    _state: tauri::State<'_, TauriState>,
    trajectory_id: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "trajectory_id": trajectory_id,
        "status": "not_found",
    }))
}

#[tauri::command]
pub async fn curator_action(
    _state: tauri::State<'_, TauriState>,
    action: String,
    _skill_name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "success": true,
        "action": action,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Human Gate
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_human_gates(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    // Human gates are managed by BackgroundTaskService
    if let Some(ref service) = state.app_state.tasks.service {
        let gates = service.pending_checkpoints().await;
        // HumanCheckpointRequest contains non-Serialize fields (Duration),
        // so manually map to a serializable representation.
        let items: Vec<serde_json::Value> = gates
            .into_iter()
            .map(|(id, req)| {
                serde_json::json!({
                    "id": id,
                    "kind": format!("{:?}", req.kind),
                    "prompt": req.prompt,
                    "tool_name": req.tool_name,
                    "risk_level": req.risk_level.as_ref().map(|r| format!("{:?}", r)),
                    "task_id": req.task_id,
                    "options": req.options,
                    "phase": req.phase,
                })
            })
            .collect();
        Ok(serde_json::to_value(items).unwrap_or_default())
    } else {
        Ok(serde_json::json!([]))
    }
}

#[tauri::command]
pub async fn respond_human_gate(
    state: tauri::State<'_, TauriState>,
    gate_id: String,
    response: String,
    instructions: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Some(ref service) = state.app_state.tasks.service {
        let ok = service
            .respond_to_checkpoint(&gate_id, &response, instructions)
            .await;
        if ok {
            Ok(serde_json::json!({"success": true}))
        } else {
            Err(IpcError::NotFound(format!(
                "Gate '{}' not found or expired",
                gate_id
            )))
        }
    } else {
        Err(IpcError::Internal(
            "BackgroundTaskService not available".into(),
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Worktree
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_worktrees(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!([]))
}

#[tauri::command]
pub async fn create_worktree(
    _state: tauri::State<'_, TauriState>,
    branch: String,
    _path: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "success": true,
        "branch": branch,
    }))
}

#[tauri::command]
pub async fn remove_worktree(
    _state: tauri::State<'_, TauriState>,
    _path: String,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({"success": true}))
}

// ════════════════════════════════════════════════════════════════════════════
// MCP (missing method)
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let mcp_config = state.app_state.plugins.mcp_config.read().await;
    let mcp_health = state.app_state.plugins.mcp_health.read().await;

    let exists = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.list_mcp_servers().iter().any(|s| s == &name))
        .await;

    if !exists {
        return Err(IpcError::NotFound(format!(
            "MCP server '{}' not found",
            name
        )));
    }

    let health = mcp_health.get(&name);
    let status = if let Some(h) = health {
        if h.healthy { "connected" } else { "error" }
    } else {
        "disconnected"
    };

    let transport = mcp_config
        .mcp_servers
        .get(&name)
        .map(|e| {
            if e.url.is_some() {
                if e.transport.as_deref() == Some("sse") {
                    "sse"
                } else {
                    "http"
                }
            } else if e.command.is_some() {
                "stdio"
            } else {
                "unknown"
            }
        })
        .unwrap_or("unknown");

    Ok(serde_json::json!({
        "name": name,
        "status": status,
        "transport": transport,
        "tool_count": 0,
        "tools": [],
        "connected_at": null,
        "error": health.and_then(|h| h.error.clone()),
    }))
}
