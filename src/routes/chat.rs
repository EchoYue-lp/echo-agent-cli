//! 对话 API

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use futures::StreamExt;
use serde_json::Value;
use echo_agent::agent::Agent;

use crate::error::WebError;
use crate::state::AppState;
use crate::types::{ChatRequest, ChatResponse, ContextStats, ToolCallInfo};

/// POST /api/chat - 阻塞式对话
#[debug_handler]
pub async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let mut agent = state.agent.lock().await;

    // 收集工具调用信息
    let mut tool_calls = Vec::new();

    // 执行并收集事件
    let answer = {
        let stream_result = agent.execute_stream(&req.message).await;
        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => return WebError::Agent(e).into_response(),
        };

        let mut final_answer = String::new();
        let mut current_tool: Option<(String, Value)> = None;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    match event {
                        echo_agent::prelude::AgentEvent::Token(_) => {
                            // 阻塞模式不收集 token
                        }
                        echo_agent::prelude::AgentEvent::ToolCall { name, args } => {
                            current_tool = Some((name, args));
                        }
                        echo_agent::prelude::AgentEvent::ToolResult { name: _, output } => {
                            if let Some((tool_name, args)) = current_tool.take() {
                                tool_calls.push(ToolCallInfo {
                                    name: tool_name,
                                    args,
                                    result: output,
                                    success: true,
                                });
                            }
                        }
                        echo_agent::prelude::AgentEvent::FinalAnswer(data) => {
                            final_answer = data;
                        }
                        echo_agent::prelude::AgentEvent::Cancelled => {
                            break;
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    return WebError::Agent(e).into_response();
                }
            }
        }

        final_answer
    };

    // 获取上下文统计
    let (message_count, estimated_tokens) = agent.context_stats();
    let context_stats = ContextStats {
        message_count,
        estimated_tokens,
    };

    Json(ChatResponse {
        answer,
        tool_calls,
        iterations: agent.config().get_max_iterations(), // 使用配置的最大迭代次数
        context_stats,
    }).into_response()
}