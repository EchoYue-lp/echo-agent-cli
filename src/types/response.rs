//! 响应类型定义

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

// ── 对话相关 ─────────────────────────────────────────────────

/// POST /api/chat 响应
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub answer: String,
    pub tool_calls: Vec<ToolCallInfo>,
    pub iterations: usize,
    pub context_stats: ContextStats,
}

#[derive(Debug, Serialize)]
pub struct ToolCallInfo {
    pub name: String,
    pub args: Value,
    pub result: String,
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct ContextStats {
    pub message_count: usize,
    pub estimated_tokens: usize,
}

// ── 工具相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub enabled: bool,
    pub need_approval: bool,
    pub source: ToolSource,
}

#[derive(Debug, Serialize)]
pub enum ToolSource {
    Builtin,
    Mcp { server_name: String },
    Skill { skill_name: String },
}

// ── MCP 相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub status: McpConnectionStatus,
    pub transport: String,
    pub tool_count: usize,
    pub tools: Vec<McpToolInfo>,
    pub connected_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub enum McpConnectionStatus {
    Connected,
}

#[derive(Debug, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

// ── 技能相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub tool_names: Vec<String>,
    pub source: SkillSource,
}

#[derive(Debug, Serialize)]
pub enum SkillSource {
    Builtin,
    External { path: String },
}

// ── 配置相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AgentConfigResponse {
    pub model: String,
    pub system_prompt: String,
    pub max_iterations: usize,
    pub token_limit: usize,
    pub enable_memory: bool,
    pub enable_human_loop: bool,
    pub session_id: Option<String>,
    /// 可用的模型列表
    pub available_models: Vec<String>,
}

// ── 会话相关 ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub session_id: Option<String>,
    pub message_count: usize,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
}

// ── WebSocket 消息类型 ─────────────────────────────────────────────────

/// 服务端 -> 客户端 WebSocket 消息
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Token 片段
    Token { data: String },

    /// 工具开始执行
    ToolStart { name: String, args: Value },

    /// 工具执行结果
    ToolResult {
        name: String,
        result: String,
        success: bool,
    },

    /// 最终答案
    FinalAnswer { data: String },

    /// 错误
    Error { message: String },

    /// 审批请求
    ApprovalRequest {
        request_id: String,
        tool_name: String,
        args: Value,
        prompt: String,
    },

    /// 输入请求
    InputRequest { request_id: String, prompt: String },

    /// 执行被取消
    Cancelled {},
}