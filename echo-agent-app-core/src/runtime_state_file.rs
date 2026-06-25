//! File-backed RuntimeStateStore (U1c: EKO is local — no SQLite).
//!
//! Implements the framework `RuntimeStateStore` trait over plain JSON files so
//! the app layer owns its storage (framework stays trait-only). One directory
//! per conversation under `<base>/runtime_state/<conversation_id>/`:
//!   - `nodes.json`       — the task-node DAG for that conversation
//!   - `checkpoint.json`  — the latest agent checkpoint (single-row upsert)
//!
//! Atomic writes use tmp+rename so a crash never leaves a half-written file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use echo_agent::state::{AgentCheckpoint, RuntimeStateStore, TaskNode, TaskNodeStatus};
use futures::future::BoxFuture;

/// File-backed runtime state store.
pub struct FileRuntimeStateStore {
    base: PathBuf,
    /// Serializes all writes (the framework trait is `&self`; a Mutex keeps the
    /// read-modify-write in `save_node`/`update_status` atomic).
    lock: Mutex<()>,
}

impl FileRuntimeStateStore {
    /// Create a file-backed state store rooted at `base/runtime_state/`.
    pub fn new(base: impl AsRef<Path>) -> anyhow::Result<Self> {
        let base = base.as_ref().join("runtime_state");
        std::fs::create_dir_all(&base)?;
        Ok(Self {
            base,
            lock: Mutex::new(()),
        })
    }

    fn conv_dir(&self, conversation_id: &str) -> PathBuf {
        self.base.join(conversation_id)
    }

    fn nodes_path(&self, conversation_id: &str) -> PathBuf {
        self.conv_dir(conversation_id).join("nodes.json")
    }

    fn checkpoint_path(&self, conversation_id: &str) -> PathBuf {
        self.conv_dir(conversation_id).join("checkpoint.json")
    }

    fn read_nodes_file(path: &Path) -> Vec<TaskNode> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => Vec::new(),
        }
    }

    fn write_nodes_file(path: &Path, nodes: &[TaskNode]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(nodes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("serialize: {e}"))
        })?;
        atomic_write(path, json.as_bytes())
    }

    fn to_react_err(e: impl std::fmt::Display) -> echo_agent::error::ReactError {
        echo_agent::error::ReactError::Other(format!("FileRuntimeStateStore: {e}"))
    }
}

impl RuntimeStateStore for FileRuntimeStateStore {
    fn save_node<'a>(
        &'a self,
        conversation_id: &'a str,
        node: &'a TaskNode,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            let path = self.nodes_path(conversation_id);
            let mut nodes = Self::read_nodes_file(&path);
            // Upsert: replace if same id, else push.
            if let Some(existing) = nodes.iter_mut().find(|n| n.id == node.id) {
                *existing = node.clone();
            } else {
                nodes.push(node.clone());
            }
            Self::write_nodes_file(&path, &nodes).map_err(Self::to_react_err)?;
            Ok(())
        })
    }

    fn load_nodes<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<Vec<TaskNode>>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            Ok(Self::read_nodes_file(&self.nodes_path(conversation_id)))
        })
    }

    fn update_status<'a>(
        &'a self,
        conversation_id: &'a str,
        node_id: &'a str,
        status: TaskNodeStatus,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            let path = self.nodes_path(conversation_id);
            let mut nodes = Self::read_nodes_file(&path);
            let now = Utc::now();
            let mut found = false;
            for n in nodes.iter_mut() {
                if n.id == node_id {
                    n.status = status.clone();
                    n.updated_at = now;
                    found = true;
                    break;
                }
            }
            if found {
                Self::write_nodes_file(&path, &nodes).map_err(Self::to_react_err)?;
            }
            // Match SQL semantics: UPDATE on 0 rows is a no-op (no error).
            Ok(())
        })
    }

    fn get_checkpoint<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<Option<AgentCheckpoint>>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            let path = self.checkpoint_path(conversation_id);
            match std::fs::read_to_string(&path) {
                Ok(s) => {
                    let cp: AgentCheckpoint =
                        serde_json::from_str(&s).map_err(Self::to_react_err)?;
                    Ok(Some(cp))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(Self::to_react_err(e)),
            }
        })
    }

    fn save_checkpoint<'a>(
        &'a self,
        checkpoint: &'a AgentCheckpoint,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            let path = self.checkpoint_path(&checkpoint.conversation_id);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(Self::to_react_err)?;
            }
            let json = serde_json::to_string_pretty(checkpoint).map_err(Self::to_react_err)?;
            atomic_write(&path, json.as_bytes()).map_err(Self::to_react_err)?;
            Ok(())
        })
    }

    fn clear_conversation<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(Self::to_react_err)?;
            let dir = self.conv_dir(conversation_id);
            // Remove the conversation directory if it exists; tolerate absence.
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Self::to_react_err(e)),
            }
        })
    }
}

/// Write `bytes` to `path` atomically (tmp + rename).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_runtime_state_lifecycle() {
        let tmp = std::env::temp_dir().join(format!(
            "echo-file-runtime-state-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = FileRuntimeStateStore::new(&tmp).unwrap();

        let node = TaskNode::new("node-1", "Plan task")
            .with_status(TaskNodeStatus::Running)
            .with_dependencies(vec!["dep-1".to_string()]);
        store.save_node("conv-1", &node).await.unwrap();

        let nodes = store.load_nodes("conv-1").await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "node-1");
        assert!(matches!(nodes[0].status, TaskNodeStatus::Running));

        store
            .update_status("conv-1", "node-1", TaskNodeStatus::Success)
            .await
            .unwrap();
        let nodes = store.load_nodes("conv-1").await.unwrap();
        assert!(matches!(nodes[0].status, TaskNodeStatus::Success));

        let checkpoint = AgentCheckpoint {
            conversation_id: "conv-1".to_string(),
            messages_json: "[]".to_string(),
            current_plan: Some("plan".to_string()),
            active_skills: vec!["coding".to_string()],
            blocked_reason: None,
            working_dir: None,
            timestamp: Utc::now(),
        };
        store.save_checkpoint(&checkpoint).await.unwrap();
        let cp = store.get_checkpoint("conv-1").await.unwrap().unwrap();
        assert_eq!(cp.active_skills, vec!["coding"]);

        // update_status on a missing node is a no-op (matches SQL).
        store
            .update_status("conv-1", "nope", TaskNodeStatus::Failed)
            .await
            .unwrap();
        let nodes = store.load_nodes("conv-1").await.unwrap();
        assert_eq!(nodes.len(), 1);

        store.clear_conversation("conv-1").await.unwrap();
        assert!(store.load_nodes("conv-1").await.unwrap().is_empty());
        assert!(store.get_checkpoint("conv-1").await.unwrap().is_none());

        // clear on a never-existing conversation is a no-op.
        store.clear_conversation("never").await.unwrap();

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
