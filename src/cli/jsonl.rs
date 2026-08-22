//! Non-interactive JSONL transport over the shared chat driver.

use std::io::Write;
use std::sync::Mutex;

use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink};
use echo_agent_app_core::chat_event_log::ChatEventEnvelope;

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
}

impl ChatSink for JsonlChatSink {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        tracing::error!("JSONL transport rejected an event that bypassed the chat journal");
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        let mut encoded = match serde_json::to_vec(&envelope) {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::error!(%error, "failed to serialize canonical JSONL chat envelope");
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
            tracing::error!(%error, "failed to write canonical JSONL chat envelope");
            return false;
        }
        true
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
}
