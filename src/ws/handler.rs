//! WebSocket 连接处理

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::state::AppState;
use crate::types::{ClientMessage, ServerMessage};
use crate::ws::WsHumanLoopHandler;

/// GET /ws/chat - WebSocket 流式对话
pub async fn ws_chat_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let session_id = Uuid::new_v4().to_string();
    let session_id_clone = session_id.clone();

    tracing::info!("WebSocket 连接建立: {}", session_id);

    // 创建消息通道
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // 创建人工介入处理器
    let human_loop_handler = Arc::new(WsHumanLoopHandler::new(tx.clone()));

    // 发送任务：将 ServerMessage 发送到 WebSocket
    let send_task = async move {
        while let Some(msg) = rx.recv().await {
            let json = serde_json::to_string(&msg).unwrap();
            if ws_tx.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    };

    // 接收任务：处理客户端消息
    let human_loop_clone = human_loop_handler.clone();
    let recv_task = async move {
        while let Some(msg_result) = ws_rx.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                        handle_client_message(client_msg, &tx, &state, &human_loop_clone).await;
                    }
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("WebSocket 客户端关闭连接: {}", session_id_clone);
                    break;
                }
                Err(e) => {
                    tracing::error!("WebSocket 错误: {}", e);
                    break;
                }
                _ => {}
            }
        }
    };

    // 并行运行发送和接收任务
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    tracing::info!("WebSocket 连接关闭: {}", session_id);
}

async fn handle_client_message(
    msg: ClientMessage,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    state: &Arc<AppState>,
    human_loop_handler: &Arc<WsHumanLoopHandler>,
) {
    match msg {
        ClientMessage::Message { data } => {
            handle_chat_message(data, tx, state.clone(), human_loop_handler.clone()).await;
        }
        ClientMessage::ApprovalResponse { request_id, approved, reason } => {
            tracing::info!(
                "审批响应: request_id={}, approved={}, reason={:?}",
                request_id,
                approved,
                reason
            );
            human_loop_handler.handle_approval_response(&request_id, approved, reason).await;
        }
        ClientMessage::InputResponse { request_id, text } => {
            tracing::info!("输入响应: request_id={}, text={}", request_id, text);
            human_loop_handler.handle_input_response(&request_id, text).await;
        }
        ClientMessage::Cancel {} => {
            tracing::info!("收到取消请求");
            let _ = tx.send(ServerMessage::Cancelled {});
        }
    }
}

async fn handle_chat_message(
    message: String,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    state: Arc<AppState>,
    human_loop_handler: Arc<WsHumanLoopHandler>,
) {
    use echo_agent::prelude::*;
    use echo_agent::agent::Agent;
    use futures::StreamExt;

    let mut agent = state.agent.lock().await;

    // 将 human_loop_handler 设置为 agent 的人工介入 provider
    agent.set_human_loop_provider(human_loop_handler);

    match agent.chat_stream(&message).await {
        Ok(mut stream) => {
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        let server_msg = match event {
                            AgentEvent::Token(data) => ServerMessage::Token { data },
                            AgentEvent::ToolCall { name, args } => {
                                ServerMessage::ToolStart {
                                    name,
                                    args,
                                }
                            }
                            AgentEvent::ToolResult { name, output } => {
                                ServerMessage::ToolResult {
                                    name,
                                    result: output,
                                    success: true,
                                }
                            }
                            AgentEvent::FinalAnswer(data) => ServerMessage::FinalAnswer { data },
                            AgentEvent::Cancelled => ServerMessage::Cancelled {},
                            _ => continue, // 忽略其他事件类型
                        };

                        if tx.send(server_msg).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                        break;
                    }
                }
            }
        }
        Err(e) => {
            let _ = tx.send(ServerMessage::Error {
                message: e.to_string(),
            });
        }
    }
}