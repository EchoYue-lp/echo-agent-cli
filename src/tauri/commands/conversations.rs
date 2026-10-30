//! Tauri IPC commands for conversation persistence.

#[cfg(test)]
#[path = "conversations_native_history_tests.rs"]
mod native_history_tests;

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::memory::{NewConversation, StoredMessage};
use echo_agent_app_core::api::chat_driver::ChatDriverEvent;
use echo_agent_app_core::api::chat_event_log::ChatEventEnvelope;
use echo_agent_app_core::api::conversation_projection::{
    AttachmentsPayload, NativeHistoryImport, SavedMessage,
};
use echo_agent_app_core::api::state::ScopedChatRuntime;
#[cfg(test)]
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationCreateRequest {
    Primary {
        id: String,
        title: String,
    },
    Side {
        parent_conversation_id: String,
        request_id: String,
        prompt: String,
        title: Option<String>,
        model_id: Option<String>,
    },
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationUpdateRequest {
    Rename { title: String },
    SideModel { model_id: Option<String> },
    SideRetry,
    SideViewed,
}

async fn known_side_first_turn(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
    message_key: &str,
) -> Result<Option<serde_json::Value>, IpcError> {
    let active = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(workspace_id, conversation_id)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    if active
        .iter()
        .any(|snapshot| snapshot.root_turn_id == message_key)
    {
        return Ok(Some(serde_json::json!({
            "kind": "started",
            "message_key": message_key,
            "root_turn_id": message_key,
        })));
    }
    let replay = state
        .app_state
        .storage
        .chat_events
        .replay(workspace_id, Some(conversation_id), message_key, 0)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let terminal = replay
        .events
        .iter()
        .filter(|event| event.root_turn_id == message_key)
        .filter_map(|event| match &event.payload {
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus { status }
                if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
            {
                Some((event.sequence, status.as_str()))
            }
            _ => None,
        })
        .max_by_key(|(sequence, _)| *sequence);
    Ok(terminal.map(|(_, status)| {
        serde_json::json!({
            "kind": status,
            "message_key": message_key,
            "root_turn_id": message_key,
        })
    }))
}

fn normalize_side_first_turn(
    value: &serde_json::Value,
    message_key: &str,
) -> Result<serde_json::Value, IpcError> {
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .filter(|kind| {
            matches!(
                *kind,
                "started" | "queued" | "completed" | "failed" | "cancelled"
            )
        })
        .ok_or_else(|| {
            IpcError::Internal("Side Conversation first turn returned an invalid state".to_string())
        })?;
    Ok(serde_json::json!({
        "kind": kind,
        "message_key": message_key,
        "root_turn_id": value
            .get("root_turn_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(message_key),
    }))
}

async fn launch_side_conversation_gui(
    state: &TauriState,
    app: tauri::AppHandle,
    mut preparation: echo_agent_app_core::api::side_conversation::SideConversationLaunchPreparation,
) -> Result<serde_json::Value, IpcError> {
    let workspace_id = preparation.creation.entry.workspace_id.clone();
    let conversation_id = preparation.creation.entry.conversation_id.clone();
    let message_key = format!("side-start:{}", preparation.creation.entry.group_id);
    let initial_prompt = preparation.prompt.clone();
    let launch = async {
        if let Some(existing) =
            known_side_first_turn(state, &workspace_id, &conversation_id, &message_key).await?
        {
            return Ok(existing);
        }
        let result = super::chat::send_chat_message_inner_with_side_admission(
            state,
            app,
            super::chat::SendChatMessageRequest::side_initial(
                workspace_id.clone(),
                conversation_id.clone(),
                message_key.clone(),
                preparation.prompt.clone(),
            ),
            Some(preparation.admission),
        )
        .await?;
        normalize_side_first_turn(&result, &message_key)
    }
    .await;

    match launch {
        Ok(first_turn) => {
            match state
                .app_state
                .set_side_conversation_launch_error(&workspace_id, &conversation_id, None)
                .await
            {
                Ok(entry) => preparation.creation.entry = entry,
                Err(error) => {
                    return Ok(serde_json::json!({
                        "creation": preparation.creation,
                        "first_turn": first_turn,
                        "initial_prompt": initial_prompt,
                        "launch_error": format!("first turn started, but launch status persistence failed: {error}"),
                    }));
                }
            }
            Ok(serde_json::json!({
                "creation": preparation.creation,
                "first_turn": first_turn,
                "initial_prompt": initial_prompt,
                "launch_error": null,
            }))
        }
        Err(error) => {
            let detail = error.to_string();
            let persisted = state
                .app_state
                .set_side_conversation_launch_error(
                    &workspace_id,
                    &conversation_id,
                    Some(detail.clone()),
                )
                .await;
            match persisted {
                Ok(entry) => preparation.creation.entry = entry,
                Err(persist_error) => {
                    return Ok(serde_json::json!({
                        "creation": preparation.creation,
                        "first_turn": null,
                        "initial_prompt": initial_prompt,
                        "launch_error": format!("{detail}; launch status persistence failed: {persist_error}"),
                    }));
                }
            }
            Ok(serde_json::json!({
                "creation": preparation.creation,
                "first_turn": null,
                "initial_prompt": initial_prompt,
                "launch_error": detail,
            }))
        }
    }
}

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

#[cfg(test)]
fn pack_ui_projection(message: &mut SavedMessage) -> Option<String> {
    let has_display_content = message.content.is_some();
    let has_steps = message.execution_steps.is_some();
    let has_attachments = message
        .attachments
        .as_ref()
        .is_some_and(|attachments| !attachments.is_empty());
    let has_message_id = message.message_id.is_some();
    if !(has_message_id || has_display_content || has_steps || has_attachments) {
        return None;
    }
    serde_json::to_string(&AttachmentsPayload {
        message_id: message.message_id.take(),
        display_content: message.content.clone(),
        execution_steps: message.execution_steps.take().unwrap_or_default(),
        native_history_import: None,
        attachments: message.attachments.take().unwrap_or_default(),
    })
    .ok()
}

#[cfg(test)]
fn is_framework_projection(raw: Option<&str>) -> bool {
    raw.and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .is_some_and(|value| value.get("_echo_message_version").is_some())
}

#[cfg(test)]
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

#[cfg(test)]
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
    include_turn: bool,
) -> Result<Vec<StoredMessage>, IpcError> {
    let human_positions = human_message_positions(stored)?;
    let boundary = human_positions.get(user_turn_index).copied();
    let boundary = boundary.ok_or_else(|| {
        IpcError::Validation(format!(
            "user turn index {user_turn_index} is outside the canonical transcript"
        ))
    })?;
    let boundary = if include_turn {
        // A visible human ordinal may contain Agent guidance. Copy that context losslessly;
        // only the next human prompt starts another GUI ordinal.
        let end = human_positions
            .iter()
            .copied()
            .find(|index| *index > boundary)
            .unwrap_or(stored.len());
        // A fork includes a complete reply and its tool results, never a half-written turn.
        let final_reply = stored.get(end.saturating_sub(1)).filter(|message| {
            message.role == "assistant"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| !content.trim().is_empty())
                && message.tool_calls_json.as_deref().is_none_or(|calls| {
                    serde_json::from_str::<Vec<serde_json::Value>>(calls)
                        .is_ok_and(|calls| calls.is_empty())
                })
        });
        if final_reply.is_none() {
            return Err(IpcError::Validation(
                "cannot fork a turn without a committed final reply".to_string(),
            ));
        }
        end
    } else {
        boundary
    };
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
    serde_json::to_string(&value).ok().or(Some(raw))
}

fn stored_agent_input(message: &StoredMessage) -> Result<bool, IpcError> {
    let canonical = echo_agent::memory::restore_message(message)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    echo_agent_app_core::api::prepared_turn::persisted_agent_input(&canonical)
        .map(|input| input.is_some())
        .map_err(|error| IpcError::Internal(error.to_string()))
}

fn human_message_positions(stored: &[StoredMessage]) -> Result<Vec<usize>, IpcError> {
    let mut result = Vec::new();
    for (index, message) in stored.iter().enumerate() {
        if message.role == "user" && !stored_agent_input(message)? {
            result.push(index);
        }
    }
    Ok(result)
}

fn retain_public_messages(
    stored: &mut Vec<StoredMessage>,
    internal_ids: &std::collections::HashSet<i64>,
) -> Result<(), IpcError> {
    // Same exact provenance policy for GUI, fork and branch. Corruption never becomes human input.
    retain_gui_messages(stored, internal_ids)
}

fn retain_gui_messages(
    stored: &mut Vec<StoredMessage>,
    internal_ids: &std::collections::HashSet<i64>,
) -> Result<(), IpcError> {
    let mut visible = Vec::with_capacity(stored.len());
    for message in stored.drain(..) {
        let canonical = echo_agent::memory::restore_message(&message)
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        let communication =
            echo_agent_app_core::api::prepared_turn::persisted_agent_input(&canonical)
                .map_err(|error| IpcError::Internal(error.to_string()))?;
        if communication.is_some()
            || (!message.id.is_some_and(|id| internal_ids.contains(&id))
                && !echo_agent_app_core::api::side_conversation::is_internal_agent_message(
                    message.attachments_json.as_deref(),
                ))
        {
            visible.push(message);
        }
    }
    *stored = visible;
    Ok(())
}

async fn load_agent_transcript(
    runtime: &ScopedChatRuntime,
    foreground_turns: &echo_agent_app_core::api::foreground_turn::ForegroundTurnControl,
    conversation_id: &str,
) -> Result<usize, IpcError> {
    let _admission = runtime
        .begin_managed_replacement(foreground_turns, conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;
    let runtime_state = runtime
        .runtime_state_store()
        .ok_or_else(|| IpcError::Internal("Runtime state store not available".to_string()))?;
    let agent_execution = runtime
        .agent_for(conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent = agent_execution.agent();
    echo_agent_app_core::api::managed_conversation::resume_or_import(
        store.as_ref(),
        runtime_state.as_ref(),
        &agent,
        conversation_id,
    )
    .await
    .map_err(|error| IpcError::Internal(error.to_string()))
}

fn project_stored_message(message: StoredMessage) -> Result<SavedMessage, IpcError> {
    project_stored_message_with_publications(message, None, None, &[])
}

fn project_stored_message_with_publications(
    message: StoredMessage,
    workspace_id: Option<&str>,
    conversation_id: Option<&str>,
    publications: &[ChatEventEnvelope],
) -> Result<SavedMessage, IpcError> {
    // Reuse canonical parsing, including structured call identity and malformed
    // metadata errors. Never turn a present but invalid call into no call.
    let restored = echo_agent::memory::restore_message(&message).map_err(|_| {
        IpcError::Internal("无法加载工具历史记录；原始对话记录已保留，请重试".to_string())
    })?;
    let resource_display =
        echo_agent_app_core::api::prepared_turn::resource_history_display_text(&message)
            .map_err(|error| IpcError::Internal(error.to_string()))?;
    let attachment_payload = message
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse);
    let native_publication = if let Some(import) = attachment_payload
        .as_ref()
        .and_then(|payload| payload.native_history_import.as_ref())
    {
        verify_native_history_import(&restored, import)?;
        Some(
            echo_agent_app_core::api::conversation_projection::project_native_history_publication(
                &restored,
                &import.source_workspace_id,
                &import.source_conversation_id,
                &import.events,
            )
            .map_err(IpcError::Internal)?,
        )
    } else if let (Some(workspace_id), Some(conversation_id)) = (workspace_id, conversation_id)
        && (restored.assistant_blocks.is_some()
            || restored.approved_assistant_blocks.is_some()
            || restored.execution_status.is_some())
    {
        Some(
            echo_agent_app_core::api::conversation_projection::project_native_history_publication(
                &restored,
                workspace_id,
                conversation_id,
                publications,
            )
            .map_err(IpcError::Internal)?,
        )
    } else {
        None
    };
    let agent_input = echo_agent_app_core::api::prepared_turn::persisted_agent_input(&restored)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let stored_native_blocks = restored
        .assistant_blocks
        .filter(|blocks| !blocks.is_empty())
        .map(echo_agent_app_core::api::conversation_projection::project_assistant_blocks)
        .transpose()
        .map_err(IpcError::Internal)?;
    let native_blocks = native_publication
        .as_ref()
        .and_then(Option::as_ref)
        .map(|view| view.blocks.clone())
        .or(stored_native_blocks);
    let tool_calls = restored.tool_calls;
    let internal_agent = agent_input.is_some()
        || echo_agent_app_core::api::side_conversation::is_internal_agent_message(
            message.attachments_json.as_deref(),
        );
    let (message_id, display_content, mut execution_steps, attachments) = attachment_payload
        .map(|payload| {
            let execution_steps =
                (!payload.execution_steps.is_empty()).then_some(payload.execution_steps);
            let attachments = (!payload.attachments.is_empty()).then_some(payload.attachments);
            (
                payload.message_id,
                payload.display_content,
                execution_steps,
                attachments,
            )
        })
        .unwrap_or((None, None, None, None));

    if let Some(blocks) = native_blocks.as_ref() {
        let mut steps: Vec<echo_agent_app_core::api::conversation_projection::SavedExecutionStep> =
            blocks
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    echo_agent_app_core::api::conversation_projection::SavedExecutionStep {
                        step_type: "assistant".to_string(),
                        index: Some(index),
                        call_id: None,
                    }
                })
                .collect();
        steps.extend(execution_steps.take().unwrap_or_default());
        execution_steps = Some(steps);
    }
    // 原生块已拥有正文，不能再次把聚合content显示为另一份最终回答。
    // 最终发布资格等待原Framework耐久结算身份，不从provider phase或文本猜测。
    let (content, final_answer_confirmed) = if let Some(Some(view)) = native_publication.as_ref() {
        (view.content.clone(), view.final_answer_confirmed)
    } else if native_blocks.is_some() {
        (None, false)
    } else {
        (
            display_content.or(resource_display).or(message.content),
            false,
        )
    };
    Ok(SavedMessage {
        message_id,
        role: message.role,
        created_at: message.created_at,
        content: agent_input
            .as_ref()
            .map(|input| input.text().to_string())
            .or(content),
        agent_input,
        assistant_blocks: native_blocks,
        final_answer_confirmed,
        execution_status: native_publication
            .as_ref()
            .and_then(Option::as_ref)
            .and_then(|view| view.execution_status.clone()),
        final_answer_source: native_publication
            .as_ref()
            .and_then(Option::as_ref)
            .and_then(|view| view.final_answer_source.clone()),
        internal_agent,
        tool_calls,
        tool_result: message.tool_result_json,
        execution_steps,
        attachments,
    })
}

// 原一次GUI请求内按消息准备事实；每个读取async自动续完固定原head，不让用户重试Pending。
async fn project_conversation_messages_with_native_history(
    stored: Vec<StoredMessage>,
    side_initial_identity: Option<(usize, String, String)>,
    internal_message_ids: &std::collections::HashSet<i64>,
    workspace_id: &str,
    conversation_id: &str,
    log: &std::sync::Arc<echo_agent_app_core::api::chat_event_log::ChatEventLog>,
    cancel: &echo_agent::agent::CancellationToken,
) -> Result<Vec<SavedMessage>, IpcError> {
    let mut publications = Vec::with_capacity(stored.len());
    for message in &stored {
        if cancel.is_cancelled() {
            return Err(IpcError::Validation("会话历史读取已取消".into()));
        }
        let imported = message
            .attachments_json
            .as_deref()
            .and_then(AttachmentsPayload::parse)
            .is_some_and(|payload| payload.native_history_import.is_some());
        let events = if imported {
            Vec::new()
        } else {
            let canonical = echo_agent::memory::restore_message(message)
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            log.native_history_publications_async(workspace_id, conversation_id, &canonical, cancel)
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?
        };
        publications.push(events);
    }
    if cancel.is_cancelled() {
        return Err(IpcError::Validation("会话历史读取已取消".into()));
    }
    project_conversation_messages(
        stored,
        side_initial_identity,
        internal_message_ids,
        Some((workspace_id, conversation_id, &publications)),
    )
}

fn project_conversation_messages(
    stored: Vec<StoredMessage>,
    side_initial_identity: Option<(usize, String, String)>,
    internal_message_ids: &std::collections::HashSet<i64>,
    publication_context: Option<(&str, &str, &[Vec<ChatEventEnvelope>])>,
) -> Result<Vec<SavedMessage>, IpcError> {
    let mut side_initial_identity = side_initial_identity;
    stored
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let internal_agent = message
                .id
                .is_some_and(|id| internal_message_ids.contains(&id));
            let is_initial_side_user = side_initial_identity.as_ref().is_some_and(
                |(snapshot_count, _, initial_prompt)| {
                    index >= *snapshot_count
                        && message.role == "user"
                        && message.content.as_deref() == Some(initial_prompt.as_str())
                        && !internal_agent
                        && !echo_agent_app_core::api::side_conversation::is_internal_agent_message(
                            message.attachments_json.as_deref(),
                        )
                },
            );
            let mut projected = match publication_context {
                Some((workspace_id, conversation_id, by_message)) => {
                    let publications = by_message.get(index).ok_or_else(|| {
                        IpcError::Internal("历史publication页与原消息索引不一致".into())
                    })?;
                    project_stored_message_with_publications(
                        message,
                        Some(workspace_id),
                        Some(conversation_id),
                        publications,
                    )?
                }
                None => project_stored_message(message)?,
            };
            projected.internal_agent |= internal_agent;
            if is_initial_side_user && let Some((_, message_id, _)) = side_initial_identity.take() {
                projected.message_id = Some(message_id);
            }
            Ok(projected)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_fixture(
        id: Option<i64>,
        conversation_id: &str,
        message: Message,
        created_at: &str,
    ) -> anyhow::Result<StoredMessage> {
        let mut stored = echo_agent::memory::project_message(conversation_id, &message)?;
        stored.id = id;
        stored.created_at = created_at.to_string();
        Ok(stored)
    }

    fn fixture_tool_call(name: &str) -> echo_agent::llm::types::ToolCall {
        echo_agent::llm::types::ToolCall {
            provider_source: None,
            id: "call-1".to_string(),
            call_type: "function".to_string(),
            function: echo_agent::llm::types::FunctionCall {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    use echo_agent::llm::types::{ContentPart, Message, MessageContent};

    fn saved_message(id: &str, role: &str, content: &str) -> SavedMessage {
        SavedMessage {
            message_id: Some(id.to_string()),
            role: role.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            content: Some(content.to_string()),
            assistant_blocks: None,
            final_answer_confirmed: false,
            execution_status: None,
            final_answer_source: None,
            internal_agent: false,
            agent_input: None,
            tool_calls: None,
            tool_result: None,
            execution_steps: None,
            attachments: None,
        }
    }

    #[test]
    fn canonical_native_blocks_reach_gui_without_final_inference() -> anyhow::Result<()> {
        let mut source = Message::assistant("聚合正文只在原块内显示".to_string());
        source.assistant_blocks = Some(vec![echo_agent::agent::AssistantMessageBlock {
            provider_source: None,
            block_id: "provider-message".to_string(),
            request_id: "request".to_string(),
            sequence: 4,
            phase: echo_agent::agent::AssistantPhase::Commentary,
            delta: "先检查，再验证。".to_string(),
            complete: true,
            provider: Some("fixture".to_string()),
            reasoning: None,
        }]);
        let stored = echo_agent::memory::project_message("conversation", &source)?;
        let projected = project_stored_message(stored)?;
        assert!(projected.content.is_none());
        let first = projected
            .assistant_blocks
            .as_ref()
            .and_then(|blocks| blocks.first())
            .ok_or_else(|| anyhow::anyhow!("canonical native block was lost"))?;
        assert_eq!(first.content, "先检查，再验证。");
        assert_eq!(first.block.request_id, "request");
        assert_eq!(first.block.sequence, 4);
        assert_eq!(
            first.block.phase,
            echo_agent::agent::AssistantPhase::Commentary
        );
        assert_eq!(
            projected
                .execution_steps
                .as_ref()
                .and_then(|steps| steps.first())
                .map(|step| step.step_type.as_str()),
            Some("assistant")
        );
        Ok(())
    }

    #[test]
    fn conversation_create_request_uses_discriminated_primary_and_side_variants()
    -> anyhow::Result<()> {
        let primary: ConversationCreateRequest = serde_json::from_value(serde_json::json!({
            "kind": "primary",
            "id": "conversation-1",
            "title": "Primary"
        }))?;
        assert!(
            matches!(primary, ConversationCreateRequest::Primary { id, .. } if id == "conversation-1")
        );

        let side: ConversationCreateRequest = serde_json::from_value(serde_json::json!({
            "kind": "side",
            "parent_conversation_id": "conversation-1",
            "request_id": "request-1",
            "prompt": "Investigate",
            "title": null,
            "model_id": "provider:model"
        }))?;
        assert!(matches!(
            side,
            ConversationCreateRequest::Side {
                parent_conversation_id,
                model_id: Some(model_id),
                ..
            } if parent_conversation_id == "conversation-1" && model_id == "provider:model"
        ));
        let retry: ConversationUpdateRequest =
            serde_json::from_value(serde_json::json!({"kind": "side_retry"}))?;
        assert!(matches!(retry, ConversationUpdateRequest::SideRetry));
        let viewed: ConversationUpdateRequest =
            serde_json::from_value(serde_json::json!({"kind": "side_viewed"}))?;
        assert!(matches!(viewed, ConversationUpdateRequest::SideViewed));
        Ok(())
    }

    #[test]
    fn side_first_turn_receipt_is_stable_for_started_and_queued_results() -> anyhow::Result<()> {
        assert_eq!(
            normalize_side_first_turn(
                &serde_json::json!({"kind": "queued", "input_id": "input-1"}),
                "side-start:group-1",
            )?,
            serde_json::json!({
                "kind": "queued",
                "message_key": "side-start:group-1",
                "root_turn_id": "side-start:group-1",
            })
        );
        assert!(
            normalize_side_first_turn(
                &serde_json::json!({"kind": "interrupt_prompt"}),
                "side-start:group-1",
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn canonical_nested_tool_calls_reach_the_gui_without_loss() -> anyhow::Result<()> {
        use echo_agent::llm::types::{FunctionCall, ToolCall};
        let calls = vec![ToolCall {
            provider_source: None,
            id: "call-canonical".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: "{\"path\":\"中文🧭.txt\"}".to_string(),
            },
        }];
        let stored = canonical_fixture(
            Some(1),
            "history",
            Message::assistant_with_tools(calls.clone()),
            "2026-10-27T00:00:00Z",
        )?;
        let projected = project_stored_message(stored)?;
        let value = serde_json::to_value(projected)?;
        assert_eq!(value.get("tool_calls"), Some(&serde_json::to_value(calls)?));
        Ok(())
    }

    #[test]
    fn malformed_tool_history_is_an_explicit_error_and_is_not_rewritten() -> anyhow::Result<()> {
        let mut stored = canonical_fixture(
            Some(1),
            "history",
            Message::assistant_with_tools(vec![fixture_tool_call("lookup")]),
            "2026-10-27T00:00:00Z",
        )?;
        stored.tool_calls_json = Some("[{invalid-json]".to_string());
        let original = stored.clone();
        assert!(project_stored_message(stored.clone()).is_err());
        assert_eq!(stored, original);
        Ok(())
    }

    #[test]
    fn stored_internal_agent_marker_projects_to_the_gui_contract() -> anyhow::Result<()> {
        use echo_agent_app_core::api::{
            agent_router::{AgentAddress, AgentMessage},
            prepared_turn::PreparedUserTurn,
            workspace::WorkspaceId,
        };
        let root = tempfile::tempdir()?;
        let input = AgentMessage::agent_text(
            Some(AgentAddress::new(
                WorkspaceId::from_raw("workspace".into()),
                "source",
            )),
            AgentAddress::new(WorkspaceId::from_raw("workspace".into()), "side-1"),
            "internal instruction",
        );
        let mut turn = PreparedUserTurn::runtime_instruction("internal instruction");
        turn.bind_agent_input(&input, root.path(), "side-1", "turn")?;
        let stored = canonical_fixture(
            Some(1),
            "side-1",
            turn.to_message()?,
            "2026-01-01T00:00:00Z",
        )?;
        let projected = project_stored_message(stored.clone())?;
        assert!(projected.internal_agent);
        assert_eq!(projected.content.as_deref(), Some("internal instruction"));
        assert_eq!(
            serde_json::to_value(&projected.agent_input)?,
            serde_json::to_value(Some(&input))?
        );
        assert_eq!(
            serde_json::to_value(projected)?
                .get("internal_agent")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        // The retired display marker alone cannot produce canonical Agent identity.
        let mut bare_marker = stored;
        bare_marker.attachments_json =
            Some(serde_json::json!({"eko_visibility":"internal_agent"}).to_string());
        let original = bare_marker.clone();
        assert!(project_stored_message(bare_marker.clone()).is_err());
        assert_eq!(bare_marker, original);
        Ok(())
    }

    #[test]
    fn exact_message_ids_do_not_relabel_identical_user_text() -> anyhow::Result<()> {
        let messages = [
            ("user", "internal instruction"),
            ("assistant", "internal result"),
            ("user", "internal instruction"),
            ("assistant", "later response"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (role, content))| {
            canonical_fixture(
                i64::try_from(index).ok().and_then(|id| id.checked_add(1)),
                "side-1",
                if role == "user" {
                    Message::user(content.to_string())
                } else {
                    Message::assistant(content.to_string())
                },
                "2026-01-01T00:00:00Z",
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
        let ids = std::collections::HashSet::from([1_i64, 2_i64]);
        let projected = project_conversation_messages(messages, None, &ids, None)?;
        assert_eq!(
            projected
                .iter()
                .map(|message| message.internal_agent)
                .collect::<Vec<_>>(),
            vec![true, true, false, false]
        );
        Ok(())
    }

    #[test]
    fn completed_side_first_prompt_recovers_its_stable_gui_identity() -> anyhow::Result<()> {
        let stored = vec![
            canonical_fixture(
                Some(1),
                "side-1",
                Message::user("inherited context".to_string()),
                "2026-01-01T00:00:00Z",
            )?,
            canonical_fixture(
                Some(2),
                "side-1",
                Message::user("initial side prompt".to_string()),
                "2026-01-01T00:01:00Z",
            )?,
            canonical_fixture(
                Some(3),
                "side-1",
                Message::assistant("finished".to_string()),
                "2026-01-01T00:02:00Z",
            )?,
        ];

        let projected = project_conversation_messages(
            stored,
            Some((
                1,
                "side-start:group-1".to_string(),
                "initial side prompt".to_string(),
            )),
            &std::collections::HashSet::new(),
            None,
        )?;

        assert_eq!(
            projected
                .get(1)
                .and_then(|message| message.message_id.as_deref()),
            Some("side-start:group-1")
        );
        assert!(
            projected
                .first()
                .is_some_and(|message| message.message_id.is_none())
        );
        Ok(())
    }

    fn fork_publication_fixture(
        status: echo_agent::agent::ExecutionStatusKind,
        product_status: &str,
    ) -> anyhow::Result<(
        echo_agent::llm::types::Message,
        Vec<echo_agent_app_core::api::chat_event_log::ChatEventEnvelope>,
    )> {
        let temporary = tempfile::tempdir()?;
        let log = echo_agent_app_core::api::chat_event_log::ChatEventLog::open(
            temporary.path(),
            echo_agent_app_core::api::chat_event_log::ChatEventRetention::default(),
        )?;
        let block = echo_agent::agent::AssistantMessageBlock {
            provider_source: None,
            block_id: "fork-final-block".to_string(),
            request_id: "fork-request".to_string(),
            sequence: 1,
            phase: echo_agent::agent::AssistantPhase::FinalAnswer,
            delta: "confirmed".to_string(),
            complete: true,
            provider: Some("fixture".to_string()),
            reasoning: None,
        };
        let mut message = echo_agent::llm::types::Message::assistant("confirmed".to_string());
        message.assistant_blocks = Some(vec![block.clone()]);
        message.approved_assistant_blocks = Some(vec![block]);
        message.execution_status = Some(serde_json::from_value(serde_json::json!({
            "execution_id": "fork-execution",
            "request_id": "fork-request",
            "sequence": 2,
            "phase": "Terminal",
            "status": status,
            "reason": null,
        }))?);
        let identity = echo_agent::agent::EventIdentity::for_chat(
            Some("fork-conversation".to_string()),
            "fork-root",
            "fork-root",
            Some("fork-run".to_string()),
        )?
        .with_execution_id("fork-execution")?;
        let append = |sequence, payload| -> anyhow::Result<()> {
            let envelope =
                echo_agent::agent::EventEnvelope::new(&identity, sequence, None, payload)?;
            log.append(
                "fork-workspace",
                Some("fork-conversation"),
                "fork-root",
                echo_agent_app_core::api::chat_driver::ChatDriverEvent::Agent(Box::new(envelope)),
            )?;
            Ok(())
        };
        append(
            1,
            echo_agent::agent::AgentEvent::AssistantMessage {
                block: message
                    .assistant_blocks
                    .as_ref()
                    .and_then(|blocks| blocks.first())
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("fixture block missing"))?,
            },
        )?;
        append(
            2,
            echo_agent::agent::AgentEvent::FinalAnswer("confirmed".to_string()),
        )?;
        append(
            3,
            echo_agent::agent::AgentEvent::ExecutionStatus(
                message
                    .execution_status
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("fixture status missing"))?,
            ),
        )?;
        log.append(
            "fork-workspace",
            Some("fork-conversation"),
            "fork-root",
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                status: product_status.to_string(),
            },
        )?;
        let events = log
            .replay("fork-workspace", Some("fork-conversation"), "fork-root", 0)?
            .events;
        Ok((message, events))
    }

    #[test]
    fn direct_fork_publication_gate_rejects_unconfirmed_native_replies() -> anyhow::Result<()> {
        for status in [
            echo_agent::agent::ExecutionStatusKind::Candidate,
            echo_agent::agent::ExecutionStatusKind::Failed,
            echo_agent::agent::ExecutionStatusKind::Cancelled,
        ] {
            let (message, events) = fork_publication_fixture(status.clone(), "completed")?;
            assert!(
                validated_reply_fork_publication_events(
                    &message,
                    "fork-workspace",
                    "fork-conversation",
                    &events,
                )
                .is_err(),
                "unconfirmed status {status:?} must reject both prepare and execute"
            );
        }
        let (message, events) =
            fork_publication_fixture(echo_agent::agent::ExecutionStatusKind::Settled, "cancelled")?;
        assert!(
            validated_reply_fork_publication_events(
                &message,
                "fork-workspace",
                "fork-conversation",
                &events,
            )
            .is_err()
        );
        let (message, events) =
            fork_publication_fixture(echo_agent::agent::ExecutionStatusKind::Settled, "completed")?;
        assert!(
            !validated_reply_fork_publication_events(
                &message,
                "fork-workspace",
                "fork-conversation",
                &events,
            )?
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn branch_prefix_keeps_canonical_tool_history_and_rekeys_messages() -> anyhow::Result<()> {
        let mut tool_call = Message::assistant("tool call".to_string());
        tool_call.tool_calls = Some(vec![fixture_tool_call("lookup")]);
        let stored = [
            Message::user("first".to_string()),
            tool_call,
            Message::tool_result(
                "call-1".to_string(),
                "lookup".to_string(),
                "result".to_string(),
            ),
            Message::assistant("answer".to_string()),
            Message::user("second".to_string()),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            canonical_fixture(
                i64::try_from(index).ok(),
                "source",
                message,
                "2026-01-01T00:00:00Z",
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

        let prefix = branch_prefix(&stored, 1, "branch", false)?;
        assert_eq!(prefix.len(), 4);
        assert!(prefix.iter().any(|message| message.role == "tool"));
        let restored = echo_agent::memory::restore_messages(&prefix)?;
        assert_eq!(
            serde_json::to_value(
                restored
                    .get(1)
                    .and_then(|message| message.tool_calls.as_ref())
            )?,
            serde_json::to_value(Some(vec![fixture_tool_call("lookup")]))?
        );
        assert_eq!(
            restored
                .get(2)
                .and_then(|message| message.tool_call_id.as_deref()),
            Some("call-1")
        );
        assert!(
            prefix
                .iter()
                .all(|message| { message.id.is_none() && message.conversation_id == "branch" })
        );
        let fork = branch_prefix(&stored, 0, "fork", true)?;
        assert_eq!(fork, branch_prefix(&stored, 1, "fork", false)?);
        assert_eq!(
            fork.last().and_then(|message| message.content.as_deref()),
            Some("answer")
        );
        assert!(branch_prefix(&stored, 1, "incomplete", true).is_err());
        assert!(branch_prefix(&stored, 2, "invalid", true).is_err());
        Ok(())
    }

    #[test]
    fn fork_prefix_excludes_internal_turns_and_future_replies() -> anyhow::Result<()> {
        let mut stored = [
            ("user", "same prompt"),
            ("assistant", "first answer"),
            ("user", "internal request"),
            ("assistant", "internal result"),
            ("user", "same prompt"),
            ("assistant", "selected answer"),
            ("user", "later prompt"),
            ("assistant", "later answer"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (role, content))| {
            canonical_fixture(
                i64::try_from(index).ok(),
                "source",
                if role == "user" {
                    Message::user(content.to_string())
                } else {
                    Message::assistant(content.to_string())
                },
                "2026-01-01T00:00:00Z",
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
        retain_public_messages(&mut stored, &std::collections::HashSet::from([2, 3]))?;
        let fork = branch_prefix(&stored, 1, "fork", true)?;
        assert_eq!(fork.len(), 4);
        assert_eq!(
            fork.last().and_then(|message| message.content.as_deref()),
            Some("selected answer")
        );
        assert!(
            fork.iter()
                .all(|message| message.id.is_none() && message.conversation_id == "fork")
        );
        assert!(!fork.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("internal") || content.contains("later"))
        }));
        Ok(())
    }

    #[test]
    fn branch_prefix_replaces_ui_identity_and_keeps_current_canonical_steps() -> anyhow::Result<()>
    {
        let original = echo_agent::llm::types::Message::assistant_with_tools(vec![
            echo_agent::llm::types::ToolCall {
                provider_source: None,
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: echo_agent::llm::types::FunctionCall {
                    name: "lookup".to_string(),
                    arguments: "{}".to_string(),
                },
            },
        ]);
        let mut stored = echo_agent::memory::project_message("source", &original)?;
        let mut value: serde_json::Value = serde_json::from_str(
            stored
                .attachments_json
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("canonical payload missing"))?,
        )?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("canonical object missing"))?;
        object.insert("message_id".into(), serde_json::json!("source-message"));
        object.insert(
            "execution_steps".into(),
            serde_json::json!([{"type":"tool","call_id":"call-1"}]),
        );
        stored.attachments_json = strip_branch_ui_references(Some(serde_json::to_string(&value)?));
        let value: serde_json::Value = serde_json::from_str(
            stored
                .attachments_json
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("branch payload missing"))?,
        )?;
        assert!(value.get("message_id").is_none());
        assert_eq!(
            value.get("execution_steps"),
            Some(&serde_json::json!([{"type":"tool","call_id":"call-1"}]))
        );
        assert_eq!(
            serde_json::to_value(echo_agent::memory::restore_message(&stored)?)?,
            serde_json::to_value(original)?
        );
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
        let tool_call_assistant = canonical_fixture(
            Some(2),
            "conv",
            Message::assistant_with_tools(vec![fixture_tool_call("grep")]),
            "2026-01-01T00:00:00Z",
        )?;
        let tool_result = canonical_fixture(
            Some(3),
            "conv",
            Message::tool_result(
                "call-1".to_string(),
                "grep".to_string(),
                "matched lines".to_string(),
            ),
            "2026-01-01T00:00:01Z",
        )?;
        let final_assistant = canonical_fixture(
            Some(4),
            "conv",
            Message::assistant("root cause".to_string()),
            "2026-01-01T00:00:02Z",
        )?;
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
    // 接入 conversations.rs 原测试模块：测试真实原flow与真实叶子I/O permit，未运行Cargo前不称通过。
    #[tokio::test]
    async fn reply_fork_eight_compositions_keep_leaf_permits_and_survive_observer_drop()
    -> anyhow::Result<()> {
        use echo_agent_app_core::api::product_data_io::ProductDataIoService;
        let io = ProductDataIoService::new();
        let arrived = Arc::new(tokio::sync::Barrier::new(9));
        let (completed_tx, mut completed_rx) = tokio::sync::mpsc::channel(8);
        let mut observers = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..8 {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            releases.push(release_tx);
            let owner_io = io.clone();
            let nested_io = io.clone();
            let entered = arrived.clone();
            let completed = completed_tx.clone();
            observers.push(tokio::spawn(async move {
                run_reply_fork_composition_owned(owner_io, async move {
                    entered.wait().await;
                    release_rx
                        .await
                        .map_err(|error| IpcError::Internal(error.to_string()))?;
                    let actual = nested_io
                        .run("reply fork actual leaf fixture", || 7)
                        .await
                        .map_err(crate::tauri::commands::product_data::blocking_error)?;
                    let _ = completed.send(actual).await;
                    Ok(serde_json::json!({ "success": true }))
                })
                .await
            }));
        }
        // 仅作死锁保护，不改变产品等待/结算期限。
        tokio::time::timeout(std::time::Duration::from_secs(30), arrived.wait()).await?;
        for observer in observers {
            observer.abort();
        }
        for release in releases {
            let _ = release.send(());
        }
        let completed = async {
            for _ in 0..8 {
                if completed_rx.recv().await != Some(7) {
                    anyhow::bail!("original detached composition lost its leaf work");
                }
            }
            io.begin_shutdown().map_err(anyhow::Error::msg)?;
            io.join_shutdown().await.map_err(anyhow::Error::msg)
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), completed).await??;
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
    let archived_ids = state
        .app_state
        .archived_conversation_ids(&workspace_id)
        .map_err(IpcError::Internal)?;
    let archived_ids: std::collections::HashSet<&str> =
        archived_ids.iter().map(String::as_str).collect();
    let mut side_conversations = state
        .app_state
        .list_side_conversations_scoped(&workspace_id)
        .await
        .map_err(IpcError::Internal)?
        .into_iter()
        .map(|entry| (entry.conversation_id.clone(), entry))
        .collect::<std::collections::HashMap<_, _>>();
    let mut list = list
        .into_iter()
        .map(|item| {
            let archived = archived_ids.contains(item.conversation_id.as_str());
            let side_conversation = side_conversations.remove(&item.conversation_id);
            serde_json::json!({
                "id": item.id,
                "conversation_id": item.conversation_id,
                "title": item.title,
                "message_count": item.message_count,
                "created_at": item.created_at,
                "updated_at": item.updated_at,
                "archived": archived,
                "side_conversation": side_conversation,
            })
        })
        .collect::<Vec<_>>();
    list.extend(side_conversations.into_values().map(|entry| {
        serde_json::json!({
            "id": -1,
            "conversation_id": entry.conversation_id,
            "title": entry.title,
            "message_count": 0,
            "created_at": entry.created_at,
            "updated_at": entry.updated_at,
            "archived": false,
            "side_conversation": entry,
        })
    }));
    serde_json::to_value(list).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn create_conversation(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    workspace_id: String,
    request: ConversationCreateRequest,
) -> Result<serde_json::Value, IpcError> {
    match request {
        ConversationCreateRequest::Primary { id, title } => {
            let runtime = scoped_runtime(&state, &workspace_id).await?;
            let conversation = runtime
                .ensure_conversation(NewConversation {
                    conversation_id: id,
                    user_id: "default".to_string(),
                    agent_type: None,
                    title: Some(title),
                })
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            Ok(serde_json::json!({
                "kind": "primary",
                "id": conversation.conversation_id,
            }))
        }
        ConversationCreateRequest::Side {
            parent_conversation_id,
            request_id,
            prompt,
            title,
            model_id,
        } => {
            let preparation = state
                .app_state
                .create_side_conversation_owned(
                    echo_agent_app_core::api::side_conversation::SideConversationCreateRequest {
                        workspace_id,
                        parent_conversation_id,
                        request_id,
                        prompt,
                        title,
                        model_id,
                    },
                )
                .await
                .map_err(IpcError::Validation)?;
            launch_side_conversation_gui(state.inner(), app, preparation).await
        }
    }
}

/// Set the EKO visibility state for one exact workspace conversation.
#[tauri::command]
pub async fn set_conversation_archived(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
    archived: bool,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;
    store
        .get_conversation(&id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{id}' not found")))?;
    state
        .app_state
        .set_conversation_archived(&workspace_id, &id, archived)
        .map_err(IpcError::Internal)?;
    Ok(serde_json::json!({
        "success": true,
        "conversation_id": id,
        "archived": archived,
    }))
}

#[tauri::command]
pub async fn get_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let control = state
        .app_state
        .workspace_control_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let workspace_generation = control.generation();
    let store = control
        .runtime()
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".into()))?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    let mut stored = store
        .get_messages(&conv.conversation_id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let side_conversations =
        echo_agent_app_core::api::side_conversation::SideConversationService::new(Arc::clone(
            &state.app_state.agent_router,
        ));
    let side_relation = side_conversations
        .relation_for_child(Arc::clone(&store), &workspace_id, &id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let internal_message_ids = state
        .app_state
        .internal_message_ids_scoped(&workspace_id, &id)
        .await
        .map_err(IpcError::Internal)?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    if side_relation.is_none() {
        retain_gui_messages(&mut stored, &internal_message_ids)?;
    } else {
        state
            .app_state
            .mark_side_conversation_viewed(&workspace_id, &id)
            .await
            .map_err(IpcError::Internal)?;
    }

    // Convert canonical StoredMessage values into the GUI-only projection.
    let side_initial_identity = match side_relation {
        Some(entry) => Some((
            entry.snapshot_message_count,
            format!("side-start:{}", entry.group_id),
            side_conversations
                .initial_prompt_for_child(&workspace_id, &id)
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?,
        )),
        None => None,
    };
    let read_cancel = state.app_state.tasks.cancel_token.clone();
    let messages = project_conversation_messages_with_native_history(
        stored,
        side_initial_identity,
        &internal_message_ids,
        &workspace_id,
        &id,
        &state.app_state.storage.chat_events,
        &read_cancel,
    )
    .await?;
    // 原workspace/control和conversation创建身份fence；前端loadGeneration/canProject继续挡旧视图。
    let _same_workspace = state
        .app_state
        .workspace_control_for_generation(&workspace_id, &workspace_generation)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let current = store
        .get_conversation(&id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .ok_or_else(|| IpcError::NotFound("历史读取期间原会话已删除".into()))?;
    if current.id != conv.id || current.created_at != conv.created_at {
        return Err(IpcError::Validation("历史读取期间会话身份已变更".into()));
    }

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
    app: tauri::AppHandle,
    workspace_id: String,
    id: String,
    request: ConversationUpdateRequest,
) -> Result<serde_json::Value, IpcError> {
    let store = scoped_store(&state, &workspace_id).await?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{}' not found", id)))?;

    match request {
        ConversationUpdateRequest::Rename { title } => {
            let runtime = scoped_runtime(&state, &workspace_id).await?;
            let _admission = runtime
                .begin_managed_replacement(&state.app_state.session.foreground_turns, &id)
                .await
                .map_err(|error| IpcError::Validation(error.to_string()))?;
            let runtime_state = runtime.runtime_state_store().ok_or_else(|| {
                IpcError::Internal("Runtime state store not available".to_string())
            })?;
            let execution = runtime
                .agent_for(&id)
                .await
                .map_err(|error| IpcError::Validation(error.to_string()))?;
            echo_agent_app_core::api::managed_conversation::resume_or_import(
                store.as_ref(),
                runtime_state.as_ref(),
                &execution.agent(),
                &id,
            )
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
            let side_conversations =
                echo_agent_app_core::api::side_conversation::SideConversationService::new(
                    Arc::clone(&state.app_state.agent_router),
                );
            let side_relation = side_conversations
                .relation_for_child(Arc::clone(&store), &workspace_id, &id)
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            if side_relation.is_some() {
                side_conversations
                    .rename(store, &workspace_id, &id, &title)
                    .await
                    .map_err(|error| IpcError::Internal(error.to_string()))?;
            } else {
                echo_agent_app_core::api::managed_conversation::update_title(
                    store.as_ref(),
                    &conv.conversation_id,
                    &title,
                )
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            }
        }
        ConversationUpdateRequest::SideModel { model_id } => {
            state
                .app_state
                .update_side_conversation_model(&workspace_id, &id, model_id)
                .await
                .map_err(IpcError::Validation)?;
        }
        ConversationUpdateRequest::SideRetry => {
            let preparation = state
                .app_state
                .prepare_side_conversation_retry(&workspace_id, &id)
                .await
                .map_err(IpcError::Validation)?;
            return launch_side_conversation_gui(state.inner(), app, preparation).await;
        }
        ConversationUpdateRequest::SideViewed => {
            let marked = state
                .app_state
                .mark_side_conversation_viewed(&workspace_id, &id)
                .await
                .map_err(IpcError::Validation)?;
            if !marked {
                return Err(IpcError::Validation(
                    "conversation is not a Side Conversation".to_string(),
                ));
            }
        }
    }

    Ok(serde_json::json!({"success": true}))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplyForkWorktreePreparation {
    pub available: bool,
    pub reason: Option<String>,
    pub repo_root: Option<String>,
    pub head: Option<String>,
    pub branch: String,
    pub path: Option<String>,
    pub project_location: echo_agent_app_core::api::workspace::ProjectLocation,
    pub ssh_target: Option<echo_agent_app_core::api::terminal::TerminalTarget>,
    pub source_connection_generation: Option<String>,
    pub draft_connection_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplyForkPreparation {
    pub request_id: String,
    pub workspace_id: String,
    pub workspace_generation: String,
    pub source_id: String,
    pub user_turn_index: usize,
    pub prefix_sha256: String,
    pub message_count: usize,
    pub worktree: ReplyForkWorktreePreparation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyForkDestination {
    CurrentWorkspace,
    NewWorktree,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReplyForkRequest {
    pub preparation: ReplyForkPreparation,
    pub destination: ReplyForkDestination,
    pub ssh_connection_id: Option<String>,
}

fn reply_fork_prefix_digest(prefix: &[StoredMessage]) -> Result<String, IpcError> {
    use sha2::{Digest as _, Sha256};
    let normalized = prefix
        .iter()
        .cloned()
        .map(|mut message| {
            message.id = None;
            message.conversation_id = "reply-fork-prefix".into();
            message
        })
        .collect::<Vec<_>>();
    let bytes =
        serde_json::to_vec(&normalized).map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validated_reply_fork_publication_events(
    message: &echo_agent::llm::types::Message,
    workspace_id: &str,
    conversation_id: &str,
    events: &[ChatEventEnvelope],
) -> Result<Vec<ChatDriverEvent>, IpcError> {
    // Fork 许可来自原 publication；正文或缺失 native 字段都不能单独授权。
    let publication =
        echo_agent_app_core::api::conversation_projection::project_native_history_publication(
            message,
            workspace_id,
            conversation_id,
            events,
        )
        .map_err(IpcError::Validation)?
        .ok_or_else(|| IpcError::Validation("原生回复 publication 不可用".into()))?;
    if !publication.final_answer_confirmed || publication.content.is_none() {
        return Err(IpcError::Validation(
            "原生回复尚未获得同一 execution 的已确认 publication，不能分叉".into(),
        ));
    }
    let execution_id = message
        .execution_status
        .as_ref()
        .map(|status| status.execution_id.as_str())
        .ok_or_else(|| IpcError::Validation("原生回复缺少 execution 身份".into()))?;
    let settled_event_sequence = events.iter().find_map(|event| match &event.payload {
        ChatDriverEvent::Agent(agent) => {
            let echo_agent::agent::AgentEvent::ExecutionStatus(status) = &agent.payload else {
                return None;
            };
            (status.execution_id == execution_id
                && status.request_id
                    == message
                        .execution_status
                        .as_ref()
                        .and_then(|value| value.request_id.clone())
                && status.sequence
                    == message
                        .execution_status
                        .as_ref()
                        .map(|value| value.sequence)
                        .unwrap_or_default()
                && status.status == echo_agent::agent::ExecutionStatusKind::Settled)
                .then_some(event.sequence)
        }
        _ => None,
    });
    let mut copied = Vec::new();
    for event in events {
        match &event.payload {
            ChatDriverEvent::Agent(agent)
                if agent
                    .execution_id
                    .as_ref()
                    .is_some_and(|value| value.as_str() == execution_id) =>
            {
                copied.push(event.payload.clone());
            }
            ChatDriverEvent::TurnStatus { status }
                if settled_event_sequence.is_some_and(|sequence| event.sequence > sequence)
                    && event.conversation_id.as_deref() == Some(conversation_id)
                    && matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
            {
                copied.push(event.payload.clone());
            }
            _ => {}
        }
    }
    if copied.is_empty() {
        return Err(IpcError::Validation(
            "原生回复缺少可迁移的 publication 事件".into(),
        ));
    }
    Ok(copied)
}

fn attach_native_history_import(
    message: &mut StoredMessage,
    import: NativeHistoryImport,
) -> Result<(), IpcError> {
    let raw = message
        .attachments_json
        .as_deref()
        .ok_or_else(|| IpcError::Internal("native history metadata is missing".into()))?;
    let mut value: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        IpcError::Internal(format!("native history metadata is invalid: {error}"))
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        IpcError::Internal("native history metadata must preserve an object payload".into())
    })?;
    object.insert(
        "native_history_import".to_string(),
        serde_json::to_value(import).map_err(|error| IpcError::Internal(error.to_string()))?,
    );
    message.attachments_json =
        Some(serde_json::to_string(&value).map_err(|error| IpcError::Internal(error.to_string()))?);
    Ok(())
}

fn verify_native_history_import(
    message: &echo_agent::llm::types::Message,
    import: &NativeHistoryImport,
) -> Result<(), IpcError> {
    use sha2::{Digest as _, Sha256};
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(message).map_err(|error| IpcError::Internal(error.to_string()))?,
    ));
    if digest != import.message_content_sha256 {
        return Err(IpcError::Internal(
            "导入 native 历史的 canonical Message SHA 不匹配".into(),
        ));
    }
    if let Some(status) = message.execution_status.as_ref() {
        if status.execution_id != import.execution_id {
            return Err(IpcError::Internal(
                "导入 native 历史 execution 身份不匹配".into(),
            ));
        }
    } else {
        let progress =
            echo_agent_app_core::api::conversation_projection::native_progress_publication_events(
                message,
                &import.source_workspace_id,
                &import.source_conversation_id,
                &import.events,
            )
            .map_err(IpcError::Internal)?;
        let original_records = progress
            .iter()
            .map(echo_agent::utils::canonical_json::canonical_json_bytes)
            .collect::<Result<std::collections::HashSet<_>, _>>()
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        let imported_records = import
            .events
            .iter()
            .map(echo_agent::utils::canonical_json::canonical_json_bytes)
            .collect::<Result<std::collections::HashSet<_>, _>>()
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        // 仅允许完整原中间块的幂等重放，额外事件或最终授权仍须拒绝。
        if original_records.is_empty() || original_records != imported_records {
            return Err(IpcError::Internal(
                "导入普通进度包含非进度事件或缺少批准记录".into(),
            ));
        }
    }
    let mut agent_seen = false;
    for event in &import.events {
        if event.workspace_id != import.source_workspace_id
            || event.conversation_id.as_deref() != Some(import.source_conversation_id.as_str())
            || event.root_turn_id != import.source_root_turn_id
            || event.turn_id != import.source_turn_id
            || event.message_id != import.source_message_id
        {
            return Err(IpcError::Internal(
                "导入 native 历史 source scope 不匹配".into(),
            ));
        }
        match &event.payload {
            ChatDriverEvent::Agent(agent) => {
                if !agent
                    .execution_id
                    .as_ref()
                    .is_some_and(|value| value.as_str() == import.execution_id)
                {
                    return Err(IpcError::Internal(
                        "导入 native 历史 Agent execution 身份不匹配".into(),
                    ));
                }
                agent_seen = true;
            }
            ChatDriverEvent::TurnStatus { .. } => {}
            _ => {
                return Err(IpcError::Internal(
                    "导入 native 历史包含未批准的事件类型".into(),
                ));
            }
        }
    }
    if !agent_seen {
        return Err(IpcError::Internal(
            "导入 native 历史缺少 Agent publication".into(),
        ));
    }
    Ok(())
}

async fn attach_reply_fork_native_history_imports(
    app_state: &echo_agent_app_core::api::AppState,
    runtime: &ScopedChatRuntime,
    source_id: &str,
    prefix: &mut [StoredMessage],
) -> Result<(), IpcError> {
    let workspace_id = runtime.execution_scope().workspace_id().to_string();
    let last_assistant = prefix
        .iter()
        .rposition(|message| message.role == "assistant");
    for (index, message) in prefix.iter_mut().enumerate() {
        if message.role != "assistant" {
            continue;
        }
        let restored = echo_agent::memory::restore_message(message)
            .map_err(|_| IpcError::Validation("无法读取分叉历史中的原生消息".into()))?;
        let imported = message
            .attachments_json
            .as_deref()
            .and_then(AttachmentsPayload::parse)
            .and_then(|payload| payload.native_history_import);
        let source_publications;
        let (publication_workspace, publication_conversation, publication_events) =
            if let Some(import) = imported.as_ref() {
                verify_native_history_import(&restored, import)?;
                (
                    import.source_workspace_id.as_str(),
                    import.source_conversation_id.as_str(),
                    import.events.as_slice(),
                )
            } else {
                source_publications = app_state
                    .storage
                    .chat_events
                    .native_history_publications_async(
                        &workspace_id,
                        source_id,
                        &restored,
                        &app_state.tasks.cancel_token,
                    )
                    .await
                    .map_err(|error| IpcError::Validation(error.to_string()))?;
                (
                    workspace_id.as_str(),
                    source_id,
                    source_publications.as_slice(),
                )
            };
        let require_final = Some(index) == last_assistant;
        let Some(status) = restored.execution_status.as_ref() else {
            if require_final
                && (restored.assistant_blocks.is_some()
                    || restored.approved_assistant_blocks.is_some())
            {
                return Err(IpcError::Validation(
                    "原生历史缺少 execution publication 坐标，不能迁移".into(),
                ));
            }
            // Reasoning 与工具前 Unknown 的资格同由原批准块和真实发布记录核对。
            // 不回填 phase/status，最后选定回复仍受上方最终确认门槛约束。
            let progress = echo_agent_app_core::api::conversation_projection::native_progress_publication_events(
                &restored, publication_workspace, publication_conversation, publication_events,
            ).map_err(IpcError::Validation)?;
            if let Some(first) = progress.first() {
                let ChatDriverEvent::Agent(agent) = &first.payload else {
                    return Err(IpcError::Internal("普通进度缺少原 Agent envelope".into()));
                };
                let execution_id = agent
                    .execution_id
                    .as_ref()
                    .ok_or_else(|| IpcError::Validation("普通进度缺少原 execution 身份".into()))?
                    .to_string();
                let source_workspace_id = first.workspace_id.clone();
                let source_conversation_id = first.conversation_id.clone().ok_or_else(|| {
                    IpcError::Validation("普通进度缺少原 conversation 身份".into())
                })?;
                let source_root_turn_id = first.root_turn_id.clone();
                let source_turn_id = first.turn_id.clone();
                let source_message_id = first.message_id.clone();
                use sha2::{Digest as _, Sha256};
                let message_content_sha256 = hex::encode(Sha256::digest(
                    serde_json::to_vec(&restored)
                        .map_err(|error| IpcError::Internal(error.to_string()))?,
                ));
                attach_native_history_import(
                    message,
                    NativeHistoryImport {
                        source_workspace_id,
                        source_conversation_id,
                        source_root_turn_id,
                        source_turn_id,
                        source_message_id,
                        execution_id,
                        message_content_sha256,
                        events: progress,
                    },
                )?;
            }
            continue;
        };
        let publication =
            echo_agent_app_core::api::conversation_projection::project_native_history_publication(
                &restored,
                publication_workspace,
                publication_conversation,
                publication_events,
            )
            .map_err(IpcError::Validation)?
            .ok_or_else(|| IpcError::Validation("原生历史 publication 不可用".into()))?;
        if require_final && (!publication.final_answer_confirmed || publication.content.is_none()) {
            return Err(IpcError::Validation(
                "分叉最终回复没有同一 execution 的已确认 publication".into(),
            ));
        }
        if !require_final
            && restored
                .approved_assistant_blocks
                .as_ref()
                .is_some_and(|blocks| !blocks.is_empty() && publication.blocks.is_empty())
        {
            return Err(IpcError::Validation(
                "分叉历史的 approved native block 缺少原 publication".into(),
            ));
        }
        let agent_events = publication_events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    ChatDriverEvent::Agent(agent)
                        if agent
                            .execution_id
                            .as_ref()
                            .is_some_and(|value| value.as_str() == status.execution_id)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let Some(first) = agent_events.first() else {
            return Err(IpcError::Validation(
                "分叉历史缺少可迁移的 Agent publication".into(),
            ));
        };
        let expected_execution_id = status.execution_id.as_str();
        let expected_sequence = status.sequence;
        let settled_outer_sequence =
            publication_events
                .iter()
                .find_map(|event| match &event.payload {
                    ChatDriverEvent::Agent(agent) => {
                        let echo_agent::agent::AgentEvent::ExecutionStatus(status) = &agent.payload
                        else {
                            return None;
                        };
                        (status.execution_id == expected_execution_id
                            && status.sequence == expected_sequence
                            && status.status == echo_agent::agent::ExecutionStatusKind::Settled)
                            .then_some(event.sequence)
                    }
                    _ => None,
                });
        let matching_events = publication_events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    ChatDriverEvent::Agent(agent)
                        if agent
                            .execution_id
                            .as_ref()
                            .is_some_and(|value| value.as_str() == status.execution_id)
                ) || (Some(index) == last_assistant
                    && settled_outer_sequence.is_some_and(|sequence| event.sequence > sequence)
                    && event.workspace_id == first.workspace_id
                    && event.conversation_id == first.conversation_id
                    && event.root_turn_id == first.root_turn_id
                    && event.turn_id == first.turn_id
                    && event.message_id == first.message_id
                    && matches!(
                        &event.payload,
                        ChatDriverEvent::TurnStatus { status }
                            if matches!(status.as_str(), "completed" | "failed" | "cancelled")
                    ))
            })
            .cloned()
            .collect::<Vec<_>>();
        let message_content_sha256 = {
            use sha2::{Digest as _, Sha256};
            hex::encode(Sha256::digest(
                serde_json::to_vec(&restored)
                    .map_err(|error| IpcError::Internal(error.to_string()))?,
            ))
        };
        attach_native_history_import(
            message,
            NativeHistoryImport {
                source_workspace_id: first.workspace_id.clone(),
                source_conversation_id: first.conversation_id.clone().unwrap_or_default(),
                source_root_turn_id: first.root_turn_id.clone(),
                source_turn_id: first.turn_id.clone(),
                source_message_id: first.message_id.clone(),
                execution_id: status.execution_id.clone(),
                message_content_sha256,
                events: matching_events,
            },
        )?;
    }
    Ok(())
}

/// Fork admission must prove the selected reply through the original journal.
/// UI confirmation fields are display projections and cannot authorize a fork.
async fn validate_reply_fork_publication(
    app_state: &echo_agent_app_core::api::AppState,
    runtime: &ScopedChatRuntime,
    source_id: &str,
    prefix: &[StoredMessage],
) -> Result<(), IpcError> {
    let workspace_id = runtime.execution_scope().workspace_id().to_string();
    let Some(last_assistant) = prefix
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
    else {
        return Err(IpcError::Validation(
            "无法分叉没有 assistant 回复的回合".into(),
        ));
    };
    let restored = echo_agent::memory::restore_message(last_assistant)
        .map_err(|_| IpcError::Validation("无法验证分叉回复的原始消息".into()))?;
    let imported = last_assistant
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse)
        .and_then(|payload| payload.native_history_import);
    if let Some(import) = imported.as_ref() {
        verify_native_history_import(&restored, import)?;
        let _ = validated_reply_fork_publication_events(
            &restored,
            &import.source_workspace_id,
            &import.source_conversation_id,
            &import.events,
        )?;
    } else {
        let events = app_state
            .storage
            .chat_events
            .native_history_publications_async(
                &workspace_id,
                source_id,
                &restored,
                &app_state.tasks.cancel_token,
            )
            .await
            .map_err(|error| IpcError::Validation(error.to_string()))?;
        let _ =
            validated_reply_fork_publication_events(&restored, &workspace_id, source_id, &events)?;
    }
    Ok(())
}

async fn reply_fork_prefix(
    app_state: &echo_agent_app_core::api::AppState,
    runtime: &ScopedChatRuntime,
    source_id: &str,
    user_turn_index: usize,
    branch_id: &str,
) -> Result<(echo_agent::memory::Conversation, Vec<StoredMessage>, String), IpcError> {
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".into()))?;
    let source = store
        .get_conversation(source_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{source_id}' not found")))?;
    let mut messages = store
        .get_messages(source_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let internal = app_state
        .internal_message_ids_scoped(runtime.execution_scope().workspace_id(), source_id)
        .await
        .map_err(IpcError::Internal)?
        .into_iter()
        .collect();
    retain_public_messages(&mut messages, &internal)?;
    let target = human_message_positions(&messages)?
        .get(user_turn_index)
        .and_then(|index| messages.get(*index))
        .ok_or_else(|| IpcError::Validation("所选用户回合已不存在".into()))?;
    let resource_display =
        echo_agent_app_core::api::prepared_turn::resource_history_display_text(target)
            .map_err(|error| IpcError::Validation(error.to_string()))?;
    let target_content = target
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse)
        .and_then(|payload| payload.display_content)
        .or(resource_display)
        .or_else(|| target.content.clone())
        .ok_or_else(|| IpcError::Validation("所选用户回合没有正文".into()))?;
    // The existing final reply and tool-history validator remains the authority.
    let prefix = branch_prefix(&messages, user_turn_index, branch_id, true)?;
    validate_reply_fork_publication(app_state, runtime, source_id, &prefix).await?;
    echo_agent_app_core::api::managed_conversation::validate_import_history(branch_id, &prefix)
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok((source, prefix, target_content))
}

#[tauri::command]
pub async fn prepare_conversation_fork(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
    user_turn_index: usize,
    request_id: String,
) -> Result<ReplyForkPreparation, IpcError> {
    let request_id = uuid::Uuid::parse_str(&request_id)
        .map_err(|_| IpcError::Validation("分叉请求 ID 无效".into()))?
        .to_string();
    let control = state
        .app_state
        .workspace_control_for_generation(&workspace_id, &workspace_generation)
        .await
        .map_err(IpcError::from)?;
    let runtime = control.runtime();
    let _admission = runtime
        .begin_managed_replacement(&state.app_state.session.foreground_turns, &id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let (_, prefix, _) = reply_fork_prefix(
        &state.app_state,
        runtime,
        &id,
        user_turn_index,
        "reply-fork-prefix",
    )
    .await?;
    let location = if workspace_id == "global" {
        echo_agent_app_core::api::workspace::ProjectLocation::Local {
            path: control.project_root(),
        }
    } else {
        let registry = Arc::clone(&state.app_state.workspace.registry);
        let workspace =
            echo_agent_app_core::api::workspace::WorkspaceId::from_raw(workspace_id.clone());
        state
            .app_state
            .session
            .product_data_io
            .run("inspect reply fork source", move || {
                registry.inspect(&workspace)
            })
            .await
            .map_err(super::product_data::blocking_error)?
            .map_err(IpcError::from)?
            .execution_location()
    };
    let mut worktree = ReplyForkWorktreePreparation {
        available: false,
        reason: None,
        repo_root: None,
        head: None,
        branch: format!("eko/fork-{request_id}"),
        path: None,
        project_location: location.clone(),
        ssh_target: None,
        source_connection_generation: None,
        draft_connection_id: None,
    };
    let prepared = async {
        if let echo_agent_app_core::api::workspace::ProjectLocation::Ssh {
            host, user, port, ..
        } = location
        {
            let connection = state
                .app_state
                .terminal
                .project_ssh(&workspace_id)
                .map_err(IpcError::Validation)?;
            connection
                .check_authenticated()
                .await
                .map_err(IpcError::Validation)?;
            let target =
                echo_agent_app_core::api::terminal::TerminalTarget::Ssh { host, user, port };
            if connection
                .effective_target()
                .map_err(IpcError::Validation)?
                != &target
            {
                return Err(IpcError::Validation(
                    "来源 SSH 有效目标已改变，请重新连接".into(),
                ));
            }
            worktree.ssh_target = Some(target);
            worktree.source_connection_generation = Some(connection.generation());
            worktree.draft_connection_id = Some(format!("reply-fork-auth-{request_id}"));
        }
        let (repo, head, path) = super::panels::prepare_reply_worktree(
            &state.app_state,
            &workspace_id,
            &workspace_generation,
            worktree.branch.clone(),
        )
        .await?;
        worktree.repo_root = Some(repo);
        worktree.head = Some(head);
        worktree.path = Some(path);
        Ok::<(), IpcError>(())
    }
    .await;
    match prepared {
        Ok(()) => worktree.available = true,
        Err(error) => {
            worktree.reason = Some(format!(
                "新 Worktree 暂不可用：需要可读取的 Git 提交与原项目连接（{error}）；当前工作空间仍可分叉"
            ))
        }
    }
    Ok(ReplyForkPreparation {
        request_id,
        workspace_id,
        workspace_generation,
        source_id: id,
        user_turn_index,
        prefix_sha256: reply_fork_prefix_digest(&prefix)?,
        message_count: prefix.len(),
        worktree,
    })
}

async fn run_reply_fork_owned(
    app_state: Arc<echo_agent_app_core::api::AppState>,
    workspace_id: String,
    source_id: String,
    user_turn_index: usize,
    request: ReplyForkRequest,
) -> Result<serde_json::Value, IpcError> {
    let io = app_state.session.product_data_io.clone();
    run_reply_fork_composition_owned(io, async move {
        let cleanup = if request.preparation.workspace_id == workspace_id
            && request.preparation.source_id == source_id
            && request.preparation.user_turn_index == user_turn_index
        {
            reply_fork_independent_draft(&app_state, &workspace_id, &request).await?
        } else {
            None
        };
        let mut result = execute_reply_fork(
            Arc::clone(&app_state),
            workspace_id,
            source_id,
            user_turn_index,
            request,
        )
        .await;
        // 这里只在真实原操作返回后执行；IPC观察丢失不提前关闭仍在用的draft。
        if let Some((owner_id, connection)) = cleanup
            && let Err(cleanup_error) = app_state
                .terminal
                .close_project_ssh_checked(&owner_id, &connection)
                .await
        {
            result = match result {
                    Err(primary) => Err(IpcError::Internal(format!(
                        "{primary}; 独立SSH draft结算未确认: {cleanup_error}"
                    ))),
                    Ok(mut facts) => {
                        let primary = facts.get("error").and_then(serde_json::Value::as_str)
                            .unwrap_or("分叉资源已保留").to_string();
                        if let Some(object) = facts.as_object_mut() {
                            object.insert("error".into(), format!("{primary}; 独立SSH draft结算未确认: {cleanup_error}").into());
                            object.insert("draft_cleanup_error".into(), cleanup_error.into());
                            object.insert("success".into(), false.into());
                            Ok(facts)
                        } else {
                            Err(IpcError::Internal(format!("{primary}; 分叉资源回执不是对象; 独立SSH draft结算未确认: {cleanup_error}")))
                        }
                    }
            };
        }
        result
    })
    .await
}

/// 只有本请求computed key、未登记且物理独立的draft才取得结算义务。
async fn reply_fork_independent_draft(
    app_state: &Arc<echo_agent_app_core::api::AppState>,
    source_workspace_id: &str,
    request: &ReplyForkRequest,
) -> Result<
    Option<(
        String,
        Arc<echo_agent_app_core::api::terminal::ProjectSshConnection>,
    )>,
    IpcError,
> {
    if request.destination != ReplyForkDestination::NewWorktree {
        return Ok(None);
    }
    let Some(target) = request.preparation.worktree.ssh_target.as_ref() else {
        return Ok(None);
    };
    let Some(key) = request.ssh_connection_id.as_ref() else {
        return Ok(None);
    };
    let request_id = uuid::Uuid::parse_str(&request.preparation.request_id)
        .map_err(|_| IpcError::Validation("分叉请求ID无效".into()))?;
    if key != &format!("reply-fork-auth-{request_id}")
        || Some(key) != request.preparation.worktree.draft_connection_id.as_ref()
        || key == source_workspace_id
    {
        return Ok(None);
    }
    let registry = Arc::clone(&app_state.workspace.registry);
    let owner = echo_agent_app_core::api::workspace::WorkspaceId::from_raw(key.clone());
    let registered = app_state
        .session
        .product_data_io
        .run("inspect reply fork draft owner", move || {
            registry.is_identity_reserved(&owner)
        })
        .await
        .map_err(super::product_data::blocking_error)?
        .map_err(IpcError::from)?;
    if registered {
        return Ok(None);
    }
    let Ok(draft) = app_state.terminal.project_ssh_exclusive(key) else {
        return Ok(None);
    };
    if app_state
        .terminal
        .project_ssh(source_workspace_id)
        .is_ok_and(|source| Arc::ptr_eq(&source, &draft))
    {
        return Ok(None);
    }
    if Some(target) != draft.effective_target().ok() {
        return Ok(None);
    }
    Ok(Some((key.clone(), draft)))
}

/// 原 flow 仅保留整段协调 owner，不占叶子阻塞 I/O permit。
/// 观察者消失不取消已准入 Git/import，原 supervisor 继续结算真实失败。
async fn run_reply_fork_composition_owned<F>(
    io: echo_agent_app_core::api::product_data_io::ProductDataIoService,
    future: F,
) -> Result<serde_json::Value, IpcError>
where
    F: std::future::Future<Output = Result<serde_json::Value, IpcError>> + Send + 'static,
{
    let flow = io
        .begin_owned_flow("reply fork composition")
        .map_err(super::product_data::blocking_error)?;
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = future.await;
        let failure = match &result {
            Err(error) => Some(error.to_string()),
            Ok(value) => value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        };
        flow.settle(failure);
        let _ = result_tx.send(result);
    });
    result_rx
        .await
        .map_err(|_| IpcError::Internal("分叉执行未返回结果，请核对原资源收据".into()))?
}

async fn execute_reply_fork(
    app_state: Arc<echo_agent_app_core::api::AppState>,
    workspace_id: String,
    source_id: String,
    user_turn_index: usize,
    request: ReplyForkRequest,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::api::workspace::{
        ProjectLocation, ProjectSource, WorkspaceId, WorkspaceKind,
    };
    let prepared = &request.preparation;
    let request_id = uuid::Uuid::parse_str(&prepared.request_id)
        .map_err(|_| IpcError::Validation("分叉请求 ID 无效".into()))?
        .to_string();
    if prepared.workspace_id != workspace_id
        || prepared.source_id != source_id
        || prepared.user_turn_index != user_turn_index
    {
        return Err(IpcError::Validation("分叉请求与原来源身份不匹配".into()));
    }
    let source_control = app_state
        .workspace_control_for_generation(&workspace_id, &prepared.workspace_generation)
        .await
        .map_err(IpcError::from)?;
    let source_runtime = source_control.runtime();
    // Serialize retries with the existing conversation identity owner and retain
    // its idle admission for the whole composed operation, including lost replies.
    let _source_admission = source_runtime
        .begin_managed_replacement(&app_state.session.foreground_turns, &source_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let branch_id = format!("conv-fork-{request_id}");
    if branch_id == source_id {
        return Err(IpcError::Validation("分叉请求 ID 已属于来源会话".into()));
    }
    let (source, mut prefix, target_content) = reply_fork_prefix(
        &app_state,
        source_runtime,
        &source_id,
        user_turn_index,
        &branch_id,
    )
    .await?;
    if prefix.len() != prepared.message_count
        || reply_fork_prefix_digest(&prefix)? != prepared.prefix_sha256
    {
        return Err(IpcError::Validation(
            "来源完整回合已改变，请重新选择回复".into(),
        ));
    }
    attach_reply_fork_native_history_imports(&app_state, source_runtime, &source_id, &mut prefix)
        .await?;
    // 此处构造且独占一个 JSON object；后续键写入不接受外部 Value 容器。
    let mut response = serde_json::json!({
        "success": false, "id": null, "request_id": request_id, "destination": request.destination,
        "source_id": source_id, "source_workspace_id": workspace_id,
        "workspace_id": workspace_id, "workspace_generation": prepared.workspace_generation,
        "message_count": prefix.len(), "target_content": target_content,
        "worktree": null, "workspace": null, "conversation_created": false, "imported": false,
    });
    let outcome = async {
        let mut target_workspace_id = workspace_id.clone();
        let mut target_workspace_generation = prepared.workspace_generation.clone();
        if request.destination == ReplyForkDestination::NewWorktree {
            let worktree = &prepared.worktree;
            if !worktree.available {
                return Err(IpcError::Validation(
                    worktree
                        .reason
                        .clone()
                        .unwrap_or_else(|| "当前项目不能创建 Worktree".into()),
                ));
            }
            let head = worktree
                .head
                .as_ref()
                .ok_or_else(|| IpcError::Validation("缺少固定源提交".into()))?;
            let path = worktree
                .path
                .as_ref()
                .ok_or_else(|| IpcError::Validation("缺少固定 Worktree 目录".into()))?;
            let branch = format!("eko/fork-{request_id}");
            if worktree.branch != branch {
                return Err(IpcError::Validation("Worktree 分支身份已改变".into()));
            }
            // 重读原项目位置/连接。HEAD 后来前进也仍使用本请求固定的 OID，
            // 重试先重读 Git 资源，不重新解释 HEAD 或新建同名目录。
            let (repo_now, _current_head, path_now) = super::panels::prepare_reply_worktree(
                &app_state,
                &workspace_id,
                &prepared.workspace_generation,
                branch.clone(),
            )
            .await?;
            if Some(&repo_now) != worktree.repo_root.as_ref()

                || path_now != *path
            {
                return Err(IpcError::Validation(
                    "来源项目或 Worktree 位置已改变，请重新准备分叉".into(),
                ));
            }
            let source_location = if workspace_id == "global" {
                ProjectLocation::Local {
                    path: source_control.project_root(),
                }
            } else {
                let registry = Arc::clone(&app_state.workspace.registry);
                let id = WorkspaceId::from_raw(workspace_id.clone());
                app_state
                    .session
                    .product_data_io
                    .run("recheck reply fork source", move || registry.inspect(&id))
                    .await
                    .map_err(super::product_data::blocking_error)?
                    .map_err(IpcError::from)?
                    .execution_location()
            };
            if source_location != worktree.project_location {
                return Err(IpcError::Validation("来源项目位置代际已改变".into()));
            }
            let target_name = format!("reply-fork-{request_id}");
            let target_id = WorkspaceId::from_name(&target_name);
            // An independent draft is verified before any source Git effect.
            // A source-bound connection is never a transferable target owner.
            let ssh_draft = match &source_location {
                ProjectLocation::Local { .. } => None,
                ProjectLocation::Ssh { .. } => {
                    let source_connection = app_state
                        .terminal
                        .project_ssh(&workspace_id)
                        .map_err(IpcError::Validation)?;
                    if worktree.source_connection_generation.as_deref()
                        != Some(source_connection.generation().as_str())
                    {
                        return Err(IpcError::Validation(
                            "来源 SSH 物理连接已更换，请重新准备分叉".into(),
                        ));
                    }
                    source_connection
                        .check_authenticated()
                        .await
                        .map_err(IpcError::Validation)?;
                    let target = worktree
                        .ssh_target
                        .as_ref()
                        .ok_or_else(|| IpcError::Validation("缺少原 SSH 有效目标".into()))?;
                    if source_connection
                        .effective_target()
                        .map_err(IpcError::Validation)?
                        != target
                    {
                        return Err(IpcError::Validation("来源 SSH 有效目标不匹配".into()));
                    }
                    let draft_id = request.ssh_connection_id.as_ref().ok_or_else(|| {
                        IpcError::Validation("请先在原 GUI 完成独立目标 SSH 认证".into())
                    })?;
                    if Some(draft_id) != worktree.draft_connection_id.as_ref()
                        || draft_id == &workspace_id
                    {
                        return Err(IpcError::Validation(
                            "目标 SSH 必须使用本次独立认证连接，不能迁移来源连接".into(),
                        ));
                    }
                    // After successful publication only the target key exists.
                    let draft = app_state
                        .terminal
                        .project_ssh(draft_id)
                        .or_else(|_| app_state.terminal.project_ssh(target_id.as_str()))
                        .map_err(IpcError::Validation)?;
                    if Arc::ptr_eq(&source_connection, &draft)
                        || draft.effective_target().map_err(IpcError::Validation)? != target
                    {
                        return Err(IpcError::Validation(
                            "目标 SSH 连接不是同目标的独立 owner".into(),
                        ));
                    }
                    draft
                        .check_authenticated()
                        .await
                        .map_err(IpcError::Validation)?;
                    Some(draft_id.clone())
                }
            };
            let identity = serde_json::to_vec(&(prepared, request.destination))
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            use sha2::{Digest as _, Sha256};
            let reason = format!("eko-reply-fork:{}", hex::encode(Sha256::digest(identity)));
            let creation = super::panels::create_worktree_for_app(
                &app_state,
                &workspace_id,
                &prepared.workspace_generation,
                branch,
                head.clone(),
                Some(path.clone()),
                Some(reason),
            )
            .await?;
            response["worktree"] = serde_json::to_value(&creation)
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            if let Some(error) = creation.error {
                return Err(IpcError::Internal(error));
            }
            if !creation.observed {
                return Err(IpcError::Internal(
                    "Worktree 创建状态未知，请先从原列表确认".into(),
                ));
            }
            // Registry remains the sole Workspace authority. Re-entry accepts
            // only the exact environment proven by the native Git receipt.
            let registry = Arc::clone(&app_state.workspace.registry);
            let lookup_id = target_id.clone();
            let existing = app_state
                .session
                .product_data_io
                .run("find reply fork workspace", move || {
                    registry
                        .list()
                        .map(|items| items.into_iter().find(|item| item.id == lookup_id))
                })
                .await
                .map_err(super::product_data::blocking_error)?
                .map_err(IpcError::from)?;
            let expected_location = match &source_location {
                ProjectLocation::Local { .. } => ProjectLocation::Local { path: path.into() },
                ProjectLocation::Ssh {
                    host, user, port, ..
                } => ProjectLocation::Ssh {
                    host: host.clone(),
                    user: user.clone(),
                    port: *port,
                    path: path.clone(),
                },
            };
            let workspace = match existing {
                Some(workspace) => {
                    if workspace.execution_location() != expected_location {
                        return Err(IpcError::Validation(
                            "目标工作空间已属于其它项目；保留已有资源".into(),
                        ));
                    }
                    if let Some(draft_id) = ssh_draft.as_ref()
                        && app_state.terminal.project_ssh(draft_id).is_ok()
                    {
                            app_state
                                .reconnect_workspace_project_owned(
                                    target_id.clone(),
                                    draft_id.clone(),
                                    Some(workspace.opaque_product_data_generation()),
                                )
                                .await
                                .map_err(IpcError::from)?;
                    }
                    workspace
                }
                None => {
                    let source = match &expected_location {
                        ProjectLocation::Local { path } => {
                            ProjectSource::ExistingLocal { path: path.clone() }
                        }
                        ProjectLocation::Ssh {
                            host,
                            user,
                            port,
                            path,
                        } => ProjectSource::Ssh {
                            host: host.clone(),
                            user: user.clone(),
                            port: *port,
                            path: path.clone(),
                            connection_id: ssh_draft.ok_or_else(|| {
                                IpcError::Validation("目标独立 SSH draft 缺失".into())
                            })?,
                        },
                    };
                    app_state
                        .create_project_workspace_owned(
                            &target_name,
                            WorkspaceKind::General,
                            source,
                        )
                        .await
                        .map_err(IpcError::from)?
                        .0
                }
            };
            response["workspace"] = serde_json::to_value(&workspace)
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            response["workspace_id"] = workspace.id.to_string().into();
            response["workspace_generation"] = workspace.opaque_product_data_generation().into();
            target_workspace_id = workspace.id.to_string();
            target_workspace_generation = workspace.opaque_product_data_generation();
        }
        // Registry 读到的产品代际必须与真正加载的 runtime 一致；一直持有原lease。
        let target_control = app_state
            .workspace_control_for_generation(&target_workspace_id, &target_workspace_generation)
            .await
            .map_err(IpcError::from)?;
        let target_runtime = target_control.runtime();
        let target_admission = target_runtime
            .begin_managed_replacement(&app_state.session.foreground_turns, &branch_id)
            .await
            .map_err(|error| IpcError::Validation(error.to_string()))?;
        let target_store = target_runtime
            .conversation_store()
            .ok_or_else(|| IpcError::Internal("目标 conversation store 不可用".into()))?;
        let existing = target_store
            .get_conversation(&branch_id)
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        let existing_import = target_store.get_latest_managed_import(&branch_id).await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        if existing.is_some() {
            response["id"] = branch_id.clone().into();
            response["existing_conversation_observed"] = true.into();
            if existing_import.is_none() {
                return Err(IpcError::Validation(
                    "目标会话已存在但没有本次 managed import 回执，保留现有资源；请从原会话列表检查，不自动覆盖或重建".into()
                ));
            }
        }
        // Transfer only references. Inline multimodal parts are already copied
        // with the canonical prefix; source control/TaskRun state is not read.
        let source_root = source_runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts().map(|config| config.root_dir))
            .await
            .or_else(|| Some(if workspace_id == "global" {
                echo_agent_app_core::api::data_root::user_data_path("artifacts")
            } else {
                echo_agent_app_core::api::workspace::layout::WorkspaceLayout::artifacts(source_control.data_root())
            }));
        let target_root = target_runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts().map(|config| config.root_dir))
            .await;
        let copy_id = branch_id.clone();
        let (copied, resources) = app_state
            .session
            .product_data_io
            .run("copy reply fork history resources", move || {
                let report = echo_agent_app_core::api::prepared_turn::copy_reply_fork_history_resources_with_receipts(
                    &mut prefix, source_root.as_deref(), target_root.as_deref(), &copy_id,
                );
                (prefix, report)
            })
            .await
            .map_err(super::product_data::blocking_error)?;
        response["resources"] = serde_json::to_value(&resources.resources)
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        response["warnings"] = serde_json::to_value(&resources.warnings)
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        if let Some(error) = resources.error {
            return Err(IpcError::Validation(error));
        }
        if existing.is_none() {
            target_runtime
                .create_managed_conversation_under_admission(
                    &target_admission,
                    NewConversation {
                        conversation_id: branch_id.clone(),
                        user_id: source.user_id,
                        agent_type: source.agent_type,
                        title: Some(
                            source
                                .title
                                .map(|title| format!("{title}（分叉）"))
                                .unwrap_or_else(|| "回复分叉".into()),
                        ),
                    },
                )
                .await
                .map_err(|error| IpcError::Internal(error.to_string()))?;
        }
        response["id"] = branch_id.clone().into();
        response["conversation_created"] = true.into();
        let rows = target_store
            .get_messages(&branch_id)
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        let runtime_state = target_runtime
            .runtime_state_store()
            .ok_or_else(|| IpcError::Internal("目标 Runtime state 不可用".into()))?;
        let branch_agent = target_runtime
            .agent_for(&branch_id)
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        if existing_import.is_some() {
            let actual = echo_agent::memory::managed_import_projection_digests(&branch_id, &rows)
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            let expected =
                echo_agent::memory::managed_import_projection_digests(&branch_id, &copied)
                    .map_err(|error| IpcError::Internal(error.to_string()))?;
            if actual != expected {
                return Err(IpcError::Validation(
                    "目标会话已有其它历史，不覆盖后续用户内容".into(),
                ));
            }
            echo_agent_app_core::api::managed_conversation::resume_or_import(
                target_store.as_ref(),
                runtime_state.as_ref(),
                &branch_agent.agent(),
                &branch_id,
            )
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        } else {
            echo_agent_app_core::api::managed_conversation::replace_and_resume(
                target_store.as_ref(),
                runtime_state.as_ref(),
                &branch_agent.agent(),
                &branch_id,
                &copied,
            )
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        }
        response["imported"] = true.into();
        response["success"] = true.into();
        Ok::<(), IpcError>(())
    }
    .await;
    if let Err(error) = outcome {
        response["error"] = error.to_string().into();
    }
    Ok(response)
}

/// Create an immutable branch before a user turn, or through its complete reply.
///
/// Edit/regenerate callers resend that user turn against the returned
/// conversation. Explicit Fork includes the turn and leaves the new conversation idle.
/// The source transcript remains untouched and the new pooled
/// Agent receives the exact canonical prefix, including prior tool messages.
#[tauri::command]
pub async fn branch_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    id: String,
    user_turn_index: usize,
    include_turn: Option<bool>,
    fork: Option<ReplyForkRequest>,
) -> Result<serde_json::Value, IpcError> {
    if let Some(request) = fork {
        if include_turn != Some(true) {
            return Err(IpcError::Validation("回复分叉必须包含完整回合".into()));
        }
        return run_reply_fork_owned(
            Arc::clone(&state.app_state),
            workspace_id,
            id,
            user_turn_index,
            request,
        )
        .await;
    }
    let runtime = scoped_runtime(&state, &workspace_id).await?;
    let _source_admission = runtime
        .begin_managed_replacement(&state.app_state.session.foreground_turns, &id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;
    let source = store
        .get_conversation(&id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Conversation '{id}' not found")))?;
    let mut stored = store
        .get_messages(&source.conversation_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    // GUI turn ordinals count visible prompts. Keep internal delivery rows out of
    // the ordinary fork so they cannot become user-authored messages in its list.
    let internal_ids = state
        .app_state
        .internal_message_ids_scoped(&workspace_id, &id)
        .await
        .map_err(IpcError::Internal)?
        .into_iter()
        .collect();
    retain_public_messages(&mut stored, &internal_ids)?;
    let target = human_message_positions(&stored)?
        .get(user_turn_index)
        .and_then(|index| stored.get(*index))
        .ok_or_else(|| {
            IpcError::Validation(format!(
                "user turn index {user_turn_index} is outside the canonical transcript"
            ))
        })?;
    let resource_display =
        echo_agent_app_core::api::prepared_turn::resource_history_display_text(target)
            .map_err(|error| IpcError::Validation(error.to_string()))?;
    let target_content = target
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse)
        .and_then(|payload| payload.display_content)
        .or(resource_display)
        .or_else(|| target.content.clone())
        .ok_or_else(|| {
            IpcError::Validation(format!(
                "user turn index {user_turn_index} has no text content"
            ))
        })?;
    let branch_id = format!("conv-{}", uuid::Uuid::new_v4());
    let prefix = branch_prefix(
        &stored,
        user_turn_index,
        &branch_id,
        include_turn.unwrap_or(false),
    )?;
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
    let admission = runtime
        .begin_managed_replacement(&state.app_state.session.foreground_turns, &branch_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let runtime_state = runtime
        .runtime_state_store()
        .ok_or_else(|| IpcError::Internal("Runtime state store not available".to_string()))?;
    let branch_agent = runtime
        .agent_for(&branch_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    if let Err(error) = echo_agent_app_core::api::managed_conversation::replace_and_resume(
        store.as_ref(),
        runtime_state.as_ref(),
        &branch_agent.agent(),
        &branch_id,
        &prefix,
    )
    .await
    {
        drop(branch_agent);
        drop(admission);
        if let Err(cleanup_error) = state
            .app_state
            .delete_conversation_scoped(&workspace_id, &branch_id)
            .await
        {
            tracing::warn!(conversation_id = %branch_id, %cleanup_error, "Failed to roll back unusable conversation branch");
        }
        return Err(IpcError::Internal(error.to_string()));
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
    if let Err(error) = state
        .app_state
        .set_conversation_archived(&workspace_id, &id, false)
    {
        // Transcript deletion already committed. A stale archive marker is
        // harmless and will be ignored when the conversation is listed.
        tracing::warn!(workspace_id, conversation_id = %id, %error, "failed to remove conversation archive marker after deletion");
    }
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
    state
        .app_state
        .prepare_side_conversation_model(&workspace_id, &id)
        .await
        .map_err(IpcError::Validation)?;
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
            load_agent_transcript(&runtime, &state.app_state.session.foreground_turns, &id).await?,
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
    let archived_ids = state
        .app_state
        .archived_conversation_ids(&workspace_id)
        .map_err(IpcError::Internal)?;
    let archived_ids: std::collections::HashSet<&str> =
        archived_ids.iter().map(String::as_str).collect();
    let results = results
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "id": item.id,
                "conversation_id": item.conversation_id,
                "title": item.title,
                "message_count": item.message_count,
                "created_at": item.created_at,
                "updated_at": item.updated_at,
                "archived": archived_ids.contains(item.conversation_id.as_str()),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_value(results).map_err(|e| IpcError::Internal(e.to_string()))
}

/// Read-only GUI observation of the existing durable inbox, with the normal scope fence.
#[tauri::command]
pub async fn get_agent_communication_inputs(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
) -> Result<Vec<echo_agent_app_core::api::agent_router::AgentDeliveryRecord>, IpcError> {
    let _control = state
        .app_state
        .workspace_control_for_generation(&workspace_id, &workspace_generation)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let target = echo_agent_app_core::api::agent_router::AgentAddress::new(
        echo_agent_app_core::api::workspace::WorkspaceId::from_raw(workspace_id.clone()),
        id,
    );
    let records = state
        .app_state
        .agent_delivery_records(&target)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let _same = state
        .app_state
        .workspace_control_for_generation(&workspace_id, &workspace_generation)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(records
        .into_iter()
        .filter(|record| {
            record.payload.origin
                == echo_agent_app_core::api::agent_router::AgentMessageOrigin::Agent
        })
        .collect())
}

#[cfg(test)]
mod agent_communication_view_tests {
    use super::*;
    use echo_agent_app_core::api::prepared_turn::{PreparedUserTurn, persisted_agent_input};
    fn source() -> anyhow::Result<(tempfile::TempDir, StoredMessage)> {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".echo-semantic/local/agent-communication-view-tests");
        std::fs::create_dir_all(&directory)?;
        let root = tempfile::tempdir_in(directory)?;
        let input = echo_agent_app_core::api::agent_router::AgentMessage::agent_text(
            Some(echo_agent_app_core::api::agent_router::AgentAddress::new(
                echo_agent_app_core::api::workspace::WorkspaceId::from_raw("w".into()),
                "source",
            )),
            echo_agent_app_core::api::agent_router::AgentAddress::new(
                echo_agent_app_core::api::workspace::WorkspaceId::from_raw("w".into()),
                "c",
            ),
            "Agent原始输入",
        );
        let mut turn = PreparedUserTurn::runtime_instruction("模型指令");
        turn.bind_agent_input(&input, root.path(), "c", "turn")?;
        Ok((
            root,
            echo_agent::memory::project_message("c", &turn.to_message()?)?,
        ))
    }
    #[test]
    fn agent_input_is_kept_in_fork_context_without_counting_as_a_human_prompt() -> anyhow::Result<()>
    {
        let (_root, input) = source()?;
        let human = echo_agent::memory::project_message(
            "c",
            &echo_agent::llm::types::Message::user("human".to_string()),
        )?;
        let reply = echo_agent::memory::project_message(
            "c",
            &echo_agent::llm::types::Message::assistant("reply".to_string()),
        )?;
        let rows = vec![human.clone(), reply.clone(), input, reply, human];
        assert_eq!(human_message_positions(&rows)?, vec![0, 4]);
        let fork = branch_prefix(&rows, 0, "fork", true)?;
        assert_eq!(fork.len(), 4);
        let retained = fork
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("Agent context disappeared"))?;
        assert!(persisted_agent_input(&echo_agent::memory::restore_message(retained)?)?.is_some());
        assert_eq!(branch_prefix(&rows, 1, "fork", false)?.len(), 4);
        Ok(())
    }
    #[test]
    fn corrupt_typed_source_is_rejected_by_gui_and_fork_instead_of_becoming_human()
    -> anyhow::Result<()> {
        let (_root, input) = source()?;
        let mut value: serde_json::Value = serde_json::from_str(
            input
                .attachments_json
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("canonical metadata missing"))?,
        )?;
        let serialized = serde_json::to_string(&value)?;
        // A known typed-source body change retains the old digest and must fail in the shared reader.
        let changed = serialized.replace("Agent原始输入", "tampered");
        value = serde_json::from_str(&changed)?;
        let mut input = input;
        input.attachments_json = Some(serde_json::to_string(&value)?);
        let mut gui = vec![input.clone()];
        assert!(retain_gui_messages(&mut gui, &std::collections::HashSet::new()).is_err());
        let mut fork = vec![input.clone()];
        assert!(retain_public_messages(&mut fork, &std::collections::HashSet::new()).is_err());
        assert!(branch_prefix(&[input], 0, "fork", false).is_err());
        Ok(())
    }
}
