//! 会话管理类型定义

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// 增强的会话记录
///
/// 支持分支、版本追踪、自动保存。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// 会话 ID (UUID v4)
    pub id: String,
    /// 会话名称
    pub name: String,
    /// 模型名称
    pub model: String,
    /// 系统提示词
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// 父会话 ID（分支来源）
    #[serde(default)]
    pub parent_id: Option<String>,
    /// 分支名称
    #[serde(default)]
    pub branch: Option<String>,
    /// 消息快照
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    /// 标签
    #[serde(default)]
    pub tags: Vec<String>,
    /// 备注
    #[serde(default)]
    pub note: Option<String>,
    /// 消息数
    pub message_count: usize,
    /// Token 估计值
    #[serde(default)]
    pub estimated_tokens: usize,
    /// 创建时间
    pub created_at: String,
    /// 最后更新时间
    pub updated_at: String,
}

/// 会话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<SessionToolCall>>,
}

/// 会话中的工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 会话摘要（列表展示用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub branch: Option<String>,
    pub message_count: usize,
    pub estimated_tokens: usize,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 会话分支信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    /// 分支名称
    pub name: String,
    /// 分支的会话 ID
    pub session_id: String,
    /// 从哪个会话分支出来的
    pub parent_id: String,
}

/// 会话差异
#[derive(Debug, Clone)]
pub struct SessionDiff {
    /// 差异行（unified diff 格式）
    pub hunks: Vec<DiffHunk>,
    /// 添加的行数
    pub added: usize,
    /// 删除的行数
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

impl Session {
    /// 创建新会话
    pub fn new(name: &str, model: &str) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            model: model.to_string(),
            system_prompt: None,
            parent_id: None,
            branch: None,
            messages: Vec::new(),
            tags: Vec::new(),
            note: None,
            message_count: 0,
            estimated_tokens: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// 从已存在的会话创建分支
    pub fn branch_from(parent: &Session, branch_name: &str) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: format!("{}/{}", parent.name, branch_name),
            model: parent.model.clone(),
            system_prompt: parent.system_prompt.clone(),
            parent_id: Some(parent.id.clone()),
            branch: Some(branch_name.to_string()),
            messages: parent.messages.clone(),
            tags: parent.tags.clone(),
            note: Some(format!("从会话 {} 创建的分支", parent.id)),
            message_count: parent.message_count,
            estimated_tokens: parent.estimated_tokens,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}
