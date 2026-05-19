//! WebSocket 人工介入处理器
//!
//! 实现 HumanLoopProvider trait，通过 WebSocket 与前端交互

use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use uuid::Uuid;

use crate::types::ServerMessage;

/// 等待响应的请求
pub struct PendingRequest {
    /// 响应通道
    pub responder: oneshot::Sender<PendingResponse>,
}

/// 客户端响应
pub enum PendingResponse {
    Approval { approved: bool, reason: Option<String> },
    Input { text: String },
}

/// WebSocket 人工介入处理器
pub struct WsHumanLoopHandler {
    /// 服务端 -> 客户端 消息通道
    tx: mpsc::UnboundedSender<ServerMessage>,
    /// 等待响应的请求映射
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
}

impl WsHumanLoopHandler {
    pub fn new(tx: mpsc::UnboundedSender<ServerMessage>) -> Self {
        Self {
            tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 处理客户端的审批响应
    pub async fn handle_approval_response(
        &self,
        request_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> bool {
        let pending = self.pending.lock().await.remove(request_id);
        if let Some(req) = pending {
            let _ = req.responder.send(PendingResponse::Approval { approved, reason });
            return true;
        }
        false
    }

    /// 处理客户端的输入响应
    pub async fn handle_input_response(&self, request_id: &str, text: String) -> bool {
        let pending = self.pending.lock().await.remove(request_id);
        if let Some(req) = pending {
            let _ = req.responder.send(PendingResponse::Input { text });
            return true;
        }
        false
    }
}

impl HumanLoopProvider for WsHumanLoopHandler {
    fn request(&self, req: HumanLoopRequest) -> BoxFuture<'_, echo_agent::error::Result<HumanLoopResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx_response, rx_response) = oneshot::channel();
        let tx = self.tx.clone();
        let pending = self.pending.clone();

        Box::pin(async move {
            match req.kind {
                echo_agent::human_loop::HumanLoopKind::Approval => {
                    let tool_name = req.tool_name.clone().unwrap_or_default();
                    let args = req.args.clone().unwrap_or(Value::Null);

                    let msg = ServerMessage::ApprovalRequest {
                        id: None,
                        request_id: request_id.clone(),
                        tool_name,
                        args,
                        prompt: req.prompt.clone(),
                    };

                    if tx.send(msg).is_err() {
                        return Err(echo_agent::error::ReactError::Other(
                            "WebSocket channel closed".to_string(),
                        ));
                    }

                    let pending_req = PendingRequest {
                        responder: tx_response,
                    };
                    pending.lock().await.insert(request_id.clone(), pending_req);

                    // Use select! for atomic timeout vs response handling,
                    // eliminating the TOCTOU window where a client response
                    // could arrive after timeout fires but before pending cleanup.
                    let result = tokio::select! {
                        response = rx_response => {
                            if let Ok(PendingResponse::Approval { approved, reason }) = response {
                                if approved {
                                    Ok(HumanLoopResponse::Approved)
                                } else {
                                    Ok(HumanLoopResponse::Rejected { reason })
                                }
                            } else {
                                // Sender dropped or unexpected variant
                                Ok(HumanLoopResponse::Timeout)
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            // Timeout — clean up pending to prevent stale entries
                            pending.lock().await.remove(&request_id);
                            Ok(HumanLoopResponse::Timeout)
                        }
                    };
                    result
                }
                echo_agent::human_loop::HumanLoopKind::Input => {
                    let msg = ServerMessage::InputRequest {
                        id: None,
                        request_id: request_id.clone(),
                        prompt: req.prompt.clone(),
                    };

                    if tx.send(msg).is_err() {
                        return Err(echo_agent::error::ReactError::Other(
                            "WebSocket channel closed".to_string(),
                        ));
                    }

                    let pending_req = PendingRequest {
                        responder: tx_response,
                    };
                    pending.lock().await.insert(request_id.clone(), pending_req);

                    // Use select! for atomic timeout vs response handling.
                    let result = tokio::select! {
                        response = rx_response => {
                            if let Ok(PendingResponse::Input { text }) = response {
                                Ok(HumanLoopResponse::Text(text))
                            } else {
                                Ok(HumanLoopResponse::Text(String::new()))
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            pending.lock().await.remove(&request_id);
                            Ok(HumanLoopResponse::Text(String::new()))
                        }
                    };
                    result
                }
            }
        })
    }
}
