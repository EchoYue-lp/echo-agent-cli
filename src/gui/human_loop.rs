//! GUI 人工介入处理器
//!
//! 实现 HumanLoopProvider trait，通过 egui 对话框与用户交互。

use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// GUI 中待处理的人机交互请求
#[derive(Clone)]
pub struct GuiHumanLoopRequest {
    pub request_id: String,
    pub kind: GuiRequestKind,
}

#[derive(Clone)]
pub enum GuiRequestKind {
    Approval {
        tool_name: String,
        args: Value,
        prompt: Option<String>,
    },
    Input {
        prompt: Option<String>,
    },
}

/// GUI 人机交互响应
pub enum GuiHumanLoopResponse {
    Approval { approved: bool, reason: Option<String> },
    Input { text: String },
}

/// GUI 人工介入处理器（进程内，通过 channel 与 egui 通信）
pub struct GuiHumanLoopHandler {
    /// 待处理的请求队列（发送给 egui 线程）
    pending_requests: Arc<Mutex<Vec<GuiHumanLoopRequest>>>,
    /// 响应通道：request_id → oneshot sender
    responders: Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<GuiHumanLoopResponse>>>>,
}

impl GuiHumanLoopHandler {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(Vec::new())),
            responders: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// 获取并清除所有待处理请求（egui 线程调用）
    pub fn drain_requests(&self) -> Vec<GuiHumanLoopRequest> {
        self.pending_requests.lock().unwrap().drain(..).collect()
    }

    /// 发送审批响应（egui 线程调用）
    pub fn send_approval(&self, request_id: &str, approved: bool, reason: Option<String>) {
        if let Some(tx) = self.responders.lock().unwrap().remove(request_id) {
            let _ = tx.send(GuiHumanLoopResponse::Approval { approved, reason });
        }
    }

    /// 发送输入响应（egui 线程调用）
    pub fn send_input(&self, request_id: &str, text: String) {
        if let Some(tx) = self.responders.lock().unwrap().remove(request_id) {
            let _ = tx.send(GuiHumanLoopResponse::Input { text });
        }
    }
}

impl HumanLoopProvider for GuiHumanLoopHandler {
    fn request(
        &self,
        req: HumanLoopRequest,
    ) -> BoxFuture<'_, echo_agent::error::Result<HumanLoopResponse>> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        let gui_req = GuiHumanLoopRequest {
            request_id: request_id.clone(),
            kind: match req.kind {
                echo_agent::human_loop::HumanLoopKind::Approval => GuiRequestKind::Approval {
                    tool_name: req.tool_name.clone().unwrap_or_default(),
                    args: req.args.clone().unwrap_or(Value::Null),
                    prompt: Some(req.prompt.clone()),
                },
                echo_agent::human_loop::HumanLoopKind::Input => GuiRequestKind::Input {
                    prompt: Some(req.prompt.clone()),
                },
            },
        };

        // 注册响应通道
        self.responders
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);

        // 添加到待处理队列
        self.pending_requests.lock().unwrap().push(gui_req);

        Box::pin(async move {
            // 等待用户响应（5 分钟超时）
            match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok(GuiHumanLoopResponse::Approval { approved, reason })) => {
                    if approved {
                        Ok(HumanLoopResponse::Approved)
                    } else {
                        Ok(HumanLoopResponse::Rejected { reason })
                    }
                }
                Ok(Ok(GuiHumanLoopResponse::Input { text })) => {
                    Ok(HumanLoopResponse::Text(text))
                }
                _ => Ok(HumanLoopResponse::Timeout),
            }
        })
    }
}
