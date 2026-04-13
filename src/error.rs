//! 错误类型定义

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

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

    #[error("内部错误: {0}")]
    Internal(String),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            WebError::ToolNotFound(_)
            | WebError::McpServerNotFound(_)
            | WebError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        let body = Json(json!({
            "error": message,
            "code": status.as_u16(),
        }));

        (status, body).into_response()
    }
}

/// 兼容别名
pub type AppError = WebError;