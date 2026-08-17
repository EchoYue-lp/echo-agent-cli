//! Tauri IPC commands for EKO's layered agent memory.
//!
//! All product writes go through `MemoryLayerManager`, matching TUI and CLI.
//! The raw Store remains a framework capability, not a second EKO write path.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::evolution::MemoryLayer;
use echo_agent::memory::{MemoryFilter, MemoryMeta, MemorySource, MemoryType, TypedMemoryEntry};

const AGENT_MEMORY_NAMESPACE: &str = "agent/memories";

fn namespace_supported(namespace: Option<&str>) -> bool {
    namespace.is_none_or(|value| value.is_empty() || value == AGENT_MEMORY_NAMESPACE)
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
    namespace: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    if !namespace_supported(namespace.as_deref()) {
        return Ok(serde_json::json!([]));
    }
    let limit = limit.unwrap_or(100);
    let layer_manager = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.memory_layer_manager().cloned())
        .await;
    let Some(layer_manager) = layer_manager else {
        return Ok(serde_json::json!([]));
    };

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
    namespace: String,
    key: String,
    value: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    if !namespace_supported(Some(&namespace)) {
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
    let integration = state
        .app_state
        .review_integration
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Layered memory is not configured".into()))?;
    let memory_lease = integration
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let layer_manager = std::sync::Arc::new(
        memory_lease
            .create_layer_manager()
            .map_err(|error| IpcError::Internal(error.to_string()))?,
    );
    let meta = MemoryMeta::new(
        MemoryType::ProjectFact,
        MemorySource::ExplicitSave,
        "explicit",
    );
    match layer_manager.write_memory(&key, content.trim(), meta).await {
        Ok(promotion) => {
            if promotion.is_some() {
                let agent = state.app_state.connection.primary_agent();
                let root = agent.read(|value| value.working_dir()).await;
                agent
                    .write_async(|value| {
                        Box::pin(async move {
                            echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                value,
                                root.as_deref(),
                            )
                            .await;
                        })
                    })
                    .await;
                if let Some(pool) = &state.app_state.connection.pool {
                    pool.refresh_hot_memory_context().await;
                }
            }
            Ok(serde_json::json!({
                "success": true,
                "key": key,
                "message": "Memory added successfully",
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
    query: String,
    namespace: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if !namespace_supported(namespace.as_deref()) {
        return Ok(serde_json::json!([]));
    }
    let layer_manager = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.memory_layer_manager().cloned())
        .await;
    let Some(layer_manager) = layer_manager else {
        return Ok(serde_json::json!([]));
    };
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
    namespace: String,
    key: String,
) -> Result<serde_json::Value, IpcError> {
    if !namespace_supported(Some(&namespace)) {
        return Ok(serde_json::json!({
            "success": false,
            "error": format!("Unsupported memory namespace: {namespace}"),
        }));
    }
    let integration = state
        .app_state
        .review_integration
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Layered memory is not configured".into()))?;
    let memory_lease = integration
        .lease_generation()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let layer_manager = std::sync::Arc::new(
        memory_lease
            .create_layer_manager()
            .map_err(|error| IpcError::Internal(error.to_string()))?,
    );
    let layer = layer_manager
        .locate(key.trim())
        .await
        .map(|(layer, _)| layer);
    match layer_manager.delete_memory(key.trim()).await {
        Ok(deleted) => {
            if deleted && layer == Some(MemoryLayer::Hot) {
                let agent = state.app_state.connection.primary_agent();
                let root = agent.read(|value| value.working_dir()).await;
                agent
                    .write_async(|value| {
                        Box::pin(async move {
                            echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                value,
                                root.as_deref(),
                            )
                            .await;
                        })
                    })
                    .await;
                if let Some(pool) = &state.app_state.connection.pool {
                    pool.refresh_hot_memory_context().await;
                }
            }
            Ok(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Memory deleted successfully" } else { "Memory not found" },
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
}
