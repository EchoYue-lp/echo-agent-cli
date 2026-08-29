//! Tauri IPC commands for conversation persistence.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent::llm::types::{Message, Role};
use echo_agent::memory::{NewConversation, StoredMessage};
use echo_agent_app_core::api::conversation_projection::{AttachmentsPayload, SavedMessage};
use echo_agent_app_core::api::state::ScopedChatRuntime;
use std::collections::BTreeMap;

async fn scoped_runtime(
    state: &TauriState,
    workspace_id: &str,
) -> Result<ScopedChatRuntime, IpcError> {
    state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))
}

async fn scoped_store(
    state: &TauriState,
    workspace_id: &str,
) -> Result<std::sync::Arc<dyn echo_agent::memory::ConversationStore>, IpcError> {
    scoped_runtime(state, workspace_id)
        .await?
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))
}

fn pack_ui_projection(message: &mut SavedMessage) -> Option<String> {
    let has_display_content = message.content.is_some();
    let has_thinking = message.thinking_segments.is_some();
    let has_steps = message.execution_steps.is_some();
    let has_rounds = message.execution_rounds.is_some();
    let has_attachments = message
        .attachments
        .as_ref()
        .is_some_and(|attachments| !attachments.is_empty());
    let has_message_id = message.message_id.is_some();
    if !(has_message_id
        || has_display_content
        || has_thinking
        || has_steps
        || has_rounds
        || has_attachments)
    {
        return None;
    }
    serde_json::to_string(&AttachmentsPayload {
        message_id: message.message_id.take(),
        display_content: message.content.clone(),
        thinking_segments: message.thinking_segments.take().unwrap_or_default(),
        execution_steps: message.execution_steps.take().unwrap_or_default(),
        execution_rounds: message.execution_rounds.take(),
        attachments: message.attachments.take().unwrap_or_default(),
    })
    .ok()
}

fn is_framework_projection(raw: Option<&str>) -> bool {
    raw.and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .is_some_and(|value| value.get("_echo_message_version").is_some())
}

fn merge_projection_json(canonical: Option<&str>, ui: Option<&str>) -> Option<String> {
    let Some(canonical) = canonical else {
        return ui.map(str::to_string);
    };
    let Ok(mut canonical_value) = serde_json::from_str::<serde_json::Value>(canonical) else {
        return Some(canonical.to_string());
    };
    let Some(canonical_object) = canonical_value.as_object_mut() else {
        return Some(canonical.to_string());
    };
    if let Some(ui) = ui
        && let Ok(ui_value) = serde_json::from_str::<serde_json::Value>(ui)
        && let Some(ui_object) = ui_value.as_object()
    {
        for (key, value) in ui_object {
            canonical_object.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_string(&canonical_value).ok()
}

fn project_saved_messages(
    conversation_id: &str,
    messages: Vec<SavedMessage>,
    existing: &[StoredMessage],
) -> Vec<StoredMessage> {
    let has_canonical_transcript = existing.iter().any(|message| {
        is_framework_projection(message.attachments_json.as_deref())
            || message.tool_calls_json.is_some()
            || message.tool_result_json.is_some()
    });
    if has_canonical_transcript {
        let users: Vec<SavedMessage> = messages
            .iter()
            .filter(|message| message.role == "user")
            .cloned()
            .collect();
        let assistants: Vec<SavedMessage> = messages
            .iter()
            .filter(|message| message.role == "assistant")
            .cloned()
            .collect();
        let user_positions: Vec<usize> = existing
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (message.role == "user").then_some(index))
            .collect();
        let assistant_positions: Vec<usize> = existing
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                (message.role == "assistant" && message.tool_calls_json.is_none()).then_some(index)
            })
            .collect();
        let mut ui_by_position = BTreeMap::new();
        let user_position_skip = user_positions.len().saturating_sub(users.len());
        for (position, message) in user_positions
            .into_iter()
            .skip(user_position_skip)
            .zip(users)
        {
            ui_by_position.insert(position, message);
        }
        let assistant_position_skip = assistant_positions.len().saturating_sub(assistants.len());
        for (position, message) in assistant_positions
            .into_iter()
            .skip(assistant_position_skip)
            .zip(assistants)
        {
            ui_by_position.insert(position, message);
        }
        return existing
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, mut stored)| {
                if let Some(mut ui_message) = ui_by_position.remove(&index) {
                    let ui_projection = pack_ui_projection(&mut ui_message);
                    stored.attachments_json = merge_projection_json(
                        stored.attachments_json.as_deref(),
                        ui_projection.as_deref(),
                    );
                }
                stored
            })
            .collect();
    }

    messages
        .into_iter()
        .map(|mut message| {
            let ui_projection = pack_ui_projection(&mut message);
            StoredMessage {
                id: None,
                conversation_id: conversation_id.to_string(),
                role: message.role,
                content: message.content,
                attachments_json: ui_projection,
                tool_calls_json: message
                    .tool_calls
                    .and_then(|calls| serde_json::to_string(&calls).ok()),
                tool_result_json: message.tool_result,
                created_at: echo_agent::utils::time::now_local().to_rfc3339(),
            }
        })
        .collect()
}

fn branch_prefix(
    stored: &[StoredMessage],
    user_turn_index: usize,
    conversation_id: &str,
) -> Result<Vec<StoredMessage>, IpcError> {
    let mut current_user_index = 0usize;
    let boundary = stored.iter().position(|message| {
        if message.role != "user" {
            return false;
        }
        if current_user_index == user_turn_index {
            return true;
        }
        current_user_index = current_user_index.saturating_add(1);
        false
    });
    let boundary = boundary.ok_or_else(|| {
        IpcError::Validation(format!(
            "user turn index {user_turn_index} is outside the canonical transcript"
        ))
    })?;
    Ok(stored
        .iter()
        .take(boundary)
        .cloned()
        .map(|mut message| {
            message.id = None;
            message.conversation_id = conversation_id.to_string();
            message.attachments_json = strip_branch_ui_references(message.attachments_json);
            message
        })
        .collect())
}

fn strip_branch_ui_references(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Some(raw);
    };
    let Some(object) = value.as_object_mut() else {
        return Some(raw);
    };
    // Tool execution details and message ids are scoped to the source
    // conversation. Canonical framework projection fields remain untouched.
    object.remove("message_id");
    object.remove("execution_steps");
    object.remove("execution_rounds");
    serde_json::to_string(&value).ok().or(Some(raw))
}

async fn load_agent_transcript(
    runtime: &ScopedChatRuntime,
    conversation_id: &str,
    stored: &[StoredMessage],
) -> Result<usize, IpcError> {
    let agent_execution = runtime
        .agent_for(conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent = agent_execution.agent();
    let mut messages = echo_agent::memory::restore_messages(stored)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let system_prompt = agent.read(|agent| agent.system_prompt().to_string()).await;
    if !messages
        .first()
        .is_some_and(|message| message.role == Role::System)
    {
        messages.insert(0, Message::system(system_prompt));
    }
    let message_count = messages.len().saturating_sub(1);
    agent
        .read_async(|agent| Box::pin(async move { agent.load_messages(messages).await }))
        .await;
    Ok(message_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::llm::types::{ContentPart, Message, MessageContent};

    fn saved_message(id: &str, role: &str, content: &str) -> SavedMessage {
        SavedMessage {
            message_id: Some(id.to_string()),
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            thinking_segments: None,
            tool_result: None,
            execution_steps: None,
            execution_rounds: None,
            attachments: None,
        }
    }

    #[test]
    fn branch_prefix_keeps_canonical_tool_history_and_rekeys_messages() -> anyhow::Result<()> {
        let stored = [
            ("user", "first"),
            ("assistant", "tool call"),
            ("tool", "result"),
            ("assistant", "answer"),
            ("user", "second"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (role, content))| StoredMessage {
            id: i64::try_from(index).ok(),
            conversation_id: "source".to_string(),
            role: role.to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: (index == 1).then(|| "[]".to_string()),
            tool_result_json: (role == "tool").then(|| "{}".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .collect::<Vec<_>>();

        let prefix = branch_prefix(&stored, 1, "branch")?;
        assert_eq!(prefix.len(), 4);
        assert!(prefix.iter().any(|message| message.role == "tool"));
        assert!(
            prefix
                .iter()
                .all(|message| { message.id.is_none() && message.conversation_id == "branch" })
        );
        Ok(())
    }

    #[test]
    fn branch_prefix_removes_source_scoped_ui_references() -> anyhow::Result<()> {
        let stored = [StoredMessage {
            id: Some(1),
            conversation_id: "source".to_string(),
            role: "assistant".to_string(),
            content: Some("answer".to_string()),
            attachments_json: Some(
                serde_json::json!({
                    "_echo_message_version": 1,
                    "content": {"Text": "canonical"},
                    "message_id": "source-message",
                    "execution_steps": [{"type": "tool", "call_id": "call-1"}],
                    "execution_rounds": [{"tool_call_ids": ["call-1"]}],
                    "thinking_segments": ["kept"]
                })
                .to_string(),
            ),
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        let raw = strip_branch_ui_references(
            stored
                .first()
                .and_then(|message| message.attachments_json.clone()),
        )
        .ok_or_else(|| anyhow::anyhow!("branch projection missing"))?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(
            value.get("_echo_message_version"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            value.get("thinking_segments"),
            Some(&serde_json::json!(["kept"]))
        );
        assert!(value.get("message_id").is_none());
        assert!(value.get("execution_steps").is_none());
        assert!(value.get("execution_rounds").is_none());
        Ok(())
    }

    #[test]
    fn ui_metadata_merges_without_overwriting_canonical_transcript() -> anyhow::Result<()> {
        let canonical_user = echo_agent::memory::project_message(
            "conv",
            &Message::user_multimodal(vec![ContentPart::Text {
                text: "artifact path: /tmp/user-input/paste.txt".to_string(),
            }]),
        )?;
        let tool_call_assistant = StoredMessage {
            id: Some(2),
            conversation_id: "conv".to_string(),
            role: "assistant".to_string(),
            content: None,
            attachments_json: None,
            tool_calls_json: Some("[]".to_string()),
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let tool_result = StoredMessage {
            id: Some(3),
            conversation_id: "conv".to_string(),
            role: "tool".to_string(),
            content: Some("matched lines".to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: Some(
                serde_json::json!({"tool_call_id": "call-1", "name": "grep"}).to_string(),
            ),
            created_at: "2026-01-01T00:00:01Z".to_string(),
        };
        let final_assistant = StoredMessage {
            id: Some(4),
            conversation_id: "conv".to_string(),
            role: "assistant".to_string(),
            content: Some("root cause".to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:02Z".to_string(),
        };
        let existing = vec![
            canonical_user,
            tool_call_assistant,
            tool_result,
            final_assistant,
        ];

        let projected = project_saved_messages(
            "conv",
            vec![
                saved_message("ui-user", "user", "raw pasted body"),
                saved_message("ui-assistant", "assistant", "root cause"),
            ],
            &existing,
        );

        assert_eq!(projected.len(), existing.len());
        let Some(user) = projected.iter().find(|message| message.role == "user") else {
            anyhow::bail!("missing canonical user message");
        };
        assert_eq!(
            user.content.as_deref(),
            Some("artifact path: /tmp/user-input/paste.txt")
        );
        assert!(is_framework_projection(user.attachments_json.as_deref()));
        assert!(
            user.attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("ui-user"))
        );
        assert!(
            projected
                .iter()
                .any(|message| message.role == "tool" && message.tool_result_json.is_some())
        );
        let Some(final_message) = projected
            .iter()
            .find(|message| message.role == "assistant" && message.tool_calls_json.is_none())
        else {
            anyhow::bail!("missing final assistant message");
        };
        assert!(
            final_message
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("ui-assistant"))
        );

        let restored = echo_agent::memory::restore_messages(&projected)?;
        let Some(restored_user) = restored.first() else {
            anyhow::bail!("missing restored user message");
        };
        assert!(matches!(restored_user.content, MessageContent::Parts(_)));
        Ok(())
    }

    #[test]
    fn ui_messages_are_saved_before_a_canonical_transcript_exists() -> anyhow::Result<()> {
        let projected =
            project_saved_messages("conv", vec![saved_message("ui-user", "user", "hello")], &[]);
        let Some(message) = projected.first() else {
            anyhow::bail!("expected projected UI message");
        };
        assert_eq!(message.content.as_deref(), Some("hello"));
        assert!(
            message
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("ui-user"))
        );
        Ok(())
    }

    #[test]
    fn ui_projection_alignment_handles_trimmed_prefix_and_pending_suffix() -> anyhow::Result<()> {
        let canonical_user = |content: &str| {
            echo_agent::memory::project_message(
                "conv",
                &Message::user_multimodal(vec![ContentPart::Text {
                    text: content.to_string(),
                }]),
            )
        };
        let canonical_assistant = |content: &str| StoredMessage {
            id: None,
            conversation_id: "conv".to_string(),
            role: "assistant".to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let existing = vec![
            canonical_user("old canonical user")?,
            canonical_assistant("old canonical assistant"),
            canonical_user("tail canonical user")?,
            canonical_assistant("tail canonical assistant"),
        ];

        let trimmed = project_saved_messages(
            "conv",
            vec![
                saved_message("tail-user", "user", "tail visible user"),
                saved_message("tail-assistant", "assistant", "tail visible assistant"),
            ],
            &existing,
        );
        let mut trimmed_users = trimmed.iter().filter(|message| message.role == "user");
        let Some(old_user) = trimmed_users.next() else {
            anyhow::bail!("missing old user");
        };
        let Some(tail_user) = trimmed_users.next() else {
            anyhow::bail!("missing tail user");
        };
        assert!(
            !old_user
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("tail-user"))
        );
        assert!(
            tail_user
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("tail-user"))
        );

        let prior_turn: Vec<StoredMessage> = existing.iter().take(2).cloned().collect();
        let pending_suffix = project_saved_messages(
            "conv",
            vec![
                saved_message("old-user", "user", "old visible user"),
                saved_message("pending-user", "user", "not canonical yet"),
                saved_message("old-assistant", "assistant", "old visible assistant"),
                saved_message("pending-assistant", "assistant", ""),
            ],
            &prior_turn,
        );
        let Some(saved_user) = pending_suffix.iter().find(|message| message.role == "user") else {
            anyhow::bail!("missing saved user");
        };
        assert!(
            saved_user
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("old-user"))
        );
        assert!(
            !saved_user
                .attachments_json
                .as_deref()
                .is_some_and(|json| json.contains("pending-user"))
        );
        Ok(())
    }
}

#[tauri::command]
pub async fn list_conversations(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;

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
    workspace_id: String,
    id: String,
    title: String,
    messages: Vec<SavedMessage>,
) -> Result<serde_json::Value, IpcError> {
    let runtime = scoped_runtime(&state, &workspace_id).await?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;

    tracing::info!(
        "[save_conversation] id={}, title={}, msgs={}",
        id,
        title,
        messages.len()
    );

    // Check if conversation exists
    let existing = store.get_conversation(&id).await.ok().flatten();

    let is_new = existing.is_none();
    if !is_new && !messages.is_empty() {
        return Err(IpcError::Validation(
            "conversation transcripts are owned by the Agent runtime".to_string(),
        ));
    }
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
        let conv = runtime
            .ensure_conversation(new_conv)
            .await
            .map_err(|e| IpcError::Internal(e.to_string()))?;
        conv.conversation_id
    };

    // Bootstrap messages are accepted only while creating a new conversation.
    // Once it exists, the Agent runtime is the sole transcript writer; a GUI
    // autosave must never replace a canonical tool/multimodal transcript.
    if is_new && !messages.is_empty() {
        let stored = project_saved_messages(&conversation_id, messages, &[]);
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
    workspace_id: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;

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
            let (
                message_id,
                display_content,
                thinking_segments,
                execution_steps,
                execution_rounds,
                attachments,
            ) = m
                .attachments_json
                .as_deref()
                .and_then(AttachmentsPayload::parse)
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
                    (
                        p.message_id,
                        p.display_content,
                        ts,
                        es,
                        p.execution_rounds,
                        att,
                    )
                })
                .unwrap_or((None, None, None, None, None, None));

            SavedMessage {
                message_id,
                role: m.role,
                content: display_content.or(m.content),
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
    workspace_id: String,
    id: String,
    title: Option<String>,
    messages: Option<Vec<SavedMessage>>,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    if messages.is_some() {
        return Err(IpcError::Validation(
            "conversation transcripts are owned by the Agent runtime".to_string(),
        ));
    }

    store
        .update_conversation(&conv.conversation_id, title.as_deref(), None, None)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    Ok(serde_json::json!({"success": true}))
}

/// Create an immutable branch immediately before one persisted user turn.
///
/// Edit/regenerate callers resend that user turn against the returned
/// conversation. The source transcript remains untouched and the new pooled
/// Agent receives the exact canonical prefix, including prior tool messages.
#[tauri::command]
pub async fn branch_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
    user_turn_index: usize,
) -> Result<serde_json::Value, IpcError> {
    if !state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(&workspace_id, &id)
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .is_empty()
    {
        return Err(IpcError::Validation(
            "cannot branch a conversation while its turn is active".to_string(),
        ));
    }
    let runtime = scoped_runtime(&state, &workspace_id).await?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;
    let source = store
        .get_conversation(&id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{id}' not found")))?;
    let stored = store
        .get_messages(&source.conversation_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let target = stored
        .iter()
        .filter(|message| message.role == "user")
        .nth(user_turn_index)
        .ok_or_else(|| {
            IpcError::Validation(format!(
                "user turn index {user_turn_index} is outside the canonical transcript"
            ))
        })?;
    let target_content = target
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse)
        .and_then(|payload| payload.display_content)
        .or_else(|| target.content.clone())
        .ok_or_else(|| {
            IpcError::Validation(format!(
                "user turn index {user_turn_index} has no text content"
            ))
        })?;
    let branch_id = format!("conv-{}", uuid::Uuid::new_v4());
    let prefix = branch_prefix(&stored, user_turn_index, &branch_id)?;
    let title = source
        .title
        .as_deref()
        .map(|title| format!("{title} (branch)"))
        .unwrap_or_else(|| "Conversation branch".to_string());
    runtime
        .ensure_conversation(NewConversation {
            conversation_id: branch_id.clone(),
            user_id: source.user_id,
            agent_type: source.agent_type,
            title: Some(title),
        })
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    if let Err(error) = store.save_messages(&branch_id, &prefix).await {
        if let Err(cleanup_error) = state
            .app_state
            .delete_conversation_scoped(&workspace_id, &branch_id)
            .await
        {
            tracing::warn!(conversation_id = %branch_id, %cleanup_error, "Failed to roll back incomplete conversation branch");
        }
        return Err(IpcError::Internal(error.to_string()));
    }
    if let Err(error) = load_agent_transcript(&runtime, &branch_id, &prefix).await {
        if let Err(cleanup_error) = state
            .app_state
            .delete_conversation_scoped(&workspace_id, &branch_id)
            .await
        {
            tracing::warn!(conversation_id = %branch_id, %cleanup_error, "Failed to roll back unusable conversation branch");
        }
        return Err(error);
    }

    Ok(serde_json::json!({
        "success": true,
        "id": branch_id,
        "source_id": id,
        "message_count": prefix.len(),
        "target_content": target_content,
    }))
}

#[tauri::command]
pub async fn delete_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let receipt = state
        .app_state
        .delete_conversation_scoped(&workspace_id, &id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(serde_json::json!({
        "success": true,
        "conversation_id": receipt.conversation_id,
        "resumed": receipt.resumed,
        "cleanup_pending": receipt.cleanup_pending,
    }))
}

#[tauri::command]
pub async fn export_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;

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
    workspace_id: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let runtime = scoped_runtime(&state, &workspace_id).await?;
    let store = runtime
        .conversation_store()
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

    let active = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(&workspace_id, &id)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let (message_count, readiness) = if active.is_empty() {
        (
            load_agent_transcript(&runtime, &id, &stored).await?,
            "ready",
        )
    } else {
        (stored.len(), "active")
    };

    Ok(serde_json::json!({
        "success": true,
        "message_count": message_count,
        "conversation_id": conv.conversation_id,
        "workspace_id": workspace_id,
        "readiness": readiness,
        "active_turn": active.first(),
    }))
}

#[tauri::command]
pub async fn search_conversations(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    if query.trim().is_empty() {
        return Ok(serde_json::json!([]));
    }

    let store = scoped_store(&state, &workspace_id).await?;

    let results = store
        .search_conversations(&query, limit.unwrap_or(20))
        .await
        .map_err(|e| IpcError::Internal(format!("search_conversations error: {e}")))?;

    serde_json::to_value(&results).map_err(|e| IpcError::Internal(e.to_string()))
}
