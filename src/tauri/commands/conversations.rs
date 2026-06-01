//! Tauri IPC commands for conversation persistence.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::memory::{NewConversation, StoredMessage};
use echo_agent_app_core::persistence::{SavedMessage, SavedToolCall};

#[tauri::command]
pub async fn list_conversations(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let filter = echo_agent::memory::ConversationFilter::default();
    let list = store
        .list_conversations(filter)
        .await
        .map_err(|e| IpcError::Internal(format!("list_conversations DB error: {e}")))?;

    tracing::info!(
        "[list_conversations] returning {} conversations",
        list.len()
    );
    serde_json::to_value(&list).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn save_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
    title: String,
    messages: Vec<SavedMessage>,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    tracing::info!(
        "[save_conversation] id={}, title={}, msgs={}",
        id,
        title,
        messages.len()
    );

    // Check if conversation exists
    let existing = store.get_conversation(&id).await.ok().flatten();

    let conversation_id = if let Some(conv) = existing {
        store
            .update_conversation(&conv.conversation_id, Some(&title), None, None)
            .await
            .map_err(|e| IpcError::Internal(e.to_string()))?;
        conv.conversation_id
    } else {
        let new_conv = NewConversation {
            conversation_id: id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some(title),
        };
        let conv = store
            .create_conversation(new_conv)
            .await
            .map_err(|e| IpcError::Internal(e.to_string()))?;
        conv.conversation_id
    };

    // Convert SavedMessage -> StoredMessage
    let stored: Vec<StoredMessage> = messages
        .into_iter()
        .map(|m| StoredMessage {
            id: None,
            conversation_id: conversation_id.clone(),
            role: m.role,
            content: m.content,
            attachments_json: None,
            tool_calls_json: m.tool_calls.and_then(|tc| serde_json::to_string(&tc).ok()),
            tool_result_json: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .collect();

    if !stored.is_empty() {
        store
            .save_messages(&conversation_id, &stored)
            .await
            .map_err(|e| IpcError::Internal(e.to_string()))?;
    }

    Ok(serde_json::json!({
        "success": true,
        "id": conversation_id,
    }))
}

#[tauri::command]
pub async fn get_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    let stored = store
        .get_messages(&conv.conversation_id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    // Convert StoredMessage -> SavedMessage for frontend
    let messages: Vec<SavedMessage> = stored
        .into_iter()
        .map(|m| SavedMessage {
            role: m.role,
            content: m.content,
            tool_calls: m
                .tool_calls_json
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
        .collect();

    Ok(serde_json::json!({
        "id": conv.id,
        "conversation_id": conv.conversation_id,
        "title": conv.title,
        "messages": messages,
        "created_at": conv.created_at,
        "updated_at": conv.updated_at,
    }))
}

#[tauri::command]
pub async fn update_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
    title: Option<String>,
    messages: Option<Vec<SavedMessage>>,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    store
        .update_conversation(&conv.conversation_id, title.as_deref(), None, None)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    if let Some(msgs) = messages {
        let stored: Vec<StoredMessage> = msgs
            .into_iter()
            .map(|m| StoredMessage {
                id: None,
                conversation_id: conv.conversation_id.clone(),
                role: m.role,
                content: m.content,
                attachments_json: None,
                tool_calls_json: m.tool_calls.and_then(|tc| serde_json::to_string(&tc).ok()),
                tool_result_json: None,
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .collect();
        if !stored.is_empty() {
            store
                .save_messages(&conv.conversation_id, &stored)
                .await
                .map_err(|e| IpcError::Internal(e.to_string()))?;
        }
    }

    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn delete_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    store
        .delete_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn export_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    let stored = store
        .get_messages(&conv.conversation_id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    let mut content = format!("# {}\n\n", conv.title.as_deref().unwrap_or("Conversation"));
    for msg in &stored {
        content.push_str(&format!(
            "## {}\n\n{}\n\n",
            msg.role,
            msg.content.as_deref().unwrap_or("")
        ));
    }

    Ok(serde_json::json!({
        "format": "markdown",
        "content": content,
        "id": id,
    }))
}

#[tauri::command]
pub async fn restore_conversation(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    let stored = store
        .get_messages(&conv.conversation_id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    let message_count = stored.len();

    // 将存储的消息转换为 agent Message 并注入到 agent 上下文中
    if !stored.is_empty() {
        use echo_agent::llm::types::{Message, MessageContent};

        let mut messages: Vec<Message> = Vec::new();
        let mut pending_tc_ids: Vec<String> = Vec::new();
        let mut tc_idx: usize = 0;

        for sm in &stored {
            let text = sm.content.clone().unwrap_or_default();
            match sm.role.as_str() {
                "system" => {
                    messages.push(Message::system(text));
                    pending_tc_ids.clear();
                    tc_idx = 0;
                }
                "user" => {
                    messages.push(Message::user(text));
                    pending_tc_ids.clear();
                    tc_idx = 0;
                }
                "assistant" => {
                    if let Some(ref tc_json) = sm.tool_calls_json {
                        if let Ok(tcs) = serde_json::from_str::<
                            Vec<echo_agent_app_core::persistence::SavedToolCall>,
                        >(tc_json)
                        {
                            use echo_agent::llm::types::{FunctionCall, ToolCall};
                            let calls: Vec<ToolCall> = tcs
                                .iter()
                                .map(|tc| ToolCall {
                                    id: tc.id.clone(),
                                    call_type: "function".to_string(),
                                    function: FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect();
                            pending_tc_ids = calls.iter().map(|c| c.id.clone()).collect();
                            tc_idx = 0;
                            let mut msg = Message::assistant_with_tools(calls);
                            if !text.is_empty() {
                                msg.content = MessageContent::Text(text);
                            }
                            messages.push(msg);
                            continue;
                        }
                    }
                    messages.push(Message::assistant(text));
                    pending_tc_ids.clear();
                    tc_idx = 0;
                }
                "tool" => {
                    let tool_id = pending_tc_ids
                        .get(tc_idx)
                        .cloned()
                        .unwrap_or_else(|| format!("restored_unknown_{tc_idx}"));
                    tc_idx += 1;
                    messages.push(Message::tool_result(tool_id, String::new(), text));
                }
                _ => {
                    messages.push(Message::user(text));
                }
            }
        }

        if !messages.is_empty() {
            state
                .app_state
                .connection
                .agent
                .read_async(|a| {
                    Box::pin(async move {
                        a.load_messages(messages).await;
                    })
                })
                .await;
        }
    }

    Ok(serde_json::json!({
        "success": true,
        "message_count": message_count,
        "conversation_id": conv.conversation_id,
    }))
}
