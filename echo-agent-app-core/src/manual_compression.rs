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
    pub workspace_id: String,
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
    #[error("manual compression was cancelled before the context transform committed")]
    Cancelled,
    #[error("context was compressed but its journal safe point failed: {0}")]
    Journal(#[from] ChatEventLogError),
    #[error("manual compression journal I/O failed: {0}")]
    JournalIo(String),
}

impl ManualCompressionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyConversationId => "manual_compression_input",
            Self::Admission(_) => "manual_compression_admission",
            Self::AgentPool(_) => "agent_pool",
            Self::ConversationIdentity { .. } => "conversation_identity",
            Self::Compression(_) => "manual_compression",
            Self::Cancelled => "manual_compression_cancelled",
            Self::Journal(_) | Self::JournalIo(_) => "chat_event_log",
        }
    }

    fn is_durable_settlement_debt(&self) -> bool {
        matches!(self, Self::Journal(_) | Self::JournalIo(_))
    }
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
        let workspace_id = request.workspace_id.trim().to_string();
        if workspace_id.is_empty() {
            return Err(ManualCompressionError::Compression(
                "workspace id must not be empty".to_string(),
            ));
        }
        let runtime = self
            .chat_runtime_for_scope(&workspace_id)
            .await
            .map_err(|error| ManualCompressionError::Compression(error.to_string()))?;
        let turn_id = format!("manual-compression:{}", uuid::Uuid::new_v4());
        let lease = runtime
            .begin_turn(
                &self.session.foreground_turns,
                request.surface,
                &conversation_id,
                turn_id.clone(),
            )
            .await?;
        let execution = match runtime.agent_for(&conversation_id).await {
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
        let workspace_io_receipt = runtime.workspace_io_receipt();
        let product_data_io = self.session.product_data_io.clone();
        let chat_events = self.storage.chat_events.clone();
        let focus = request.focus;
        let keep_messages = request.keep_messages;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.session
            .foreground_turns
            .supervise(lease, move |lease| async move {
                let result = start_manual_compression_owned(
                    product_data_io,
                    chat_events,
                    workspace_id,
                    conversation_id.clone(),
                    conversation_id,
                    turn_id,
                    agent,
                    focus,
                    keep_messages,
                    workspace_io_receipt,
                    None,
                )
                .await;
                drop(execution);
                match &result {
                    Ok(_) => lease.settle(TurnOutcome::Completed),
                    Err(error) => lease.settle(TurnOutcome::Failed(AgentFailure::message(
                        error.code(),
                        error.to_string(),
                    ))),
                };
                let _delivered = result_tx.send(result);
            })
            .map_err(|error| {
                ManualCompressionError::Compression(format!(
                    "manual compression supervision failed: {error}"
                ))
            })?;
        result_rx.await.map_err(|_| {
            ManualCompressionError::Compression(
                "manual compression foreground owner ended without a typed result".to_string(),
            )
        })?
    }

    /// Execute compression and journal its safe point using a caller-owned
    /// runtime admission and exact pooled Agent receipt.
    ///
    /// Surface adapters that must publish their own foreground root use this
    /// method to avoid a second admission authority. The caller owns terminal
    /// settlement and must retain its pool execution receipt until this future
    /// returns or is cancelled.
    #[allow(clippy::too_many_arguments)]
    pub async fn compress_conversation_with_agent(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        agent_conversation_id: &str,
        turn_id: &str,
        agent: &crate::agent_handle::AgentHandle,
        focus: Option<String>,
        keep_messages: usize,
        workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
        cancel: Option<echo_agent::agent::CancellationToken>,
    ) -> Result<ManualCompressionReceipt, ManualCompressionError> {
        start_manual_compression_owned(
            self.session.product_data_io.clone(),
            self.storage.chat_events.clone(),
            workspace_id.to_string(),
            conversation_id.to_string(),
            agent_conversation_id.to_string(),
            turn_id.to_string(),
            agent.clone(),
            focus,
            keep_messages,
            workspace_io_receipt,
            cancel,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_manual_compression_owned(
    product_data_io: crate::product_data_io::ProductDataIoService,
    chat_events: std::sync::Arc<crate::chat_event_log::ChatEventLog>,
    workspace_id: String,
    conversation_id: String,
    agent_conversation_id: String,
    turn_id: String,
    agent: crate::agent_handle::AgentHandle,
    focus: Option<String>,
    keep_messages: usize,
    workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
    cancel: Option<echo_agent::agent::CancellationToken>,
) -> Result<ManualCompressionReceipt, ManualCompressionError> {
    let actual = agent
        .read(|agent| agent.conversation_id().map(str::to_string))
        .await;
    if actual.as_deref() != Some(agent_conversation_id.as_str()) {
        return Err(ManualCompressionError::ConversationIdentity {
            expected: agent_conversation_id,
            actual,
        });
    }

    let flow = product_data_io
        .begin_owned_flow("manual context compression")
        .map_err(|error| ManualCompressionError::JournalIo(error.to_string()))?;
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = run_manual_compression_flow(
            &flow,
            chat_events,
            workspace_id,
            conversation_id,
            turn_id,
            agent,
            focus,
            keep_messages,
            workspace_io_receipt,
            cancel,
        )
        .await;
        let durable_failure = result
            .as_ref()
            .err()
            .filter(|error| error.is_durable_settlement_debt())
            .map(ToString::to_string);
        let _delivered = result_tx.send(result);
        flow.settle(durable_failure);
    });
    result_rx.await.map_err(|_| {
        ManualCompressionError::JournalIo(
            "manual compression owner ended without a typed result".to_string(),
        )
    })?
}

#[allow(clippy::too_many_arguments)]
async fn run_manual_compression_flow(
    flow: &crate::product_data_io::ProductDataIoFlow,
    chat_events: std::sync::Arc<crate::chat_event_log::ChatEventLog>,
    workspace_id: String,
    conversation_id: String,
    turn_id: String,
    agent: crate::agent_handle::AgentHandle,
    focus: Option<String>,
    keep_messages: usize,
    workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
    cancel: Option<echo_agent::agent::CancellationToken>,
) -> Result<ManualCompressionReceipt, ManualCompressionError> {
    let focus = focus.filter(|value| !value.trim().is_empty());
    let compression = agent.read_async(|agent| {
        Box::pin(async move {
            if let Some(focus) = focus {
                agent
                    .force_compress_with_focus_and_hooks(&focus, keep_messages, "manual")
                    .await
            } else {
                agent.force_compress_context().await
            }
        })
    });
    tokio::pin!(compression);
    let compression = match cancel {
        Some(cancel) => {
            tokio::select! {
                biased;
                result = &mut compression => result,
                _ = cancel.cancelled() => return Err(ManualCompressionError::Cancelled),
            }
        }
        None => compression.await,
    };
    let (stats, checkpoint) = match compression {
        Ok(result) => result,
        Err(error) => {
            let detail = error.to_string();
            return Err(ManualCompressionError::Compression(detail));
        }
    };

    let event = ChatDriverEvent::ContextCompressed {
        before_count: stats.before_count,
        after_count: stats.after_count,
        before_tokens: stats.before_tokens,
        after_tokens: stats.after_tokens,
    };
    let append_conversation_id = conversation_id.clone();
    let appended = flow
        .run("persist manual compression safe point", move || {
            let _workspace_receipt = workspace_io_receipt;
            chat_events.append(
                &workspace_id,
                Some(&append_conversation_id),
                &turn_id,
                event,
            )
        })
        .await
        .map_err(|error| ManualCompressionError::JournalIo(error.to_string()));
    let envelope = match appended {
        Ok(Ok(envelope)) => envelope,
        Err(error) => return Err(error),
        Ok(Err(error)) => return Err(error.into()),
    };
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

    #[test]
    fn manual_compression_errors_preserve_typed_terminal_codes() {
        assert_eq!(
            ManualCompressionError::ConversationIdentity {
                expected: "expected".to_string(),
                actual: Some("actual".to_string()),
            }
            .code(),
            "conversation_identity"
        );
        assert_eq!(
            ManualCompressionError::Compression("failed".to_string()).code(),
            "manual_compression"
        );
        assert_eq!(
            ManualCompressionError::Journal(ChatEventLogError::InvalidIdentity(
                "invalid".to_string(),
            ))
            .code(),
            "chat_event_log"
        );
        assert_eq!(
            ManualCompressionError::JournalIo("failed".to_string()).code(),
            "chat_event_log"
        );
    }

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
            crate::product_data_io::ProductDataIoService::new(),
        )?;
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
                workspace_id: "global".to_string(),
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
        let replay = state.storage.chat_events.replay(
            "global",
            Some(id),
            &receipt.envelope.root_turn_id,
            0,
        )?;
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
                workspace_id: "global".to_string(),
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

    #[tokio::test]
    async fn cancellation_after_transform_waits_for_the_journal_safe_point()
    -> Result<(), Box<dyn Error>> {
        let id = "manual-compression-commit-barrier";
        let (state, _temp) = state_fixture(id).await?;
        let state = Arc::new(state);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        state.session.product_data_io.install_test_barrier(
            "persist manual compression safe point",
            entered_tx,
            release_rx,
        );
        let cancel = echo_agent::agent::CancellationToken::new();
        let operation_cancel = cancel.clone();
        let operation_state = Arc::clone(&state);
        let agent = state.connection.agent.clone();
        let operation = tokio::spawn(async move {
            operation_state
                .compress_conversation_with_agent(
                    "global",
                    id,
                    id,
                    "manual-compression-barrier-turn",
                    &agent,
                    None,
                    12,
                    crate::state::ScopedWorkspaceIoReceipt::global_for_test("."),
                    Some(operation_cancel),
                )
                .await
        });
        entered_rx.await?;
        cancel.cancel();
        release_tx
            .send(())
            .map_err(|_| std::io::Error::other("manual compression release receiver closed"))?;
        let receipt = operation.await??;
        let replay = state.storage.chat_events.replay(
            "global",
            Some(id),
            &receipt.envelope.root_turn_id,
            0,
        )?;
        assert_eq!(replay.events.len(), 1);
        assert!(matches!(
            replay.events.first().map(|event| &event.payload),
            Some(ChatDriverEvent::ContextCompressed { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_keeps_foreground_and_flow_owned_until_safe_point()
    -> Result<(), Box<dyn Error>> {
        let id = "manual-compression-caller-drop";
        let (state, _temp) = state_fixture(id).await?;
        let state = Arc::new(state);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        state.session.product_data_io.install_test_barrier(
            "persist manual compression safe point",
            entered_tx,
            release_rx,
        );
        let operation_state = Arc::clone(&state);
        let caller = tokio::spawn(async move {
            operation_state
                .compress_conversation_owned(ManualCompressionRequest {
                    workspace_id: "global".to_string(),
                    conversation_id: id.to_string(),
                    surface: ForegroundTurnSurface::Gui,
                    focus: None,
                    keep_messages: 12,
                })
                .await
        });
        entered_rx.await?;
        caller.abort();
        let _cancelled_waiter = caller.await;
        assert!(
            state
                .session
                .foreground_turns
                .snapshot(ForegroundTurnSurface::Gui, id)
                .is_some()
        );
        state.session.product_data_io.begin_shutdown()?;
        let shutdown_service = state.session.product_data_io.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.join_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release_tx
            .send(())
            .map_err(|_| std::io::Error::other("manual compression release receiver closed"))?;
        shutdown.await?.map_err(std::io::Error::other)?;
        assert!(
            state
                .session
                .foreground_turns
                .snapshot(ForegroundTurnSurface::Gui, id)
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn append_domain_failure_is_typed_and_reported_as_shutdown_debt()
    -> Result<(), Box<dyn Error>> {
        let id = "manual-compression-journal-debt";
        let (state, temp) = state_fixture(id).await?;
        let chat_root = temp.path().join("chat-events");
        std::fs::remove_dir_all(&chat_root)?;
        std::fs::write(&chat_root, b"block chat journal directory")?;
        let agent = state.connection.agent.clone();
        let result = state
            .compress_conversation_with_agent(
                "global",
                id,
                id,
                "manual-compression-journal-debt-turn",
                &agent,
                None,
                12,
                crate::state::ScopedWorkspaceIoReceipt::global_for_test("."),
                None,
            )
            .await;
        assert!(matches!(
            result,
            Err(ManualCompressionError::Journal(_)) | Err(ManualCompressionError::JournalIo(_))
        ));
        let debt = state
            .session
            .product_data_io
            .join_shutdown()
            .await
            .err()
            .ok_or_else(|| {
                std::io::Error::other("journal failure was absent from shutdown debt")
            })?;
        assert!(debt.contains("chat event") || debt.contains("journal"));
        Ok(())
    }
}
