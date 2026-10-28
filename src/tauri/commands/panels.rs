//! IPC commands for panels not yet migrated from HTTP server.
//!
//! This module provides Tauri IPC commands for functionality that was
//! previously served via Axum HTTP routes. Each section corresponds to
//! a deleted server route module.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::sandbox::{IsolationLevel, SandboxCommand, SandboxExecutor};
use echo_agent::tools::permission::{PermissionRule, RuleBehavior, RuleMatcher, RuleSource};
use echo_agent_app_core::api::auto_memory::AUTO_MEMORY_ENABLED;
use echo_agent_app_core::api::state::AuditDecision;
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
        "mode": mode.id()
    }))
}

#[tauri::command]
pub async fn set_permissions_mode(
    state: tauri::State<'_, TauriState>,
    mode: String,
) -> Result<serde_json::Value, IpcError> {
    let framework_mode = mode
        .parse::<echo_agent::tools::permission::PermissionMode>()
        .map_err(IpcError::Validation)?;
    let normalized = framework_mode.id();
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
    serde_json::to_value(&*rules).map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn add_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
    behavior: String,
    source: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let matcher = matcher
        .parse::<RuleMatcher>()
        .map_err(IpcError::Validation)?;
    let behavior = behavior
        .parse::<RuleBehavior>()
        .map_err(IpcError::Validation)?;
    let source = source
        .unwrap_or_else(|| "manual".to_string())
        .parse::<RuleSource>()
        .map_err(IpcError::Validation)?;
    let rule = PermissionRule {
        matcher: matcher.clone(),
        behavior,
        source,
        description: Some("EKO application permission rule".to_string()),
    };
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
        *existing = rule.clone();
    } else {
        rules.push(rule.clone());
    }
    drop(rules);
    if let Some(previous) = previous {
        permission_service.remove_rule(&previous).await;
    }
    let response =
        serde_json::to_value(&rule).map_err(|error| IpcError::Internal(error.to_string()))?;
    permission_service.add_rule(rule).await;

    Ok(response)
}

#[tauri::command]
pub async fn remove_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
) -> Result<serde_json::Value, IpcError> {
    let matcher = matcher
        .parse::<RuleMatcher>()
        .map_err(IpcError::Validation)?;
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
    let removed_rule = removed_rule
        .ok_or_else(|| IpcError::NotFound(format!("Permission rule '{}' not found", matcher)))?;
    permission_service.remove_rule(&removed_rule).await;

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
        enabled: AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
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
    AUTO_MEMORY_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
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
        enabled: AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
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
        enabled: AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
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

/// Isolation floor every GUI sandbox entry point must clear.
///
/// EKO is a local personal assistant on the user's own machine, so this floor is
/// **not** about separating tenants from each other. A process-only backend
/// inherits the user's whole filesystem, network and credentials: if the framework
/// mis-renders a command, mis-evaluates the policy, or a shell/code language is
/// mapped to the wrong interpreter, a process-only run damages the host instead of
/// failing inside `sandbox-exec` / `bwrap`. The floor therefore exists to contain
/// framework bugs (AGENTS.md: 防框架自身 bug 造成破坏), not to gate the user.
///
/// It is derived from backend capability only, never from `permission_mode`:
/// terminal/code execution is a user-initiated developer action, so an
/// agent-automatic approval gate (`require_full_auto` & friends) would be the
/// wrong control and must not be reintroduced here.
const EKO_LOCAL_SANDBOX_ISOLATION_FLOOR: IsolationLevel = IsolationLevel::OsSandbox;

/// Capability probe: can this executor actually provide OS-level isolation?
///
/// `is_available_at` is a probe rather than a configuration claim: for the local
/// backend it requires both an OS-isolation backend *and* a successful start of
/// `sandbox-exec` (macOS) / `bwrap` (Linux/WSL2). A backend that only spawns plain
/// host processes reports [`IsolationLevel::Process`] and is rejected here even
/// though [`SandboxExecutor::is_available`] alone would answer `true`.
async fn eko_local_sandbox_available(manager: &dyn SandboxExecutor) -> bool {
    if cfg!(target_os = "windows") {
        // Windows has no local OS sandbox backend yet; WSL2 supplies bwrap.
        false
    } else {
        manager
            .is_available_at(EKO_LOCAL_SANDBOX_ISOLATION_FLOOR)
            .await
    }
}

/// Backend label derived from the same probe as [`eko_local_sandbox_available`],
/// so the reported backend cannot advertise OS isolation that was never probed.
fn eko_local_sandbox_backend_label(local_available: bool) -> &'static str {
    if cfg!(target_os = "windows") {
        "wsl2-required"
    } else if local_available {
        "local"
    } else {
        "process-only"
    }
}

fn eko_local_sandbox_unavailable_message() -> &'static str {
    if cfg!(target_os = "windows") {
        "Native Windows sandbox is not supported by EKO. Run EKO inside WSL2 so it can use Linux bubblewrap (bwrap)."
    } else {
        "Local sandbox backend is unavailable. On macOS this requires sandbox-exec; on Linux/WSL2 this requires bubblewrap (bwrap)."
    }
}

/// Guard for the actual execution entry point.
///
/// There is deliberately **no** implicit fallback: when OS isolation is not
/// available the GUI must return this error instead of quietly running the command
/// as a plain host process. Note this is not a permission gate — it does not look
/// at `permission_mode` and it does not ask for `full-auto`; it only refuses to
/// execute without the OS-level containment the sandbox contract promises.
async fn eko_require_os_sandbox(manager: &dyn SandboxExecutor) -> Result<(), IpcError> {
    if eko_local_sandbox_available(manager).await {
        Ok(())
    } else {
        Err(IpcError::Validation(
            eko_local_sandbox_unavailable_message().to_string(),
        ))
    }
}

/// Build a sandbox command that carries the OS-isolation floor into the executor.
///
/// The probe above already rejects an unavailable backend, but the floor rides on
/// the command as defence in depth: a `SandboxManager` configured with
/// `allow_fallback` refuses to downgrade a command whose `minimum_isolation` is
/// unmet, so no future routing change can silently turn this into a host process.
fn eko_sandbox_command(is_shell: bool, language: &str, code: String) -> SandboxCommand {
    let command = if is_shell {
        SandboxCommand::shell(code)
    } else {
        SandboxCommand::code(language, code)
    };
    command.with_minimum_isolation(EKO_LOCAL_SANDBOX_ISOLATION_FLOOR)
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
        Some(manager) => eko_local_sandbox_available(manager.as_ref()).await,
        None => false,
    };
    Ok(serde_json::json!({
        "local_available": local_available,
        "docker_available": false,
        "k8s_available": false,
        "current_backend": eko_local_sandbox_backend_label(local_available),
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
    // Fail closed before touching the executor: a process-only backend would run
    // this as a plain host process, which is exactly the silent downgrade the
    // sandbox contract forbids. The GUI has no "run unsandboxed anyway" opt-in,
    // so there is no explicit non-isolated path to preserve here.
    eko_require_os_sandbox(manager.as_ref()).await?;

    let command = eko_sandbox_command(is_shell, &lang, code);
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
    focus: Option<String>,
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
                focus,
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
        "tokens_before": receipt.tokens_before,
        "tokens_after": receipt.tokens_after,
        "checkpoint": receipt.checkpoint,
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
    let needs_compression = token_limit > 0 && current_tokens > token_limit.saturating_mul(3) / 4;

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
    let handle = reviewer.review_by_run_id(&run_id);
    let mut pass = review_lease
        .clone()
        .track_background_review(handle)
        .await
        .map_err(IpcError::Internal)?;
    let operation_id = pass.operation_id().to_string();
    let settlement = match pass.settle().await {
        Ok(settlement) => settlement,
        Err(error) => {
            return Ok(json!({
                "success": false,
                "review_operation_id": operation_id,
                "run_id": run_id,
                "actions": [],
                "nothing_to_save": false,
                "candidate": null,
                "evidence_candidate": null,
                "error": error,
                "projection_settlement": null,
            }));
        }
    };
    let projection_settlement = if settlement.evidence_candidate.is_some() {
        Some(review_lease.settle_hot_memory_projection().await)
    } else {
        None
    };
    let outcome = settlement.outcome;
    let evidence_candidate = settlement.evidence_candidate;
    Ok(json!({
        "success": outcome.error.is_none(),
        "review_operation_id": settlement.operation_id,
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
pub async fn get_review_receipt(
    state: tauri::State<'_, TauriState>,
    operation_id: String,
) -> Result<serde_json::Value, IpcError> {
    if operation_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "review operation_id is required".into(),
        ));
    }
    let control = current_evolution_control(&state).await?;
    let receipt = control
        .generation
        .background_review_receipt(&operation_id)
        .map_err(IpcError::Internal)?
        .ok_or_else(|| IpcError::NotFound("Review receipt not found".into()))?;
    Ok(json!(receipt))
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
            let transitions = control
                .generation
                .run_skill_transitions("eko:gui-user")
                .await
                .map_err(IpcError::Internal)?;
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
            control
                .generation
                .set_skill_pinned(&name, true, "eko:gui-user")
                .await
                .map_err(IpcError::Internal)?;
            Ok(
                json!({"success": true, "pinned": name, "status": curator_status_json(curator.status().map_err(|e| IpcError::Internal(e.to_string()))?)}),
            )
        }
        "unpin" => {
            let name = skill_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| IpcError::Validation("skill_name is required for unpin".into()))?;
            control
                .generation
                .set_skill_pinned(&name, false, "eko:gui-user")
                .await
                .map_err(IpcError::Internal)?;
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
    let result = generation
        .generate_skill_draft(&name, "eko:gui-user")
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

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub(super) struct WorktreeInfo {
    pub(super) path: String,
    pub(super) branch: String,
    pub(super) managed: bool,
    pub(super) head: String,
    pub(super) commit_oid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lock_reason: Option<String>,
}

struct ScopedWorktreeControl {
    _global: Option<echo_agent_app_core::api::state::ScopedWorkspaceControl>,
    _project: Option<echo_agent_app_core::api::state::ScopedProjectResources>,
    repo_root: PathBuf,
    store: Option<Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>>,
    files: Arc<dyn echo_agent::tools::filesystem::FileSystem>,
}

impl ScopedWorktreeControl {
    fn merge_lock(&self, root: &Path) -> Arc<tokio::sync::Mutex<()>> {
        match self.store.as_deref() {
            Some(store) => store.project_worktree_lock(root),
            None => echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(root),
        }
    }
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
            echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
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
    workspace_generation: &str,
) -> Result<ScopedWorktreeControl, IpcError> {
    worktree_control_for_app(&state.app_state, workspace_id, workspace_generation).await
}

async fn worktree_control_for_app(
    app_state: &Arc<echo_agent_app_core::api::AppState>,
    workspace_id: &str,
    workspace_generation: &str,
) -> Result<ScopedWorktreeControl, IpcError> {
    if workspace_id == "global" {
        // Preserve only the explicit global local owner, never as a fallback.
        let product = app_state
            .workspace_control_for_generation(workspace_id, workspace_generation)
            .await
            .map_err(IpcError::from)?;
        let control = app_state
            .workspace_control_for_scope(workspace_id)
            .await
            .map_err(|error| IpcError::Validation(error.to_string()))?;
        let repo_root = product.project_root();
        return Ok(ScopedWorktreeControl {
            store: control.runtime().task_runtime(),
            _global: Some(control),
            _project: None,
            repo_root,
            files: Arc::new(echo_agent::tools::files::LocalFileSystem),
        });
    }
    let project = app_state
        .project_resources_for_workspace(workspace_id, workspace_generation)
        .await
        .map_err(IpcError::from)?;
    let store = project.task_runtime().await.map_err(IpcError::from)?;
    let repo_root = project.working_dir().to_path_buf();
    let files = project.file_system();
    Ok(ScopedWorktreeControl {
        _global: None,
        _project: Some(project),
        repo_root,
        store: Some(store),
        files,
    })
}

fn run_git(
    store: Option<&echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    repo: &Path,
    args: &[&str],
) -> Result<String, IpcError> {
    if let Some(store) = store {
        return store
            .project_run_git(repo, args)
            .map_err(|error| IpcError::Validation(error.to_string()));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| IpcError::Internal(format!("Failed to run git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(IpcError::Validation(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn git_repo_root(control: &ScopedWorktreeControl) -> Result<PathBuf, IpcError> {
    match control.store.as_deref() {
        Some(store) => store
            .project_git_repo_root(&control.repo_root)
            .map_err(|error| IpcError::Validation(error.to_string())),
        None => {
            run_git(None, &control.repo_root, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
        }
    }
}

async fn git_repo_root_async(
    control: ScopedWorktreeControl,
) -> Result<(ScopedWorktreeControl, PathBuf), IpcError> {
    tokio::task::spawn_blocking(move || {
        let root = git_repo_root(&control)?;
        Ok((control, root))
    })
    .await
    .map_err(|error| IpcError::Internal(format!("Failed to join git root lookup: {error}")))?
}

fn parse_worktree_list(output: &str, repo_root: &Path) -> Vec<WorktreeInfo> {
    let canonical_repo = repo_root.to_path_buf();
    let mut items = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch = String::new();
    let mut current_lock_reason: Option<String> = None;

    let flush = |items: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut String,
                 lock_reason: &mut Option<String>| {
        let Some(path_buf) = path.take() else {
            return;
        };
        let canonical = path_buf.clone();
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
            commit_oid: head.clone(),
            lock_reason: lock_reason.take(),
        });
        head.clear();
        branch.clear();
    };

    // -z 是原生 Git 无歧义路径合同，避免中文、换行及空白路径被 C quote。
    // 保留非 NUL 入口只供既有历史/静态夹具读取。
    let fields = if output.contains('\0') {
        output.split('\0').collect::<Vec<_>>()
    } else {
        output.lines().collect::<Vec<_>>()
    };
    for line in fields {
        if line.is_empty() {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut current_lock_reason,
            );
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut current_lock_reason,
            );
            current_path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = branch.to_string();
        } else if let Some(reason) = line.strip_prefix("locked ") {
            current_lock_reason = Some(reason.to_string());
        }
    }
    flush(
        &mut items,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
        &mut current_lock_reason,
    );
    items
}

fn validate_branch_name(
    control: &ScopedWorktreeControl,
    repo: &Path,
    branch: &str,
) -> Result<(), IpcError> {
    if branch.trim().is_empty() {
        return Err(IpcError::Validation("Branch name cannot be empty".into()));
    }
    if branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(IpcError::Validation(
            "Branch name cannot start with '-' or contain whitespace".into(),
        ));
    }
    run_git(
        control.store.as_deref(),
        repo,
        &["check-ref-format", "--branch", branch],
    )
    .map(|_| ())
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

fn validate_worktree_target(
    files: &dyn echo_agent::tools::filesystem::FileSystem,
    repo_root: &Path,
    target: &Path,
) -> Result<(), IpcError> {
    let repo_parent = repo_root
        .parent()
        .ok_or_else(|| IpcError::Validation("Repository root has no parent directory".into()))?
        .to_path_buf();
    let repo_parent = files
        .canonicalize(&repo_parent)
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let target_parent = target
        .parent()
        .ok_or_else(|| IpcError::Validation("Worktree path has no parent directory".into()))?;
    let canonical_parent = files.canonicalize(target_parent).map_err(|error| {
        IpcError::Validation(format!("Cannot resolve worktree parent: {error}"))
    })?;
    if !canonical_parent.starts_with(&repo_parent) {
        return Err(IpcError::Validation(format!(
            "Worktree path must stay under repository parent: {}",
            repo_parent.display()
        )));
    }
    if files
        .exists(target)
        .map_err(|error| IpcError::Validation(error.to_string()))?
    {
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
    workspace_generation: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let worktrees = state
        .app_state
        .session
        .product_data_io
        .run("list project worktrees", move || {
            let repo_root = git_repo_root(&control)?;
            let output = run_git(
                control.store.as_deref(),
                &repo_root,
                &["worktree", "list", "--porcelain", "-z"],
            )?;
            Ok::<Vec<WorktreeInfo>, IpcError>(parse_worktree_list(&output, &repo_root))
        })
        .await
        .map_err(super::product_data::blocking_error)??;
    Ok(json!(worktrees))
}

/// Snapshot Git in the exact original project environment. A commit OID also
/// supports detached HEAD, while unborn/non-Git projects fail before creation.
pub(super) async fn prepare_reply_worktree(
    app_state: &Arc<echo_agent_app_core::api::AppState>,
    workspace_id: &str,
    workspace_generation: &str,
    branch: String,
) -> Result<(String, String, String), IpcError> {
    let control = worktree_control_for_app(app_state, workspace_id, workspace_generation).await?;
    app_state
        .session
        .product_data_io
        .run("prepare reply worktree", move || {
            let root = git_repo_root(&control)?;
            validate_branch_name(&control, &root, &branch)?;
            let head = run_git(
                control.store.as_deref(),
                &root,
                &["rev-parse", "--verify", "HEAD^{commit}"],
            )?;
            let path = default_worktree_path(&root, &branch)?;
            Ok((
                root.to_string_lossy().to_string(),
                head,
                path.to_string_lossy().to_string(),
            ))
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

/// Resource facts survive a successful add followed by a failed list. The
/// native Git lock reason is the operation receipt; no second registry/store.
#[derive(Debug, Clone, Serialize)]
pub(super) struct WorktreeCreation {
    pub(super) created: bool,
    pub(super) observed: bool,
    pub(super) path: String,
    pub(super) head: String,
    pub(super) descriptor: Option<WorktreeInfo>,
    pub(super) error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn create_worktree_in_control(
    control: &ScopedWorktreeControl,
    repo_root: &Path,
    branch: &str,
    base_ref: &str,
    path: Option<String>,
    lock_reason: Option<&str>,
) -> Result<WorktreeCreation, IpcError> {
    validate_branch_name(control, repo_root, branch)?;
    let target = path
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repo_root.join(path)
            }
        })
        .unwrap_or(default_worktree_path(repo_root, branch)?);
    let target_str = target.to_string_lossy().to_string();
    let head = run_git(
        control.store.as_deref(),
        repo_root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )?;
    // The listing is read before every mutation. A matching native receipt is
    // required; a directory/branch with the same name is never adopted.
    if let Some(reason) = lock_reason {
        let output = run_git(
            control.store.as_deref(),
            repo_root,
            &["worktree", "list", "--porcelain", "-z"],
        )?;
        if let Some(existing) = parse_worktree_list(&output, repo_root)
            .into_iter()
            .find(|item| item.path == target_str)
        {
            if existing.branch != branch
                || existing.commit_oid != head
                || existing.lock_reason.as_deref() != Some(reason)
            {
                return Err(IpcError::Validation(
                    "Worktree 已被其它操作占用，保留原目录且不会覆盖".into(),
                ));
            }
            return Ok(WorktreeCreation {
                created: true,
                observed: true,
                path: target_str,
                head,
                descriptor: Some(existing),
                error: None,
            });
        }
    }
    validate_worktree_target(control.files.as_ref(), repo_root, &target)?;
    let mut args = vec!["worktree", "add"];
    if let Some(reason) = lock_reason {
        args.extend(["--lock", "--reason", reason]);
    }
    args.extend(["-b", branch, &target_str, &head]);
    let add = run_git(control.store.as_deref(), repo_root, &args);
    let mut receipt = WorktreeCreation {
        created: add.is_ok(),
        observed: false,
        path: target_str.clone(),
        head,
        descriptor: None,
        error: add.err().map(|error| error.to_string()),
    };
    // Even an unconfirmed Git execution is observed first. Never replay add on
    // an error and never force-remove a user directory to manufacture rollback.
    match run_git(
        control.store.as_deref(),
        repo_root,
        &["worktree", "list", "--porcelain", "-z"],
    ) {
        Ok(output) => {
            if let Some(item) = parse_worktree_list(&output, repo_root)
                .into_iter()
                .find(|item| item.path == target_str)
            {
                let matches = item.branch == branch
                    && item.commit_oid == receipt.head
                    && lock_reason.is_none_or(|reason| item.lock_reason.as_deref() == Some(reason));
                if matches {
                    receipt.created = true;
                    receipt.observed = true;
                    receipt.descriptor = Some(item);
                } else {
                    receipt.error =
                        Some("Git 目录已存在但资源身份不匹配；请从 Worktree 列表检查".into());
                }
            } else if receipt.error.is_none() {
                receipt.error =
                    Some("Git add 已返回，但未能在原 Worktree 列表确认目标；保留创建事实".into());
            }
        }
        Err(error) => {
            receipt.error = Some(match receipt.error {
                Some(primary) => format!("{primary}; Worktree 重读失败: {error}"),
                None => format!("Worktree 已创建，列表重读失败: {error}"),
            })
        }
    }
    Ok(receipt)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn create_worktree_for_app(
    app_state: &Arc<echo_agent_app_core::api::AppState>,
    workspace_id: &str,
    workspace_generation: &str,
    branch: String,
    base: String,
    path: Option<String>,
    lock_reason: Option<String>,
) -> Result<WorktreeCreation, IpcError> {
    let control = worktree_control_for_app(app_state, workspace_id, workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let merge_guard = control.merge_lock(&repo_root).lock_owned().await;
    app_state
        .session
        .product_data_io
        .run("create project worktree", move || {
            let _merge_guard = merge_guard;
            create_worktree_in_control(
                &control,
                &repo_root,
                &branch,
                &base,
                path,
                lock_reason.as_deref(),
            )
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

#[tauri::command]
pub async fn create_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    branch: String,
    base: Option<String>,
    path: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let base = base
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "HEAD".into());
    let receipt = create_worktree_for_app(
        &state.app_state,
        &workspace_id,
        &workspace_generation,
        branch.trim().to_string(),
        base,
        path,
        None,
    )
    .await?;
    if let Some(error) = receipt.error {
        return Err(IpcError::Internal(error));
    }
    receipt
        .descriptor
        .map(|item| json!(item))
        .ok_or_else(|| IpcError::Internal("Created worktree was not found in git output".into()))
}

/// 复用原 Git 删除约束；任何失败都不 force、不重建目录。
fn remove_worktree_in_control(
    control: &ScopedWorktreeControl,
    repo_root: &Path,
    path: &str,
) -> Result<(), IpcError> {
    let target = PathBuf::from(path);
    if target.as_os_str().is_empty() {
        return Err(IpcError::Validation("Worktree path cannot be empty".into()));
    }
    let canonical_repo = control
        .files
        .canonicalize(&repo_root)
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let canonical_target = control
        .files
        .canonicalize(&target)
        .map_err(|error| IpcError::Validation(format!("Cannot resolve worktree path: {error}")))?;
    if canonical_target == canonical_repo {
        return Err(IpcError::Validation(
            "Refusing to remove the primary repository worktree".into(),
        ));
    }
    let output = run_git(
        control.store.as_deref(),
        &repo_root,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    let known = parse_worktree_list(&output, &repo_root)
        .into_iter()
        .find(|worktree| {
            control
                .files
                .canonicalize(&PathBuf::from(&worktree.path))
                .map(|path| path == canonical_target)
                .unwrap_or(false)
        });
    let known = known
        .ok_or_else(|| IpcError::Validation("Path is not a registered git worktree".into()))?;
    let target_str = target.to_string_lossy().to_string();
    let reply_lock = known
        .lock_reason
        .as_deref()
        .filter(|reason| reason.starts_with("eko-reply-fork:"));
    if reply_lock.is_some() {
        // Local data-loss protection still comes from Git's ordinary
        // remove check; no --force is used after unlocking our receipt.
        let dirty = run_git(
            control.store.as_deref(),
            &target,
            &["status", "--porcelain"],
        )?;
        if !dirty.is_empty() {
            return Err(IpcError::Validation(
                "Worktree 含未提交文件，保留原目录与分叉回执".into(),
            ));
        }
        run_git(
            control.store.as_deref(),
            &repo_root,
            &["worktree", "unlock", &target_str],
        )?;
    }
    let removal = run_git(
        control.store.as_deref(),
        repo_root,
        &["worktree", "remove", &target_str],
    );
    if let Err(primary) = removal {
        // 用户可能在 status 后写入。普通 remove 失败时仍保数据，
        // 并重读真实 Git 登记再恢复本请求锁，避免丢失重试身份。
        if let Some(reason) = reply_lock {
            let restored = (|| -> Result<(), IpcError> {
                let listing = run_git(
                    control.store.as_deref(),
                    repo_root,
                    &["worktree", "list", "--porcelain", "-z"],
                )?;
                let present = parse_worktree_list(&listing, repo_root)
                    .into_iter()
                    .find(|item| {
                        item.path == known.path
                            && item.branch == known.branch
                            && item.commit_oid == known.commit_oid
                    })
                    .ok_or_else(|| {
                        IpcError::Validation("删除失败后无法确认原 Worktree 身份；保留资源".into())
                    })?;
                if present.lock_reason.as_deref() == Some(reason) {
                    return Ok(());
                }
                if present.lock_reason.is_some() {
                    return Err(IpcError::Validation(
                        "Worktree 已被其它 owner 锁定，未覆盖".into(),
                    ));
                }
                run_git(
                    control.store.as_deref(),
                    repo_root,
                    &["worktree", "lock", "--reason", reason, &target_str],
                )
                .map(|_| ())
            })();
            if let Err(restore) = restored {
                return Err(IpcError::Internal(format!(
                    "{primary}; 分叉锁回执恢复失败: {restore}"
                )));
            }
        }
        return Err(primary);
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_worktree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    path: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let merge_guard = control.merge_lock(&repo_root).lock_owned().await;
    state
        .app_state
        .session
        .product_data_io
        .run("remove project worktree", move || {
            let _merge_guard = merge_guard;
            remove_worktree_in_control(&control, &repo_root, &path)
        })
        .await
        .map_err(super::product_data::blocking_error)??;
    Ok(json!({"success": true}))
}

#[tauri::command]
pub async fn list_unattended_worktrees(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let store = control.store.clone();

    let unattended =
        run_taskruntime_worktree_operation(store, "list unattended worktrees", move |store| {
            let _scope = control;
            echo_agent_app_core::api::tasks::task_runtime::worktree::list_unattended_worktrees(
                &repo_root, store,
            )
        })
        .await?;

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
    workspace_generation: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let store = control.store.clone();
    if let Some(store) = store.as_ref() {
        let lookup_run_id = run_id.clone();
        let run =
            echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store.clone())
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
    let merge_guard = control.merge_lock(&repo_root).lock_owned().await;
    let run_id_for_merge = run_id.clone();
    let outcome =
        run_taskruntime_worktree_operation(store, "merge unattended worktree", move |store| {
            let _scope = control;
            let _merge_guard = merge_guard;
            echo_agent_app_core::api::tasks::task_runtime::worktree::merge_unattended_worktree(
                &repo_root,
                &run_id_for_merge,
                store,
            )
        })
        .await?;

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
    workspace_generation: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let store = control.store.clone();
    let merge_guard = control.merge_lock(&repo_root).lock_owned().await;
    let run_id_for_discard = run_id.clone();
    run_taskruntime_worktree_operation(store, "discard unattended worktree", move |store| {
        let _scope = control;
        let _merge_guard = merge_guard;
        echo_agent_app_core::api::tasks::task_runtime::worktree::discard_unattended_worktree(
            &repo_root,
            &run_id_for_discard,
            store,
        )
    })
    .await?;

    Ok(json!({"success": true, "discarded": run_id}))
}

#[tauri::command]
pub async fn cleanup_unattended_worktrees(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<serde_json::Value, IpcError> {
    let control =
        worktree_control_for_workspace(&state, &workspace_id, &workspace_generation).await?;
    let (control, repo_root) = git_repo_root_async(control).await?;
    let store = control.store.clone();
    let merge_guard = control.merge_lock(&repo_root).lock_owned().await;
    let result =
        run_taskruntime_worktree_operation(store, "clean unattended worktrees", move |store| {
            let _scope = control;
            let _merge_guard = merge_guard;
            echo_agent_app_core::api::tasks::task_runtime::worktree::cleanup_unattended_worktrees(
                &repo_root, store,
            )
        })
        .await?;

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
        let previous = AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
        let result = commit_auto_memory_toggle_after_validation::<()>(
            Err(IpcError::Validation("workspace was deleted".to_string())),
            !previous,
        );
        assert!(result.is_err());
        assert_eq!(
            AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn project_worktree_commands_use_bound_store_and_original_unattended_algorithm()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::tauri::commands::files::tests::{ProjectGitExecutor, namespaced_files};
        let host = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        let journal = tempfile::tempdir()?;
        let visible = host.path().canonicalize()?;
        let actual = target.path().canonicalize()?;
        let git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
            let output = Command::new("git")
                .args(args)
                .current_dir(&actual)
                .output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(
                    String::from_utf8_lossy(&output.stderr).to_string(),
                )
                .into());
            }
            Ok(())
        };
        git(&["init", "-q"])?;
        std::fs::write(actual.join("notes.txt"), "target initial\n")?;
        std::fs::write(visible.join("notes.txt"), "host not a Git repo\n")?;
        git(&["add", "--", "notes.txt"])?;
        git(&[
            "-c",
            "user.name=EKO Test",
            "-c",
            "user.email=eko@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "target initial",
        ])?;
        let files = Arc::new(namespaced_files(&visible, &actual));
        let store = Arc::new(echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(journal.path().join("tasks"))?);
        store.bind_project_environment(
            &store.active_workspace_id(),
            echo_agent_app_core::api::workspace::ProjectLocation::Local {
                path: visible.clone(),
            },
            1,
            files.clone(),
            Arc::new(ProjectGitExecutor {
                files: files.clone(),
            }),
        )?;
        let control = ScopedWorktreeControl {
            _global: None,
            _project: None,
            repo_root: visible.clone(),
            store: Some(store.clone()),
            files,
        };
        let (control, repo) = git_repo_root_async(control).await?;
        assert_eq!(repo, visible);
        assert!(Arc::ptr_eq(
            &control.merge_lock(&repo),
            &store.project_worktree_lock(&repo)
        ));
        let operation_store = store.clone();
        run_taskruntime_worktree_operation(Some(store), "test target worktree route", move |store| {
            let _scope = control;
            let output = run_git(store, &repo, &["worktree", "list", "--porcelain", "-z"])?;
            let listed = parse_worktree_list(&output, &repo);
            assert!(listed.first().is_some_and(|entry| entry.path == repo.to_string_lossy() && !entry.managed));
            let unattended = echo_agent_app_core::api::tasks::task_runtime::worktree::list_unattended_worktrees(&repo, store).map_err(|error| IpcError::Internal(error.to_string()))?;
            assert!(unattended.is_empty());
            // A wrong local fallback cannot pass: the visible host directory
            // contains no .git, while the actual provider target does.
            assert_eq!(operation_store.project_git_repo_root(&repo).map_err(|error| IpcError::Internal(error.to_string()))?, repo);
            Ok::<(), IpcError>(())
        }).await?;
        assert!(!visible.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(visible.join("notes.txt"))?,
            "host not a Git repo\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn worktree_observer_cancel_keeps_original_lock_until_physical_closure_settles()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let lock =
            echo_agent_app_core::api::tasks::task_runtime::worktree::repo_merge_lock(root.path());
        let guard = lock.clone().lock_owned().await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let observer = tokio::spawn(run_taskruntime_worktree_operation(
            None,
            "test scoped worktree cancellation",
            move |_| {
                let retained_guard = guard;
                let _ = entered_tx.send(());
                release_rx.recv().map_err(|error| error.to_string())?;
                drop(retained_guard);
                let _ = settled_tx.send(());
                Ok::<(), String>(())
            },
        ));
        entered_rx.await?;
        observer.abort();
        assert!(observer.await.is_err());
        assert!(lock.try_lock().is_err());
        release_tx.send(())?;
        settled_rx.await?;
        assert!(lock.try_lock().is_ok());
        Ok(())
    }

    #[test]
    fn worktree_projection_does_not_canonicalize_target_paths_on_host() {
        let target = Path::new("/remote/不存在的项目🙂");
        let output = "worktree /remote/不存在的项目🙂\nHEAD abcdef1234567890\nbranch refs/heads/main\n\nworktree /remote/不存在的项目🙂-review\nHEAD 1234567890abcdef\nbranch refs/heads/review\n\n";
        let listed = parse_worktree_list(output, target);
        assert_eq!(listed.len(), 2);
        assert!(listed.first().is_some_and(|entry| !entry.managed));
        assert!(listed.get(1).is_some_and(|entry| entry.managed && entry.path == "/remote/不存在的项目🙂-review"));
    }

    #[test]
    fn reply_fork_worktree_is_head_only_and_reenters_using_native_receipt() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let repo = temporary.path().join("source");
        std::fs::create_dir(&repo)?;
        run_git(None, &repo, &["init"])?;
        std::fs::write(repo.join("tracked.txt"), "提交原文🙂")?;
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        run_git(None, &repo, &["add", "tracked.txt", ".gitignore"])?;
        run_git(
            None,
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=EKO Fixture",
                "-c",
                "user.email=fixture@eko.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )?;
        let head = run_git(None, &repo, &["rev-parse", "HEAD"])?;
        // Detached source heads are valid commit authorities.
        run_git(None, &repo, &["checkout", "--detach", &head])?;
        std::fs::write(repo.join("tracked.txt"), "未提交🙂")?;
        std::fs::write(repo.join("untracked.txt"), "未跟踪🙂")?;
        std::fs::write(repo.join("ignored.txt"), "忽略🙂")?;
        let before = ["tracked.txt", "untracked.txt", "ignored.txt"]
            .into_iter()
            .map(|name| std::fs::read(repo.join(name)).map(|bytes| (name, bytes)))
            .collect::<std::io::Result<Vec<_>>>()?;
        let control = ScopedWorktreeControl {
            _global: None,
            _project: None,
            repo_root: repo.clone(),
            store: None,
            files: Arc::new(echo_agent::tools::files::LocalFileSystem),
        };
        let target = temporary.path().join("支线🙂\n尾部 ");
        let reason = "eko-reply-fork:fixture-identity";
        let created = create_worktree_in_control(
            &control,
            &repo,
            "eko/fork-fixture",
            &head,
            Some(target.to_string_lossy().to_string()),
            Some(reason),
        )?;
        assert!(created.created && created.observed && created.error.is_none());
        assert_eq!(run_git(None, &target, &["rev-parse", "HEAD"])?, head);
        assert_eq!(
            std::fs::read_to_string(target.join("tracked.txt"))?,
            "提交原文🙂"
        );
        assert!(!target.join("untracked.txt").exists());
        assert!(!target.join("ignored.txt").exists());
        for (name, bytes) in before {
            assert_eq!(std::fs::read(repo.join(name))?, bytes);
        }
        let resumed = create_worktree_in_control(
            &control,
            &repo,
            "eko/fork-fixture",
            &head,
            Some(target.to_string_lossy().to_string()),
            Some(reason),
        )?;
        assert!(resumed.observed && resumed.error.is_none());
        let listing = run_git(None, &repo, &["worktree", "list", "--porcelain", "-z"])?;
        assert_eq!(parse_worktree_list(&listing, &repo).len(), 2);
        assert!(
            create_worktree_in_control(
                &control,
                &repo,
                "eko/fork-fixture",
                &head,
                Some(target.to_string_lossy().to_string()),
                Some("eko-reply-fork:other-request")
            )
            .is_err()
        );
        std::fs::write(target.join("user-later.txt"), "保留后续用户文件")?;
        assert!(
            run_git(
                None,
                &repo,
                &["worktree", "remove", target.to_string_lossy().as_ref()]
            )
            .is_err()
        );
        assert!(target.join("user-later.txt").exists());
        Ok(())
    }

    #[test]
    fn reply_fork_rejects_unborn_and_non_git_without_creating_a_directory() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let repo = temporary.path().join("source");
        std::fs::create_dir(&repo)?;
        let control = ScopedWorktreeControl {
            _global: None,
            _project: None,
            repo_root: repo.clone(),
            store: None,
            files: Arc::new(echo_agent::tools::files::LocalFileSystem),
        };
        assert!(git_repo_root(&control).is_err());
        run_git(None, &repo, &["init"])?;
        let target = temporary.path().join("branch");
        assert!(
            create_worktree_in_control(
                &control,
                &repo,
                "eko/fork-unborn",
                "HEAD",
                Some(target.to_string_lossy().to_string()),
                Some("eko-reply-fork:unborn")
            )
            .is_err()
        );
        assert!(!target.exists());
        Ok(())
    }

    struct ReplyForkFaultExecutor {
        inner: crate::tauri::commands::files::tests::ProjectGitExecutor,
        fail_after_add: std::sync::atomic::AtomicBool,
        pending_list_failure: std::sync::atomic::AtomicBool,
        late_write: std::sync::Mutex<Option<PathBuf>>,
        adds: std::sync::atomic::AtomicUsize,
    }

    impl SandboxExecutor for ReplyForkFaultExecutor {
        fn name(&self) -> &str {
            "reply-fork-native-git-fault-fixture"
        }
        fn isolation_level(&self) -> IsolationLevel {
            IsolationLevel::None
        }
        fn is_available(&self) -> futures::future::BoxFuture<'_, bool> {
            Box::pin(async { true })
        }
        fn execute(
            &self,
            command: SandboxCommand,
        ) -> futures::future::BoxFuture<
            '_,
            echo_agent::error::Result<echo_agent::sandbox::ExecutionResult>,
        > {
            Box::pin(async move {
                use std::sync::atomic::Ordering;
                let echo_agent::sandbox::CommandKind::Program { program, args } = &command.kind
                else {
                    return Err(std::io::Error::other("expected actual native Git program").into());
                };
                if program != "git" {
                    return Err(std::io::Error::other("expected actual Git").into());
                }
                let worktree = args.first().is_some_and(|arg| arg == "worktree");
                let action = args.get(1).map(String::as_str);
                if worktree
                    && action == Some("list")
                    && self.pending_list_failure.swap(false, Ordering::AcqRel)
                {
                    return Err(std::io::Error::other("injected post-add list failure").into());
                }
                if worktree && action == Some("remove") {
                    let path = self
                        .late_write
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    if let Some(path) = path {
                        std::fs::write(path, "保留迟到用户写入🙂")?;
                    }
                }
                let add = worktree && action == Some("add");
                let result = self.inner.execute(command).await?;
                if add && result.exit_code == 0 {
                    self.adds.fetch_add(1, Ordering::AcqRel);
                    if self.fail_after_add.swap(false, Ordering::AcqRel) {
                        self.pending_list_failure.store(true, Ordering::Release);
                    }
                }
                Ok(result)
            })
        }
    }

    // Tokio 的固定测试属性名，不是本项目的角色/调度概念。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reply_fork_native_git_partial_list_and_late_write_preserve_original_resources()
    -> anyhow::Result<()> {
        use crate::tauri::commands::files::tests::{ProjectGitExecutor, namespaced_files};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let temporary = tempfile::tempdir()?;
        let repo = temporary.path().join("source");
        std::fs::create_dir(&repo)?;
        run_git(None, &repo, &["init"])?;
        std::fs::write(repo.join("tracked.txt"), "已提交🙂")?;
        run_git(None, &repo, &["add", "tracked.txt"])?;
        run_git(
            None,
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=EKO Fixture",
                "-c",
                "user.email=fixture@eko.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        )?;
        let repo = repo.canonicalize()?;
        let head = run_git(None, &repo, &["rev-parse", "HEAD"])?;
        let files = Arc::new(namespaced_files(&repo, &repo));
        let executor = Arc::new(ReplyForkFaultExecutor {
            inner: ProjectGitExecutor {
                files: files.clone(),
            },
            fail_after_add: AtomicBool::new(true),
            pending_list_failure: AtomicBool::new(false),
            late_write: std::sync::Mutex::new(None),
            adds: AtomicUsize::new(0),
        });
        let store = Arc::new(echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(temporary.path().join("tasks"))?);
        store.bind_project_environment(
            &store.active_workspace_id(),
            echo_agent_app_core::api::workspace::ProjectLocation::Local { path: repo.clone() },
            1,
            files.clone(),
            executor.clone(),
        )?;
        let control = ScopedWorktreeControl {
            _global: None,
            _project: None,
            repo_root: repo.clone(),
            store: Some(store),
            files,
        };
        let target = temporary.path().canonicalize()?.join("partial");
        let path = target.to_string_lossy().to_string();
        let reason = "eko-reply-fork:partial-native";
        let first = create_worktree_in_control(
            &control,
            &repo,
            "eko/fork-partial",
            &head,
            Some(path.clone()),
            Some(reason),
        )?;
        assert!(first.created && !first.observed && first.error.is_some());
        let retry = create_worktree_in_control(
            &control,
            &repo,
            "eko/fork-partial",
            &head,
            Some(path.clone()),
            Some(reason),
        )?;
        assert!(retry.observed && retry.error.is_none());
        assert_eq!(executor.adds.load(Ordering::Acquire), 1);
        *executor
            .late_write
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(target.join("late-user.txt"));
        assert!(remove_worktree_in_control(&control, &repo, &path).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("late-user.txt"))?,
            "保留迟到用户写入🙂"
        );
        let listing = run_git(
            control.store.as_deref(),
            &repo,
            &["worktree", "list", "--porcelain", "-z"],
        )?;
        let actual = parse_worktree_list(&listing, &repo)
            .into_iter()
            .find(|item| item.path == path)
            .ok_or_else(|| anyhow::anyhow!("actual retained worktree absent"))?;
        assert_eq!(actual.lock_reason.as_deref(), Some(reason));
        Ok(())
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

/// GUI sandbox isolation contract.
///
/// The Tauri commands themselves need a live `tauri::State`, so the isolation
/// decision is factored into `eko_local_sandbox_available` / `eko_require_os_sandbox`
/// / `eko_sandbox_command` and exercised directly against executor doubles.
#[cfg(test)]
mod sandbox_isolation_tests {
    use super::*;
    use echo_agent::error::{ReactError, SandboxError};
    use echo_agent::sandbox::ExecutionResult;
    use futures::future::BoxFuture;

    /// Executor double that separates "claims an isolation level" from "the
    /// backend actually starts", mirroring how the real local backend probes
    /// `sandbox-exec` / `bwrap` instead of trusting configuration.
    struct ProbeSandbox {
        level: IsolationLevel,
        backend_starts: bool,
    }

    impl ProbeSandbox {
        fn new(level: IsolationLevel, backend_starts: bool) -> Self {
            Self {
                level,
                backend_starts,
            }
        }
    }

    impl SandboxExecutor for ProbeSandbox {
        fn name(&self) -> &str {
            "probe"
        }

        fn isolation_level(&self) -> IsolationLevel {
            self.level
        }

        fn is_available(&self) -> BoxFuture<'_, bool> {
            Box::pin(async move { self.backend_starts })
        }

        fn execute(
            &self,
            _command: SandboxCommand,
        ) -> BoxFuture<'_, echo_agent::error::Result<ExecutionResult>> {
            Box::pin(async move {
                Err(ReactError::Sandbox(Box::new(SandboxError::Unavailable(
                    "probe executor must never execute".to_string(),
                ))))
            })
        }
    }

    #[tokio::test]
    async fn process_only_backend_reports_unavailable() {
        // `is_available()` is true: the process runner can spawn commands.
        let manager = ProbeSandbox::new(IsolationLevel::Process, true);
        assert!(
            !eko_local_sandbox_available(&manager).await,
            "a process-only backend must not be advertised as an available sandbox"
        );
    }

    #[tokio::test]
    async fn process_only_backend_rejects_execution_with_the_canonical_message() {
        let manager = ProbeSandbox::new(IsolationLevel::Process, true);
        let message = eko_require_os_sandbox(&manager)
            .await
            .err()
            .map(|error| error.to_string());
        assert_eq!(
            message.as_deref(),
            Some(eko_local_sandbox_unavailable_message()),
            "process-only execution must be rejected with the canonical message, not degraded"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn os_isolated_backend_is_not_misreported() {
        let manager = ProbeSandbox::new(IsolationLevel::OsSandbox, true);
        assert!(eko_local_sandbox_available(&manager).await);
        assert!(eko_require_os_sandbox(&manager).await.is_ok());
        assert_eq!(eko_local_sandbox_backend_label(true), "local");
    }

    #[tokio::test]
    async fn os_isolation_label_without_a_starting_backend_is_unavailable() {
        // Claims OS isolation, but the backend cannot start: capability probing,
        // not the reported level, decides availability.
        let manager = ProbeSandbox::new(IsolationLevel::OsSandbox, false);
        assert!(!eko_local_sandbox_available(&manager).await);
        assert!(eko_require_os_sandbox(&manager).await.is_err());
    }

    #[test]
    fn backend_label_follows_probed_capability() {
        if cfg!(target_os = "windows") {
            assert_eq!(eko_local_sandbox_backend_label(true), "wsl2-required");
            assert_eq!(eko_local_sandbox_backend_label(false), "wsl2-required");
        } else {
            assert_eq!(eko_local_sandbox_backend_label(false), "process-only");
        }
    }

    #[test]
    fn sandbox_commands_carry_the_os_isolation_floor() {
        let shell = eko_sandbox_command(true, "bash", "echo ok".to_string());
        assert_eq!(
            shell.minimum_isolation,
            Some(IsolationLevel::OsSandbox),
            "shell execution must not be allowed to downgrade to process-only"
        );
        let code = eko_sandbox_command(false, "python", "print(1)".to_string());
        assert_eq!(
            code.minimum_isolation,
            Some(IsolationLevel::OsSandbox),
            "code execution must not be allowed to downgrade to process-only"
        );
    }
}
