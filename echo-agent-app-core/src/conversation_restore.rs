//! Restore framework messages from the application conversation projection.

use echo_agent::llm::types::{Message, MessageContent, ToolCall};
use echo_agent::memory::StoredMessage;
use serde::Deserialize;

#[derive(Deserialize)]
struct ToolResultMeta {
    tool_call_id: Option<String>,
    name: Option<String>,
}

/// Convert persisted transcript messages back into runtime messages.
///
/// Tool results retain their original call id and name when the projection
/// contains them. Malformed optional JSON is ignored without losing the text.
pub fn restore_messages(stored: &[StoredMessage]) -> Vec<Message> {
    stored
        .iter()
        .map(|item| {
            let text = item.content.clone().unwrap_or_default();
            match item.role.as_str() {
                "system" => Message::system(text),
                "assistant" => {
                    let calls = item
                        .tool_calls_json
                        .as_deref()
                        .and_then(|json| serde_json::from_str::<Vec<ToolCall>>(json).ok());
                    match calls {
                        Some(calls) => {
                            let mut message = Message::assistant_with_tools(calls);
                            if !text.is_empty() {
                                message.content = MessageContent::Text(text);
                            }
                            message
                        }
                        None => Message::assistant(text),
                    }
                }
                "tool" => {
                    let meta = item
                        .tool_result_json
                        .as_deref()
                        .and_then(|json| serde_json::from_str::<ToolResultMeta>(json).ok());
                    let tool_call_id = meta
                        .as_ref()
                        .and_then(|value| value.tool_call_id.clone())
                        .unwrap_or_else(|| "unknown_tool_call".to_string());
                    let name = meta.and_then(|value| value.name).unwrap_or_default();
                    Message::tool_result(tool_call_id, name, text)
                }
                _ => Message::user(text),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::restore_messages;
    use echo_agent::memory::StoredMessage;

    fn stored(role: &str, content: &str) -> StoredMessage {
        StoredMessage {
            id: None,
            conversation_id: "conv".to_string(),
            role: role.to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn restores_unicode_text_without_truncation() {
        let messages = restore_messages(&[stored("user", "你好 👋")]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages.first().and_then(|m| m.text_content()).as_deref(),
            Some("你好 👋")
        );
    }

    #[test]
    fn restores_tool_result_identity() {
        let mut item = stored("tool", "done");
        item.tool_result_json =
            Some(serde_json::json!({"tool_call_id": "call-1", "name": "read_file"}).to_string());
        let messages = restore_messages(&[item]);
        let message = messages.first();
        assert_eq!(
            message.and_then(|m| m.tool_call_id.as_deref()),
            Some("call-1")
        );
        assert_eq!(message.and_then(|m| m.name.as_deref()), Some("read_file"));
    }
}
