//! IPC commands for panels not yet migrated from HTTP server.
//!
//! This module provides Tauri IPC commands for functionality that was
//! previously served via Axum HTTP routes. Each section corresponds to
//! a deleted server route module.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::api::state::{AuditDecision, PermissionBehavior, PermissionRuleConfig};
use echo_agent_app_core::api::structured_extraction::{
    StructuredExtractionError, StructuredExtractionExample, StructuredExtractionOutcome,
    StructuredExtractionRequest, StructuredExtractionValidation,
};
use echo_agent_app_core::api::tasks::task_runtime::compact_context::RUNTIME_RECOVERY_MARKER;
use echo_agent_app_core::api::workflow_service::{
    StoredWorkflow, WorkflowExecution, WorkflowMutationReceipt, WorkflowServiceError,
};
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tauri::Emitter;

struct ScopedEvolutionControl {
    runtime: echo_agent_app_core::api::state::ScopedChatRuntime,
    integration: Arc<echo_agent_app_core::api::evolution::ReviewIntegration>,
    generation: echo_agent_app_core::api::evolution::ReviewGenerationLease,
}

async fn current_evolution_control(state: &TauriState) -> Result<ScopedEvolutionControl, IpcError> {
    let runtime = state
        .app_state
        .current_control_runtime()
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let integration = runtime.review_integration().ok_or_else(|| {
        IpcError::Internal(format!(
            "Review integration is not configured for workspace '{}'",
            runtime.execution_scope().workspace_id()
        ))
    })?;
    let generation = integration
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(ScopedEvolutionControl {
        runtime,
        integration,
        generation,
    })
}

fn map_workflow_error(error: WorkflowServiceError) -> IpcError {
    match error {
        WorkflowServiceError::NotFound(id) => {
            IpcError::NotFound(format!("Workflow '{id}' not found"))
        }
        WorkflowServiceError::InvalidDefinition(message) => IpcError::Validation(message),
        other => IpcError::Internal(other.to_string()),
    }
}

fn map_structured_extraction_error(error: StructuredExtractionError) -> IpcError {
    if error.is_validation() {
        IpcError::Validation(error.to_string())
    } else {
        IpcError::Internal(error.to_string())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Permissions
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_permissions_mode(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mode = *state.app_state.config.permission_mode.read().await;
    Ok(serde_json::json!({
        "mode": echo_agent_app_core::api::permission::permission_mode_id(mode)
    }))
}

#[tauri::command]
pub async fn set_permissions_mode(
    state: tauri::State<'_, TauriState>,
    mode: String,
) -> Result<serde_json::Value, IpcError> {
    let framework_mode = echo_agent_app_core::api::permission::parse_permission_mode(&mode)
        .map_err(IpcError::Validation)?;
    let normalized = echo_agent_app_core::api::permission::permission_mode_id(framework_mode);
    let mut mode_lock = state.app_state.config.permission_mode.write().await;
    *mode_lock = framework_mode;
    drop(mode_lock);

    state
        .app_state
        .apply_permission_mode_to_agents(framework_mode)
        .await;

    Ok(serde_json::json!({"success": true, "mode": normalized}))
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

    let framework_rule = rule.to_framework_rule().map_err(IpcError::Validation)?;
    let permission_service = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.permission_service().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("Permission service not available".to_string()))?;

    let mut rules = state.app_state.config.permission_rules.write().await;
    let previous = rules
        .iter()
        .find(|candidate| candidate.matcher == matcher)
        .cloned();
    if let Some(existing) = rules
        .iter_mut()
        .find(|candidate| candidate.matcher == matcher)
    {
        *existing = rule;
    } else {
        rules.push(rule);
    }
    drop(rules);
    if let Some(previous) = previous {
        let previous = previous.to_framework_rule().map_err(IpcError::Validation)?;
        permission_service.remove_rule(&previous).await;
    }
    permission_service.add_rule(framework_rule).await;

    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn remove_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
) -> Result<serde_json::Value, IpcError> {
    let permission_service = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.permission_service().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("Permission service not available".to_string()))?;
    let mut rules = state.app_state.config.permission_rules.write().await;
    let removed_rule = rules.iter().find(|rule| rule.matcher == matcher).cloned();
    let before = rules.len();
    rules.retain(|r| r.matcher != matcher);
    let removed = before - rules.len();

    if removed == 0 {
        return Err(IpcError::NotFound(format!(
            "Permission rule '{}' not found",
            matcher
        )));
    }
    drop(rules);
    let framework_rule = removed_rule
        .ok_or_else(|| IpcError::NotFound(format!("Permission rule '{}' not found", matcher)))?
        .to_framework_rule()
        .map_err(IpcError::Validation)?;
    permission_service.remove_rule(&framework_rule).await;

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

struct ScopedAutoMemoryControl {
    runtime: echo_agent_app_core::api::state::ScopedChatRuntime,
    generation: echo_agent_app_core::api::evolution::ReviewGenerationLease,
}

async fn auto_memory_control_for_workspace(
    state: &TauriState,
    workspace_id: &str,
) -> Result<ScopedAutoMemoryControl, IpcError> {
    if workspace_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "workspace_id must not be empty".to_string(),
        ));
    }
    let runtime = state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(IpcError::from)?;
    let integration = runtime.review_integration().ok_or_else(|| {
        IpcError::Internal(format!(
            "Review integration is not configured for workspace '{workspace_id}'"
        ))
    })?;
    let generation = integration
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(ScopedAutoMemoryControl {
        runtime,
        generation,
    })
}

async fn scoped_agent_messages(control: &ScopedAutoMemoryControl) -> Vec<(String, String)> {
    let agent = control.runtime.primary_agent();
    agent
        .read_async(|agent| {
            Box::pin(async move {
                let ctx = agent.context().lock().await;
                ctx.messages()
                    .iter()
                    .map(|m| {
                        (
                            m.role.as_str().to_string(),
                            m.content.as_text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
        })
        .await
}

async fn auto_memory_config_status(
    control: &ScopedAutoMemoryControl,
) -> Result<
    (
        echo_agent_app_core::api::auto_memory::AutoMemoryConfig,
        usize,
        PathBuf,
    ),
    IpcError,
> {
    let config = echo_agent_app_core::api::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    let messages = scoped_agent_messages(control).await;
    let observations =
        echo_agent_app_core::api::auto_memory::extract_observations(&messages, &config);
    let inbox_path = control.generation.evidence_store().path().to_path_buf();
    Ok((config, observations.len(), inbox_path))
}

fn commit_auto_memory_toggle_after_validation<T>(
    validation: Result<T, IpcError>,
    enabled: bool,
) -> Result<T, IpcError> {
    let validated = validation?;
    crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    Ok(validated)
}

fn auto_memory_extract_control_after_validation<T>(
    validation: Result<T, IpcError>,
    enabled: bool,
) -> Result<Option<T>, IpcError> {
    let validated = validation?;
    Ok(enabled.then_some(validated))
}

#[tauri::command]
pub async fn get_auto_memory_status(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = auto_memory_control_for_workspace(&state, &workspace_id).await?;
    let (config, observation_count, memory_path) = auto_memory_config_status(&control).await?;
    Ok(json!({
        "enabled": config.enabled,
        "observation_count": observation_count,
        "config": config,
        "memory_path": memory_path.display().to_string(),
    }))
}

#[tauri::command]
pub async fn toggle_auto_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    enabled: bool,
) -> Result<serde_json::Value, IpcError> {
    // Enablement is intentionally process-global product policy; workspace_id
    // selects and validates the exact observation/review generation returned
    // with the receipt. Validation must happen before the global policy write.
    let control = commit_auto_memory_toggle_after_validation(
        auto_memory_control_for_workspace(&state, &workspace_id).await,
        enabled,
    )?;
    let (config, observation_count, memory_path) = auto_memory_config_status(&control).await?;
    Ok(json!({
        "enabled": config.enabled,
        "observation_count": observation_count,
        "config": config,
        "memory_path": memory_path.display().to_string(),
    }))
}

#[tauri::command]
pub async fn extract_auto_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let config = echo_agent_app_core::api::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    let control = auto_memory_extract_control_after_validation(
        auto_memory_control_for_workspace(&state, &workspace_id).await,
        config.enabled,
    )?;
    let Some(control) = control else {
        return Ok(json!({
            "success": false,
            "count": 0,
            "observations": [],
            "formatted": "",
            "message": "Auto Memory is disabled",
        }));
    };

    let messages = scoped_agent_messages(&control).await;
    let observations =
        echo_agent_app_core::api::auto_memory::extract_observations(&messages, &config);
    let store = control.generation.evidence_store();
    let candidates =
        echo_agent_app_core::api::auto_memory::queue_observations(&store, &observations, &messages)
            .map_err(IpcError::Internal)?;
    let projection_settlement = if candidates.is_empty() {
        None
    } else {
        Some(control.generation.settle_hot_memory_projection().await)
    };
    let count = observations.len();
    let formatted =
        echo_agent_app_core::api::auto_memory::format_observations_for_memory(&observations);

    Ok(json!({
        "success": true,
        "count": count,
        "queued": candidates.len(),
        "candidates": candidates,
        "observations": observations,
        "formatted": formatted,
        "memory_path": store.path().display().to_string(),
        "projection_settlement": projection_settlement,
    }))
}

#[tauri::command]
pub async fn get_auto_memory_observations(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let config = echo_agent_app_core::api::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    let control = auto_memory_control_for_workspace(&state, &workspace_id).await?;
    let messages = scoped_agent_messages(&control).await;
    let observations =
        echo_agent_app_core::api::auto_memory::extract_observations(&messages, &config);
    let count = observations.len();
    let formatted =
        echo_agent_app_core::api::auto_memory::format_observations_for_memory(&observations);
    Ok(json!({
        "observations": observations,
        "count": count,
        "formatted": formatted,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Workflow
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_workflows(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<StoredWorkflow>, IpcError> {
    state
        .app_state
        .history
        .workflows
        .list()
        .map_err(map_workflow_error)
}

#[tauri::command]
pub async fn get_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<StoredWorkflow, IpcError> {
    state
        .app_state
        .history
        .workflows
        .get(&id)
        .map_err(map_workflow_error)
}

#[tauri::command]
pub async fn create_workflow(
    state: tauri::State<'_, TauriState>,
    name: Option<String>,
    definition: String,
) -> Result<StoredWorkflow, IpcError> {
    state
        .app_state
        .history
        .workflows
        .create(name.unwrap_or_default(), definition)
        .map_err(map_workflow_error)
}

#[tauri::command]
pub async fn delete_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<WorkflowMutationReceipt, IpcError> {
    state
        .app_state
        .history
        .workflows
        .delete(&id)
        .map_err(map_workflow_error)?;
    Ok(WorkflowMutationReceipt { success: true })
}

#[tauri::command]
pub async fn execute_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
    input: Option<serde_json::Value>,
) -> Result<WorkflowExecution, IpcError> {
    state
        .app_state
        .history
        .workflows
        .execute(&id, input)
        .await
        .map_err(map_workflow_error)
}

// ════════════════════════════════════════════════════════════════════════════
// Sandbox
// ════════════════════════════════════════════════════════════════════════════

async fn eko_local_sandbox_available(manager: &echo_agent::sandbox::SandboxManager) -> bool {
    if cfg!(target_os = "windows") {
        false
    } else {
        manager.has_local_sandbox().await
    }
}

fn eko_local_sandbox_backend_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "wsl2-required"
    } else {
        "local"
    }
}

fn eko_local_sandbox_unavailable_message() -> &'static str {
    if cfg!(target_os = "windows") {
        "Native Windows sandbox is not supported by EKO. Run EKO inside WSL2 so it can use Linux bubblewrap (bwrap)."
    } else {
        "Local sandbox backend is unavailable. On macOS this requires sandbox-exec; on Linux/WSL2 this requires bubblewrap (bwrap)."
    }
}

#[tauri::command]
pub async fn get_sandbox_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.config.sandbox_config.read().await.clone();
    let manager = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.sandbox_manager().cloned())
        .await;
    let local_available = match manager {
        Some(manager) => eko_local_sandbox_available(&manager).await,
        None => false,
    };
    Ok(serde_json::json!({
        "local_available": local_available,
        "docker_available": false,
        "k8s_available": false,
        "current_backend": eko_local_sandbox_backend_label(),
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
    let new_config: echo_agent_app_core::api::state::SandboxConfigData =
        serde_json::from_value(config).map_err(|e| IpcError::Validation(e.to_string()))?;
    let mut config_lock = state.app_state.config.sandbox_config.write().await;
    *config_lock = new_config;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn execute_sandbox(
    state: tauri::State<'_, TauriState>,
    code: String,
    language: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let lang = language.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let is_shell = matches!(
        lang.map(str::to_ascii_lowercase).as_deref(),
        Some("shell") | Some("sh") | Some("bash") | Some("zsh") | Some("fish")
    );
    let Some(lang) = lang else {
        return Err(IpcError::Validation("language is required".to_string()));
    };
    let lang = lang.to_ascii_lowercase();
    if matches!(lang.as_str(), "zsh" | "fish") {
        return Err(IpcError::Validation(format!(
            "Unsupported sandbox shell language: {lang}"
        )));
    }

    let manager = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.sandbox_manager().cloned())
        .await;
    let manager = match manager {
        Some(m) => m,
        None => {
            return Err(IpcError::Validation(
                "No sandbox manager configured on the agent; code execution disabled.".to_string(),
            ));
        }
    };
    if !eko_local_sandbox_available(&manager).await {
        return Err(IpcError::Validation(
            eko_local_sandbox_unavailable_message().to_string(),
        ));
    }

    let command = if is_shell {
        echo_agent::sandbox::SandboxCommand::shell(code)
    } else {
        echo_agent::sandbox::SandboxCommand::code(lang, code)
    };
    let config = state.app_state.config.sandbox_config.read().await.clone();
    let memory_bytes = u64::from(config.max_memory_mb)
        .checked_mul(1024)
        .and_then(|v| v.checked_mul(1024))
        .ok_or_else(|| IpcError::Validation("max_memory_mb is too large".to_string()))?;
    let limits = echo_agent::sandbox::ResourceLimits {
        cpu_time_secs: Some(u64::from(config.max_cpu_seconds)),
        memory_bytes: Some(memory_bytes),
        max_output_bytes: Some(1024 * 1024),
        max_processes: Some(64),
        network: config.network_enabled,
        read_only_paths: vec![],
        writable_paths: vec![],
    };
    let result = manager
        .execute_with_limits(command, limits)
        .await
        .map_err(|e| IpcError::Internal(format!("Sandbox execution failed: {e}")))?;
    Ok(json!({
        "success": result.success(),
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "duration_ms": result.duration.as_millis(),
        "sandbox_type": result.sandbox_type,
        "timed_out": result.timed_out,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Compress
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn compress_context(
    app: tauri::AppHandle,
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let conversation_id = match conversation_id.filter(|value| !value.trim().is_empty()) {
        Some(conversation_id) => conversation_id,
        None => state
            .app_state
            .connection
            .primary_agent()
            .read(|agent| agent.conversation_id().map(str::to_string))
            .await
            .ok_or_else(|| {
                IpcError::Validation("No active conversation to compress".to_string())
            })?,
    };
    let receipt = state
        .app_state
        .compress_conversation_owned(
            echo_agent_app_core::api::manual_compression::ManualCompressionRequest {
                workspace_id,
                conversation_id,
                surface: echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
                focus: None,
                keep_messages: 12,
            },
        )
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    app.emit("chat://event", &receipt.envelope)
        .map_err(|error| {
            IpcError::Internal(format!("compression event delivery failed: {error}"))
        })?;
    Ok(serde_json::json!({
        "success": true,
        "message": format!(
            "Compressed: {} → {} messages",
            receipt.messages_before, receipt.messages_after
        ),
        "messages_before": receipt.messages_before,
        "messages_after": receipt.messages_after,
        "tokens_saved": receipt.tokens_saved(),
    }))
}

#[tauri::command]
pub async fn get_compression_stats(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let execution = match conversation_id.filter(|value| !value.trim().is_empty()) {
        Some(conversation_id) => Some(
            runtime
                .agent_for(&conversation_id)
                .await
                .map_err(|error| IpcError::Validation(error.to_string()))?,
        ),
        None => None,
    };
    let agent = execution
        .as_ref()
        .map(echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| runtime.primary_agent());
    let (
        message_count,
        current_tokens,
        token_limit,
        compression_ratio,
        protected_message_count,
        protected_tokens,
        runtime_recovery_active,
    ) = agent
        .read_async(|agent| {
            Box::pin(async move {
                let token_limit = agent.config().get_token_limit();
                let ctx = agent.context().lock().await;
                let ratio = ctx.compression_metrics().compression_ratio();
                (
                    ctx.messages().len(),
                    ctx.token_estimate(),
                    token_limit,
                    ratio,
                    ctx.protected_message_count(),
                    ctx.protected_token_estimate(),
                    ctx.has_projection(RUNTIME_RECOVERY_MARKER),
                )
            })
        })
        .await;
    let needs_compression = token_limit > 0 && current_tokens > token_limit * 3 / 4;

    Ok(serde_json::json!({
        "message_count": message_count,
        "current_tokens": current_tokens,
        "token_limit": token_limit,
        "compression_ratio": compression_ratio,
        "protected_message_count": protected_message_count,
        "protected_tokens": protected_tokens,
        "runtime_recovery_active": runtime_recovery_active,
        "needs_compression": needs_compression,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Extract
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn extract_data(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    input: String,
    schema: serde_json::Value,
    schema_name: Option<String>,
) -> Result<StructuredExtractionOutcome, IpcError> {
    state
        .app_state
        .extract_structured_for_scope(
            &workspace_id,
            &conversation_id,
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            StructuredExtractionRequest {
                input,
                schema,
                schema_name,
            },
        )
        .await
        .map_err(map_structured_extraction_error)
}

#[tauri::command]
pub async fn validate_schema(
    state: tauri::State<'_, TauriState>,
    schema: serde_json::Value,
) -> Result<StructuredExtractionValidation, IpcError> {
    Ok(state
        .app_state
        .history
        .structured_extraction
        .validate_schema(&schema))
}

#[tauri::command]
pub async fn get_extract_examples(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<StructuredExtractionExample>, IpcError> {
    Ok(state.app_state.history.structured_extraction.examples())
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
// Run diagnostics
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_diagnostic_runs(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let run_store = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.run_store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No run store configured".to_string()))?;
    let summaries =
        echo_agent_app_core::api::observability::list_diagnostic_runs(run_store.as_ref())
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
    serde_json::to_value(summaries).map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn get_run_diagnostics(
    state: tauri::State<'_, TauriState>,
    diagnostic_id: String,
) -> Result<serde_json::Value, IpcError> {
    let run_store = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.run_store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No run store configured".to_string()))?;
    let prompt_assembly = state
        .app_state
        .observability
        .prompt_assembly
        .read()
        .await
        .clone();
    let diagnostics = echo_agent_app_core::api::observability::load_run_diagnostics(
        run_store.as_ref(),
        &diagnostic_id,
        prompt_assembly,
    )
    .await
    .map_err(|error| IpcError::Internal(error.to_string()))?
    .ok_or_else(|| IpcError::NotFound(format!("Run diagnostics not found: {diagnostic_id}")))?;
    serde_json::to_value(diagnostics).map_err(|error| IpcError::Internal(error.to_string()))
}

// ════════════════════════════════════════════════════════════════════════════
// Evolution
// ════════════════════════════════════════════════════════════════════════════

fn curator_status_json(status: echo_agent::evolution::CuratorStatus) -> serde_json::Value {
    json!({
        "total": status.total,
        "candidate": status.candidate,
        "draft": status.draft,
        "active": status.active,
        "stale": status.stale,
        "deprecated": status.deprecated,
        "archived": status.archived,
        "pinned": status.pinned,
        "last_run_at": status.last_run_at,
    })
}

#[tauri::command]
pub async fn review_run(
    state: tauri::State<'_, TauriState>,
    run_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let review_lease = control.generation;
    let agent = control.runtime.primary_agent();
    let (llm_client, run_store) = agent
        .read(|a| (a.llm_client().cloned(), a.run_store().cloned()))
        .await;

    let llm_client = llm_client
        .ok_or_else(|| IpcError::Internal("No LLM client available for review".into()))?;
    let run_store =
        run_store.ok_or_else(|| IpcError::Internal("No run store configured".into()))?;

    let run_id = match run_id {
        Some(id) if !id.trim().is_empty() => id,
        _ => {
            let runs = run_store
                .list_all(1)
                .await
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            runs.first()
                .map(|r| r.run_id.clone())
                .ok_or_else(|| IpcError::NotFound("No runs to review".into()))?
        }
    };

    let reviewer = echo_agent::evolution::BackgroundReviewer::new(
        echo_agent::evolution::BackgroundReviewConfig::default(),
        llm_client,
        Some(review_lease.memory_store()),
        Some(run_store),
    );
    let reviewer = reviewer.with_layer_manager(
        review_lease
            .layer_manager()
            .map_err(|error| IpcError::Internal(error.to_string()))?,
    );
    let handle = reviewer
        .review_by_run_id(&run_id)
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let mut pass = review_lease
        .clone()
        .track_background_review(handle)
        .await
        .map_err(IpcError::Internal)?;
    let settlement = pass.settle().await.map_err(IpcError::Internal)?;
    let projection_settlement = if settlement.evidence_candidate.is_some() {
        Some(review_lease.settle_hot_memory_projection().await)
    } else {
        None
    };
    let outcome = settlement.outcome;
    let evidence_candidate = settlement.evidence_candidate;
    Ok(json!({
        "success": outcome.error.is_none(),
        "run_id": outcome.run_id,
        "actions": outcome.actions,
        "nothing_to_save": outcome.nothing_to_save,
        "candidate": outcome.candidate,
        "evidence_candidate": evidence_candidate,
        "error": outcome.error,
        "projection_settlement": projection_settlement,
    }))
}

#[tauri::command]
pub async fn list_evidence_candidates(
    state: tauri::State<'_, TauriState>,
    status: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::api::evolution::EvidenceReviewFilter;

    let filter = match status.as_deref() {
        None | Some("pending") => EvidenceReviewFilter::Pending,
        Some("expired") => EvidenceReviewFilter::Expired,
        Some("applied") | Some("undoable") => EvidenceReviewFilter::Undoable,
        Some(other) => {
            return Err(IpcError::Validation(format!(
                "Unknown Review Inbox filter: {other}"
            )));
        }
    };
    let control = current_evolution_control(&state).await?;
    let store = control.generation.evidence_store();
    let candidates: Vec<_> = store
        .review_items()
        .map_err(IpcError::Internal)?
        .into_iter()
        .filter(|candidate| filter.matches(candidate))
        .collect();
    Ok(json!({
        "candidates": candidates,
        "count": candidates.len(),
        "path": store.path().display().to_string(),
    }))
}

#[tauri::command]
pub async fn evidence_candidate_action(
    state: tauri::State<'_, TauriState>,
    action: String,
    candidate_id: String,
    content: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let evidence_lease = control.generation;
    let store = evidence_lease.evidence_store();
    let candidate = match action.as_str() {
        "edit" => store
            .edit(&candidate_id, content.as_deref().unwrap_or_default())
            .map_err(IpcError::Internal)?,
        "reject" => store.reject(&candidate_id).map_err(IpcError::Internal)?,
        "accept" | "undo" => {
            let layer_manager = evidence_lease
                .layer_manager()
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            if action == "accept" {
                store
                    .accept(&candidate_id, content.as_deref(), &layer_manager)
                    .await
                    .map_err(IpcError::Internal)?
            } else {
                store
                    .undo(&candidate_id, &layer_manager)
                    .await
                    .map_err(IpcError::Internal)?
            }
        }
        other => {
            return Err(IpcError::Validation(format!(
                "Unknown evidence action: {other}"
            )));
        }
    };
    let projection_settlement = evidence_lease.settle_hot_memory_projection().await;
    Ok(json!({
        "success": true,
        "candidate": candidate,
        "projection_settlement": projection_settlement,
    }))
}

#[tauri::command]
pub async fn curator_action(
    state: tauri::State<'_, TauriState>,
    action: String,
    skill_name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let curator = control.integration.curator();
    match action.as_str() {
        "status" => Ok(json!({
            "success": true,
            "status": curator_status_json(curator.status().map_err(|e| IpcError::Internal(e.to_string()))?),
        })),
        "run" => {
            let transitions = curator
                .apply_transitions()
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            control
                .runtime
                .primary_agent()
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.reconcile_skill_load_policy().await;
                    })
                })
                .await;
            let transition_values: Vec<serde_json::Value> = transitions
                .iter()
                .map(|(skill, from, to)| {
                    json!({
                        "skill": skill,
                        "from": format!("{from:?}"),
                        "to": format!("{to:?}"),
                    })
                })
                .collect();
            Ok(json!({
                "success": true,
                "transitions": transition_values,
                "count": transitions.len(),
                "status": curator_status_json(curator.status().map_err(|e| IpcError::Internal(e.to_string()))?),
            }))
        }
        "pin" => {
            let name = skill_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| IpcError::Validation("skill_name is required for pin".into()))?;
            curator
                .pin_skill(&name)
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(
                json!({"success": true, "pinned": name, "status": curator_status_json(curator.status().map_err(|e| IpcError::Internal(e.to_string()))?)}),
            )
        }
        "unpin" => {
            let name = skill_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| IpcError::Validation("skill_name is required for unpin".into()))?;
            curator
                .unpin_skill(&name)
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(
                json!({"success": true, "unpinned": name, "status": curator_status_json(curator.status().map_err(|e| IpcError::Internal(e.to_string()))?)}),
            )
        }
        _ => Err(IpcError::Validation(format!(
            "Unknown curator action '{action}'. Valid: status, run, pin, unpin"
        ))),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Evolution Dashboard
// ════════════════════════════════════════════════════════════════════════════

/// 返回自进化系统的按需概览:分层记忆统计、最近变更活动,以及跨 run
/// 重复工具错误。只在用户打开面板时扫描,不启动后台诊断任务。
///
/// 这是阶段 1(Dashboard 接线)的后端入口;前端 `EvolutionPanel` 据此渲染
/// "进化概览"段。构造 pattern 复刻自 `cmd_evolution_dashboard`
/// (src/cli/cmd_impls/evolution.rs:1373)。
#[tauri::command]
pub async fn get_evolution_dashboard(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let run_store = control
        .runtime
        .primary_agent()
        .read(|agent| agent.run_store().cloned())
        .await;
    let store = control.generation.memory_store();
    let echo_agent_dir = control.generation.echo_agent_dir();
    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_dir.join("evolution").join("change-log.jsonl"),
    )
    .map_err(|error| IpcError::Internal(error.to_string()))?;

    let dashboard = echo_agent_app_core::api::evolution::Dashboard::new(store, change_log)
        .with_run_store(run_store);
    let metrics = dashboard.generate_metrics().await;
    let trigger_delivery = Some(control.integration.trigger_delivery_status());

    Ok(json!({
        "metrics": metrics,
        "trigger_delivery": trigger_delivery,
    }))
}

/// 返回「记忆 → AGENTS.md 规则」的晋升候选列表(高置信 + 满足 age/type 门槛)。
///
/// 这是 review gate 的「展示」侧:用户在 EvolutionPanel 看到候选(置信度/
/// 类型/规则文本),决定是否采纳。采纳走 `promote_rule` —— 只有用户点按钮
/// 才会写 AGENTS.md,agent 不会静默改规则。复刻 CLI 的两步式
/// (src/cli/cmd_impls/evolution.rs:1297 scan)。
#[tauri::command]
pub async fn scan_rule_proposals(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let proposals = control
        .integration
        .scan_rule_proposals()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;

    Ok(json!({ "proposals": proposals, "count": proposals.len() }))
}

/// 采纳一条规则候选:按 memory_key 找到候选,写 AGENTS.md `## Rules` 段,
/// 并在原记忆打 `<!-- PROMOTED_TO_RULE -->` 标记防重复。
///
/// review gate 的「执行」侧 —— 由用户在前端点「采纳」触发,不经此路径
/// 不会改 AGENTS.md。复刻 CLI (evolution.rs:1338 promote_rule)。
#[tauri::command]
pub async fn promote_rule(
    state: tauri::State<'_, TauriState>,
    memory_key: String,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    // 找到对应候选(scan 已过置信度/age/type 门槛)
    let proposal = control
        .integration
        .scan_rule_proposals()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .into_iter()
        .find(|p| p.memory_key == memory_key)
        .ok_or_else(|| {
            IpcError::NotFound(format!(
                "Memory '{memory_key}' not found or does not meet promotion criteria"
            ))
        })?;

    let receipt = control
        .integration
        .promote_rule(&proposal)
        .await
        .map_err(|error| IpcError::Internal(format!("Failed to promote rule: {error}")))?;
    let projection_settlement = control.generation.settle_hot_memory_projection().await;

    Ok(json!({
        "success": true,
        "memory_key": memory_key,
        "rule_text": proposal.rule_text,
        "promotion_id": receipt.promotion_id,
        "projection_settlement": projection_settlement,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Skill Candidates(技能自动创建闭环的可见可控出口)
// ════════════════════════════════════════════════════════════════════════════

/// 列出已检测到的技能候选(来自 SkillCandidateDetector,存在 CANDIDATE_NAMESPACE)。
///
/// 闭环已存在:review 时 SkillCandidateDetector 扫描 WorkflowPattern/
/// DebuggingLesson 记忆,超阈值(≥3)的进 CANDIDATE_NAMESPACE。本 command 只是把
/// 它们暴露给前端,让用户看见「系统发现了哪些可复用模式」。
#[tauri::command]
pub async fn scan_skill_candidates(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let store = control.generation.memory_store();

    let typed = echo_agent::memory::TypedMemoryStore::new(store);
    let entries = typed
        .list_typed(
            echo_agent::evolution::candidate::CANDIDATE_NAMESPACE,
            &echo_agent::memory::MemoryFilter::new(),
        )
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to list candidates: {e}")))?;

    let echo_agent_dir = control.generation.echo_agent_dir();
    let candidates: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            let name = e.key.clone();
            // 草稿是否已生成(_drafts/<name>/SKILL.md)
            let draft_path = echo_agent_dir
                .join("skills")
                .join("_drafts")
                .join(&name)
                .join("SKILL.md");
            let activated = echo_agent_dir
                .join("skills")
                .join(&name)
                .join("SKILL.md")
                .exists();
            // 尝试解析 content 为 SkillCandidate(JSON),取 description/sample_count;
            // 解析失败则 fallback 到截断的原始 content(UTF-8 安全:chars().take)。
            let (description, sample_count, source_type) =
                serde_json::from_str::<echo_agent::evolution::SkillCandidate>(&e.content)
                    .ok()
                    .map(|c| {
                        (
                            c.description,
                            c.sample_count,
                            format!("{:?}", c.source_type),
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            e.content.chars().take(80).collect::<String>(),
                            0,
                            "unknown".to_string(),
                        )
                    });
            json!({
                "name": name,
                "description": description,
                "sample_count": sample_count,
                "source_type": source_type,
                "has_draft": draft_path.exists(),
                "activated": activated,
                "confidence": e.meta.confidence,
            })
        })
        .collect();

    Ok(json!({ "candidates": candidates, "count": candidates.len() }))
}

/// 为一个候选生成草稿 SKILL.md(review gate:用户触发才生成)。
///
/// ReviewConfig.auto_generate_drafts 默认 false —— 草稿不会自动生成。本 command
/// 让用户显式触发:点「生成草稿」→ SkillDraftGenerator 写 _drafts/<name>/SKILL.md
/// + curator 状态 Candidate→Draft。
#[tauri::command]
pub async fn generate_skill_draft(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let control = current_evolution_control(&state).await?;
    let generation = control.generation;
    let agent = control.runtime.primary_agent();
    let store = generation.memory_store();
    let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_dir.join("evolution").join("change-log.jsonl"),
    )
    .map_err(|error| IpcError::Internal(error.to_string()))?;
    let typed = echo_agent::memory::TypedMemoryStore::new(store);

    let curator = echo_agent_app_core::api::evolution::workspace_curator(&echo_agent_dir);
    let generator = echo_agent::evolution::SkillDraftGenerator::new(echo_agent_dir, &change_log)
        .with_curator(curator);
    let result = generator
        .generate(&name, &typed)
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to generate draft: {e}")))?;

    echo_agent_app_core::api::evolution::fire_evolution_hook(
        &agent,
        echo_agent::hooks::HookEvent::SkillLifecycleTransition,
        &result.name,
    )
    .await;

    Ok(json!({
        "success": true,
        "name": result.name,
        "path": result.skill_md_path.to_string_lossy(),
    }))
}

/// 激活一个草稿技能:复制 _drafts/<name>/ → skills/<name>/,并 curator
/// 状态 Draft→Active。激活后技能进入 DiscoveryScope::Project 扫描路径,下次
/// agent 启动/技能重载时自动加载。review gate:用户显式触发才激活。
#[tauri::command]
pub async fn activate_skill_draft(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let runtime = state
        .app_state
        .current_control_runtime()
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let generation = runtime
        .review_integration()
        .ok_or_else(|| {
            IpcError::Internal(format!(
                "Review integration is not configured for workspace '{}'",
                runtime.execution_scope().workspace_id()
            ))
        })?
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let receipt = state
        .app_state
        .extension_control
        .publish_curated_skill(&state.app_state, Some(&runtime), generation, &name)
        .await
        .map_err(|error| IpcError::Internal(format!("Failed to promote Skill: {error}")))?;
    let success = receipt.status
        == echo_agent_app_core::api::extension_control::SkillSettlementStatus::Settled;

    Ok(json!({
        "success": success,
        "status": receipt.status,
        "name": receipt.name,
        "path": receipt.active_path,
        "durable_committed": receipt.durable_committed,
        "idempotent": receipt.idempotent,
        "loaded_entries": receipt.loaded_entries,
        "runtime_error": receipt.runtime_error,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Worktree
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
struct WorktreeInfo {
    path: String,
    branch: String,
    managed: bool,
    head: String,
}

struct ScopedWorktreeControl {
    _control: echo_agent_app_core::api::state::ScopedWorkspaceControl,
    repo_root: PathBuf,
    store: Option<Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>>,
}

async fn run_taskruntime_worktree_operation<T, E, F>(
    store: Option<Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>>,
    operation: &'static str,
    function: F,
) -> Result<T, IpcError>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce(
            Option<&echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
        ) -> Result<T, E>
        + Send
        + 'static,
{
    match store {
        Some(store) => {
            let operation_store = store.clone();
            echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store)
                .run_owned(operation, move || {
                    function(Some(operation_store.as_ref())).map_err(|error| {
                        echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(
                            error.to_string(),
                        )
                    })
                })
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))
        }
        None => tokio::task::spawn_blocking(move || function(None))
            .await
            .map_err(|error| IpcError::Internal(format!("Failed to join {operation}: {error}")))?
            .map_err(|error| IpcError::Internal(error.to_string())),
    }
}

async fn worktree_control_for_workspace(
    state: &TauriState,
    workspace_id: &str,
) -> Result<ScopedWorktreeControl, IpcError> {
    let control = state
        .app_state
        .workspace_control_for_scope(workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let repo_root = control.project_root();
    let store = control.runtime().task_runtime();
    Ok(ScopedWorktreeControl {
        _control: control,
        repo_root,
        store,
    })
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, IpcError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| IpcError::Internal(format!("Failed to run git: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(IpcError::Validation(if stderr.is_empty() {
            format!("git {:?} failed", args)
        } else {
            stderr
        }))
    }
}

fn git_repo_root(start: &Path) -> Result<PathBuf, IpcError> {
    let root = run_git(start, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root))
}

async fn git_repo_root_async(start: PathBuf) -> Result<PathBuf, IpcError> {
    tokio::task::spawn_blocking(move || git_repo_root(&start))
        .await
        .map_err(|error| IpcError::Internal(format!("Failed to join git root lookup: {error}")))?
}

fn parse_worktree_list(output: &str, repo_root: &Path) -> Vec<WorktreeInfo> {
    let canonical_repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let mut items = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch = String::new();

    let flush = |items: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut String| {
        let Some(path_buf) = path.take() else {
            return;
        };
        let canonical = path_buf.canonicalize().unwrap_or_else(|_| path_buf.clone());
        let display_branch = if branch.is_empty() {
            "(detached)".to_string()
        } else {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch.as_str())
                .to_string()
        };
        items.push(WorktreeInfo {
            path: path_buf.to_string_lossy().to_string(),
            branch: display_branch,
            managed: canonical != canonical_repo,
            head: head.chars().take(12).collect(),
        });
        head.clear();
        branch.clear();
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            current_path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = branch.to_string();
        }
    }
    flush(
        &mut items,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
    );
    items
}

fn validate_branch_name(repo: &Path, branch: &str) -> Result<(), IpcError> {
    if branch.trim().is_empty() {
        return Err(IpcError::Validation("Branch name cannot be empty".into()));
    }
    if branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(IpcError::Validation(
            "Branch name cannot start with '-' or contain whitespace".into(),
        ));
    }
    run_git(repo, &["check-ref-format", "--branch", branch]).map(|_| ())
}

fn default_worktree_path(repo_root: &Path, branch: &str) -> Result<PathBuf, IpcError> {
    let parent = repo_root
        .parent()
        .ok_or_else(|| IpcError::Validation("Repository root has no parent directory".into()))?;
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".to_string());
    let safe_branch: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(parent.join(format!("{repo_name}-{safe_branch}")))
}

fn validate_worktree_target(repo_root: &Path, target: &Path) -> Result<(), IpcError> {
    let repo_parent = repo_root
        .parent()
        .ok_or_else(|| IpcError::Validation("Repository root has no parent directory".into()))?
        .canonicalize()
        .map_err(|e| IpcError::Validation(format!("Cannot resolve repository parent: {e}")))?;
    let target_parent = target
        .parent()
        .ok_or_else(|| IpcError::Validation("Worktree path has no parent directory".into()))?;
    let canonical_parent = target_parent
        .canonicalize()
        .map_err(|e| IpcError::Validation(format!("Cannot resolve worktree parent: {e}")))?;
    if !canonical_parent.starts_with(&repo_parent) {
        return Err(IpcError::Validation(format!(
            "Worktree path must stay under repository parent: {}",
            repo_parent.display()
        )));
    }
    if target.exists() {
        return Err(IpcError::Validation(format!(
            "Worktree path already exists: {}",
            target.display()
        )));
    }
    Ok(())
}

#[tauri::command]
pub async fn list_worktrees(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let start = control.repo_root.clone();
    let worktrees = tokio::task::spawn_blocking(move || {
        let repo_root = git_repo_root(&start)?;
        let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
        Ok::<Vec<WorktreeInfo>, IpcError>(parse_worktree_list(&output, &repo_root))
    })
    .await
    .map_err(|error| IpcError::Internal(format!("Failed to join worktree listing: {error}")))??;
    drop(control);
    Ok(json!(worktrees))
}

#[tauri::command]
pub async fn create_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    branch: String,
    base: Option<String>,
    path: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let start = control.repo_root.clone();
    let branch = branch.trim().to_string();
    let base_ref = base
        .and_then(|s| {
            let trimmed = s.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or_else(|| "HEAD".to_string());
    let repo_root = git_repo_root_async(start).await?;
    let merge_lock =
        echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo_root);
    let _merge_guard = merge_lock.lock().await;
    let created = tokio::task::spawn_blocking(move || {
        validate_branch_name(&repo_root, &branch)?;
        let target = path
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or(default_worktree_path(&repo_root, &branch)?);
        validate_worktree_target(&repo_root, &target)?;
        let target_str = target.to_string_lossy().to_string();
        run_git(
            &repo_root,
            &["worktree", "add", "-b", &branch, &target_str, &base_ref],
        )?;
        let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
        parse_worktree_list(&output, &repo_root)
            .into_iter()
            .find(|worktree| worktree.path == target_str)
            .ok_or_else(|| {
                IpcError::Internal("Created worktree was not found in git output".into())
            })
    })
    .await
    .map_err(|error| IpcError::Internal(format!("Failed to join worktree create: {error}")))??;
    drop(control);
    Ok(json!(created))
}

#[tauri::command]
pub async fn remove_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    path: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let start = control.repo_root.clone();
    let repo_root = git_repo_root_async(start).await?;
    let merge_lock =
        echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo_root);
    let _merge_guard = merge_lock.lock().await;
    tokio::task::spawn_blocking(move || {
        let target = PathBuf::from(path.trim());
        if target.as_os_str().is_empty() {
            return Err(IpcError::Validation("Worktree path cannot be empty".into()));
        }
        let canonical_repo = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.clone());
        let canonical_target = target.canonicalize().map_err(|error| {
            IpcError::Validation(format!("Cannot resolve worktree path: {error}"))
        })?;
        if canonical_target == canonical_repo {
            return Err(IpcError::Validation(
                "Refusing to remove the primary repository worktree".into(),
            ));
        }
        let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
        let known = parse_worktree_list(&output, &repo_root)
            .into_iter()
            .any(|worktree| {
                PathBuf::from(worktree.path)
                    .canonicalize()
                    .map(|path| path == canonical_target)
                    .unwrap_or(false)
            });
        if !known {
            return Err(IpcError::Validation(
                "Path is not a registered git worktree".into(),
            ));
        }
        let target_str = target.to_string_lossy().to_string();
        run_git(&repo_root, &["worktree", "remove", &target_str]).map(|_| ())
    })
    .await
    .map_err(|error| IpcError::Internal(format!("Failed to join worktree removal: {error}")))??;
    drop(control);
    Ok(json!({"success": true}))
}

#[tauri::command]
pub async fn list_unattended_worktrees(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let repo_root = git_repo_root_async(control.repo_root.clone()).await?;
    let store = control.store.clone();

    let unattended =
        run_taskruntime_worktree_operation(store, "list unattended worktrees", move |store| {
            echo_agent_app_core::api::tasks::task_runtime::worktree::list_unattended_worktrees(
                &repo_root, store,
            )
        })
        .await?;
    drop(control);

    let result: Vec<serde_json::Value> = unattended
        .into_iter()
        .map(|wt| {
            json!({
                "run_id": wt.run_id,
                "branch": wt.branch,
                "path": wt.path.map(|path| path.to_string_lossy().to_string()),
                "head": wt.head,
                "status": wt.status,
                "active": wt.active,
                "locked": wt.locked,
                "lock_reason": wt.lock_reason,
                "uncommitted_changes": wt.uncommitted_changes,
                "ahead_commits": wt.ahead_commits,
                "has_changes": wt.has_changes,
                "orphan_branch": wt.orphan_branch,
            })
        })
        .collect();

    Ok(json!(result))
}

#[tauri::command]
pub async fn merge_unattended_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let repo_root = git_repo_root_async(control.repo_root.clone()).await?;
    let store = control.store.clone();
    if let Some(store) = store.as_ref() {
        let lookup_run_id = run_id.clone();
        let run = echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(
            store.clone(),
        )
        .run_store("validate unattended worktree TaskRun", move |store| {
            store.get_run(&lookup_run_id)
        })
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
        if run.is_none_or(|run| run.workspace_id != workspace_id) {
            return Err(IpcError::Validation(format!(
                "TaskRun '{run_id}' does not belong to workspace '{workspace_id}'"
            )));
        }
    } else {
        return Err(IpcError::Validation(
            "TaskRuntime store is unavailable for unattended worktree merge".to_string(),
        ));
    }
    let merge_lock =
        echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo_root);
    let _merge_guard = merge_lock.lock().await;
    let run_id_for_merge = run_id.clone();
    let outcome =
        run_taskruntime_worktree_operation(store, "merge unattended worktree", move |store| {
            echo_agent_app_core::api::tasks::task_runtime::worktree::merge_unattended_worktree(
                &repo_root,
                &run_id_for_merge,
                store,
            )
        })
        .await?;
    drop(control);

    Ok(json!({
        "success": true,
        "run_id": run_id,
        "status": outcome.status.as_str(),
        "branch": outcome.branch,
        "changed_files": outcome.changed_files,
        "merge_commit": outcome.merge_commit,
        "cleanup_warning": outcome.cleanup_warning,
    }))
}

#[tauri::command]
pub async fn discard_unattended_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let repo_root = git_repo_root_async(control.repo_root.clone()).await?;
    let store = control.store.clone();
    let merge_lock =
        echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo_root);
    let _merge_guard = merge_lock.lock().await;
    let run_id_for_discard = run_id.clone();
    run_taskruntime_worktree_operation(store, "discard unattended worktree", move |store| {
        echo_agent_app_core::api::tasks::task_runtime::worktree::discard_unattended_worktree(
            &repo_root,
            &run_id_for_discard,
            store,
        )
    })
    .await?;
    drop(control);

    Ok(json!({"success": true, "discarded": run_id}))
}

#[tauri::command]
pub async fn cleanup_unattended_worktrees(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = worktree_control_for_workspace(&state, &workspace_id).await?;
    let repo_root = git_repo_root_async(control.repo_root.clone()).await?;
    let store = control.store.clone();
    let merge_lock =
        echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo_root);
    let _merge_guard = merge_lock.lock().await;
    let result =
        run_taskruntime_worktree_operation(store, "clean unattended worktrees", move |store| {
            echo_agent_app_core::api::tasks::task_runtime::worktree::cleanup_unattended_worktrees(
                &repo_root, store,
            )
        })
        .await?;
    drop(control);

    Ok(json!({
        "removed": result.removed,
        "unlocked": result.unlocked,
        "kept": result.kept,
        "errors": result.errors,
    }))
}

#[cfg(test)]
mod scoped_control_tests {
    use super::*;

    #[test]
    fn skill_wire_loaded_state_comes_from_extension_projection() {
        let entry = echo_agent_app_core::api::extension_control::ExtensionSkillEntry {
            catalog: echo_agent_app_core::api::skills_hub::SkillHubEntry {
                name: "review".to_string(),
                description: "Review changes".to_string(),
                path: PathBuf::from("/tmp/skills/review"),
                category: "development".to_string(),
                is_baseline: false,
                is_builtin: true,
                upstream_version: None,
                source: None,
                license: None,
                compatibility: None,
                version: None,
                author: None,
                tags: Vec::new(),
                has_sandbox: false,
                depends_on: Vec::new(),
                missing_dependencies: Vec::new(),
            },
            loaded: true,
        };

        let wire = serde_json::to_value(&entry).unwrap_or_default();

        assert!(wire.get("file").is_none());
        assert_eq!(
            wire.get("path").and_then(serde_json::Value::as_str),
            Some("/tmp/skills/review")
        );
        assert_eq!(
            wire.get("loaded").and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn invalid_auto_memory_scope_does_not_commit_global_toggle() {
        let previous = crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed);
        let result = commit_auto_memory_toggle_after_validation::<()>(
            Err(IpcError::Validation("workspace was deleted".to_string())),
            !previous,
        );
        assert!(result.is_err());
        assert_eq!(
            crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
                .load(std::sync::atomic::Ordering::Relaxed),
            previous
        );
    }

    #[test]
    fn invalid_auto_memory_scope_precedes_disabled_policy() {
        let result = auto_memory_extract_control_after_validation::<()>(
            Err(IpcError::Validation("workspace was deleted".to_string())),
            false,
        );
        assert!(matches!(
            result,
            Err(IpcError::Validation(message)) if message == "workspace was deleted"
        ));
    }

    #[test]
    fn primary_and_unattended_mutations_share_repo_lock() {
        let repo = std::env::temp_dir().join("eko-worktree-lock-contract");
        let primary =
            echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo);
        let unattended =
            echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(&repo);
        assert!(Arc::ptr_eq(&primary, &unattended));
    }
}
