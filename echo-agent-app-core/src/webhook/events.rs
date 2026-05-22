//! Webhook 事件定义

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Webhook 事件类型
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WebhookEvent {
    /// 对话完成
    ChatCompleted {
        model: String,
        input_tokens: usize,
        output_tokens: usize,
        elapsed_ms: u64,
    },
    /// 工具调用成功
    ToolCalled {
        name: String,
        args_summary: String,
        elapsed_ms: u64,
    },
    /// 工具调用失败
    ToolFailed { name: String, error: String },
    /// Agent 错误
    AgentError { error: String },
    /// 定时任务完成
    CronTaskCompleted {
        task_id: String,
        task_name: String,
        result_summary: String,
    },
}

impl WebhookEvent {
    /// 事件名称（用于匹配订阅过滤）
    pub fn event_name(&self) -> &'static str {
        match self {
            WebhookEvent::ChatCompleted { .. } => "chat_completed",
            WebhookEvent::ToolCalled { .. } => "tool_called",
            WebhookEvent::ToolFailed { .. } => "tool_failed",
            WebhookEvent::AgentError { .. } => "agent_error",
            WebhookEvent::CronTaskCompleted { .. } => "cron_task_completed",
        }
    }
}

/// Webhook 投递载荷
#[derive(Debug, Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub timestamp: DateTime<Utc>,
    pub data: WebhookEvent,
}
