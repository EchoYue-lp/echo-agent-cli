//! 对话历史持久化 API
//!
//! 提供对话历史的 CRUD + restore，基于 `ConversationStore` trait（默认 SQLite）。

use axum::{
    Json,
    extract::{Path, State},
};
use echo_agent::memory::{ConversationFilter, ConversationStore, NewConversation, StoredMessage};
use echo_agent::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::persistence::SavedMessage;
use crate::state::AppState;

// -- Request / Response types --

#[derive(Debug, Deserialize)]
pub struct SaveConversationRequest {
    pub id: String,
    pub title: String,
    pub messages: Vec<SavedMessage>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
    pub messages: Option<Vec<SavedMessage>>,
}

#[derive(Debug, Serialize)]
pub struct ConversationListItem {
    pub id: String,
    pub conversation_id: String,
    pub title: Option<String>,
    pub message_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ConversationDetail {
    pub id: i64,
    pub conversation_id: String,
    pub title: Option<String>,
    pub messages: Vec<SavedMessage>,
    pub created_at: String,
    pub updated_at: String,
}

// -- Constants --

const MAX_CONVERSATION_ID_LEN: usize = 64;
const MAX_TITLE_LEN: usize = 255;
const MAX_MESSAGES: usize = 10000;

// -- Helpers --

fn store_err(e: echo_agent::error::ReactError) -> AppError {
    AppError::Internal(e.to_string())
}

/// Validate a conversation ID: non-empty, max length, alphanumeric + hyphens + underscores only.
fn validate_conversation_id(id: &str) -> std::result::Result<(), AppError> {
    if id.is_empty() {
        return Err(AppError::Validation(
            "conversation id must not be empty".to_string(),
        ));
    }
    if id.len() > MAX_CONVERSATION_ID_LEN {
        return Err(AppError::Validation(format!(
            "conversation id must be at most {} characters",
            MAX_CONVERSATION_ID_LEN
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::Validation(
            "conversation id must contain only alphanumeric characters, hyphens, and underscores"
                .to_string(),
        ));
    }
    Ok(())
}

/// Validate a title string length.
fn validate_title(title: &str) -> std::result::Result<(), AppError> {
    if title.len() > MAX_TITLE_LEN {
        return Err(AppError::Validation(format!(
            "title must be at most {} characters",
            MAX_TITLE_LEN
        )));
    }
    Ok(())
}

/// Validate the number of messages in a conversation.
fn validate_message_count(count: usize) -> std::result::Result<(), AppError> {
    if count > MAX_MESSAGES {
        return Err(AppError::Validation(format!(
            "conversation must have at most {} messages",
            MAX_MESSAGES
        )));
    }
    Ok(())
}

/// Get the conversation store, or return error if disabled
fn get_store(state: &AppState) -> std::result::Result<&Arc<dyn ConversationStore>, AppError> {
    state
        .storage
        .conversation_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("Conversation persistence is disabled".to_string()))
}

// -- API handlers --

/// GET /api/conversations -- list all conversations
pub async fn list_conversations(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<Vec<ConversationListItem>>, AppError> {
    let store = get_store(&state)?;
    let metas = store
        .list_conversations(ConversationFilter::default())
        .await
        .map_err(store_err)?;

    let items: Vec<ConversationListItem> = metas
        .into_iter()
        .map(|m| ConversationListItem {
            id: m.conversation_id.clone(),
            conversation_id: m.conversation_id,
            title: m.title,
            message_count: m.message_count,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
        .collect();

    Ok(Json(items))
}

/// POST /api/conversations -- save (upsert) a conversation
pub async fn save_conversation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SaveConversationRequest>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    validate_conversation_id(&req.id)?;
    validate_title(&req.title)?;
    validate_message_count(req.messages.len())?;

    let store = get_store(&state)?;

    // Ensure conversation row exists
    let existing = store.get_conversation(&req.id).await.map_err(store_err)?;

    if existing.is_none() {
        store
            .create_conversation(NewConversation {
                conversation_id: req.id.clone(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some(req.title.clone()),
            })
            .await
            .map_err(store_err)?;
    } else {
        store
            .update_conversation(&req.id, Some(&req.title), None, None)
            .await
            .map_err(store_err)?;
    }

    // Convert SavedMessage -> StoredMessage
    let now = chrono::Utc::now().to_rfc3339();
    let stored: Vec<StoredMessage> = req
        .messages
        .iter()
        .map(|m| StoredMessage {
            id: None,
            conversation_id: req.id.clone(),
            role: m.role.clone(),
            content: m.content.clone(),
            attachments_json: None,
            tool_calls_json: m
                .tool_calls
                .as_ref()
                .and_then(|calls| serde_json::to_string(calls).ok()),
            tool_result_json: None,
            created_at: now.clone(),
        })
        .collect();

    store
        .save_messages(&req.id, &stored)
        .await
        .map_err(store_err)?;

    Ok(Json(serde_json::json!({
        "success": true,
        "id": req.id
    })))
}

/// GET /api/conversations/:id -- get conversation detail with messages
pub async fn get_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<ConversationDetail>, AppError> {
    validate_conversation_id(&id)?;

    let store = get_store(&state)?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    let stored = store.get_messages(&id).await.map_err(store_err)?;

    let messages: Vec<SavedMessage> = stored
        .into_iter()
        .map(|m| SavedMessage {
            role: m.role,
            content: m.content,
            tool_calls: m
                .tool_calls_json
                .and_then(|j| serde_json::from_str(&j).ok()),
        })
        .collect();

    Ok(Json(ConversationDetail {
        id: conv.id,
        conversation_id: conv.conversation_id.clone(),
        title: conv.title,
        messages,
        created_at: conv.created_at,
        updated_at: conv.updated_at,
    }))
}

/// PUT /api/conversations/:id -- update conversation
pub async fn update_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateConversationRequest>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    validate_conversation_id(&id)?;
    if let Some(title) = &req.title {
        validate_title(title)?;
    }
    if let Some(messages) = &req.messages {
        validate_message_count(messages.len())?;
    }

    let store = get_store(&state)?;

    if let Some(title) = &req.title {
        store
            .update_conversation(&id, Some(title), None, None)
            .await
            .map_err(store_err)?;
    }

    if let Some(messages) = &req.messages {
        let now = chrono::Utc::now().to_rfc3339();
        let stored: Vec<StoredMessage> = messages
            .iter()
            .map(|m| StoredMessage {
                id: None,
                conversation_id: id.clone(),
                role: m.role.clone(),
                content: m.content.clone(),
                attachments_json: None,
                tool_calls_json: m
                    .tool_calls
                    .as_ref()
                    .and_then(|calls| serde_json::to_string(calls).ok()),
                tool_result_json: None,
                created_at: now.clone(),
            })
            .collect();

        store.save_messages(&id, &stored).await.map_err(store_err)?;
    }

    Ok(Json(serde_json::json!({"success": true})))
}

/// DELETE /api/conversations/:id -- delete conversation
pub async fn delete_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    validate_conversation_id(&id)?;

    let store = get_store(&state)?;

    store.delete_conversation(&id).await.map_err(store_err)?;

    Ok(Json(serde_json::json!({"success": true})))
}

/// GET /api/conversations/:id/export -- export conversation as Markdown
pub async fn export_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    validate_conversation_id(&id)?;

    // Try JSON-file-based export first (backward compat)
    let md = {
        let persistence = state.storage.persistence.read().await;
        persistence.export_conversation_markdown(&id)
    };

    if let Ok(content) = md {
        return Ok(Json(serde_json::json!({
            "format": "markdown",
            "content": content,
            "id": id,
        })));
    }

    // Fallback: generate Markdown from ConversationStore
    let store = get_store(&state)?;

    let conv = store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    let stored = store.get_messages(&id).await.map_err(store_err)?;

    let mut content = String::new();
    content.push_str(&format!(
        "# {}\n\n",
        conv.title.as_deref().unwrap_or("Untitled")
    ));
    content.push_str(&format!(
        "> Created: {} | Updated: {}\n\n",
        conv.created_at, conv.updated_at
    ));

    for msg in &stored {
        let role_label = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            "tool" => "Tool",
            _ => &msg.role,
        };
        content.push_str(&format!("### {}\n\n", role_label));
        if let Some(text) = &msg.content {
            content.push_str(text);
            content.push_str("\n\n");
        }
        if let Some(calls_json) = &msg.tool_calls_json {
            let calls: Vec<crate::persistence::SavedToolCall> =
                match serde_json::from_str(calls_json) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
            content.push_str("**Tool Calls:**\n");
            for tc in &calls {
                content.push_str(&format!("- `{}`: {}\n", tc.name, tc.arguments));
            }
            content.push('\n');
        }
    }

    Ok(Json(serde_json::json!({
        "format": "markdown",
        "content": content,
        "id": id,
    })))
}

/// POST /api/conversations/:id/restore -- restore conversation into agent context
///
/// Loads all messages from the conversation store and injects them into
/// the ReactAgent's context, allowing the user to continue the conversation.
pub async fn restore_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    validate_conversation_id(&id)?;

    let store = get_store(&state)?;

    // 1. Verify conversation exists
    let _conv = store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    // 2. Load stored messages
    let stored_messages = store.get_messages(&id).await.map_err(store_err)?;

    let count = stored_messages.len();

    // 3. Convert StoredMessage -> echo_agent Message, sanitizing for LLM compatibility
    use echo_agent::llm::types::MessageContent as Mc;
    let mut messages: Vec<Message> = Vec::new();
    for m in stored_messages {
        let role: Role = m.role.clone().into();
        let content: MessageContent = m.content.map(Mc::Text).unwrap_or(Mc::Empty);
        let content_str = content.as_deref().unwrap_or("");

        // Skip messages that would break LLM API calls
        if role == Role::Assistant {
            let is_empty = content_str.is_empty();
            let is_error = content_str.starts_with("[Error]");
            let tool_calls: Option<Vec<echo_agent::llm::types::ToolCall>> = m
                .tool_calls_json
                .as_ref()
                .and_then(|j| serde_json::from_str(j).ok());
            let has_empty_tool_calls = tool_calls.as_ref().is_some_and(|tc| tc.is_empty());

            if is_empty && has_empty_tool_calls {
                continue;
            }
            if is_empty {
                continue;
            }
            if is_error {
                messages.push(Message {
                    role,
                    content: Mc::Text("(上一轮对话出现问题，已跳过)".to_string()),
                    tool_calls: None,
                    name: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }
            if has_empty_tool_calls {
                messages.push(Message {
                    role,
                    content,
                    tool_calls: None,
                    name: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }

            messages.push(Message {
                role,
                content,
                tool_calls,
                name: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        } else {
            messages.push(Message {
                role,
                content,
                tool_calls: m
                    .tool_calls_json
                    .and_then(|j| serde_json::from_str(&j).ok()),
                name: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }
    }

    // 4. Inject into agent context (preserving system prompt)
    // WARNING: This replaces the entire conversation context. Any ongoing conversation will be lost.
    tracing::warn!(
        conversation_id = %id,
        message_count = count,
        "Restoring conversation - this will replace the current conversation context"
    );

    state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let system_prompt = agent.system_prompt().to_string();

                if messages.first().map(|m| m.role.as_str()) != Some("system") {
                    messages.insert(0, Message::system(system_prompt));
                }

                let mut seen_system = false;
                messages.retain(|m| {
                    if m.role == Role::System {
                        if seen_system {
                            false
                        } else {
                            seen_system = true;
                            true
                        }
                    } else {
                        true
                    }
                });

                agent.load_messages(messages).await;
            })
        })
        .await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message_count": count,
        "conversation_id": id,
        "warning": "Conversation restored. Previous conversation context has been replaced."
    })))
}
