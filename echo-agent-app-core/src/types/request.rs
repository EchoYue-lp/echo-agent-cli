//! 请求类型定义

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

// ── 对话相关 ─────────────────────────────────────────────────

/// POST /api/chat
#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "ChatRequest")]
pub struct ChatRequest {
    pub message: String,
    /// Optional session ID — when provided, the agent's conversation history
    /// is restored from the persisted session before processing this message.
    #[serde(default)]
    pub session_id: Option<String>,
}

// ── MCP 相关 ─────────────────────────────────────────────────

/// POST /api/mcp/connect
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, rename = "ConnectMcpRequest")]
pub struct ConnectMcpRequest {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
}

/// MCP 传输配置
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, rename = "McpTransportConfig")]
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
#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "UpdateConfigRequest")]
pub struct UpdateConfigRequest {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub token_limit: Option<usize>,
}

/// PUT /api/config/full — 完整配置更新
#[derive(Debug, Deserialize, Default, TS)]
#[ts(export, rename = "UpdateFullConfigRequest")]
pub struct UpdateFullConfigRequest {
    pub model: Option<ModelUpdate>,
    pub agent: Option<AgentUpdate>,
    pub mcp: Option<McpUpdate>,
    pub channels: Option<ChannelsUpdate>,
    pub server: Option<ServerUpdate>,
    pub logging: Option<LoggingUpdate>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "ModelUpdate")]
pub struct ModelUpdate {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "AgentUpdate")]
pub struct AgentUpdate {
    pub name: Option<String>,
    pub system_prompt: Option<String>,
    pub max_iterations: Option<usize>,
    pub enable_tools: Option<bool>,
    pub enable_memory: Option<bool>,
    pub enable_human_in_loop: Option<bool>,
    pub memory_path: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "McpUpdate")]
pub struct McpUpdate {
    pub config_path: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "ChannelsUpdate")]
pub struct ChannelsUpdate {
    pub qq: Option<QqUpdate>,
    pub feishu: Option<FeishuUpdate>,
    pub session: Option<SessionUpdate>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "QqUpdate")]
pub struct QqUpdate {
    pub enabled: Option<bool>,
    pub app_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "FeishuUpdate")]
pub struct FeishuUpdate {
    pub enabled: Option<bool>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "SessionUpdate")]
pub struct SessionUpdate {
    pub timeout_minutes: Option<u64>,
    pub reset_keywords: Option<Vec<String>>,
    pub reset_commands: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "ServerUpdate")]
pub struct ServerUpdate {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "LoggingUpdate")]
pub struct LoggingUpdate {
    pub level: Option<String>,
}

// ── WebSocket 消息类型 ─────────────────────────────────────────────────

/// 附件数据
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AttachmentSource")]
pub enum AttachmentSource {
    #[default]
    Upload,
    Paste,
    Channel,
    Message,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, rename = "AttachmentData")]
pub struct AttachmentData {
    pub name: String,
    pub mime_type: String,
    /// Base64 编码的文件内容
    pub data: String,
    pub size: u64,
    #[serde(default)]
    pub source: AttachmentSource,
}

/// 客户端 -> 服务端 WebSocket 消息
#[derive(Debug, Deserialize, TS)]
#[ts(export, rename = "ClientMessage")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 发送消息（可带附件）
    Message {
        #[serde(default)]
        id: Option<String>,
        data: String,
        #[serde(default)]
        attachments: Vec<AttachmentData>,
    },

    /// 审批响应
    ApprovalResponse {
        #[serde(default)]
        id: Option<String>,
        request_id: String,
        approved: bool,
        reason: Option<String>,
    },

    /// 输入响应
    InputResponse {
        #[serde(default)]
        id: Option<String>,
        request_id: String,
        text: String,
    },

    /// 取消执行
    Cancel {
        #[serde(default)]
        id: Option<String>,
    },

    /// 心跳检测
    Ping,
}
