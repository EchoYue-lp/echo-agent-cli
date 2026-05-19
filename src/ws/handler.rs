//! WebSocket 连接处理
//!
//! 此模块管理 WebSocket 生命周期，负责：
//! - 将客户端消息路由到对应的处理器
//! - 将服务端流式事件推送给客户端
//! - 保持接收端始终可读，避免阻塞导致连接重置

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::state::{AppState, AuditDecision, AuditLogEntry};
use crate::types::{AttachmentData, ClientMessage, ServerMessage};
use crate::ws::WsHumanLoopHandler;

/// GET /ws/chat - WebSocket 流式对话
pub async fn ws_chat_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let session_id = Uuid::new_v4().to_string();

    tracing::info!("WebSocket 连接建立: {}", session_id);

    // 创建消息通道
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // 创建人工介入处理器
    let human_loop_handler = Arc::new(WsHumanLoopHandler::new(tx.clone()));

    // 跟踪当前活跃的 chat 任务，以便在客户端断开时取消
    let active_chat: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(None));

    // 发送任务：将 ServerMessage 发送到 WebSocket，每 30s 发一次 ping 保活
    let send_task = {
        let mut ws_tx = ws_tx; // move into async block
        async move {
            let ping_interval = std::time::Duration::from_secs(30);
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                let json = match serde_json::to_string(&msg) {
                                    Ok(j) => j,
                                    Err(e) => {
                                        tracing::error!("WebSocket message serialization failed: {}", e);
                                        continue;
                                    }
                                };
                                if ws_tx.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                            None => break, // channel closed
                        }
                    }
                    _ = tokio::time::sleep(ping_interval) => {
                        // tungstenite 自动回复 Pong，无需额外处理
                        if ws_tx.send(Message::Ping(vec![])).await.is_err() {
                            break;
                        }
                        tracing::debug!("WebSocket ping sent");
                    }
                }
            }
            // 发送 close frame 后再断开
            let _ = ws_tx.send(Message::Close(None)).await;
        }
    };

    // 接收任务：持续读取客户端消息，chat 请求 spawn 到独立 task
    let recv_task = {
        let session_id = session_id.clone();
        let active_chat = active_chat.clone();
        async move {
            while let Some(msg_result) = ws_rx.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            match client_msg {
                                // 对话消息：spawn 独立 task，不阻塞接收循环
                                ClientMessage::Message { id, data, attachments } => {
                                    let task_tx = tx.clone();
                                    let task_state = state.clone();
                                    let task_human_loop = human_loop_handler.clone();
                                    let handle = tokio::spawn(async move {
                                        handle_chat_message(
                                            id, data, attachments,
                                            &task_tx, task_state, task_human_loop,
                                        ).await;
                                    });
                                    *active_chat.lock().await = Some(handle);
                                }
                                ClientMessage::ApprovalResponse { request_id, approved, reason, .. } => {
                                    tracing::info!(
                                        "审批响应: request_id={}, approved={}, reason={:?}",
                                        request_id, approved, reason
                                    );
                                    // 记录审计日志
                                    state.add_audit_entry(AuditLogEntry {
                                        id: Uuid::new_v4().to_string(),
                                        tool_name: String::new(),
                                        args_hash: String::new(),
                                        decision: if approved { AuditDecision::Allow } else { AuditDecision::Deny },
                                        reason: reason.clone().unwrap_or_default(),
                                        source: "websocket".to_string(),
                                        duration_us: 0,
                                        elapsed_ms: 0,
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    }).await;
                                    human_loop_handler
                                        .handle_approval_response(&request_id, approved, reason)
                                        .await;
                                }
                                ClientMessage::InputResponse { request_id, text, .. } => {
                                    tracing::debug!(
                                        "输入响应: request_id={}, text_length={}",
                                        request_id, text.len()
                                    );
                                    human_loop_handler
                                        .handle_input_response(&request_id, text)
                                        .await;
                                }
                                ClientMessage::Cancel { id } => {
                                    tracing::info!("收到取消请求");
                                    let cancel_key = id.clone().unwrap_or_default();
                                    if let Some(token) = state.session.cancel_token.get(&cancel_key) {
                                        token.cancel();
                                    }
                                    let _ = tx.send(ServerMessage::Cancelled { id });
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        tracing::info!("WebSocket 客户端正常关闭: {}", session_id);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("WebSocket 错误: {}", e);
                        break;
                    }
                    _ => {}
                }
            }

            // 客户端断开时，取消正在进行的 chat 任务
            if let Some(handle) = active_chat.lock().await.take() {
                handle.abort();
            }
        }
    };

    // 并行运行发送和接收任务
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    // 确保 chat 任务被清理
    if let Some(handle) = active_chat.lock().await.take() {
        handle.abort();
    }

    tracing::info!("WebSocket 连接关闭: {}", session_id);
}

use base64::Engine;
use echo_agent::llm::types::{ContentPart, ImageUrl, Message as LlmMessage};
use std::path::PathBuf;

/// 判断是否为图片 MIME 类型
fn is_image_mime(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// 构建 data URL
fn build_data_url(mime: &str, data: &str) -> String {
    format!("data:{mime};base64,{data}")
}

/// 获取上传文件存放的临时目录
fn upload_dir() -> PathBuf {
    std::env::temp_dir().join("echo-agent-uploads")
}

/// 清理所有过期的上传目录（应在服务启动时调用）
pub async fn cleanup_stale_uploads() {
    let root = upload_dir();
    if !root.exists() {
        return;
    }
    match tokio::fs::remove_dir_all(&root).await {
        Ok(()) => tracing::info!("Cleaned up stale upload directory: {}", root.display()),
        Err(e) => tracing::warn!("Failed to clean up upload directory {}: {}", root.display(), e),
    }
}

/// 将附件保存到磁盘，返回文件路径
fn save_attachment_to_disk(
    upload_dir: &std::path::Path,
    att: &AttachmentData,
    max_size: u64,
) -> Result<String, String> {
    use std::fs;

    // 检查文件大小限制
    if att.size > max_size {
        return Err(format!(
            "文件大小 {} 字节超过限制 {} 字节 ({} MB)",
            att.size,
            max_size,
            max_size / (1024 * 1024)
        ));
    }

    fs::create_dir_all(upload_dir).map_err(|e| format!("创建上传目录失败: {e}"))?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&att.data)
        .map_err(|e| format!("Base64 解码失败: {e}"))?;

    let safe_name = std::path::Path::new(&att.name)
        .file_name()
        .ok_or_else(|| format!("非法文件名: {}", att.name))?;

    // 规范化上传目录，失败则拒绝（不再 fallback 到未验证路径）
    let canonical_upload = upload_dir
        .canonicalize()
        .map_err(|e| format!("无法解析上传目录: {e}"))?;

    // file 尚不存在，无法直接 canonicalize。改为规范化父目录后拼接文件名。
    let candidate = upload_dir.join(safe_name);
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("文件路径无父目录: {}", att.name))?;
    fs::create_dir_all(parent).map_err(|e| format!("创建上传父目录失败: {e}"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("无法解析上传父目录: {e}"))?;
    let resolved = canonical_parent.join(safe_name);

    // 路径穿越检查
    if !resolved.starts_with(&canonical_upload) {
        return Err(format!("文件路径越界: {}", att.name));
    }

    // 如果同名文件已存在，添加数字后缀
    let file_path = if resolved.exists() {
        let stem = resolved
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let ext = resolved
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        let mut counter = 1u32;
        loop {
            let candidate = upload_dir.join(format!("{}_{}{}", stem, counter, ext));
            if !candidate.exists() {
                break candidate;
            }
            counter += 1;
        }
    } else {
        resolved
    };

    fs::write(&file_path, &bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(file_path.to_string_lossy().to_string())
}

/// 从附件构建多模态 ContentPart 列表（文件先保存到磁盘）
fn build_attachment_parts(
    attachments: &[AttachmentData],
    text: &str,
    upload_dir: &std::path::Path,
    max_upload_size: u64,
) -> Vec<ContentPart> {
    let mut parts = Vec::with_capacity(attachments.len() + 1);
    parts.push(ContentPart::Text {
        text: text.to_string(),
    });

    for att in attachments {
        if is_image_mime(&att.mime_type) {
            // 图片：使用 ContentPart::ImageUrl 多模态格式
            parts.push(ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: build_data_url(&att.mime_type, &att.data),
                    detail: None,
                },
            });
        } else {
            // 非图片文件：先保存到磁盘，再构建文本提示
            let saved_path = save_attachment_to_disk(upload_dir, att, max_upload_size);

            match saved_path {
                Ok(path) => {
                    // 尝试解码文本内容作为预览
                    let preview = base64::engine::general_purpose::STANDARD
                        .decode(&att.data)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .map(|content| {
                            let lines: Vec<&str> = content.lines().take(50).collect();
                            format!(
                                "\n--- 文件内容预览 (前 {} 行) ---\n{}\n--- 预览结束 ---",
                                lines.len(),
                                lines.join("\n")
                            )
                        })
                        .unwrap_or_default();

                    parts.push(ContentPart::Text {
                        text: format!(
                            "\n[已上传文件: {} ({} 字节, {})]\n文件已保存至: {}\n请使用工具读取此文件进行分析。{}\n",
                            att.name, att.size, att.mime_type, path, preview,
                        ),
                    });
                }
                Err(e) => {
                    parts.push(ContentPart::Text {
                        text: format!(
                            "\n[附件上传失败: {}] 错误: {}\n",
                            att.name, e
                        ),
                    });
                }
            }
        }
    }
    parts
}

async fn handle_chat_message(
    id: Option<String>,
    message: String,
    attachments: Vec<AttachmentData>,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    state: Arc<AppState>,
    human_loop_handler: Arc<WsHumanLoopHandler>,
) {
    use echo_agent::prelude::*;
    use echo_agent::agent::Agent;
    use futures::StreamExt;

    let cancel_token = echo_agent::agent::CancellationToken::new();
    let message_key = id.clone().unwrap_or_default();
    {
        state.session.cancel_token.insert(message_key.clone(), cancel_token.clone());
    }

    // Brief write lock to configure human-in-the-loop provider.
    // SAFETY NOTE: In multi-WS-session scenarios, this overwrites a global provider.
    // This is acceptable for single-user local mode; for multi-user deployment,
    // use per-session agent instances or a session-keyed provider map.
    state.connection.agent.write(|a| {
        a.set_human_loop_provider(human_loop_handler);
    }).await;

    // 使用 inner() 逃生舱口：chat_stream_with_cancel 返回的流同时借用
    // agent 和 message 字符串，其生命周期无法用 read_async 的 HRTB 表达。
    // 注意：agent guard 必须存活到 stream 处理完毕。
    let agent = state.connection.agent.inner().read().await;

    // Track session upload dir for cleanup after stream finishes
    let mut session_upload_dir: Option<std::path::PathBuf> = None;

    let stream_result = if attachments.is_empty() {
        agent.chat_stream_with_cancel(&message, cancel_token.clone()).await
    } else {
        let dir = upload_dir().join(Uuid::new_v4().to_string());
        let max_upload_size = state.config.web_config.read().await.max_upload_size_bytes;
        let parts = build_attachment_parts(&attachments, &message, &dir, max_upload_size);
        session_upload_dir = Some(dir);
        let msg = LlmMessage::user_multimodal(parts);
        agent.chat_stream_message(msg).await
    };
    // agent guard 在此之后仍然存活，stream 处理完毕后自动 drop

    // 流结束后清除此消息的取消令牌
    {
        state.session.cancel_token.remove(&message_key);
    }

    // Clean up session upload directory
    if let Some(ref dir) = session_upload_dir
        && let Err(e) = tokio::fs::remove_dir_all(dir).await {
            tracing::warn!(dir = %dir.display(), error = %e, "Failed to clean up session upload directory");
        }

    match stream_result {
        Ok(mut stream) => {
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        let server_msg = match event {
                            AgentEvent::Token(data) => ServerMessage::Token { id: id.clone(), data },
                            AgentEvent::ThinkStart => ServerMessage::ThinkingStart { id: id.clone() },
                            AgentEvent::ThinkEnd { prompt_tokens, completion_tokens } => {
                                ServerMessage::ThinkingEnd { id: id.clone(), prompt_tokens, completion_tokens }
                            }
                            AgentEvent::ToolCall { name, args } => {
                                ServerMessage::ToolStart {
                                    id: id.clone(),
                                    name,
                                    args,
                                }
                            }
                            AgentEvent::ToolResult { name, output } => {
                                ServerMessage::ToolResult {
                                    id: id.clone(),
                                    name,
                                    result: output,
                                    success: true,
                                }
                            }
                            AgentEvent::ToolError { name, error } => {
                                ServerMessage::ToolResult {
                                    id: id.clone(),
                                    name,
                                    result: error,
                                    success: false,
                                }
                            }
                            AgentEvent::MemoryRecalled { count } => {
                                tracing::debug!(count = count, "Memory recalled event received");
                                continue
                            }
                            AgentEvent::Chart { spec } => ServerMessage::Chart { id: id.clone(), spec },
                            AgentEvent::FinalAnswer(data) => ServerMessage::FinalAnswer { id: id.clone(), data },
                            AgentEvent::Cancelled => ServerMessage::Cancelled { id: id.clone() },
                            _ => continue, // 忽略其他事件类型
                        };

                        if tx.send(server_msg).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ServerMessage::Error {
                            id: id.clone(),
                            message: e.to_string(),
                        });
                        break;
                    }
                }
            }
        }
        Err(e) => {
            let _ = tx.send(ServerMessage::Error {
                id,
                message: e.to_string(),
            });
        }
    }
}