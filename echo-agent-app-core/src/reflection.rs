//! Shared session-reflection command and service.
//!
//! Interaction surfaces resolve their current scoped runtime and Agent, then
//! delegate here. This keeps prompt construction, LLM generation, layered
//! memory writes, and hot-memory projection settlement under one authority.

use std::sync::Arc;
use std::time::Duration;

use echo_agent::llm::{LlmClient, SimpleChatOptions};
use echo_agent::memory::{MemoryMeta, MemorySource, MemoryType};
use echo_agent::prelude::Message;

use crate::agent_handle::AgentHandle;
use crate::evolution::{MemoryProjectionSettlementReceipt, ReviewGenerationLease};
use crate::state::ScopedChatRuntime;

const REFLECTION_INSTRUCTION: &str = "Reflect on the conversation above and summarize the key reusable learnings in 1-2 sentences. Be specific and stay within 200 tokens.";
const REFLECTION_TIMEOUT: Duration = Duration::from_secs(30);
const REFLECTION_SUMMARY_CHARS: usize = 240;

/// Canonical slash command recognized by surfaces that parse free-form input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionCommand {
    Reflect,
}

impl ReflectionCommand {
    /// Parse only the exact shared `/reflect` command.
    ///
    /// `Ok(None)` leaves unrelated input to the caller's normal dispatcher.
    pub fn parse(input: &str) -> Result<Option<Self>, ReflectionCommandParseError> {
        let mut parts = input.split_whitespace();
        let Some(command) = parts.next() else {
            return Ok(None);
        };
        if command != "/reflect" {
            return Ok(None);
        }
        if parts.next().is_some() {
            return Err(ReflectionCommandParseError);
        }
        Ok(Some(Self::Reflect))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Usage: /reflect")]
pub struct ReflectionCommandParseError;

/// Typed, bounded result shared by GUI, TUI, CLI, JSONL, and channel adapters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReflectionReceipt {
    pub authority_scope: String,
    pub workspace_generation: String,
    pub workspace_id: String,
    pub conversation_id: Option<String>,
    pub key: String,
    pub content_summary: String,
    pub content_chars: usize,
    pub projection: MemoryProjectionSettlementReceipt,
}

impl ReflectionReceipt {
    /// Human-readable projection for text surfaces. The typed receipt remains
    /// the wire authority used by structured surfaces.
    pub fn display_message(&self) -> String {
        let mut message = format!(
            "Reflection saved with key: {}\n{}",
            self.key, self.content_summary
        );
        if let Some(error) = self.projection.error.as_deref() {
            message.push_str("\nProjection remains pending: ");
            message.push_str(error);
        }
        message
    }
}

/// Canonical receipt fixture shared by surface adapter contract tests.
#[doc(hidden)]
pub fn reflection_receipt_fixture() -> ReflectionReceipt {
    ReflectionReceipt {
        authority_scope: "authority-a".to_string(),
        workspace_generation: "generation-a".to_string(),
        workspace_id: "workspace-a".to_string(),
        conversation_id: Some("conversation-a".to_string()),
        key: "session-reflection:fixture".to_string(),
        content_summary: "fixture insight".to_string(),
        content_chars: 15,
        projection: MemoryProjectionSettlementReceipt {
            authority_scope: "authority-a".to_string(),
            workspace_generation: "generation-a".to_string(),
            revision: "revision-a".to_string(),
            changed: true,
            status: crate::evolution::MemoryProjectionSettlementStatus::Settled,
            primary_bound: true,
            pool_bound: true,
            future_bound: true,
            pending_revision: None,
            error: None,
        },
    }
}

/// Validate the stable typed receipt fields at structured surface boundaries.
#[doc(hidden)]
pub fn validate_reflection_receipt_wire(value: &serde_json::Value) -> Result<(), String> {
    const RECEIPT_FIELDS: [&str; 8] = [
        "authority_scope",
        "workspace_generation",
        "workspace_id",
        "conversation_id",
        "key",
        "content_summary",
        "content_chars",
        "projection",
    ];
    let object = value
        .as_object()
        .ok_or_else(|| "reflection receipt did not serialize as an object".to_string())?;
    for field in RECEIPT_FIELDS {
        if !object.contains_key(field) {
            return Err(format!("reflection receipt is missing field {field}"));
        }
    }
    if object.len() != RECEIPT_FIELDS.len() {
        return Err("reflection receipt contains an unexpected field".to_string());
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ReflectionError {
    #[error("reflection is unavailable for workspace '{workspace_id}'")]
    IntegrationUnavailable { workspace_id: String },
    #[error("reflection generation could not pin the workspace memory authority: {0}")]
    Generation(String),
    #[error("the current conversation Agent has no LLM client")]
    LlmUnavailable,
    #[error("reflection generation failed: {0}")]
    Llm(String),
    #[error("reflection generation timed out")]
    Timeout,
    #[error("reflection generation returned empty content")]
    EmptyContent,
    #[error("reflection memory write failed: {0}")]
    MemoryWrite(String),
}

/// Reflect on the exact Agent context supplied by a scoped interaction surface.
///
/// The generation lease pins memory authority across context capture, the LLM
/// call, the canonical layered write, and one projection settlement.
pub async fn reflect_session(
    runtime: &ScopedChatRuntime,
    agent: &AgentHandle,
    conversation_id: Option<&str>,
) -> Result<ReflectionReceipt, ReflectionError> {
    let workspace_id = runtime.execution_scope().workspace_id().to_string();
    let integration =
        runtime
            .review_integration()
            .ok_or_else(|| ReflectionError::IntegrationUnavailable {
                workspace_id: workspace_id.clone(),
            })?;
    let generation = integration
        .lease_generation()
        .map_err(|error| ReflectionError::Generation(error.to_string()))?;

    let (llm, messages, agent_conversation_id) = agent
        .read_async(|agent| {
            Box::pin(async move {
                (
                    agent.llm_client().cloned(),
                    agent.get_messages().await,
                    agent.conversation_id().map(str::to_string),
                )
            })
        })
        .await;
    let llm = llm.ok_or(ReflectionError::LlmUnavailable)?;
    let content = generate_reflection(llm, messages, REFLECTION_TIMEOUT).await?;
    let conversation_id = conversation_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(agent_conversation_id);

    persist_reflection(&generation, workspace_id, conversation_id, content).await
}

async fn generate_reflection(
    llm: Arc<dyn LlmClient>,
    mut messages: Vec<Message>,
    timeout: Duration,
) -> Result<String, ReflectionError> {
    messages.push(Message::user(REFLECTION_INSTRUCTION.to_string()));
    let options = SimpleChatOptions::default()
        .with_temperature(0.2)
        .with_max_tokens(300);
    let generated = tokio::time::timeout(timeout, llm.chat_simple_with_options(messages, options))
        .await
        .map_err(|_| ReflectionError::Timeout)?
        .map_err(|error| ReflectionError::Llm(error.to_string()))?;
    let content = generated.trim().to_string();
    if content.is_empty() {
        return Err(ReflectionError::EmptyContent);
    }
    Ok(content)
}

async fn persist_reflection(
    generation: &ReviewGenerationLease,
    workspace_id: String,
    conversation_id: Option<String>,
    content: String,
) -> Result<ReflectionReceipt, ReflectionError> {
    let layer_manager = generation
        .layer_manager()
        .map_err(|error| ReflectionError::MemoryWrite(error.to_string()))?;
    let key = format!("session-reflection:{}", uuid::Uuid::new_v4());
    let meta = MemoryMeta::new(
        MemoryType::ProjectFact,
        MemorySource::AutoExtracted,
        "session_reflection",
    );
    layer_manager
        .write_memory(&key, &content, meta)
        .await
        .map_err(|error| ReflectionError::MemoryWrite(error.to_string()))?;

    // Exactly one settlement follows the single canonical write.
    let projection = generation.settle_hot_memory_projection().await;
    Ok(ReflectionReceipt {
        authority_scope: projection.authority_scope.clone(),
        workspace_generation: projection.workspace_generation.clone(),
        workspace_id,
        conversation_id,
        key,
        content_summary: bounded_summary(&content, REFLECTION_SUMMARY_CHARS),
        content_chars: content.chars().count(),
        projection,
    })
}

fn bounded_summary(content: &str, max_chars: usize) -> String {
    let mut chars = content.chars();
    let summary = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::evolution::ReviewConfig;
    use echo_agent::memory::{InMemoryStore, Store};
    use echo_agent::testing::MockLlmClient;

    #[test]
    fn command_parser_is_exact_and_shared() -> Result<(), String> {
        assert_eq!(
            ReflectionCommand::parse(" /reflect \n").map_err(|error| error.to_string())?,
            Some(ReflectionCommand::Reflect)
        );
        assert_eq!(
            ReflectionCommand::parse("ordinary message").map_err(|error| error.to_string())?,
            None
        );
        assert!(ReflectionCommand::parse("/reflect extra").is_err());
        assert_eq!(bounded_summary("你好 world", 2), "你好...");
        Ok(())
    }

    #[tokio::test]
    async fn generation_reuses_available_conversation_messages() -> Result<(), String> {
        let mock = Arc::new(MockLlmClient::new().with_response("  reusable insight  "));
        let messages = vec![
            Message::user("session question".to_string()),
            Message::assistant("session answer".to_string()),
        ];
        let content = generate_reflection(mock.clone(), messages, Duration::from_secs(1))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(content, "reusable insight");
        assert_eq!(mock.call_count(), 1);
        let sent = mock
            .last_messages()
            .ok_or_else(|| "reflection LLM call was not recorded".to_string())?;
        assert_eq!(sent.len(), 3);
        assert_eq!(
            sent.first().and_then(Message::text_content).as_deref(),
            Some("session question")
        );
        assert_eq!(
            sent.get(1).and_then(Message::text_content).as_deref(),
            Some("session answer")
        );
        assert_eq!(
            sent.last().and_then(Message::text_content).as_deref(),
            Some(REFLECTION_INSTRUCTION)
        );
        Ok(())
    }

    #[tokio::test]
    async fn persistence_returns_scoped_receipt_after_canonical_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let integration = crate::evolution::ReviewIntegration::new_scoped(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            store,
            "authority-a".to_string(),
            "generation-a".to_string(),
        );
        let generation = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let receipt = persist_reflection(
            &generation,
            "workspace-a".to_string(),
            Some("conversation-a".to_string()),
            "A durable insight".to_string(),
        )
        .await
        .map_err(|error| error.to_string())?;

        assert_eq!(receipt.authority_scope, "authority-a");
        assert_eq!(receipt.workspace_generation, "generation-a");
        assert_eq!(receipt.workspace_id, "workspace-a");
        assert_eq!(receipt.conversation_id.as_deref(), Some("conversation-a"));
        assert_eq!(receipt.content_summary, "A durable insight");
        assert_eq!(receipt.content_chars, 17);
        assert!(receipt.key.starts_with("session-reflection:"));
        assert!(
            generation
                .layer_manager()
                .map_err(|error| error.to_string())?
                .locate(&receipt.key)
                .await
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn typed_receipt_fixture_preserves_canonical_fields() -> Result<(), String> {
        let receipt = reflection_receipt_fixture();
        let canonical = serde_json::to_value(&receipt).map_err(|error| error.to_string())?;
        validate_reflection_receipt_wire(&canonical)
    }
}
