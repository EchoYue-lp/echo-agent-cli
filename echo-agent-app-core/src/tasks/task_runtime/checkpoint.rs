//! Discardable checkpoint projection for the EKO TaskRuntime event fold.
//!
//! `events.jsonl` remains the sole authority. This module only serializes and
//! verifies a cursor plus [`EventFoldState`] so projection refresh can fold the
//! suffix after a validated checkpoint.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::event_rebuild::EventFoldState;

pub(crate) const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RuntimeCheckpoint {
    pub(crate) schema_version: u32,
    pub(crate) run_id: String,
    pub(crate) seq: i64,
    /// Byte immediately after `seq` in events.jsonl. This is a cache cursor,
    /// not part of the state hash or authority.
    pub(crate) event_byte_offset: u64,
    pub(crate) state_hash: String,
    pub(crate) state: EventFoldState,
}

impl RuntimeCheckpoint {
    pub(crate) fn new(
        run_id: &str,
        event_byte_offset: u64,
        state: EventFoldState,
    ) -> Result<Self, CheckpointError> {
        let seq = state.last_seq();
        let state_hash = checkpoint_hash(CHECKPOINT_SCHEMA_VERSION, run_id, seq, &state)?;
        Ok(Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            seq,
            event_byte_offset,
            state_hash,
            state,
        })
    }

    pub(crate) fn validate(&self, expected_run_id: &str) -> Result<(), CheckpointError> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err(CheckpointError::SchemaVersion {
                expected: CHECKPOINT_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if self.run_id != expected_run_id || self.state.run_id() != Some(expected_run_id) {
            return Err(CheckpointError::RunIdentity {
                expected: expected_run_id.to_string(),
                actual: self.run_id.clone(),
            });
        }
        if self.seq <= 0 || self.state.last_seq() != self.seq {
            return Err(CheckpointError::Sequence {
                envelope: self.seq,
                state: self.state.last_seq(),
            });
        }
        let actual = checkpoint_hash(
            self.schema_version,
            self.run_id.as_str(),
            self.seq,
            &self.state,
        )?;
        if actual != self.state_hash {
            return Err(CheckpointError::StateHash);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct CheckpointHashInput<'a> {
    schema_version: u32,
    run_id: &'a str,
    seq: i64,
    state: &'a EventFoldState,
}

fn checkpoint_hash(
    schema_version: u32,
    run_id: &str,
    seq: i64,
    state: &EventFoldState,
) -> Result<String, CheckpointError> {
    let value = serde_json::to_value(CheckpointHashInput {
        schema_version,
        run_id,
        seq,
        state,
    })
    .map_err(|error| CheckpointError::Encode(error.to_string()))?;
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| CheckpointError::Encode(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn canonicalize(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonicalize).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonicalize(value));
            }
            serde_json::Value::Object(sorted)
        }
        scalar => scalar,
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckpointError {
    #[error("checkpoint schema mismatch: expected {expected}, got {actual}")]
    SchemaVersion { expected: u32, actual: u32 },
    #[error("checkpoint run identity mismatch: expected {expected}, got {actual}")]
    RunIdentity { expected: String, actual: String },
    #[error("checkpoint seq mismatch: envelope {envelope}, state {state}")]
    Sequence { envelope: i64, state: i64 },
    #[error("checkpoint state hash mismatch")]
    StateHash,
    #[error("checkpoint encode failed: {0}")]
    Encode(String),
}

#[cfg(test)]
mod tests {
    use super::canonicalize;

    #[test]
    fn canonical_json_sorts_nested_object_keys() -> Result<(), String> {
        let left = serde_json::json!({"z": {"b": 2, "a": 1}, "a": 0});
        let right = serde_json::json!({"a": 0, "z": {"a": 1, "b": 2}});
        let left = serde_json::to_vec(&canonicalize(left)).map_err(|error| error.to_string())?;
        let right = serde_json::to_vec(&canonicalize(right)).map_err(|error| error.to_string())?;
        assert_eq!(left, right);
        Ok(())
    }
}
