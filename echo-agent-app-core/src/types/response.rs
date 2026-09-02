//! 响应类型定义

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Lifecycle phase returned by the GUI active-steer command. The command
/// waits for the framework tracked receipt and never reports an unconfirmed
/// mailbox as a successful continuation.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ChatSteerPhase")]
pub enum ChatSteerPhase {
    Drained,
    TurnSettled,
}

/// Framework terminal outcome of the turn that owned the active steer.
///
/// `ChatSteerOutcome` remains the stable EKO wire name, while its Rust value
/// is the framework authority directly. This response has no product-specific
/// outcome semantics that would justify a second enum or a conversion helper.
pub use echo_agent::agent::AgentSteerTurnOutcome as ChatSteerOutcome;

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ChatSteerKind")]
pub enum ChatSteerKind {
    Accepted,
    Settled,
    NotSteerable,
    NoActiveTurn,
    TurnMismatch,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, rename = "ChatSteerReceipt")]
pub struct ChatSteerReceipt {
    pub kind: ChatSteerKind,
    pub phase: Option<ChatSteerPhase>,
    pub turn_id: Option<String>,
    #[ts(type = "ChatSteerOutcome | null")]
    pub outcome: Option<ChatSteerOutcome>,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub cleanup_error: Option<String>,
}

// ── 对话相关 ─────────────────────────────────────────────────

/// POST /api/chat 响应
#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ChatResponse")]
pub struct ChatResponse {
    pub answer: String,
    pub tool_calls: Vec<ToolCallInfo>,
    pub iterations: usize,
    pub context_stats: ContextStats,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, rename = "ToolCallInfo")]
pub struct ToolCallInfo {
    pub name: String,
    pub args: Value,
    pub result: String,
    pub success: bool,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ContextStats")]
pub struct ContextStats {
    pub message_count: usize,
    pub estimated_tokens: usize,
}

// ── 工具相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ToolInfo")]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub enabled: bool,
    pub source: ToolSource,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ToolSource")]
pub enum ToolSource {
    Builtin,
}

// ── MCP 相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "McpServerInfo")]
pub struct McpServerInfo {
    pub name: String,
    pub status: McpConnectionStatus,
    pub transport: String,
    pub tool_count: usize,
    pub tools: Vec<McpToolInfo>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub connected_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "McpConnectionStatus")]
pub enum McpConnectionStatus {
    Connected,
    #[allow(dead_code)]
    Disconnected,
    #[serde(rename = "error")]
    Error(String),
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "McpToolInfo")]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

// ── 技能相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "SkillInfo")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub tool_names: Vec<String>,
    pub source: SkillSource,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "SkillSource")]
pub enum SkillSource {
    Builtin,
    External { path: String },
}

// ── 配置相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "AgentConfigResponse")]
pub struct AgentConfigResponse {
    pub model: String,
    pub system_prompt: String,
    pub max_iterations: usize,
    pub token_limit: usize,
    pub session_id: Option<String>,
    /// 可用的模型列表
    pub available_models: Vec<String>,
}

/// GET /api/config/full — 完整配置（用于前端配置面板）
#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "FullConfigResponse")]
pub struct FullConfigResponse {
    pub model: ModelConfigResponse,
    pub agent: AgentConfigResponse,
    pub mcp: McpConfigResponse,
    pub channels: ChannelsConfigResponse,
    pub server: ServerConfigResponse,
    pub logging: LoggingConfigResponse,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ModelConfigResponse")]
pub struct ModelConfigResponse {
    pub provider: String,
    pub name: String,
    pub has_auth_token: bool,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "McpConfigResponse")]
pub struct McpConfigResponse {
    pub config_path: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ChannelsConfigResponse")]
pub struct ChannelsConfigResponse {
    pub qq: QqConfigResponse,
    pub feishu: FeishuConfigResponse,
    pub session: SessionConfigResponse,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "QqConfigResponse")]
pub struct QqConfigResponse {
    pub enabled: bool,
    pub app_id: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "FeishuConfigResponse")]
pub struct FeishuConfigResponse {
    pub enabled: bool,
    pub app_id: String,
    pub mode: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "SessionConfigResponse")]
pub struct SessionConfigResponse {
    pub timeout_minutes: u64,
    pub reset_keywords: Vec<String>,
    pub reset_commands: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ServerConfigResponse")]
pub struct ServerConfigResponse {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "LoggingConfigResponse")]
pub struct LoggingConfigResponse {
    pub level: String,
}

// ── 会话相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "SessionInfo")]
pub struct SessionInfo {
    pub session_id: Option<String>,
    pub message_count: usize,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
}

// ── WebSocket 消息类型 ─────────────────────────────────────────────────

/// 服务端 -> 客户端 WebSocket 消息
#[derive(Debug, Serialize, TS)]
#[ts(export, rename = "ServerMessage")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Token 片段
    Token {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        data: String,
    },

    /// 工具开始执行
    ToolStart {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        args: Value,
    },

    /// 工具执行结果
    ToolResult {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        result: String,
        success: bool,
    },

    /// 最终答案
    FinalAnswer {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        data: String,
    },

    /// 错误
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
    },

    /// 审批请求
    ApprovalRequest {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        request_id: String,
        tool_name: String,
        args: Value,
        prompt: String,
    },

    /// 输入请求
    InputRequest {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        request_id: String,
        prompt: String,
    },

    /// 图表（vega-lite JSON 规范）
    Chart {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        spec: Value,
    },

    /// 执行被取消
    Cancelled {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// 思考阶段开始
    ThinkingStart {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// 思考阶段结束
    ThinkingEnd {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        prompt_tokens: usize,
        completion_tokens: usize,
    },

    /// 工具批次开始
    ToolBatchStart {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        tool_count: usize,
    },
    /// 工具批次结束
    ToolBatchEnd {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// 心跳响应
    Pong,
}
