//! Lossless projection from canonical tool events into EKO's detail repository.
//!
//! The framework owns `ToolInvocation` and `ToolResult`; the application owns
//! durable UI detail. This module is the only adapter between those layers. It
//! does not maintain another event log, output file, cursor, or terminal state.

use crate::chat_driver::ChatDriverEvent;
use crate::chat_event_log::ChatEventEnvelope;
use crate::tasks::task_runtime::TaskRuntimeStore;
use crate::tasks::task_runtime::executor::{ExecEvent, ExecEventScope};
use crate::tasks::task_runtime::types::RuntimeEventKind;
use crate::tool_execution::{
    ToolExecutionDetailChannel, ToolExecutionError, ToolExecutionMutation, ToolExecutionOwner,
    ToolExecutionRepository, ToolExecutionStatus, ToolExecutionSummary,
};
use echo_agent::agent::{AgentEvent, ToolInvocation};
use echo_agent::tools::{ToolOutputChannel, ToolResult, ToolStreamEvent};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ToolExecutionProjectionError {
    #[error("invalid {event} projection payload: {message}")]
    InvalidPayload {
        event: &'static str,
        message: String,
    },
    #[error("TaskRuntime store is required to resolve run {0}")]
    RuntimeStoreUnavailable(String),
    #[error("TaskRuntime run not found: {0}")]
    RunNotFound(String),
    #[error("failed to read TaskRuntime run {run_id}: {message}")]
    RuntimeStore { run_id: String, message: String },
    #[error(transparent)]
    Repository(#[from] ToolExecutionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionProjectionKind {
    Started,
    Finished,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionProjectionUpdate {
    pub kind: ToolExecutionProjectionKind,
    pub agent: String,
    pub summary: ToolExecutionSummary,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolStartedPayload {
    call_id: String,
    invocation: ToolInvocation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCompletedPayload {
    call_id: String,
    name: String,
    result: ToolResult,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolOutputPayload {
    call_id: String,
    name: String,
    channel: Option<String>,
    chunk: Option<String>,
    message: Option<String>,
    percent: Option<u8>,
}

/// Sole app-core adapter from canonical event facts to tool-execution detail.
pub struct ToolExecutionProjector {
    repository: Arc<ToolExecutionRepository>,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
}

impl ToolExecutionProjector {
    pub fn new(
        repository: Arc<ToolExecutionRepository>,
        task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    ) -> Self {
        Self {
            repository,
            task_runtime_store,
        }
    }

    pub fn project_envelope(
        &self,
        envelope: &ChatEventEnvelope,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        match &envelope.payload {
            ChatDriverEvent::Agent(event) => self.project_agent_event(
                &event.payload,
                envelope.conversation_id.as_deref(),
                &envelope.message_id,
                &envelope.turn_id,
            ),
            ChatDriverEvent::Execution(event) => self.project_execution_event_for_conversation(
                event,
                envelope.conversation_id.as_deref(),
            ),
            _ => Ok(Vec::new()),
        }
    }

    /// Rebuild secondary detail from a bounded retained journal window. A
    /// retained result may lack an evicted start; canonical replay remains
    /// valid, and later complete pairs are still projected.
    pub fn rebuild_from_retained(
        &self,
        envelopes: &[ChatEventEnvelope],
    ) -> Result<(), ToolExecutionProjectionError> {
        for envelope in envelopes {
            match self.project_envelope(envelope) {
                Ok(_) => {}
                Err(ToolExecutionProjectionError::Repository(ToolExecutionError::NotFound(
                    call_id,
                ))) => {
                    tracing::warn!(
                        %call_id,
                        sequence = envelope.sequence,
                        "retained chat replay lacks the tool start needed to rebuild detail"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn project_agent_event(
        &self,
        event: &AgentEvent,
        conversation_id: Option<&str>,
        message_id: &str,
        turn_id: &str,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        let owner = ToolExecutionOwner::Chat {
            message_id: message_id.to_string(),
        };
        match event {
            AgentEvent::ToolCall {
                call_id,
                invocation,
            } => {
                let mutation = self.repository.project_start(
                    owner,
                    conversation_id,
                    Some(turn_id),
                    call_id,
                    invocation,
                )?;
                Ok(projection_updates_from_mutation(
                    ToolExecutionProjectionKind::Started,
                    "echo-assistant",
                    mutation,
                ))
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                result,
            } => {
                self.ensure_name_matches(&owner, call_id, name, "tool_result")?;
                let mutation = self.repository.project_finish(&owner, call_id, result)?;
                Ok(projection_updates_from_mutation(
                    ToolExecutionProjectionKind::Finished,
                    "echo-assistant",
                    mutation,
                ))
            }
            AgentEvent::ToolStream {
                call_id,
                name,
                event,
            } => {
                self.ensure_name_matches(&owner, call_id, name, "tool_stream")?;
                match event {
                    ToolStreamEvent::Progress { message, percent } => {
                        self.repository.project_stream(
                            &owner,
                            call_id,
                            ToolExecutionDetailChannel::Log,
                            &progress_text(message, *percent),
                        )?
                    }
                    ToolStreamEvent::Output { channel, chunk } => self.repository.project_stream(
                        &owner,
                        call_id,
                        detail_channel(*channel),
                        chunk,
                    )?,
                    ToolStreamEvent::Complete(_) => {}
                }
                Ok(Vec::new())
            }
            AgentEvent::Cancelled => {
                self.terminate_owner(&owner, ToolExecutionStatus::Cancelled, "echo-assistant")
            }
            AgentEvent::Error { failure, .. } => {
                let status = if failure.terminal_kind
                    == echo_agent::error::AgentTerminalKind::Cancelled
                {
                    ToolExecutionStatus::Cancelled
                } else if failure.terminal_kind == echo_agent::error::AgentTerminalKind::TimedOut {
                    ToolExecutionStatus::TimedOut
                } else {
                    ToolExecutionStatus::Unknown
                };
                self.terminate_owner(&owner, status, "echo-assistant")
            }
            AgentEvent::FinalAnswer(_) => {
                self.terminate_owner(&owner, ToolExecutionStatus::Unknown, "echo-assistant")
            }
            _ => Ok(Vec::new()),
        }
    }

    pub fn project_execution_event(
        &self,
        event: &ExecEvent,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        self.project_execution_event_for_conversation(event, None)
    }

    pub fn project_execution_event_for_conversation(
        &self,
        event: &ExecEvent,
        known_conversation_id: Option<&str>,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        if event.scope != ExecEventScope::Subagent {
            return Ok(Vec::new());
        }
        let Some(subagent_run_id) = event.subagent_run_id.as_deref() else {
            return Err(invalid_payload(
                "subagent event",
                "subagent_run_id is missing",
            ));
        };
        let owner = ToolExecutionOwner::Subagent {
            subagent_run_id: subagent_run_id.to_string(),
        };
        let agent = event.agent.as_deref().unwrap_or("echo-assistant");

        match event.event {
            RuntimeEventKind::ToolStarted => {
                let payload = decode_payload::<ToolStartedPayload>(&event.payload, "tool_started")?;
                let conversation_id = self.conversation_id(&event.run_id, known_conversation_id)?;
                let mutation = self.repository.project_start(
                    owner,
                    Some(&conversation_id),
                    Some(&event.run_id),
                    &payload.call_id,
                    &payload.invocation,
                )?;
                Ok(projection_updates_from_mutation(
                    ToolExecutionProjectionKind::Started,
                    agent,
                    mutation,
                ))
            }
            RuntimeEventKind::ToolCompleted => {
                let payload =
                    decode_payload::<ToolCompletedPayload>(&event.payload, "tool_completed")?;
                self.ensure_name_matches(
                    &owner,
                    &payload.call_id,
                    &payload.name,
                    "tool_completed",
                )?;
                let mutation =
                    self.repository
                        .project_finish(&owner, &payload.call_id, &payload.result)?;
                Ok(projection_updates_from_mutation(
                    ToolExecutionProjectionKind::Finished,
                    agent,
                    mutation,
                ))
            }
            RuntimeEventKind::ToolOutput => {
                let payload = decode_payload::<ToolOutputPayload>(&event.payload, "tool_output")?;
                self.ensure_name_matches(&owner, &payload.call_id, &payload.name, "tool_output")?;
                let (channel, text) = match (payload.channel, payload.chunk, payload.message) {
                    (Some(channel), Some(chunk), None) => (detail_channel_name(&channel)?, chunk),
                    (None, None, Some(message)) => (
                        ToolExecutionDetailChannel::Log,
                        progress_text(&message, payload.percent),
                    ),
                    _ => {
                        return Err(invalid_payload(
                            "tool_output",
                            "expected channel+chunk or message+optional percent",
                        ));
                    }
                };
                self.repository
                    .project_stream(&owner, &payload.call_id, channel, &text)?;
                Ok(Vec::new())
            }
            RuntimeEventKind::Cancelled => {
                self.terminate_owner(&owner, ToolExecutionStatus::Cancelled, agent)
            }
            RuntimeEventKind::TimedOut => {
                self.terminate_owner(&owner, ToolExecutionStatus::TimedOut, agent)
            }
            RuntimeEventKind::Completed | RuntimeEventKind::Failed => {
                self.terminate_owner(&owner, ToolExecutionStatus::Unknown, agent)
            }
            _ => Ok(Vec::new()),
        }
    }

    pub fn terminate_chat(
        &self,
        message_id: &str,
        status: ToolExecutionStatus,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        self.terminate_owner(
            &ToolExecutionOwner::Chat {
                message_id: message_id.to_string(),
            },
            status,
            "echo-assistant",
        )
    }

    pub fn project_subagent_started(
        &self,
        subagent_run_id: &str,
        conversation_id: Option<&str>,
        run_id: Option<&str>,
        call_id: &str,
        invocation: &ToolInvocation,
        agent: &str,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        let mutation = self.repository.project_start(
            ToolExecutionOwner::Subagent {
                subagent_run_id: subagent_run_id.to_string(),
            },
            conversation_id,
            run_id,
            call_id,
            invocation,
        )?;
        Ok(projection_updates_from_mutation(
            ToolExecutionProjectionKind::Started,
            agent,
            mutation,
        ))
    }

    pub fn project_subagent_completed(
        &self,
        subagent_run_id: &str,
        call_id: &str,
        name: &str,
        result: &ToolResult,
        agent: &str,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        let owner = ToolExecutionOwner::Subagent {
            subagent_run_id: subagent_run_id.to_string(),
        };
        self.ensure_name_matches(&owner, call_id, name, "subagent_tool_completed")?;
        let mutation = self.repository.project_finish(&owner, call_id, result)?;
        Ok(projection_updates_from_mutation(
            ToolExecutionProjectionKind::Finished,
            agent,
            mutation,
        ))
    }

    pub fn terminate_subagent(
        &self,
        subagent_run_id: &str,
        status: ToolExecutionStatus,
        agent: &str,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        self.terminate_owner(
            &ToolExecutionOwner::Subagent {
                subagent_run_id: subagent_run_id.to_string(),
            },
            status,
            agent,
        )
    }

    fn conversation_id(
        &self,
        run_id: &str,
        known_conversation_id: Option<&str>,
    ) -> Result<String, ToolExecutionProjectionError> {
        if let Some(conversation_id) = known_conversation_id {
            return Ok(conversation_id.to_string());
        }
        let store = self.task_runtime_store.as_ref().ok_or_else(|| {
            ToolExecutionProjectionError::RuntimeStoreUnavailable(run_id.to_string())
        })?;
        store
            .get_run(run_id)
            .map_err(|error| ToolExecutionProjectionError::RuntimeStore {
                run_id: run_id.to_string(),
                message: error.to_string(),
            })?
            .map(|run| run.conversation_id)
            .ok_or_else(|| ToolExecutionProjectionError::RunNotFound(run_id.to_string()))
    }

    fn ensure_name_matches(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        name: &str,
        event: &'static str,
    ) -> Result<(), ToolExecutionProjectionError> {
        let summary = self
            .repository
            .summary_for(owner, call_id)
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        if summary.name == name {
            Ok(())
        } else {
            Err(invalid_payload(
                event,
                format!(
                    "tool name {name:?} conflicts with persisted name {:?}",
                    summary.name
                ),
            ))
        }
    }

    fn terminate_owner(
        &self,
        owner: &ToolExecutionOwner,
        status: ToolExecutionStatus,
        agent: &str,
    ) -> Result<Vec<ToolExecutionProjectionUpdate>, ToolExecutionProjectionError> {
        self.repository
            .terminate_running_for_owner(owner, status)?
            .into_iter()
            .map(|summary| {
                Ok(projection_update(
                    ToolExecutionProjectionKind::Finished,
                    agent,
                    summary,
                ))
            })
            .collect()
    }
}

fn detail_channel(channel: ToolOutputChannel) -> ToolExecutionDetailChannel {
    match channel {
        ToolOutputChannel::Stdout => ToolExecutionDetailChannel::Stdout,
        ToolOutputChannel::Stderr => ToolExecutionDetailChannel::Stderr,
        ToolOutputChannel::Log => ToolExecutionDetailChannel::Log,
    }
}

fn detail_channel_name(
    channel: &str,
) -> Result<ToolExecutionDetailChannel, ToolExecutionProjectionError> {
    match channel {
        "stdout" => Ok(ToolExecutionDetailChannel::Stdout),
        "stderr" => Ok(ToolExecutionDetailChannel::Stderr),
        "log" => Ok(ToolExecutionDetailChannel::Log),
        _ => Err(invalid_payload(
            "tool_output",
            format!("unknown output channel {channel:?}"),
        )),
    }
}

fn progress_text(message: &str, percent: Option<u8>) -> String {
    percent.map_or_else(
        || message.to_string(),
        |percent| format!("[{percent}%] {message}"),
    )
}

fn projection_update(
    kind: ToolExecutionProjectionKind,
    agent: &str,
    summary: ToolExecutionSummary,
) -> ToolExecutionProjectionUpdate {
    ToolExecutionProjectionUpdate {
        kind,
        agent: agent.to_string(),
        summary,
    }
}

fn projection_updates_from_mutation(
    kind: ToolExecutionProjectionKind,
    agent: &str,
    mutation: ToolExecutionMutation,
) -> Vec<ToolExecutionProjectionUpdate> {
    mutation
        .changed
        .then(|| projection_update(kind, agent, mutation.summary))
        .into_iter()
        .collect()
}

fn invalid_payload(
    event: &'static str,
    message: impl Into<String>,
) -> ToolExecutionProjectionError {
    ToolExecutionProjectionError::InvalidPayload {
        event,
        message: message.into(),
    }
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    payload: &serde_json::Value,
    event: &'static str,
) -> Result<T, ToolExecutionProjectionError> {
    serde_json::from_value(payload.clone())
        .map_err(|error| invalid_payload(event, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ToolInvocationRewrite;
    use echo_agent::tools::{ToolFailure, ToolFailureCategory, ToolOutputChannel};

    fn invocation() -> ToolInvocation {
        ToolInvocation {
            requested_name: "run".to_string(),
            requested_args: serde_json::json!({"command": "build"}),
            name: "shell".to_string(),
            args: serde_json::json!({"command": "./build"}),
            rewrites: vec![ToolInvocationRewrite::Approval],
        }
    }

    fn tool_started(run_id: &str) -> ExecEvent {
        ExecEvent::subagent(
            run_id,
            "task-1",
            format!("{run_id}:task-1:1:1"),
            RuntimeEventKind::ToolStarted,
            serde_json::json!({
                "call_id": "call-1",
                "invocation": invocation(),
            }),
        )
    }

    #[test]
    fn agent_projection_preserves_rewrite_and_rich_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        let started = projector.project_agent_event(
            &AgentEvent::ToolCall {
                call_id: "call-1".to_string(),
                invocation: invocation(),
            },
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;
        assert_eq!(started.len(), 1);
        let started = started.first().ok_or("started projection missing")?;

        let mut result = ToolResult::error("deadline exceeded");
        result.failure = Some(ToolFailure::new(ToolFailureCategory::Timeout));
        result.data = Some(serde_json::json!({"retry_after_ms": 1000}));
        result.truncated = true;
        result
            .metadata
            .insert("attempt".to_string(), "1".to_string());
        projector.project_agent_event(
            &AgentEvent::ToolResult {
                call_id: "call-1".to_string(),
                name: "shell".to_string(),
                result,
            },
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;

        let detail = repository.detail_manifest(&started.summary.detail_ref)?;
        assert_eq!(detail.invocation.requested_name, "run");
        assert_eq!(detail.invocation.name, "shell");
        assert_eq!(detail.invocation.rewrites.len(), 1);
        let result = detail.result.ok_or("terminal result missing")?;
        assert_eq!(
            result.failure.map(|failure| failure.category),
            Some(ToolFailureCategory::Timeout)
        );
        assert_eq!(
            result.data,
            Some(serde_json::json!({"retry_after_ms": 1000}))
        );
        assert!(result.truncated);
        Ok(())
    }

    #[test]
    fn agent_stream_output_remains_nonterminal_and_visible_in_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        let started = projector.project_agent_event(
            &AgentEvent::ToolCall {
                call_id: "call-stream".to_string(),
                invocation: invocation(),
            },
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;
        let started = started.first().ok_or("started projection missing")?;
        let updates = projector.project_agent_event(
            &AgentEvent::ToolStream {
                call_id: "call-stream".to_string(),
                name: "shell".to_string(),
                event: ToolStreamEvent::Output {
                    channel: ToolOutputChannel::Stdout,
                    chunk: "live output".to_string(),
                },
            },
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;
        assert!(updates.is_empty());
        let detail = repository.detail_manifest(&started.summary.detail_ref)?;
        assert_eq!(detail.status, ToolExecutionStatus::Running);
        assert!(detail.result.is_none());
        let page = repository.read_output(&started.summary.detail_ref, None, 64)?;
        assert_eq!(
            page.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("live output")
        );
        assert!(!page.complete);
        Ok(())
    }

    #[test]
    fn runtime_projection_requires_conversation_authority() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository, None);
        assert!(matches!(
            projector.project_execution_event(&tool_started("run-1")),
            Err(ToolExecutionProjectionError::RuntimeStoreUnavailable(run_id))
                if run_id == "run-1"
        ));
        Ok(())
    }

    #[test]
    fn runtime_tool_output_uses_the_same_nonterminal_trace()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        let started = projector.project_execution_event_for_conversation(
            &tool_started("run-1"),
            Some("conversation-1"),
        )?;
        let started = started.first().ok_or("started projection missing")?;
        let output = ExecEvent::subagent(
            "run-1",
            "task-1",
            "run-1:task-1:1:1",
            RuntimeEventKind::ToolOutput,
            serde_json::json!({
                "call_id": "call-1",
                "name": "shell",
                "channel": "stderr",
                "chunk": "warning",
            }),
        );
        assert!(
            projector
                .project_execution_event_for_conversation(&output, Some("conversation-1"))?
                .is_empty()
        );
        let page = repository.read_output(&started.summary.detail_ref, None, 64)?;
        assert_eq!(
            page.chunks,
            vec![crate::tool_execution::ToolExecutionDetailChunk {
                channel: ToolExecutionDetailChannel::Stderr,
                text: "warning".to_string(),
            }]
        );
        assert!(!page.complete);
        Ok(())
    }

    #[test]
    fn runtime_projection_rejects_missing_run_before_repository_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let runtime = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let projector = ToolExecutionProjector::new(repository.clone(), Some(runtime));
        assert!(matches!(
            projector.project_execution_event(&tool_started("missing-run")),
            Err(ToolExecutionProjectionError::RunNotFound(run_id))
                if run_id == "missing-run"
        ));
        assert!(
            repository
                .summaries_for_conversation("conversation-1")
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn parent_terminal_closes_orphan_without_inventing_tool_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        projector.project_agent_event(
            &AgentEvent::ToolCall {
                call_id: "call-1".to_string(),
                invocation: invocation(),
            },
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;
        let updates = projector.project_agent_event(
            &AgentEvent::error_message("test", "provider failed"),
            Some("conversation-1"),
            "message-1",
            "turn-1",
        )?;
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates.first().map(|update| &update.summary.status),
            Some(&ToolExecutionStatus::Unknown)
        );
        Ok(())
    }

    #[test]
    fn duplicate_subagent_observers_do_not_reemit_the_same_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let projector = ToolExecutionProjector::new(repository, None);

        let first = projector.project_subagent_started(
            "subagent-run-1",
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &invocation(),
            "explorer",
        )?;
        let replay = projector.project_subagent_started(
            "subagent-run-1",
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &invocation(),
            "explorer",
        )?;
        assert_eq!(first.len(), 1);
        assert!(replay.is_empty());

        let result = ToolResult::success("done");
        let first = projector.project_subagent_completed(
            "subagent-run-1",
            "call-1",
            "shell",
            &result,
            "explorer",
        )?;
        let replay = projector.project_subagent_completed(
            "subagent-run-1",
            "call-1",
            "shell",
            &result,
            "explorer",
        )?;
        assert_eq!(first.len(), 1);
        assert!(replay.is_empty());
        Ok(())
    }
}
