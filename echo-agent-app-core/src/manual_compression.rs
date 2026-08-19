//! Application-owned manual compression settlement shared by every surface.

use crate::chat_driver::{ChatDriverEvent, TurnOutcome};
use crate::chat_event_log::{ChatEventEnvelope, ChatEventLogError};
use crate::conversation_deletion::ConversationDeletionError;
use crate::foreground_turn::ForegroundTurnSurface;
use crate::state::AppState;
use echo_agent::error::AgentFailure;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ManualCompressionRequest {
    pub conversation_id: String,
    pub surface: ForegroundTurnSurface,
    pub focus: Option<String>,
    pub keep_messages: usize,
}

#[derive(Debug, Serialize)]
pub struct ManualCompressionReceipt {
    pub conversation_id: String,
    pub messages_before: usize,
    pub messages_after: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub checkpoint: Option<echo_agent::compression::CompressionCheckpoint>,
    pub envelope: ChatEventEnvelope,
}

impl ManualCompressionReceipt {
    pub fn tokens_saved(&self) -> usize {
        self.tokens_before.saturating_sub(self.tokens_after)
    }
}

#[derive(Debug, Error)]
pub enum ManualCompressionError {
    #[error("conversation id must not be empty")]
    EmptyConversationId,
    #[error("manual compression admission failed: {0}")]
    Admission(#[from] ConversationDeletionError),
    #[error("manual compression agent admission failed: {0}")]
    AgentPool(#[from] crate::agent_pool::PoolError),
    #[error("manual compression resolved conversation {actual:?}, expected {expected:?}")]
    ConversationIdentity {
        expected: String,
        actual: Option<String>,
    },
    #[error("manual compression failed: {0}")]
    Compression(String),
    #[error("context was compressed but its journal safe point failed: {0}")]
    Journal(#[from] ChatEventLogError),
}

impl AppState {
    pub async fn compress_conversation_owned(
        &self,
        request: ManualCompressionRequest,
    ) -> Result<ManualCompressionReceipt, ManualCompressionError> {
        let conversation_id = request.conversation_id.trim().to_string();
        if conversation_id.is_empty() {
            return Err(ManualCompressionError::EmptyConversationId);
        }
        let turn_id = format!("manual-compression:{}", uuid::Uuid::new_v4());
        let lease = self
            .begin_conversation_turn_owned(request.surface, &conversation_id, turn_id.clone())
            .await?;
        let execution = match self.connection.agent_for(&conversation_id).await {
            Ok(execution) => execution,
            Err(error) => {
                let detail = error.to_string();
                lease.settle(TurnOutcome::Failed(AgentFailure::message(
                    "agent_pool",
                    detail,
                )));
                return Err(error.into());
            }
        };
        let agent = execution.agent();
        let actual = agent
            .read(|agent| agent.conversation_id().map(str::to_string))
            .await;
        if actual.as_deref() != Some(conversation_id.as_str()) {
            lease.settle(TurnOutcome::Failed(AgentFailure::message(
                "conversation_identity",
                "manual compression resolved a different conversation agent",
            )));
            return Err(ManualCompressionError::ConversationIdentity {
                expected: conversation_id,
                actual,
            });
        }

        let focus = request.focus.filter(|value| !value.trim().is_empty());
        let keep_messages = request.keep_messages;
        let compression = agent
            .read_async(|agent| {
                Box::pin(async move {
                    if let Some(focus) = focus {
                        agent
                            .force_compress_with_focus_and_hooks(&focus, keep_messages, "manual")
                            .await
                    } else {
                        agent.force_compress_context().await
                    }
                })
            })
            .await;
        let (stats, checkpoint) = match compression {
            Ok(result) => result,
            Err(error) => {
                let detail = error.to_string();
                lease.settle(TurnOutcome::Failed(AgentFailure::message(
                    "manual_compression",
                    detail.clone(),
                )));
                return Err(ManualCompressionError::Compression(detail));
            }
        };

        let event = ChatDriverEvent::ContextCompressed {
            before_count: stats.before_count,
            after_count: stats.after_count,
            before_tokens: stats.before_tokens,
            after_tokens: stats.after_tokens,
        };
        let envelope =
            match self
                .storage
                .chat_events
                .append(Some(&conversation_id), &turn_id, event)
            {
                Ok(envelope) => envelope,
                Err(error) => {
                    lease.settle(TurnOutcome::Failed(AgentFailure::message(
                        "chat_event_log",
                        error.to_string(),
                    )));
                    return Err(error.into());
                }
            };
        lease.settle(TurnOutcome::Completed);
        Ok(ManualCompressionReceipt {
            conversation_id,
            messages_before: stats.before_count,
            messages_after: stats.after_count,
            tokens_before: stats.before_tokens,
            tokens_after: stats.after_tokens,
            checkpoint,
            envelope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_handle::AgentHandle;
    use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
    use crate::mcp_config_runtime::McpConfigRuntime;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::memory::{ConversationStore, FileConversationStore, NewConversation};
    use echo_agent::testing::MockLlmClient;
    use std::error::Error;
    use std::sync::Arc;

    async fn state_fixture(
        conversation_id: &str,
    ) -> Result<(AppState, tempfile::TempDir), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let agent = ReactAgentBuilder::new()
            .model("manual-compression-test")
            .llm_client(Arc::new(MockLlmClient::new()))
            .conversation_id(conversation_id)
            .build()?;
        let store: Arc<dyn ConversationStore> = Arc::new(FileConversationStore::new(
            temp.path().join("conversation-state"),
        )?);
        store
            .create_conversation(NewConversation {
                conversation_id: conversation_id.to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some("Compression fixture".to_string()),
            })
            .await?;
        let mcp = Arc::new(McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            AgentHandle::new(agent),
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            Some(store),
            None,
            Default::default(),
            mcp,
        );
        state.storage.chat_events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(temp.path().join("tools"))?,
        );
        state.tasks.runtime = None;
        {
            let mut binding = state.storage.conversation.write().await;
            binding.deletions = Arc::new(
                crate::conversation_deletion::ConversationDeletionService::new(
                    temp.path().join("deletions"),
                ),
            );
        }
        Ok((state, temp))
    }

    #[tokio::test]
    async fn successful_noop_compression_is_still_journaled() -> Result<(), Box<dyn Error>> {
        let id = "manual-compression-noop";
        let (state, _temp) = state_fixture(id).await?;
        let receipt = state
            .compress_conversation_owned(ManualCompressionRequest {
                conversation_id: id.to_string(),
                surface: ForegroundTurnSurface::Cli,
                focus: None,
                keep_messages: 12,
            })
            .await?;

        assert!(matches!(
            receipt.envelope.payload,
            ChatDriverEvent::ContextCompressed { .. }
        ));
        let replay = state
            .storage
            .chat_events
            .replay(Some(id), &receipt.envelope.turn_id, 0)?;
        assert_eq!(replay.events.len(), 1);
        assert!(
            state
                .session
                .foreground_turns
                .snapshot(ForegroundTurnSurface::Cli, id)
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_agent_identity_fails_closed_and_releases_admission()
    -> Result<(), Box<dyn Error>> {
        let (state, _temp) = state_fixture("actual-conversation").await?;
        let error = state
            .compress_conversation_owned(ManualCompressionRequest {
                conversation_id: "requested-conversation".to_string(),
                surface: ForegroundTurnSurface::Tui,
                focus: None,
                keep_messages: 12,
            })
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("mismatched agent identity was accepted"))?;
        assert!(matches!(
            error,
            ManualCompressionError::ConversationIdentity { .. }
        ));
        assert!(
            state
                .session
                .foreground_turns
                .snapshot(ForegroundTurnSurface::Tui, "requested-conversation")
                .is_none()
        );
        Ok(())
    }
}
