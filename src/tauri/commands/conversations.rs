//! Tauri IPC commands for conversation persistence.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::memory::{NewConversation, StoredMessage};
use echo_agent_app_core::persistence::{AttachmentsPayload, SavedMessage};

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
        .map(|m| {
            // Pack thinking_segments + execution_steps + attachments into
            // attachments_json (backward compatible; the column predates real
            // attachments and historically held thinking segments).
            let has_thinking = m.thinking_segments.is_some();
            let has_steps = m.execution_steps.is_some();
            let has_rounds = m.execution_rounds.is_some();
            let has_attachments = m.attachments.as_ref().is_some_and(|a| !a.is_empty());
            let attachments_json = if has_thinking || has_steps || has_rounds || has_attachments {
                let payload = AttachmentsPayload {
                    thinking_segments: m.thinking_segments.unwrap_or_default(),
                    execution_steps: m.execution_steps.unwrap_or_default(),
                    execution_rounds: m.execution_rounds,
                    attachments: m.attachments.unwrap_or_default(),
                };
                serde_json::to_string(&payload).ok()
            } else {
                None
            };

            StoredMessage {
                id: None,
                conversation_id: conversation_id.clone(),
                role: m.role,
                content: m.content,
                attachments_json,
                tool_calls_json: m.tool_calls.and_then(|tc| serde_json::to_string(&tc).ok()),
                tool_result_json: m.tool_result,
                created_at: echo_agent::utils::time::now_local().to_rfc3339(),
            }
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
        .map(|m| {
            // Parse attachments_json which may contain thinking_segments +
            // execution_steps + real attachments, or legacy plain array format.
            let (thinking_segments, execution_steps, execution_rounds, attachments) = m
                .attachments_json
                .and_then(|s| AttachmentsPayload::parse(&s))
                .map(|p| {
                    let ts = if p.thinking_segments.is_empty() {
                        None
                    } else {
                        Some(p.thinking_segments)
                    };
                    let es = if p.execution_steps.is_empty() {
                        None
                    } else {
                        Some(p.execution_steps)
                    };
                    let att = if p.attachments.is_empty() {
                        None
                    } else {
                        Some(p.attachments)
                    };
                    (ts, es, p.execution_rounds, att)
                })
                .unwrap_or((None, None, None, None));

            SavedMessage {
                role: m.role,
                content: m.content,
                tool_calls: m
                    .tool_calls_json
                    .and_then(|s| serde_json::from_str(&s).ok()),
                thinking_segments,
                tool_result: m.tool_result_json,
                execution_steps,
                execution_rounds,
                attachments,
            }
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
            .map(|m| {
                let has_thinking = m.thinking_segments.is_some();
                let has_steps = m.execution_steps.is_some();
                let has_rounds = m.execution_rounds.is_some();
                let has_attachments = m.attachments.as_ref().is_some_and(|a| !a.is_empty());
                let attachments_json = if has_thinking || has_steps || has_rounds || has_attachments
                {
                    let payload = AttachmentsPayload {
                        thinking_segments: m.thinking_segments.unwrap_or_default(),
                        execution_steps: m.execution_steps.unwrap_or_default(),
                        execution_rounds: m.execution_rounds,
                        attachments: m.attachments.unwrap_or_default(),
                    };
                    serde_json::to_string(&payload).ok()
                } else {
                    None
                };

                StoredMessage {
                    id: None,
                    conversation_id: conv.conversation_id.clone(),
                    role: m.role,
                    content: m.content,
                    attachments_json,
                    tool_calls_json: m.tool_calls.and_then(|tc| serde_json::to_string(&tc).ok()),
                    tool_result_json: m.tool_result,
                    created_at: echo_agent::utils::time::now_local().to_rfc3339(),
                }
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

    if let Err(error) = state
        .app_state
        .storage
        .tool_executions
        .remove_conversation(&id)
    {
        tracing::warn!(conversation_id = %id, %error, "Failed to remove conversation tool execution details");
    }

    let artifact_config = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.tool_output_artifacts())
        .await;
    if let Some(config) = artifact_config {
        let conversation_id = id.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = echo_agent::tools::artifact::cleanup_tool_output_scope(
                &config,
                &conversation_id,
                None,
            ) {
                tracing::warn!(conversation_id = %conversation_id, %error, "Failed to clean conversation tool artifacts");
            }
        });
    }

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
                    if let Some(ref tc_json) = sm.tool_calls_json
                        && let Ok(tcs) = serde_json::from_str::<
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
                    messages.push(Message::assistant(text));
                    pending_tc_ids.clear();
                    tc_idx = 0;
                }
                "tool" => {
                    // Extract tool name and tool_call_id from tool_result_json
                    // Format: {"tool_call_id": "...", "name": "..."}
                    let (tool_id, tool_name) = if let Some(ref tr_json) = sm.tool_result_json {
                        if let Ok(tr) = serde_json::from_str::<serde_json::Value>(tr_json) {
                            let name = tr
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Prefer the stored tool_call_id over positional matching
                            let id = tr
                                .get("tool_call_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| pending_tc_ids.get(tc_idx).cloned())
                                .unwrap_or_else(|| format!("restored_unknown_{tc_idx}"));
                            (id, name)
                        } else {
                            let id = pending_tc_ids
                                .get(tc_idx)
                                .cloned()
                                .unwrap_or_else(|| format!("restored_unknown_{tc_idx}"));
                            (id, String::new())
                        }
                    } else {
                        let id = pending_tc_ids
                            .get(tc_idx)
                            .cloned()
                            .unwrap_or_else(|| format!("restored_unknown_{tc_idx}"));
                        (id, String::new())
                    };
                    tc_idx += 1;
                    messages.push(Message::tool_result(tool_id, tool_name, text));
                }
                _ => {
                    messages.push(Message::user(text));
                }
            }
        }

        if !messages.is_empty() {
            // Route to pool agent for this conversation if pool is active
            let agent = state.app_state.connection.agent_for(&id).await;
            agent
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

#[tauri::command]
pub async fn search_conversations(
    state: tauri::State<'_, TauriState>,
    query: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    if query.trim().is_empty() {
        return Ok(serde_json::json!([]));
    }

    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = store_guard
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    let results = store
        .search_conversations(&query, limit.unwrap_or(20))
        .await
        .map_err(|e| IpcError::Internal(format!("search_conversations error: {e}")))?;

    serde_json::to_value(&results).map_err(|e| IpcError::Internal(e.to_string()))
}
