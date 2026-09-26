//! Tauri IPC commands for conversation persistence.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::memory::{NewConversation, StoredMessage};
use echo_agent_app_core::api::conversation_projection::{AttachmentsPayload, SavedMessage};
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

fn project_stored_message(message: StoredMessage) -> SavedMessage {
    let internal_agent = echo_agent_app_core::api::side_conversation::is_internal_agent_message(
        message.attachments_json.as_deref(),
    );
    let (
        message_id,
        display_content,
        thinking_segments,
        execution_steps,
        execution_rounds,
        attachments,
    ) = message
        .attachments_json
        .as_deref()
        .and_then(AttachmentsPayload::parse)
        .map(|payload| {
            let thinking_segments =
                (!payload.thinking_segments.is_empty()).then_some(payload.thinking_segments);
            let execution_steps =
                (!payload.execution_steps.is_empty()).then_some(payload.execution_steps);
            let attachments = (!payload.attachments.is_empty()).then_some(payload.attachments);
            (
                payload.message_id,
                payload.display_content,
                thinking_segments,
                execution_steps,
                payload.execution_rounds,
                attachments,
            )
        })
        .unwrap_or((None, None, None, None, None, None));

    SavedMessage {
        message_id,
        role: message.role,
        content: display_content.or(message.content),
        internal_agent,
        tool_calls: message
            .tool_calls_json
            .and_then(|value| serde_json::from_str(&value).ok()),
        thinking_segments,
        tool_result: message.tool_result_json,
        execution_steps,
        execution_rounds,
        attachments,
    }
}

fn project_conversation_messages(
    stored: Vec<StoredMessage>,
    side_initial_identity: Option<(usize, String, String)>,
    internal_message_ids: &std::collections::HashSet<i64>,
) -> Vec<SavedMessage> {
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
            let mut projected = project_stored_message(message);
            projected.internal_agent |= internal_agent;
            if is_initial_side_user && let Some((_, message_id, _)) = side_initial_identity.take() {
                projected.message_id = Some(message_id);
            }
            projected
        })
        .collect()
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
            internal_agent: false,
            tool_calls: None,
            thinking_segments: None,
            tool_result: None,
            execution_steps: None,
            execution_rounds: None,
            attachments: None,
        }
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
    fn stored_internal_agent_marker_projects_to_the_gui_contract() -> anyhow::Result<()> {
        let stored = StoredMessage {
            id: Some(1),
            conversation_id: "side-1".to_string(),
            role: "user".to_string(),
            content: Some("internal instruction".to_string()),
            attachments_json: Some(
                serde_json::json!({"eko_visibility": "internal_agent"}).to_string(),
            ),
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let projected = project_stored_message(stored);
        assert!(projected.internal_agent);
        assert_eq!(projected.content.as_deref(), Some("internal instruction"));
        assert_eq!(
            serde_json::to_value(projected)?
                .get("internal_agent")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn exact_message_ids_do_not_relabel_identical_user_text() {
        let messages = [
            ("user", "internal instruction"),
            ("assistant", "internal result"),
            ("user", "internal instruction"),
            ("assistant", "later response"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (role, content))| StoredMessage {
            id: i64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1)),
            conversation_id: "side-1".to_string(),
            role: role.to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .collect();
        let ids = std::collections::HashSet::from([1_i64, 2_i64]);
        let projected = project_conversation_messages(messages, None, &ids);
        assert_eq!(
            projected
                .iter()
                .map(|message| message.internal_agent)
                .collect::<Vec<_>>(),
            vec![true, true, false, false]
        );
    }

    #[test]
    fn completed_side_first_prompt_recovers_its_stable_gui_identity() {
        let stored = vec![
            StoredMessage {
                id: Some(1),
                conversation_id: "side-1".to_string(),
                role: "user".to_string(),
                content: Some("inherited context".to_string()),
                attachments_json: None,
                tool_calls_json: None,
                tool_result_json: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            StoredMessage {
                id: Some(2),
                conversation_id: "side-1".to_string(),
                role: "user".to_string(),
                content: Some("initial side prompt".to_string()),
                attachments_json: None,
                tool_calls_json: None,
                tool_result_json: None,
                created_at: "2026-01-01T00:01:00Z".to_string(),
            },
            StoredMessage {
                id: Some(3),
                conversation_id: "side-1".to_string(),
                role: "assistant".to_string(),
                content: Some("finished".to_string()),
                attachments_json: None,
                tool_calls_json: None,
                tool_result_json: None,
                created_at: "2026-01-01T00:02:00Z".to_string(),
            },
        ];

        let projected = project_conversation_messages(
            stored,
            Some((
                1,
                "side-start:group-1".to_string(),
                "initial side prompt".to_string(),
            )),
            &std::collections::HashSet::new(),
        );

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
    let store = scoped_store(&state, &workspace_id).await?;

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
        stored.retain(|message| {
            !message
                .id
                .is_some_and(|id| internal_message_ids.contains(&id))
                && !echo_agent_app_core::api::side_conversation::is_internal_agent_message(
                    message.attachments_json.as_deref(),
                )
        });
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
    let messages =
        project_conversation_messages(stored, side_initial_identity, &internal_message_ids);

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
