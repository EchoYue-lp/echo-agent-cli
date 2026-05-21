//! 聊天相关 Tauri 命令

use std::sync::Arc;
use echo_agent::prelude::{AgentEvent, Message};
use futures::StreamExt;
use tauri::{AppHandle, Emitter, State};

use super::super::state::TauriState;

static CANCEL_TOKENS: std::sync::LazyLock<Arc<tokio::sync::Mutex<std::collections::HashMap<String, bool>>>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())));

/// 流式聊天命令
#[tauri::command]
pub async fn chat_stream(
    app: AppHandle,
    state: State<'_, TauriState>,
    message: String,
    conversation_id: Option<String>,
) -> Result<(), String> {
    let agent = state.agent.inner().clone();
    let cid = conversation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // 注册取消标记
    {
        let mut tokens = CANCEL_TOKENS.lock().await;
        tokens.insert(cid.clone(), false);
    }

    let app_clone = app.clone();
    let cid_clone = cid.clone();
    let msg = Message::user(message);

    tokio::spawn(async move {
        let guard = agent.read().await;
        match guard.chat_stream_message(msg).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    // Check cancellation
                    {
                        let tokens = CANCEL_TOKENS.lock().await;
                        if tokens.get(&cid_clone).copied().unwrap_or(false) {
                            let _ = app_clone.emit("chat-event", serde_json::json!({
                                "conversation_id": cid_clone,
                                "type": "cancelled"
                            }));
                            break;
                        }
                    }
                    match result {
                        Ok(event) => {
                            let payload = agent_event_to_json(&cid_clone, &event);
                            let _ = app_clone.emit("chat-event", payload);
                        }
                        Err(e) => {
                            let _ = app_clone.emit("chat-event", serde_json::json!({
                                "conversation_id": cid_clone,
                                "type": "error",
                                "error": e.to_string()
                            }));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = app_clone.emit("chat-event", serde_json::json!({
                    "conversation_id": cid_clone,
                    "type": "error",
                    "error": e.to_string()
                }));
            }
        }
        let mut tokens = CANCEL_TOKENS.lock().await;
        tokens.remove(&cid_clone);
    });

    Ok(())
}

/// 取消指定对话的流式响应
#[tauri::command]
pub async fn cancel_chat(conversation_id: String) -> Result<(), String> {
    let mut tokens = CANCEL_TOKENS.lock().await;
    if let Some(flag) = tokens.get_mut(&conversation_id) {
        *flag = true;
    }
    Ok(())
}

fn agent_event_to_json(cid: &str, event: &AgentEvent) -> serde_json::Value {
    match event {
        AgentEvent::Token(t) => serde_json::json!({
            "conversation_id": cid, "type": "token", "data": t
        }),
        AgentEvent::ThinkStart => serde_json::json!({
            "conversation_id": cid, "type": "think_start"
        }),
        AgentEvent::ThinkEnd { prompt_tokens, completion_tokens } => serde_json::json!({
            "conversation_id": cid, "type": "think_end",
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens
        }),
        AgentEvent::ToolCall { name, args } => serde_json::json!({
            "conversation_id": cid, "type": "tool_call",
            "name": name, "args": args
        }),
        AgentEvent::ToolResult { name, output } => serde_json::json!({
            "conversation_id": cid, "type": "tool_result",
            "name": name, "output": output
        }),
        AgentEvent::ToolError { name, error } => serde_json::json!({
            "conversation_id": cid, "type": "tool_error",
            "name": name, "error": error
        }),
        AgentEvent::FinalAnswer(d) => serde_json::json!({
            "conversation_id": cid, "type": "final_answer", "data": d
        }),
        AgentEvent::Cancelled => serde_json::json!({
            "conversation_id": cid, "type": "cancelled"
        }),
        AgentEvent::PlanGenerated { steps } => serde_json::json!({
            "conversation_id": cid, "type": "plan", "steps": steps
        }),
        AgentEvent::StepStart { step_index, description } => serde_json::json!({
            "conversation_id": cid, "type": "step_start",
            "step_index": step_index, "description": description
        }),
        AgentEvent::ContextCompressed { before_count, after_count, before_tokens, after_tokens } => serde_json::json!({
            "conversation_id": cid, "type": "context_compressed",
            "before_count": before_count, "after_count": after_count,
            "before_tokens": before_tokens, "after_tokens": after_tokens
        }),
        _ => serde_json::json!({"conversation_id": cid, "type": "unknown"}),
    }
}
