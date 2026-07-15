//! Tauri IPC commands for raw Store memory management.
//!
//! These commands are a legacy/admin surface for arbitrary Store namespaces.
//! Runtime-recallable agent memories should be written through
//! `MemoryLayerManager::write_memory` via AutoMemory, TriggerDetector, an
//! explicitly accepted review candidate, or the layered `remember` tool.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

#[tauri::command]
pub async fn list_memory(
    state: tauri::State<'_, TauriState>,
    namespace: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    let ns = namespace.unwrap_or_else(|| "default".to_string());
    let lim = limit.unwrap_or(100);
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                match agent.store() {
                    Some(store) => match store.search(&[&ns], "", lim).await {
                        Ok(items) => serde_json::to_value(items).unwrap_or_default(),
                        Err(e) => serde_json::json!({"error": e.to_string()}),
                    },
                    None => serde_json::json!({"error": "Memory store not initialized"}),
                }
            })
        })
        .await;
    Ok(result)
}

#[tauri::command]
pub async fn add_memory(
    state: tauri::State<'_, TauriState>,
    namespace: String,
    key: String,
    value: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                match agent.store() {
                    Some(store) => match store.put(&[&namespace], &key, value).await {
                        Ok(_) => serde_json::json!({
                            "success": true,
                            "key": key,
                            "message": "Memory added successfully",
                        }),
                        Err(e) => serde_json::json!({
                            "success": false,
                            "error": e.to_string(),
                        }),
                    },
                    None => serde_json::json!({
                        "success": false,
                        "error": "Memory store not initialized",
                    }),
                }
            })
        })
        .await;
    Ok(result)
}

#[tauri::command]
pub async fn search_memory(
    state: tauri::State<'_, TauriState>,
    query: String,
    namespace: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let ns = namespace.unwrap_or_else(|| "default".to_string());
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                match agent.store() {
                    Some(store) => match store.search(&[&ns], &query, 10).await {
                        Ok(items) => serde_json::to_value(items).unwrap_or_default(),
                        Err(e) => serde_json::json!({"error": e.to_string()}),
                    },
                    None => serde_json::json!({"error": "Memory store not initialized"}),
                }
            })
        })
        .await;
    Ok(result)
}

#[tauri::command]
pub async fn delete_memory(
    state: tauri::State<'_, TauriState>,
    namespace: String,
    key: String,
) -> Result<serde_json::Value, IpcError> {
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                match agent.store() {
                    Some(store) => match store.delete(&[&namespace], &key).await {
                        Ok(_) => serde_json::json!({
                            "success": true,
                            "message": "Memory deleted successfully",
                        }),
                        Err(e) => serde_json::json!({
                            "success": false,
                            "error": e.to_string(),
                        }),
                    },
                    None => serde_json::json!({
                        "success": false,
                        "error": "Memory store not initialized",
                    }),
                }
            })
        })
        .await;
    Ok(result)
}

#[tauri::command]
pub async fn list_namespaces(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                match agent.store() {
                    Some(store) => match store.list_namespaces(None).await {
                        Ok(namespaces) => serde_json::json!({ "namespaces": namespaces }),
                        Err(e) => serde_json::json!({"error": e.to_string()}),
                    },
                    None => serde_json::json!({"error": "Memory store not initialized"}),
                }
            })
        })
        .await;
    Ok(result)
}
