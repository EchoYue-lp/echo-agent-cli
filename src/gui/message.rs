//! GUI 消息类型 — 支持折叠/展开状态

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

/// 一条聊天消息
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    /// 累积的文本内容（用户消息一次性，助手消息逐步追加）
    pub content: String,
    /// 思考过程（可折叠）
    pub thinking: Vec<ThinkingBlock>,
    /// 工具调用记录
    pub tool_calls: Vec<ToolCallRecord>,
    /// 消息是否已完成（false = 仍在流式输出中）
    #[serde(default)]
    pub finished: bool,
    /// 错误信息
    #[serde(default)]
    pub error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// 一次思考过程
#[derive(Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub tokens: String,
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
}

/// 工具调用记录
#[derive(Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: serde_json::Value,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub finished: bool,
}

impl ChatMessage {
    pub fn new_user(content: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content,
            thinking: Vec::new(),
            tool_calls: Vec::new(),
            finished: true,
            error: None,
            timestamp: Utc::now(),
        }
    }

    pub fn new_assistant() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: String::new(),
            thinking: Vec::new(),
            tool_calls: Vec::new(),
            finished: false,
            error: None,
            timestamp: Utc::now(),
        }
    }

    pub fn append_token(&mut self, token: &str) {
        self.content.push_str(token);
    }

    pub fn start_thinking(&mut self) {
        self.thinking.push(ThinkingBlock {
            tokens: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
        });
    }

    pub fn append_thinking(&mut self, token: &str) {
        if let Some(last) = self.thinking.last_mut() {
            last.tokens.push_str(token);
        }
    }

    pub fn end_thinking(&mut self, prompt: usize, completion: usize) {
        if let Some(last) = self.thinking.last_mut() {
            last.prompt_tokens = prompt;
            last.completion_tokens = completion;
        }
    }

    pub fn add_tool_call(&mut self, name: String, args: serde_json::Value) {
        self.tool_calls.push(ToolCallRecord {
            name,
            args,
            result: None,
            success: true,
            finished: false,
        });
    }

    pub fn complete_tool_call(&mut self, name: &str, result: String, success: bool) {
        for tc in &mut self.tool_calls {
            if tc.name == name && !tc.finished {
                tc.result = Some(result);
                tc.success = success;
                tc.finished = true;
                return;
            }
        }
    }

    /// 思考块总 token 数
    pub fn thinking_token_total(&self) -> usize {
        self.thinking.iter().map(|t| t.prompt_tokens + t.completion_tokens).sum()
    }
}

/// 格式化时间戳为本地时间字符串
pub fn format_timestamp(ts: &DateTime<Utc>) -> String {
    let local: DateTime<Local> = ts.with_timezone(&Local);
    local.format("%H:%M").to_string()
}

/// GUI 对话持久化数据格式
#[derive(Clone, Serialize, Deserialize)]
pub struct GuiConversationData {
    /// 数据格式版本（向前兼容）
    #[serde(default = "default_version")]
    pub version: u32,
    pub id: String,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub model: String,
}

fn default_version() -> u32 { 1 }