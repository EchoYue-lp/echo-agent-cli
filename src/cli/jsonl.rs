//! Non-interactive JSONL transport over the shared chat driver.
//!
//! Ordinary chat events are accepted only after journaling. Finite app-core
//! control commands may additionally emit their typed receipt as one JSONL
//! record before the journaled terminal status.

use std::io::Write;
use std::sync::{Arc, Mutex};

use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink};
use echo_agent_app_core::chat_event_log::ChatEventEnvelope;

use crate::cli::args::JsonlApprovalPolicy;

/// Writes one canonical, already-journaled chat envelope per line.
pub struct JsonlChatSink {
    output: Mutex<Box<dyn Write + Send>>,
}

impl JsonlChatSink {
    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()))
    }

    fn new(output: Box<dyn Write + Send>) -> Self {
        Self {
            output: Mutex::new(output),
        }
    }

    /// Emit the typed result of the finite reflection control command.
    pub fn write_reflection_receipt(
        &self,
        receipt: &echo_agent_app_core::reflection::ReflectionReceipt,
    ) -> bool {
        self.write_json_line(&serde_json::json!({
            "source": "reflection_receipt",
            "event": receipt,
        }))
    }

    fn write_json_line(&self, value: &impl serde::Serialize) -> bool {
        let mut encoded = match serde_json::to_vec(value) {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::error!(%error, "failed to serialize JSONL output");
                return false;
            }
        };
        encoded.push(b'\n');
        let mut output = match self.output.lock() {
            Ok(output) => output,
            Err(error) => {
                tracing::error!(%error, "JSONL output lock is unavailable");
                return false;
            }
        };
        if let Err(error) = output.write_all(&encoded).and_then(|()| output.flush()) {
            tracing::error!(%error, "failed to write JSONL output");
            return false;
        }
        true
    }
}

impl ChatSink for JsonlChatSink {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        tracing::error!("JSONL transport rejected an event that bypassed the chat journal");
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        self.write_json_line(&envelope)
    }
}

/// Non-interactive HITL adapter for one-shot JSONL execution. Requests remain
/// visible in the canonical event stream; the configured policy determines
/// whether approval requests are accepted. Input and selection requests are
/// rejected because this transport has no follow-up input channel.
pub struct JsonlHumanLoopProvider {
    sink: Arc<dyn ChatSink>,
    approval_policy: JsonlApprovalPolicy,
}

impl JsonlHumanLoopProvider {
    pub fn new(sink: Arc<dyn ChatSink>, approval_policy: JsonlApprovalPolicy) -> Self {
        Self {
            sink,
            approval_policy,
        }
    }
}

impl echo_agent::human_loop::HumanLoopProvider for JsonlHumanLoopProvider {
    fn request(
        &self,
        request: echo_agent::human_loop::HumanLoopRequest,
    ) -> futures::future::BoxFuture<
        '_,
        echo_agent::error::Result<echo_agent::human_loop::HumanLoopResponse>,
    > {
        use echo_agent::human_loop::{HumanLoopKind, HumanLoopResponse};

        let request_id = request
            .request_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let event = match &request.kind {
            HumanLoopKind::Approval => ChatDriverEvent::ApprovalRequest {
                request_id,
                tool_name: request.tool_name.unwrap_or_else(|| "unknown".to_string()),
                args: request.args.unwrap_or(serde_json::Value::Null),
                prompt: request.prompt,
            },
            HumanLoopKind::Input => ChatDriverEvent::InputRequest {
                request_id,
                prompt: request.prompt,
            },
            HumanLoopKind::Selection => ChatDriverEvent::SelectionRequest {
                request_id,
                prompt: request.prompt,
                options: request.options.unwrap_or_default(),
                task_id: request.task_id,
                context: request.context,
                phase: request.phase,
            },
        };
        let delivered = self.sink.on_event(event);
        let response = match (&request.kind, self.approval_policy) {
            (HumanLoopKind::Approval, JsonlApprovalPolicy::AutoApprove) => {
                HumanLoopResponse::Approved
            }
            (HumanLoopKind::Approval, JsonlApprovalPolicy::Reject) => HumanLoopResponse::Rejected {
                reason: Some("JSONL approval policy rejected the request".to_string()),
            },
            (HumanLoopKind::Input | HumanLoopKind::Selection, _) => HumanLoopResponse::Rejected {
                reason: Some(
                    "JSONL one-shot mode cannot accept follow-up HITL input or selection"
                        .to_string(),
                ),
            },
        };
        Box::pin(async move {
            if delivered {
                Ok(response)
            } else {
                Err(echo_agent::error::ReactError::Other(
                    "JSONL output closed before the HITL request was delivered".to_string(),
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, MutexGuard};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Result<Self, String> {
            let path = std::env::temp_dir().join(format!("eko-jsonl-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Default)]
    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            lock_output(&self.0).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lock_output(output: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
        output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<serde_json::Value>>);

    impl ChatSink for RecordingSink {
        fn on_event(&self, event: ChatDriverEvent) -> bool {
            let value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
            lock_events(&self.0).push(value);
            true
        }
    }

    fn lock_events(
        events: &Mutex<Vec<serde_json::Value>>,
    ) -> std::sync::MutexGuard<'_, Vec<serde_json::Value>> {
        events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn stdout_contains_only_canonical_journal_envelopes() -> Result<(), String> {
        let temp = TestDir::new()?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let shared = SharedOutput::default();
        let captured = shared.0.clone();
        let sink = JsonlChatSink::new(Box::new(shared));
        let events = [
            ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        ];
        for event in events {
            let envelope = log
                .append("global", Some("jsonl-conversation"), "jsonl-turn", event)
                .map_err(|error| error.to_string())?;
            assert!(sink.on_journaled_event(envelope));
        }

        let bytes = lock_output(&captured).clone();
        let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        let envelopes = text
            .lines()
            .map(serde_json::from_str::<ChatEventEnvelope>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        assert_eq!(envelopes.len(), 2);
        assert_eq!(envelopes.first().map(|event| event.sequence), Some(1));
        assert_eq!(envelopes.get(1).map(|event| event.sequence), Some(2));
        let canonical = envelopes
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .join("\n")
            + "\n";
        assert_eq!(bytes, canonical.as_bytes());
        Ok(())
    }

    #[test]
    fn raw_events_are_rejected_before_stdout() {
        let output = SharedOutput::default();
        let captured = output.0.clone();
        let sink = JsonlChatSink::new(Box::new(output));
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "completed".to_string(),
        }));
        assert!(lock_output(&captured).is_empty());
    }

    #[test]
    fn extension_receipt_keeps_its_typed_journal_tag() -> Result<(), String> {
        let temp = TestDir::new()?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let shared = SharedOutput::default();
        let captured = shared.0.clone();
        let sink = JsonlChatSink::new(Box::new(shared));
        let receipt = echo_agent_app_core::extension_commands::ExtensionCommandReceipt::failed(
            echo_agent_app_core::extension_commands::ExtensionKind::Mcp,
            echo_agent_app_core::extension_commands::ExtensionCommandIdentity {
                request_id: "request-1".to_string(),
                operation_id: "operation-1".to_string(),
            },
            "global",
            "fixture failure",
        );
        let envelope = log
            .append(
                "global",
                Some("jsonl-conversation"),
                "jsonl-turn",
                ChatDriverEvent::ExtensionReceipt(Box::new(receipt)),
            )
            .map_err(|error| error.to_string())?;
        assert!(sink.on_journaled_event(envelope));

        let bytes = lock_output(&captured).clone();
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        assert_eq!(
            value
                .get("payload")
                .and_then(|payload| payload.get("source"))
                .and_then(serde_json::Value::as_str),
            Some("extension_receipt")
        );
        let decoded: ChatEventEnvelope =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        assert!(matches!(
            decoded.payload,
            ChatDriverEvent::ExtensionReceipt(receipt)
                if receipt.status()
                    == echo_agent_app_core::extension_commands::ExtensionCommandStatus::Failed
        ));
        Ok(())
    }

    #[test]
    fn reflection_receipt_keeps_its_typed_jsonl_fields() -> Result<(), String> {
        let shared = SharedOutput::default();
        let captured = shared.0.clone();
        let sink = JsonlChatSink::new(Box::new(shared));
        let receipt = echo_agent_app_core::reflection::reflection_receipt_fixture();
        assert!(sink.write_reflection_receipt(&receipt));

        let bytes = lock_output(&captured).clone();
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("source").and_then(serde_json::Value::as_str),
            Some("reflection_receipt")
        );
        echo_agent_app_core::reflection::validate_reflection_receipt_wire(
            value
                .get("event")
                .ok_or_else(|| "JSONL reflection event is missing".to_string())?,
        )
    }

    #[test]
    fn hitl_provider_emits_typed_request_and_applies_policy() -> Result<(), String> {
        let sink = Arc::new(RecordingSink::default());
        let provider = JsonlHumanLoopProvider::new(sink.clone(), JsonlApprovalPolicy::AutoApprove);
        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        let response = runtime
            .block_on(echo_agent::human_loop::HumanLoopProvider::request(
                &provider,
                echo_agent::human_loop::HumanLoopRequest::approval(
                    "shell",
                    serde_json::json!({"command": "cargo test"}),
                ),
            ))
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            response,
            echo_agent::human_loop::HumanLoopResponse::Approved
        ));
        let events = lock_events(&sink.0);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events
                .first()
                .and_then(|event| event.get("source"))
                .and_then(serde_json::Value::as_str),
            Some("approval_request")
        );
        Ok(())
    }

    #[tokio::test]
    async fn one_shot_jsonl_rejects_follow_up_input_and_selection_exact_once() -> Result<(), String>
    {
        let sink = Arc::new(RecordingSink::default());
        let provider = JsonlHumanLoopProvider::new(sink.clone(), JsonlApprovalPolicy::AutoApprove);

        let input = echo_agent::human_loop::HumanLoopProvider::request(
            &provider,
            echo_agent::human_loop::HumanLoopRequest::input("Add missing context"),
        )
        .await
        .map_err(|error| error.to_string())?;
        let selection = echo_agent::human_loop::HumanLoopProvider::request(
            &provider,
            echo_agent::human_loop::HumanLoopRequest::selection(
                "task-1",
                "Choose next step",
                vec!["Retry".to_string(), "Skip".to_string()],
            ),
        )
        .await
        .map_err(|error| error.to_string())?;

        for response in [input, selection] {
            assert!(matches!(
                response,
                echo_agent::human_loop::HumanLoopResponse::Rejected { reason: Some(reason) }
                    if reason == "JSONL one-shot mode cannot accept follow-up HITL input or selection"
            ));
        }
        let events = lock_events(&sink.0);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter_map(|event| event.get("source"))
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>(),
            vec!["input_request", "selection_request"]
        );
        Ok(())
    }
}
