//! Checkpoint persistence for long-running tasks.

use chrono::{DateTime, Utc};
use echo_agent::memory::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Persisted state for resuming a long-running task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongRunningCheckpoint {
    /// Task ID this checkpoint belongs to
    pub task_id: String,
    /// Index of the last completed phase
    pub last_completed_phase: usize,
    /// Output data from completed phases (phase_id -> output JSON)
    pub phase_outputs: HashMap<String, serde_json::Value>,
    /// When this checkpoint was created
    pub created_at: DateTime<Utc>,
    /// Phase-internal state (e.g., "revision_count: 2" for writing loop)
    pub phase_state: HashMap<String, serde_json::Value>,
    /// Number of resume attempts
    pub resume_count: u32,
}

/// Store for long-running task checkpoints.
///
/// Uses the framework's `Store` trait (typically SQLite-backed).
pub struct LongRunningCheckpointStore {
    store: Arc<dyn Store>,
}

impl LongRunningCheckpointStore {
    const NAMESPACE: &'static [&'static str] = &["long_running_checkpoints"];

    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Save a checkpoint (upsert).
    pub async fn save(&self, checkpoint: &LongRunningCheckpoint) -> anyhow::Result<()> {
        let value = serde_json::to_value(checkpoint)?;
        self.store
            .put(Self::NAMESPACE, &checkpoint.task_id, value)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to save checkpoint: {e}"))
    }

    /// Load a checkpoint for a task.
    pub async fn load(&self, task_id: &str) -> anyhow::Result<Option<LongRunningCheckpoint>> {
        match self.store.get(Self::NAMESPACE, task_id).await {
            Ok(Some(item)) => {
                let cp: LongRunningCheckpoint = serde_json::from_value(item.value)?;
                Ok(Some(cp))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Failed to load checkpoint: {e}")),
        }
    }

    /// Delete a checkpoint (after successful completion).
    pub async fn delete(&self, task_id: &str) -> anyhow::Result<()> {
        self.store
            .delete(Self::NAMESPACE, task_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete checkpoint: {e}"))?;
        Ok(())
    }

    /// List all checkpoint task IDs.
    pub async fn list_pending(&self) -> anyhow::Result<Vec<String>> {
        let items = self
            .store
            .list(Self::NAMESPACE)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list checkpoints: {e}"))?;
        Ok(items.into_iter().map(|i| i.key).collect())
    }
}
