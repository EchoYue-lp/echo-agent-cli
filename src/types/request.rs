//! 请求类型定义

use serde::Deserialize;
use std::collections::HashMap;

// ── 对话相关 ─────────────────────────────────────────────────

/// POST /api/chat
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

// ── MCP 相关 ─────────────────────────────────────────────────

/// POST /api/mcp/connect
#[derive(Debug, Deserialize)]
pub struct ConnectMcpRequest {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
}

/// MCP 传输配置
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

// ── 配置相关 ─────────────────────────────────────────────────

/// PUT /api/config
#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub max_iterations: Option<usize>,
    pub token_limit: Option<usize>,
}

// ── WebSocket 消息类型 ─────────────────────────────────────────────────

/// 客户端 -> 服务端 WebSocket 消息
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 发送消息
    Message { data: String },

    /// 审批响应
    ApprovalResponse {
        request_id: String,
        approved: bool,
        reason: Option<String>,
    },

    /// 输入响应
    InputResponse { request_id: String, text: String },

    /// 取消执行
    Cancel {},
}