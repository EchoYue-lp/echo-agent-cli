//! 对话历史持久化 API
//!
//! 提供对话历史的 CRUD + restore，基于 `ConversationStore` trait（默认 SQLite）。

use axum::{
    extract::{Path, State},
    Json,
};
use echo_agent::memory::conversation::{
    ConversationFilter, NewConversation, StoredMessage,
};
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
    pub model: Option<String>,
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

// -- Helper: convert ReactError -> AppError --

fn store_err(e: echo_agent::error::ReactError) -> AppError {
    AppError::Internal(e.to_string())
}

// -- API handlers --

/// GET /api/conversations -- list all conversations
pub async fn list_conversations(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<Vec<ConversationListItem>>, AppError> {
    let metas = state
        .conversation_store
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
    // Ensure conversation row exists
    let existing = state
        .conversation_store
        .get_conversation(&req.id)
        .await
        .map_err(store_err)?;

    if existing.is_none() {
        state
            .conversation_store
            .create_conversation(NewConversation {
                conversation_id: req.id.clone(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some(req.title.clone()),
            })
            .await
            .map_err(store_err)?;
    } else {
        // Update title
        state
            .conversation_store
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

    state
        .conversation_store
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
    let conv = state
        .conversation_store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    let stored = state
        .conversation_store
        .get_messages(&id)
        .await
        .map_err(store_err)?;

    // Convert StoredMessage -> SavedMessage (for frontend compatibility)
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
    // Update title if provided
    if let Some(title) = &req.title {
        state
            .conversation_store
            .update_conversation(&id, Some(title), None, None)
            .await
            .map_err(store_err)?;
    }

    // Update messages if provided
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

        state
            .conversation_store
            .save_messages(&id, &stored)
            .await
            .map_err(store_err)?;
    }

    Ok(Json(serde_json::json!({"success": true})))
}

/// DELETE /api/conversations/:id -- delete conversation
pub async fn delete_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    state
        .conversation_store
        .delete_conversation(&id)
        .await
        .map_err(store_err)?;

    Ok(Json(serde_json::json!({"success": true})))
}

/// GET /api/conversations/:id/export -- export conversation as Markdown
pub async fn export_conversation(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<serde_json::Value>, AppError> {
    // Try JSON-file-based export first (backward compat)
    let md = state.persistence.export_conversation_markdown(&id);

    if let Ok(content) = md {
        return Ok(Json(serde_json::json!({
            "format": "markdown",
            "content": content,
            "id": id,
        })));
    }

    // Fallback: generate Markdown from ConversationStore
    let conv = state
        .conversation_store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    let stored = state
        .conversation_store
        .get_messages(&id)
        .await
        .map_err(store_err)?;

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
            content.push_str("\n");
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
    // 1. Verify conversation exists
    let _conv = state
        .conversation_store
        .get_conversation(&id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AppError::NotFound(format!("Conversation {} not found", id)))?;

    // 2. Load stored messages
    let stored_messages = state
        .conversation_store
        .get_messages(&id)
        .await
        .map_err(store_err)?;

    let count = stored_messages.len();

    // 3. Convert StoredMessage -> echo_agent Message, sanitizing for LLM compatibility
    let mut messages: Vec<Message> = Vec::new();
    for m in stored_messages {
        let role = m.role.clone();
        let content = m.content.clone();
        let content_str = content.as_deref().unwrap_or("");

        // Skip messages that would break LLM API calls:
        // - Empty assistant content (no text and no tool_calls)
        // - Error messages from previous failed agent runs
        // - Assistant messages with empty tool_calls array
        if role == "assistant" {
            let is_empty = content_str.is_empty();
            let is_error = content_str.starts_with("[Error]");
            let tool_calls: Option<Vec<echo_agent::llm::types::ToolCall>> =
                m.tool_calls_json.as_ref().and_then(|j| serde_json::from_str(j).ok());
            let has_empty_tool_calls = tool_calls.as_ref().map_or(false, |tc| tc.is_empty());

            if is_empty && has_empty_tool_calls {
                continue; // Skip: empty assistant + empty tool_calls
            }
            if is_empty {
                continue; // Skip: assistant with no content
            }
            if is_error {
                // Replace error messages with a placeholder so conversation stays coherent
                messages.push(Message {
                    role,
                    content: Some("(上一轮对话出现问题，已跳过)".to_string()),
                    content_parts: None,
                    tool_calls: None, // Explicitly None, not empty array
                    name: None,
                    tool_call_id: None,
                });
                continue;
            }
            if has_empty_tool_calls {
                // Keep content but strip empty tool_calls
                messages.push(Message {
                    role,
                    content,
                    content_parts: None,
                    tool_calls: None,
                    name: None,
                    tool_call_id: None,
                });
                continue;
            }

            messages.push(Message {
                role,
                content,
                content_parts: None,
                tool_calls,
                name: None,
                tool_call_id: None,
            });
        } else {
            messages.push(Message {
                role,
                content,
                content_parts: None,
                tool_calls: m
                    .tool_calls_json
                    .and_then(|j| serde_json::from_str(&j).ok()),
                name: None,
                tool_call_id: None,
            });
        }
    }

    // 4. Inject into agent context (preserving system prompt)
    let mut agent = state.agent.lock().await;

    // Preserve the agent's current system prompt
    let system_prompt = agent.system_prompt().to_string();

    // Ensure system prompt is present as the first message
    if messages.first().map(|m| m.role.as_str()) != Some("system") {
        messages.insert(0, Message::system(system_prompt));
    }

    // Remove any existing system messages from loaded data to avoid duplicates
    messages.dedup_by(|a, b| a.role == "system" && b.role == "system");

    agent.load_messages(messages);

    Ok(Json(serde_json::json!({
        "success": true,
        "message_count": count,
        "conversation_id": id
    })))
}
