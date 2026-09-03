//! EKO-owned archive projection for persisted conversations.
//!
//! Archive is a product-level visibility choice, not transcript state. Keeping
//! it here lets GUI, TUI, and channels share one durable projection without
//! adding EKO-specific fields to the reusable framework ConversationStore.

use echo_agent::utils::fs::atomic_write;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ArchiveFile {
    #[serde(default)]
    workspaces: HashMap<String, BTreeSet<String>>,
}

/// Durable workspace-scoped conversation archive projection.
pub struct ConversationArchiveStore {
    path: PathBuf,
    entries: Mutex<HashMap<String, BTreeSet<String>>>,
}

impl ConversationArchiveStore {
    pub fn at_default_path() -> Result<Self, String> {
        Self::open(crate::data_root::user_data_path(
            "conversation-archives.json",
        ))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let entries = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ArchiveFile>(&bytes) {
                Ok(file) => file.workspaces,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignoring corrupt conversation archive projection");
                    HashMap::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "conversation archive projection unavailable");
                HashMap::new()
            }
        };
        Ok(Self {
            path,
            entries: Mutex::new(entries),
        })
    }

    pub fn archived_ids(&self, workspace_id: &str) -> Result<Vec<String>, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "conversation archive store lock poisoned".to_string())?;
        Ok(entries
            .get(workspace_id)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default())
    }

    pub fn set_archived(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        archived: bool,
    ) -> Result<(), String> {
        if workspace_id.trim().is_empty() || conversation_id.trim().is_empty() {
            return Err("workspace_id and conversation_id must not be empty".to_string());
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "conversation archive store lock poisoned".to_string())?;
        let mut next = entries.clone();
        if archived {
            next.entry(workspace_id.to_string())
                .or_default()
                .insert(conversation_id.to_string());
        } else if let Some(ids) = next.get_mut(workspace_id) {
            ids.remove(conversation_id);
            if ids.is_empty() {
                next.remove(workspace_id);
            }
        }
        self.persist(&next)?;
        *entries = next;
        Ok(())
    }

    pub fn remove(&self, workspace_id: &str, conversation_id: &str) -> Result<(), String> {
        self.set_archived(workspace_id, conversation_id, false)
    }

    fn persist(&self, entries: &HashMap<String, BTreeSet<String>>) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&ArchiveFile {
            workspaces: entries.clone(),
        })
        .map_err(|error| format!("failed to encode conversation archive: {error}"))?;
        atomic_write(&self.path, &bytes)
            .map_err(|error| format!("failed to persist {}: {error}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_updates_are_workspace_scoped_and_durable() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("archives.json");
        let store = ConversationArchiveStore::open(&path)?;
        store.set_archived("workspace-a", "conversation-1", true)?;
        store.set_archived("workspace-b", "conversation-1", true)?;
        assert_eq!(
            store.archived_ids("workspace-a")?,
            vec!["conversation-1".to_string()]
        );
        assert_eq!(
            store.archived_ids("workspace-b")?,
            vec!["conversation-1".to_string()]
        );

        let reopened = ConversationArchiveStore::open(&path)?;
        assert_eq!(
            reopened.archived_ids("workspace-a")?,
            vec!["conversation-1".to_string()]
        );
        reopened.remove("workspace-a", "conversation-1")?;
        assert!(reopened.archived_ids("workspace-a")?.is_empty());
        Ok(())
    }
}
