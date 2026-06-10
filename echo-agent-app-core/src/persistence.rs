//! 会话与对话历史持久化模块
//!
//! 提供基于 JSON 文件的会话存储，支持：
//! - 保存/加载对话历史
//! - 列出已保存的会话
//! - 导出为 Markdown 格式
//!
//! 存储目录: `~/.echo-agent/sessions/`

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use echo_agent::prelude::Message;
use serde::{Deserialize, Serialize};

/// 持久化存储管理器
pub struct Persistence {
    base_dir: PathBuf,
}

/// 已保存的会话
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedSession {
    pub name: String,
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<SavedMessage>,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

/// 消息的序列化表示
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<SavedToolCall>>,
    /// Thinking/reasoning segments from the LLM (e.g., DeepSeek thinking process)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_segments: Option<Vec<String>>,
    /// Tool call result (for tool role messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
}

/// 工具调用的序列化表示
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 会话元信息（列表展示用）
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub name: String,
    pub message_count: usize,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 对话历史记录（用于前端持久化）
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConversationRecord {
    pub id: String,
    pub title: String,
    pub messages: Vec<SavedMessage>,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Default for Persistence {
    fn default() -> Self {
        Self::new()
    }
}

impl Persistence {
    /// 创建持久化管理器，自动创建目录
    pub fn new() -> Self {
        let base_dir = Self::base_dir();
        fs::create_dir_all(&base_dir).ok();
        Self { base_dir }
    }

    /// 创建持久化管理器，使用指定基础目录（工作区模式）。
    ///
    /// 当工作区激活时，使用工作区的 `sessions/` 目录替代全局路径。
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        fs::create_dir_all(&base_dir).ok();
        Self { base_dir }
    }

    /// 基础存储目录（可能已重定向到工作区）。
    pub fn current_base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// 获取基础存储目录
    pub fn base_dir() -> PathBuf {
        let home = dirs_home();
        PathBuf::from(home).join(".echo-agent").join("sessions")
    }

    // ── CLI 会话管理 ──

    /// 保存当前会话
    pub fn save_session(
        &self,
        name: &str,
        messages: &[Message],
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<()> {
        let saved = SavedSession {
            name: name.to_string(),
            model: model.to_string(),
            system_prompt: system_prompt.to_string(),
            messages: messages.iter().map(Self::convert_message).collect(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            message_count: messages.len(),
        };

        // 如果文件已存在，保留 created_at
        let path = self.session_path(name);
        if path.exists()
            && let Ok(existing) = self.load_session_raw(name)
        {
            let mut updated = saved;
            updated.created_at = existing.created_at;
            return self.write_json(&path, &updated);
        }

        self.write_json(&path, &saved)
    }

    /// 加载会话
    pub fn load_session(&self, name: &str) -> anyhow::Result<SavedSession> {
        self.load_session_raw(name)
    }

    /// 列出所有已保存的会话
    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionMeta>> {
        let mut sessions = Vec::new();

        let entries = match fs::read_dir(&self.base_dir) {
            Ok(e) => e,
            Err(_) => return Ok(sessions),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Ok(data) = fs::read_to_string(&path)
                && let Ok(session) = serde_json::from_str::<SavedSession>(&data)
            {
                sessions.push(SessionMeta {
                    name: session.name,
                    message_count: session.message_count,
                    model: session.model,
                    created_at: session.created_at,
                    updated_at: session.updated_at,
                });
            }
        }

        // 按更新时间降序排列
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    // ── 对话历史管理（前端持久化）──

    /// 获取对话历史目录
    pub fn conversations_dir(&self) -> PathBuf {
        let dir = self.base_dir.join("conversations");
        fs::create_dir_all(&dir).ok();
        dir
    }

    /// 导出对话为 Markdown
    pub fn export_conversation_markdown(&self, id: &str) -> anyhow::Result<String> {
        let record = self.load_conversation(id)?;
        let mut md = String::new();
        md.push_str(&format!("# {}\n\n", record.title));
        md.push_str(&format!(
            "> Created: {} | Model: {}\n\n",
            record.created_at, record.model
        ));

        for msg in &record.messages {
            let role_label = match msg.role.as_str() {
                "user" => "👤 **User**",
                "assistant" => "🤖 **Assistant**",
                "system" => "⚙️ **System**",
                "tool" => "🔧 **Tool**",
                _ => &msg.role,
            };
            md.push_str(&format!("### {}\n\n", role_label));
            if let Some(content) = &msg.content {
                md.push_str(content);
                md.push_str("\n\n");
            }
            if let Some(calls) = &msg.tool_calls {
                md.push_str("**Tool Calls:**\n");
                for tc in calls {
                    md.push_str(&format!("- `{}`: {}\n", tc.name, tc.arguments));
                }
                md.push('\n');
            }
        }

        Ok(md)
    }

    /// 加载指定 ID 的对话记录
    pub fn load_conversation(&self, id: &str) -> anyhow::Result<ConversationRecord> {
        let path = self.conversations_dir().join(format!("{}.json", id));
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    // ── 内部辅助 ──

    fn session_path(&self, name: &str) -> PathBuf {
        // 文件名安全化
        let safe_name: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join(format!("{}.json", safe_name))
    }

    fn load_session_raw(&self, name: &str) -> anyhow::Result<SavedSession> {
        let path = self.session_path(name);
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    fn write_json<T: Serialize>(&self, path: &Path, data: &T) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(data)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn convert_message(msg: &Message) -> SavedMessage {
        SavedMessage {
            role: msg.role.as_str().to_string(),
            content: msg.content.as_deref().map(|s| s.to_string()),
            tool_calls: msg.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|tc| SavedToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect()
            }),
            thinking_segments: None,
            tool_result: None,
        }
    }
}

/// 获取用户 home 目录
fn dirs_home() -> String {
    // 优先使用 HOME 环境变量
    if let Ok(home) = std::env::var("HOME") {
        return home;
    }
    // 回退
    "~".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saved_message_serialize() {
        let msg = SavedMessage {
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_calls: None,
            thinking_segments: None,
            tool_result: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
    }

    #[test]
    fn test_session_meta_ordering() {
        let a = SessionMeta {
            name: "a".into(),
            message_count: 1,
            model: "test".into(),
            created_at: "2024-01-01".into(),
            updated_at: "2024-01-01".into(),
        };
        let b = SessionMeta {
            name: "b".into(),
            message_count: 2,
            model: "test".into(),
            created_at: "2024-01-02".into(),
            updated_at: "2024-01-02".into(),
        };
        let mut list = vec![a, b];
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        assert_eq!(list[0].name, "b");
    }
}
