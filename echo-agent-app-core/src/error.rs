//! 错误类型定义

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use ts_rs::TS;

/// 统一的 API 错误响应结构
#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ApiError")]
pub struct ApiError {
    /// 错误码，如 "VALIDATION_ERROR", "NOT_FOUND", "INTERNAL_ERROR"
    pub code: String,
    /// 人类可读的错误消息
    pub message: String,
    /// HTTP 状态码
    pub status: u16,
    /// 可选的调试信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// 请求追踪 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// 统一的流式事件（SSE 和 WebSocket 共用）
#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "StreamingEvent")]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamingEvent {
    /// Token 片段
    Token { data: String },
    /// 工具开始执行
    ToolStart { name: String, args: serde_json::Value },
    /// 工具执行结果
    ToolResult { name: String, result: String, success: bool },
    /// 最终答案
    FinalAnswer { data: String },
    /// 错误
    Error { message: String },
    /// 审批请求
    ApprovalRequest { request_id: String, tool_name: String, args: serde_json::Value, prompt: String },
    /// 输入请求
    InputRequest { request_id: String, prompt: String },
    /// 图表
    Chart { spec: serde_json::Value },
    /// 执行被取消
    Cancelled,
    /// 思考阶段开始
    ThinkingStart,
    /// 思考阶段结束
    ThinkingEnd { prompt_tokens: usize, completion_tokens: usize },
    /// 流式结束标记
    Done,
}

/// Web CLI 错误类型
#[derive(Debug, Error)]
pub enum WebError {
    #[error("Agent 错误: {0}")]
    Agent(#[from] echo_agent::error::ReactError),

    #[error("JSON 序列化错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("工具未找到: {0}")]
    ToolNotFound(String),

    #[error("MCP 服务端未找到: {0}")]
    McpServerNotFound(String),

    #[error("资源未找到: {0}")]
    NotFound(String),

    #[error("请求验证失败: {0}")]
    Validation(String),

    #[error("内部错误: {0}")]
    Internal(String),

    #[error("认证失败: {0}")]
    Auth(String),

    #[error("令牌过期")]
    TokenExpired,

    #[error("速率限制超出")]
    RateLimitExceeded,
}

impl WebError {
    /// 转换为结构化的 ApiError
    pub fn to_api_error(&self, request_id: Option<String>) -> ApiError {
        let (status, code) = match self {
            WebError::ToolNotFound(_) | WebError::McpServerNotFound(_) | WebError::NotFound(_) => {
                (StatusCode::NOT_FOUND, "NOT_FOUND")
            }
            WebError::Validation(_) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            WebError::Auth(_) => (StatusCode::UNAUTHORIZED, "AUTH_ERROR"),
            WebError::TokenExpired => (StatusCode::UNAUTHORIZED, "TOKEN_EXPIRED"),
            WebError::RateLimitExceeded => (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMIT_EXCEEDED"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        };

        ApiError {
            code: code.to_string(),
            message: self.to_string(),
            status: status.as_u16(),
            details: None,
            request_id,
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let api_error = self.to_api_error(None);
        let status = StatusCode::from_u16(api_error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = Json(&api_error);
        (status, body).into_response()
    }
}

/// 兼容别名
pub type AppError = WebError;
