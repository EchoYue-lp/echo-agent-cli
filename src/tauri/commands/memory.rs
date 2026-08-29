//! Tauri IPC commands for EKO's layered agent memory.
//!
//! All product writes go through `MemoryLayerManager`, matching TUI and CLI.
//! The raw Store remains a framework capability, not a second EKO write path.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::evolution::MemoryLayer;
use echo_agent::evolution::MemoryLayerManager;
use echo_agent::memory::{MemoryFilter, MemoryMeta, MemorySource, MemoryType, TypedMemoryEntry};
use echo_agent_app_core::evolution::ReviewGenerationLease;
use echo_agent_app_core::reflection::ReflectionReceipt;
use std::sync::Arc;

const AGENT_MEMORY_NAMESPACE: &str = "agent/memories";

struct ScopedMemoryControl {
    generation: ReviewGenerationLease,
    layer_manager: Arc<MemoryLayerManager>,
}

async fn memory_control_for_workspace(
    state: &tauri::State<'_, TauriState>,
    workspace_id: &str,
) -> Result<ScopedMemoryControl, IpcError> {
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
            "Layered memory is not configured for workspace '{workspace_id}'"
        ))
    })?;
    let generation = integration
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let layer_manager = generation
        .layer_manager()
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(ScopedMemoryControl {
        generation,
        layer_manager,
    })
}

fn namespace_supported(namespace: Option<&str>) -> bool {
    namespace.is_none_or(|value| value.is_empty() || value == AGENT_MEMORY_NAMESPACE)
}

fn validate_namespace_after_scope<T>(
    control: Result<T, IpcError>,
    namespace: Option<&str>,
) -> Result<(T, bool), IpcError> {
    let control = control?;
    Ok((control, namespace_supported(namespace)))
}

fn memory_content(value: &serde_json::Value) -> Option<String> {
    if let Some(content) = value.as_str() {
        return Some(content.to_string());
    }
    if let Some(content) = value.get("content").and_then(serde_json::Value::as_str) {
        return Some(content.to_string());
    }
    serde_json::to_string(value).ok()
}

fn entry_json(layer: MemoryLayer, entry: TypedMemoryEntry) -> serde_json::Value {
    let layer_name = match layer {
        MemoryLayer::Hot => "hot",
        MemoryLayer::Warm => "warm",
        MemoryLayer::Cold => "cold",
    };
    serde_json::json!({
        "namespace": AGENT_MEMORY_NAMESPACE,
        "key": entry.key,
        "value": entry.content,
        "created_at": entry.raw.created_at,
        "updated_at": entry.raw.updated_at,
        "score": entry.raw.score,
        "layer": layer_name,
        "memory_type": entry.meta.memory_type,
        "source": entry.meta.source,
        "status": entry.meta.status,
    })
}

#[tauri::command]
pub async fn list_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    namespace: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    let (control, supported) = validate_namespace_after_scope(
        memory_control_for_workspace(&state, &workspace_id).await,
        namespace.as_deref(),
    )?;
    if !supported {
        return Ok(serde_json::json!([]));
    }
    let limit = limit.unwrap_or(100);
    let layer_manager = &control.layer_manager;

    let mut entries = layer_manager
        .list_hot()
        .into_iter()
        .map(|entry| entry_json(MemoryLayer::Hot, entry))
        .collect::<Vec<_>>();
    match layer_manager.list_warm(&MemoryFilter::new()).await {
        Ok(warm) => entries.extend(
            warm.into_iter()
                .map(|entry| entry_json(MemoryLayer::Warm, entry)),
        ),
        Err(error) => {
            tracing::warn!(%error, "Failed to list warm memories");
        }
    }
    entries.truncate(limit);
    Ok(serde_json::Value::Array(entries))
}

#[tauri::command]
pub async fn add_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    namespace: String,
    key: String,
    value: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let (control, supported) = validate_namespace_after_scope(
        memory_control_for_workspace(&state, &workspace_id).await,
        Some(&namespace),
    )?;
    if !supported {
        return Ok(serde_json::json!({
            "success": false,
            "error": format!("Unsupported memory namespace: {namespace}"),
        }));
    }
    let Some(content) = memory_content(&value).filter(|text| !text.trim().is_empty()) else {
        return Ok(serde_json::json!({
            "success": false,
            "error": "Memory content cannot be empty",
        }));
    };
    let key = if key.trim().is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        key.trim().to_string()
    };
    let layer_manager = &control.layer_manager;
    let meta = MemoryMeta::new(
        MemoryType::ProjectFact,
        MemorySource::ExplicitSave,
        "explicit",
    );
    match layer_manager.write_memory(&key, content.trim(), meta).await {
        Ok(_) => {
            let projection_settlement = control.generation.settle_hot_memory_projection().await;
            Ok(serde_json::json!({
                "success": true,
                "key": key,
                "message": "Memory added successfully",
                "projection_settlement": projection_settlement,
            }))
        }
        Err(error) => Ok(serde_json::json!({
            "success": false,
            "error": error.to_string(),
        })),
    }
}

#[tauri::command]
pub async fn search_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    query: String,
    namespace: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let (control, supported) = validate_namespace_after_scope(
        memory_control_for_workspace(&state, &workspace_id).await,
        namespace.as_deref(),
    )?;
    if !supported {
        return Ok(serde_json::json!([]));
    }
    let layer_manager = &control.layer_manager;
    match layer_manager.search_layered(query.trim(), 10).await {
        Ok(entries) => Ok(serde_json::Value::Array(
            entries
                .into_iter()
                .map(|(layer, entry)| entry_json(layer, entry))
                .collect(),
        )),
        Err(error) => {
            tracing::warn!(%error, "Failed to search layered memories");
            Ok(serde_json::json!([]))
        }
    }
}

#[tauri::command]
pub async fn delete_memory(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    namespace: String,
    key: String,
) -> Result<serde_json::Value, IpcError> {
    let (control, supported) = validate_namespace_after_scope(
        memory_control_for_workspace(&state, &workspace_id).await,
        Some(&namespace),
    )?;
    if !supported {
        return Ok(serde_json::json!({
            "success": false,
            "error": format!("Unsupported memory namespace: {namespace}"),
        }));
    }
    let layer_manager = &control.layer_manager;
    match layer_manager.delete_memory(key.trim()).await {
        Ok(deleted) => {
            let projection_settlement = if deleted {
                Some(control.generation.settle_hot_memory_projection().await)
            } else {
                None
            };
            Ok(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Memory deleted successfully" } else { "Memory not found" },
                "projection_settlement": projection_settlement,
            }))
        }
        Err(error) => Ok(serde_json::json!({
            "success": false,
            "error": error.to_string(),
        })),
    }
}

#[tauri::command]
pub async fn list_namespaces(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({ "namespaces": [["agent", "memories"]] }))
}

#[tauri::command]
pub async fn reflect_session(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: Option<String>,
) -> Result<ReflectionReceipt, IpcError> {
    if workspace_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "workspace_id must not be empty".to_string(),
        ));
    }
    if conversation_id
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(IpcError::Validation(
            "conversation_id must not be empty when supplied".to_string(),
        ));
    }
    let runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(IpcError::from)?;
    let execution = match conversation_id.as_deref() {
        Some(conversation_id) => Some(
            runtime
                .agent_for(conversation_id)
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?,
        ),
        None => None,
    };
    let agent = execution
        .as_ref()
        .map(echo_agent_app_core::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| runtime.primary_agent());
    echo_agent_app_core::reflection::reflect_session(&runtime, &agent, conversation_id.as_deref())
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_accepts_strings_and_content_objects() {
        assert_eq!(
            memory_content(&serde_json::json!("remember me")),
            Some("remember me".to_string())
        );
        assert_eq!(
            memory_content(&serde_json::json!({"content": "remember me"})),
            Some("remember me".to_string())
        );
    }

    #[test]
    fn only_layered_namespace_is_supported() {
        assert!(namespace_supported(None));
        assert!(namespace_supported(Some(AGENT_MEMORY_NAMESPACE)));
        assert!(!namespace_supported(Some("default")));
    }

    #[test]
    fn invalid_scope_precedes_unsupported_namespace_policy() {
        let validation = Err::<(), _>(IpcError::Validation("workspace is deleted".to_string()));
        assert!(matches!(
            validate_namespace_after_scope(validation, Some("unsupported")),
            Err(IpcError::Validation(message)) if message == "workspace is deleted"
        ));
    }

    #[test]
    fn reflection_receipt_keeps_typed_gui_wire_fields() -> Result<(), String> {
        let value =
            serde_json::to_value(echo_agent_app_core::reflection::reflection_receipt_fixture())
                .map_err(|error| error.to_string())?;
        echo_agent_app_core::reflection::validate_reflection_receipt_wire(&value)
    }
}
