//! Application-owned conversation projection DTOs.
//!
//! Canonical conversation persistence and search belong to the framework
//! `ConversationStore`. These types only carry UI metadata at the Tauri
//! boundary; this module owns no files or storage lifecycle.

use serde::{Deserialize, Serialize};

/// 消息的序列化表示
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<SavedToolCall>>,
    /// Thinking/reasoning segments from the LLM (e.g., DeepSeek thinking process)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_segments: Option<Vec<String>>,
    /// Tool call result (for tool role messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// Execution order tracking: records the sequence of thinking and tool calls
    /// for correct chronological interleaving when loading from history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_steps: Option<Vec<SavedExecutionStep>>,
    /// Lightweight UI ordering for thinking/tool rounds. Tool payloads live in
    /// the application-owned tool execution repository; this stores IDs only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_rounds: Option<Vec<SavedExecutionRound>>,
    /// User-uploaded attachments (images/documents) attached to this message.
    /// Small resources retain their data URL; artifact-backed text keeps only
    /// display metadata so its body is not duplicated in conversation JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<SavedAttachment>>,
}

/// A persisted attachment reference (stored inside SavedMessage).
///
/// `url` is a complete data URL for small inline resources and empty for
/// artifact-backed text resources.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedAttachment {
    pub name: String,
    pub mime_type: String,
    pub url: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Execution step for tracking thinking/tool interleaving order
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedExecutionStep {
    #[serde(rename = "type")]
    pub step_type: String, // "thinking" or "tool"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SavedExecutionRound {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<SavedRoundThinking>,
    #[serde(default)]
    pub tool_call_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SavedRoundThinking {
    pub content: String,
}

/// Combined payload stored in attachments_json (backward compatible).
/// Old format: `["thinking1", "thinking2"]` (plain array)
/// New format: `{"thinking_segments": [...], "execution_steps": [...], "attachments": [...]}`
#[derive(Debug, Serialize, Deserialize)]
pub struct AttachmentsPayload {
    /// Stable GUI message identity used to attach TaskRun/Subagent history after reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Frontend display text. The canonical `StoredMessage.content` remains
    /// the agent-facing projection (for example an artifact reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default)]
    pub thinking_segments: Vec<String>,
    #[serde(default)]
    pub execution_steps: Vec<SavedExecutionStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_rounds: Option<Vec<SavedExecutionRound>>,
    /// Real user-uploaded attachments (images/documents). Despite the column
    /// name `attachments_json` historically holding thinking segments, this key
    /// holds the actual attachment data URL so messages render on reload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SavedAttachment>,
}

impl AttachmentsPayload {
    /// Parse from JSON string, handling both old and new formats.
    pub fn parse(s: &str) -> Option<Self> {
        let value = serde_json::from_str::<serde_json::Value>(s).ok()?;
        if value.is_object() {
            return serde_json::from_value(value).ok();
        }
        if value.is_array() {
            let segments = serde_json::from_value::<Vec<String>>(value).ok()?;
            return Some(Self {
                message_id: None,
                display_content: None,
                thinking_segments: segments,
                execution_steps: Vec::new(),
                execution_rounds: None,
                attachments: Vec::new(),
            });
        }
        None
    }
}

/// 工具调用的序列化表示
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saved_message_serialize() -> Result<(), String> {
        let msg = SavedMessage {
            message_id: Some("message-1".to_string()),
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_calls: None,
            thinking_segments: None,
            execution_steps: None,
            execution_rounds: None,
            tool_result: None,
            attachments: None,
        };
        let json = serde_json::to_string(&msg).map_err(|error| error.to_string())?;
        assert!(json.contains("\"role\":\"user\""));
        Ok(())
    }

    #[test]
    fn attachments_payload_round_trips_tool_execution_ids() {
        let rounds = vec![SavedExecutionRound {
            thinking: Some(SavedRoundThinking {
                content: "inspect".to_string(),
            }),
            tool_call_ids: vec!["detail-1".to_string()],
        }];
        let payload = AttachmentsPayload {
            message_id: Some("message-1".to_string()),
            display_content: Some("visible text".to_string()),
            thinking_segments: Vec::new(),
            execution_steps: Vec::new(),
            execution_rounds: Some(rounds.clone()),
            attachments: Vec::new(),
        };
        let encoded = serde_json::to_string(&payload).unwrap_or_default();
        let decoded = AttachmentsPayload::parse(&encoded);

        assert_eq!(
            decoded.and_then(|value| value.execution_rounds),
            Some(rounds)
        );
    }

    #[test]
    fn execution_steps_store_tool_call_identity() -> Result<(), String> {
        let step = SavedExecutionStep {
            step_type: "tool".to_string(),
            index: None,
            call_id: Some("call-42".to_string()),
        };
        let value = serde_json::to_value(step).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("call_id").and_then(serde_json::Value::as_str),
            Some("call-42")
        );
        assert!(value.get("index").is_none());
        Ok(())
    }

    #[test]
    fn attachments_payload_accepts_legacy_thinking_array() {
        let decoded = AttachmentsPayload::parse(r#"["分析","完成"]"#);
        assert_eq!(
            decoded.map(|value| value.thinking_segments),
            Some(vec!["分析".to_string(), "完成".to_string()])
        );
    }
}
