//! 对话历史 API
//!
//! 提供对话历史的获取和导出功能。
//!
//! # 背景
//!
//! Agent 在多轮对话中会积累消息历史。此 API 允许用户：
//! - 查看完整对话历史
//! - 导出对话历史用于备份或分析
//!
//! # API 端点
//!
//! | 端点 | 方法 | 说明 |
//! |------|------|------|
//! | `/api/history` | GET | 获取对话历史 |
//! | `/api/history/export` | GET | 导出对话历史 |

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

// ── 响应类型 ─────────────────────────────────────────────────

/// 消息项
///
/// 单条对话消息的结构化表示。
#[derive(Debug, Serialize)]
pub struct MessageItem {
    /// 角色：system / user / assistant / tool
    pub role: String,
    /// 消息内容
    pub content: Option<String>,
    /// 工具调用列表（如果有）
    pub tool_calls: Option<Vec<ToolCallItem>>,
}

/// 工具调用项
///
/// 记录一次工具调用的详情。
#[derive(Debug, Serialize)]
pub struct ToolCallItem {
    /// 工具调用 ID
    pub id: String,
    /// 工具名称
    pub name: String,
    /// 工具参数（JSON 字符串）
    pub arguments: String,
}

/// 对话历史响应
#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    /// 消息列表
    pub messages: Vec<MessageItem>,
    /// 总消息数
    pub total: usize,
}

/// 导出历史响应
#[derive(Debug, Serialize)]
pub struct ExportHistoryResponse {
    /// 导出格式
    pub format: String,
    /// 导出内容
    pub content: String,
    /// 消息数量
    pub message_count: usize,
}

/// 导出查询参数
#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// 导出格式：json 或 markdown
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "markdown".to_string()
}

// ── API 处理函数 ─────────────────────────────────────────────────

/// GET /api/history - 获取对话历史
///
/// 返回当前会话的所有消息历史。
///
/// # 响应
///
/// ```json
/// {
///   "messages": [
///     {
///       "role": "user",
///       "content": "你好",
///       "tool_calls": null
///     },
///     {
///       "role": "assistant",
///       "content": "你好！有什么可以帮助你的？",
///       "tool_calls": null
///     }
///   ],
///   "total": 2
/// }
/// ```
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_history(State(state): State<Arc<AppState>>) -> Response {
    let messages = state
        .connection
        .agent
        .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
        .await;
    let total = messages.len();

    let items: Vec<MessageItem> = messages
        .iter()
        .map(|msg| {
            let tool_calls = msg.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|tc| ToolCallItem {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect()
            });

            MessageItem {
                role: msg.role.as_str().to_string(),
                content: msg.content.as_deref().map(|s| s.to_string()),
                tool_calls,
            }
        })
        .collect();

    Json(HistoryResponse {
        messages: items,
        total,
    })
    .into_response()
}

/// GET /api/history/export - 导出对话历史
///
/// 将对话历史导出为 JSON 或 Markdown 格式。
///
/// # 查询参数
///
/// - `format`: 导出格式，可选值：`json`、`markdown`（默认）
///
/// # 示例请求
///
/// ```text
/// GET /api/history/export?format=markdown
/// ```
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn export_history(
    State(state): State<Arc<AppState>>,
    query: axum::extract::Query<ExportQuery>,
) -> Response {
    let messages = state
        .connection
        .agent
        .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
        .await;
    let message_count = messages.len();

    let content = match query.format.as_str() {
        "json" => export_as_json(&messages),
        _ => export_as_markdown(&messages),
    };

    Json(ExportHistoryResponse {
        format: query.format.clone(),
        content,
        message_count,
    })
    .into_response()
}

// ── 导出辅助函数 ─────────────────────────────────────────────────

/// 将消息列表导出为 JSON 格式
fn export_as_json(messages: &[echo_agent::prelude::Message]) -> String {
    let items: Vec<MessageItem> = messages
        .iter()
        .map(|msg| {
            let tool_calls = msg.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|tc| ToolCallItem {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect()
            });

            MessageItem {
                role: msg.role.as_str().to_string(),
                content: msg.content.as_deref().map(|s| s.to_string()),
                tool_calls,
            }
        })
        .collect();

    serde_json::to_string_pretty(&items).unwrap_or_default()
}

/// 将消息列表导出为 Markdown 格式
fn export_as_markdown(messages: &[echo_agent::prelude::Message]) -> String {
    let mut md = String::new();
    md.push_str("# 对话历史\n\n");

    for msg in messages {
        // 角色图标
        let role_emoji = match msg.role.as_str() {
            "system" => "⚙️",
            "user" => "👤",
            "assistant" => "🤖",
            "tool" => "🔧",
            _ => "💬",
        };

        md.push_str(&format!("## {} {}\n\n", role_emoji, msg.role.as_str()));

        // 消息内容
        if let Some(content) = msg.content.as_deref() {
            md.push_str(content);
            md.push_str("\n\n");
        }

        // 工具调用
        if let Some(calls) = &msg.tool_calls {
            md.push_str("**工具调用:**\n");
            for tc in calls {
                md.push_str(&format!(
                    "- `{}`: {}\n",
                    tc.function.name, tc.function.arguments
                ));
            }
            md.push('\n');
        }
    }

    md
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_item_serialize() {
        let item = MessageItem {
            role: "user".to_string(),
            content: Some("你好".to_string()),
            tool_calls: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"你好\""));
    }

    #[test]
    fn test_message_item_with_tool_calls() {
        let item = MessageItem {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCallItem {
                id: "call_123".to_string(),
                name: "search".to_string(),
                arguments: r#"{"query":"test"}"#.to_string(),
            }]),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"name\":\"search\""));
    }

    #[test]
    fn test_tool_call_item_serialize() {
        let item = ToolCallItem {
            id: "call_001".to_string(),
            name: "calculator".to_string(),
            arguments: r#"{"expr":"1+1"}"#.to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"id\":\"call_001\""));
        assert!(json.contains("\"name\":\"calculator\""));
    }

    #[test]
    fn test_history_response_serialize() {
        let resp = HistoryResponse {
            messages: vec![MessageItem {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
            }],
            total: 1,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"total\":1"));
    }

    #[test]
    fn test_export_query_default() {
        let query: ExportQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(query.format, "markdown");
    }

    #[test]
    fn test_export_query_json() {
        let query: ExportQuery = serde_json::from_str(r#"{"format":"json"}"#).unwrap();
        assert_eq!(query.format, "json");
    }

    #[test]
    fn test_export_history_response_serialize() {
        let resp = ExportHistoryResponse {
            format: "markdown".to_string(),
            content: "# 对话历史".to_string(),
            message_count: 5,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"format\":\"markdown\""));
        assert!(json.contains("\"message_count\":5"));
    }

    #[test]
    fn test_role_emoji_mapping() {
        assert_eq!(get_role_emoji("system"), "⚙️");
        assert_eq!(get_role_emoji("user"), "👤");
        assert_eq!(get_role_emoji("assistant"), "🤖");
        assert_eq!(get_role_emoji("tool"), "🔧");
        assert_eq!(get_role_emoji("unknown"), "💬");
    }

    fn get_role_emoji(role: &str) -> &'static str {
        match role {
            "system" => "⚙️",
            "user" => "👤",
            "assistant" => "🤖",
            "tool" => "🔧",
            _ => "💬",
        }
    }
}
