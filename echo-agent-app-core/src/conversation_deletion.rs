//! Application-owned aggregate deletion for one conversation.
//!
//! Each participant keeps its existing storage authority. This service only
//! coordinates idempotent cleanup and persists the final visibility boundary.

use crate::chat_event_log::ChatEventLog;
use crate::foreground_turn::{
    ForegroundTurnControl, ForegroundTurnError, ForegroundTurnLease, ForegroundTurnSurface,
};
use crate::tasks::task_runtime::TaskRuntimeStore;
use crate::tool_execution::{ToolExecutionRepository, ToolExecutionStatus, ToolExecutionSummary};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use echo_agent::memory::{Conversation, ConversationStore, NewConversation};
use echo_agent::state::RuntimeStateStore;
use echo_agent::tools::artifact::ToolOutputArtifactConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

const TOMBSTONE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum ConversationDeletionError {
    #[error("conversation id must not be empty")]
    EmptyConversationId,
    #[error("conversation store is unavailable")]
    StoreUnavailable,
    #[error("conversation not found: {0}")]
    NotFound(String),
    #[error("conversation {0} is blocked by a pending aggregate deletion")]
    DeletionPending(String),
    #[error("conversation {0} reappeared after its aggregate deletion was committed")]
    CommittedIdentityReappeared(String),
    #[error(
        "conversation {0} is visible after its authority deletion began; its generation is ambiguous"
    )]
    AmbiguousAuthorityCommit(String),
    #[error("foreground admission could not be suspended: {0}")]
    Foreground(#[from] ForegroundTurnError),
    #[error("conversation store operation failed: {0}")]
    ConversationStore(String),
    #[error("conversation agent cleanup failed: {0}")]
    AgentPool(String),
    #[error("task-runtime cleanup failed: {0}")]
    TaskRuntime(String),
    #[error("tool-execution cleanup failed: {0}")]
    ToolExecution(String),
    #[error("ordinary-chat journal cleanup failed: {0}")]
    ChatEvents(String),
    #[error("agent runtime-state cleanup failed: {0}")]
    RuntimeState(String),
    #[error("conversation artifact cleanup failed: {0}")]
    Artifacts(String),
    #[error("conversation deletion tombstone is corrupt at {path}: {message}")]
    CorruptTombstone { path: PathBuf, message: String },
    #[error("conversation deletion root is unsafe at {path}: {message}")]
    UnsafeTombstoneRoot { path: PathBuf, message: String },
    #[error("conversation deletion I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConversationDeletionReceipt {
    pub conversation_id: String,
    pub resumed: bool,
    pub cleanup_pending: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum DeletionStep {
    TaskRuntime,
    ToolExecutions,
    ChatEvents,
    RuntimeState,
    Artifacts,
    ConversationCommitStarted,
    Conversation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DeletionTombstone {
    schema_version: u32,
    conversation_id: String,
    created_at_ms: u64,
    completed: BTreeSet<DeletionStep>,
}

impl DeletionTombstone {
    fn new(conversation_id: &str) -> Self {
        Self {
            schema_version: TOMBSTONE_SCHEMA_VERSION,
            conversation_id: conversation_id.to_string(),
            created_at_ms: echo_agent::utils::time::now_millis(),
            completed: BTreeSet::new(),
        }
    }
}

fn authority_commit_started(tombstone: &DeletionTombstone) -> bool {
    tombstone.completed.contains(&DeletionStep::Conversation)
        || tombstone
            .completed
            .contains(&DeletionStep::ConversationCommitStarted)
}

/// Sole EKO owner for deleting the aggregate rooted at a conversation.
pub struct ConversationDeletionService {
    root: PathBuf,
    locks: DashMap<String, Arc<Mutex<()>>>,
}

struct ConversationLockRegistration<'a> {
    locks: &'a DashMap<String, Arc<Mutex<()>>>,
    key: String,
    lock: Arc<Mutex<()>>,
}

impl Drop for ConversationLockRegistration<'_> {
    fn drop(&mut self) {
        let Entry::Occupied(entry) = self.locks.entry(self.key.clone()) else {
            return;
        };
        if Arc::ptr_eq(entry.get(), &self.lock) && Arc::strong_count(&self.lock) == 2 {
            entry.remove();
        }
    }
}

impl ConversationDeletionService {
    pub fn at_default_root() -> Self {
        Self::new(crate::data_root::user_data_path("conversation-deletions"))
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            locks: DashMap::new(),
        }
    }

    pub fn ensure_admission_allowed(
        &self,
        conversation_id: &str,
    ) -> Result<(), ConversationDeletionError> {
        let conversation_id = validated_id(conversation_id)?;
        let path = self.tombstone_path(conversation_id);
        if self.load_tombstone(&path, conversation_id)?.is_some() {
            return Err(ConversationDeletionError::DeletionPending(
                conversation_id.to_string(),
            ));
        }
        Ok(())
    }

    pub async fn create_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
    ) -> Result<Conversation, ConversationDeletionError> {
        self.write_conversation(store, conversation, false).await
    }

    pub async fn ensure_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
    ) -> Result<Conversation, ConversationDeletionError> {
        self.write_conversation(store, conversation, true).await
    }

    pub async fn begin_foreground_turn(
        &self,
        foreground_turns: &ForegroundTurnControl,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: impl Into<String>,
    ) -> Result<ForegroundTurnLease, ConversationDeletionError> {
        self.begin_foreground_turn_scoped(
            foreground_turns,
            "global",
            surface,
            conversation_id,
            turn_id,
        )
        .await
    }

    pub async fn begin_foreground_turn_scoped(
        &self,
        foreground_turns: &ForegroundTurnControl,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: impl Into<String>,
    ) -> Result<ForegroundTurnLease, ConversationDeletionError> {
        let conversation_id = validated_id(conversation_id)?.to_string();
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        self.ensure_admission_allowed(&conversation_id)?;
        foreground_turns
            .begin_scoped(workspace_id, surface, conversation_id, turn_id)
            .map_err(ConversationDeletionError::Foreground)
    }

    pub async fn recover_committed_deletions(
        &self,
        conversation_store: &dyn ConversationStore,
    ) -> Result<Vec<ConversationDeletionReceipt>, ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let entries = fs::read_dir(&self.root).map_err(|source| ConversationDeletionError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ConversationDeletionError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json") {
                paths.push(path);
            }
        }
        paths.sort();

        let mut recovered = Vec::new();
        let mut first_error = None;
        for path in paths {
            let discovered = match self.read_tombstone(&path) {
                Ok(tombstone) => tombstone,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            };
            if self.tombstone_path(&discovered.conversation_id) != path {
                if first_error.is_none() {
                    first_error = Some(ConversationDeletionError::CorruptTombstone {
                        path,
                        message: "tombstone filename does not match its conversation identity"
                            .to_string(),
                    });
                }
                continue;
            }
            let conversation_id = discovered.conversation_id;
            let registration = self.lock_registration(&conversation_id);
            let _identity_lock = registration.lock.lock().await;
            let tombstone = match self.load_tombstone(&path, &conversation_id) {
                Ok(Some(tombstone)) => tombstone,
                Ok(None) => continue,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            };
            if !authority_commit_started(&tombstone) {
                continue;
            }
            match self
                .finish_committed_cleanup(&path, &conversation_id, tombstone, conversation_store)
                .await
            {
                Ok(receipt) => recovered.push(receipt),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(recovered),
        }
    }

    async fn write_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
        ensure: bool,
    ) -> Result<Conversation, ConversationDeletionError> {
        let conversation_id = validated_id(&conversation.conversation_id)?.to_string();
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        self.ensure_admission_allowed(&conversation_id)?;
        let result = if ensure {
            store.ensure_conversation(conversation).await
        } else {
            store.create_conversation(conversation).await
        };
        result.map_err(|error| ConversationDeletionError::ConversationStore(error.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn delete(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        agent_pool: Option<Arc<crate::agent_pool::AgentPool>>,
        task_runtime: Option<Arc<TaskRuntimeStore>>,
        tool_executions: Arc<ToolExecutionRepository>,
        chat_events: Arc<ChatEventLog>,
        runtime_state: Option<Arc<dyn RuntimeStateStore>>,
        foreground_turns: &ForegroundTurnControl,
        artifact_config: Option<ToolOutputArtifactConfig>,
    ) -> Result<ConversationDeletionReceipt, ConversationDeletionError> {
        let conversation_id = validated_id(conversation_id)?.to_string();
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        let _foreground_suspension = foreground_turns
            .suspend_conversation_admission_if_idle_scoped(workspace_id, &conversation_id)?;
        let tombstone_path = self.tombstone_path(&conversation_id);
        let (mut tombstone, resumed) =
            match self.load_tombstone(&tombstone_path, &conversation_id)? {
                Some(tombstone) => (tombstone, true),
                None => {
                    let store = conversation_store
                        .as_ref()
                        .ok_or(ConversationDeletionError::StoreUnavailable)?;
                    if store
                        .get_conversation(&conversation_id)
                        .await
                        .map_err(|error| {
                            ConversationDeletionError::ConversationStore(error.to_string())
                        })?
                        .is_none()
                    {
                        return Err(ConversationDeletionError::NotFound(conversation_id));
                    }
                    let tombstone = DeletionTombstone::new(&conversation_id);
                    self.persist_tombstone(&tombstone_path, &tombstone)?;
                    (tombstone, false)
                }
            };

        if authority_commit_started(&tombstone) {
            let store = conversation_store
                .as_ref()
                .ok_or(ConversationDeletionError::StoreUnavailable)?;
            return self
                .finish_committed_cleanup(
                    &tombstone_path,
                    &conversation_id,
                    tombstone,
                    store.as_ref(),
                )
                .await;
        }

        if let Some(pool) = agent_pool {
            retire_cached_agent(&pool, &conversation_id).await?;
        }
        if let Some(store) = task_runtime {
            quiesce_task_runs(&store, &conversation_id).await?;
            let remove_id = conversation_id.clone();
            crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store)
                .run_store("remove conversation TaskRuns", move |store| {
                    store.remove_conversation(&remove_id)
                })
                .await
                .map_err(|error| ConversationDeletionError::TaskRuntime(error.to_string()))?;
        }
        self.complete_step(&tombstone_path, &mut tombstone, DeletionStep::TaskRuntime)?;

        terminate_active_tools(&tool_executions, workspace_id, &conversation_id)?;
        tool_executions
            .remove_conversation(workspace_id, &conversation_id)
            .map_err(|error| ConversationDeletionError::ToolExecution(error.to_string()))?;
        self.complete_step(
            &tombstone_path,
            &mut tombstone,
            DeletionStep::ToolExecutions,
        )?;

        chat_events
            .remove_conversation(workspace_id, &conversation_id)
            .map_err(|error| ConversationDeletionError::ChatEvents(error.to_string()))?;
        self.complete_step(&tombstone_path, &mut tombstone, DeletionStep::ChatEvents)?;

        if let Some(store) = runtime_state {
            store
                .clear_conversation(&conversation_id)
                .await
                .map_err(|error| ConversationDeletionError::RuntimeState(error.to_string()))?;
        }
        self.complete_step(&tombstone_path, &mut tombstone, DeletionStep::RuntimeState)?;

        if let Some(config) = artifact_config {
            let id = conversation_id.clone();
            tokio::task::spawn_blocking(move || cleanup_artifacts(&config, &id))
                .await
                .map_err(|error| {
                    ConversationDeletionError::Artifacts(format!(
                        "artifact cleanup task did not settle: {error}"
                    ))
                })??;
        }
        self.complete_step(&tombstone_path, &mut tombstone, DeletionStep::Artifacts)?;

        let store = conversation_store
            .as_ref()
            .ok_or(ConversationDeletionError::StoreUnavailable)?;
        self.complete_step(
            &tombstone_path,
            &mut tombstone,
            DeletionStep::ConversationCommitStarted,
        )?;
        store
            .delete_conversation(&conversation_id)
            .await
            .map_err(|error| ConversationDeletionError::ConversationStore(error.to_string()))?;

        tombstone.completed.insert(DeletionStep::Conversation);
        let cleanup_pending = if let Err(error) =
            self.persist_tombstone(&tombstone_path, &tombstone)
        {
            tracing::warn!(conversation_id, %error, "conversation deletion committed but its marker could not record completion");
            true
        } else {
            self.retire_tombstone(&tombstone_path, &conversation_id)
        };
        Ok(ConversationDeletionReceipt {
            conversation_id,
            resumed,
            cleanup_pending,
        })
    }

    async fn finish_committed_cleanup(
        &self,
        path: &Path,
        conversation_id: &str,
        mut tombstone: DeletionTombstone,
        store: &dyn ConversationStore,
    ) -> Result<ConversationDeletionReceipt, ConversationDeletionError> {
        if store
            .get_conversation(conversation_id)
            .await
            .map_err(|error| ConversationDeletionError::ConversationStore(error.to_string()))?
            .is_some()
        {
            return if tombstone.completed.contains(&DeletionStep::Conversation) {
                Err(ConversationDeletionError::CommittedIdentityReappeared(
                    conversation_id.to_string(),
                ))
            } else {
                Err(ConversationDeletionError::AmbiguousAuthorityCommit(
                    conversation_id.to_string(),
                ))
            };
        }

        let cleanup_pending = if tombstone.completed.contains(&DeletionStep::Conversation) {
            self.retire_tombstone(path, conversation_id)
        } else {
            tombstone.completed.insert(DeletionStep::Conversation);
            if let Err(error) = self.persist_tombstone(path, &tombstone) {
                tracing::warn!(conversation_id, %error, "recovered deletion could not record completion");
                true
            } else {
                self.retire_tombstone(path, conversation_id)
            }
        };
        Ok(ConversationDeletionReceipt {
            conversation_id: conversation_id.to_string(),
            resumed: true,
            cleanup_pending,
        })
    }

    fn lock_registration(&self, conversation_id: &str) -> ConversationLockRegistration<'_> {
        let lock = self
            .locks
            .entry(conversation_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        ConversationLockRegistration {
            locks: &self.locks,
            key: conversation_id.to_string(),
            lock,
        }
    }

    fn tombstone_path(&self, conversation_id: &str) -> PathBuf {
        let digest = Sha256::digest(conversation_id.as_bytes());
        self.root.join(format!("{}.json", hex::encode(digest)))
    }

    fn load_tombstone(
        &self,
        path: &Path,
        expected_conversation_id: &str,
    ) -> Result<Option<DeletionTombstone>, ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let tombstone = match self.read_tombstone(path) {
            Ok(tombstone) => tombstone,
            Err(ConversationDeletionError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if tombstone.conversation_id != expected_conversation_id {
            return Err(ConversationDeletionError::CorruptTombstone {
                path: path.to_path_buf(),
                message: "conversation identity does not match".to_string(),
            });
        }
        Ok(Some(tombstone))
    }

    fn read_tombstone(&self, path: &Path) -> Result<DeletionTombstone, ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let bytes = echo_agent::utils::fs::read_existing(path).map_err(|source| {
            ConversationDeletionError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let tombstone: DeletionTombstone = serde_json::from_slice(&bytes).map_err(|error| {
            ConversationDeletionError::CorruptTombstone {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        if tombstone.schema_version != TOMBSTONE_SCHEMA_VERSION {
            return Err(ConversationDeletionError::CorruptTombstone {
                path: path.to_path_buf(),
                message: "schema version does not match".to_string(),
            });
        }
        Ok(tombstone)
    }

    fn complete_step(
        &self,
        path: &Path,
        tombstone: &mut DeletionTombstone,
        step: DeletionStep,
    ) -> Result<(), ConversationDeletionError> {
        tombstone.completed.insert(step);
        self.persist_tombstone(path, tombstone)
    }

    fn persist_tombstone(
        &self,
        path: &Path,
        tombstone: &DeletionTombstone,
    ) -> Result<(), ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let bytes = serde_json::to_vec(tombstone).map_err(|error| {
            ConversationDeletionError::CorruptTombstone {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        echo_agent::utils::fs::atomic_write(path, &bytes).map_err(|source| {
            ConversationDeletionError::Io {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    fn retire_tombstone(&self, path: &Path, conversation_id: &str) -> bool {
        if let Err(error) = self.remove_tombstone(path) {
            tracing::warn!(conversation_id, %error, "conversation deletion marker cleanup remains pending");
            true
        } else {
            false
        }
    }

    fn remove_tombstone(&self, path: &Path) -> Result<(), ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(ConversationDeletionError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ConversationDeletionError::CorruptTombstone {
                path: path.to_path_buf(),
                message: "completed tombstone path is not a real file".to_string(),
            });
        }
        fs::remove_file(path).map_err(|source| ConversationDeletionError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn ensure_tombstone_root(&self) -> Result<(), ConversationDeletionError> {
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(ConversationDeletionError::UnsafeTombstoneRoot {
                path: self.root.clone(),
                message: "coordinator root is not a real directory".to_string(),
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.root).map_err(|source| ConversationDeletionError::Io {
                    path: self.root.clone(),
                    source,
                })?;
                let metadata = fs::symlink_metadata(&self.root).map_err(|source| {
                    ConversationDeletionError::Io {
                        path: self.root.clone(),
                        source,
                    }
                })?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    Ok(())
                } else {
                    Err(ConversationDeletionError::UnsafeTombstoneRoot {
                        path: self.root.clone(),
                        message: "new coordinator root is not a real directory".to_string(),
                    })
                }
            }
            Err(source) => Err(ConversationDeletionError::Io {
                path: self.root.clone(),
                source,
            }),
        }
    }
}

fn validated_id(conversation_id: &str) -> Result<&str, ConversationDeletionError> {
    let conversation_id = conversation_id.trim();
    if conversation_id.is_empty() {
        Err(ConversationDeletionError::EmptyConversationId)
    } else {
        Ok(conversation_id)
    }
}

async fn retire_cached_agent(
    pool: &crate::agent_pool::AgentPool,
    conversation_id: &str,
) -> Result<(), ConversationDeletionError> {
    let Some(execution) = pool
        .lease_existing(conversation_id)
        .await
        .map_err(|error| ConversationDeletionError::AgentPool(error.to_string()))?
    else {
        return Ok(());
    };
    if pool
        .retire_execution(conversation_id, execution)
        .await
        .map_err(|error| ConversationDeletionError::AgentPool(error.to_string()))?
    {
        Ok(())
    } else {
        Err(ConversationDeletionError::AgentPool(format!(
            "conversation {conversation_id} still has an active pool execution"
        )))
    }
}

async fn quiesce_task_runs(
    store: &Arc<TaskRuntimeStore>,
    conversation_id: &str,
) -> Result<(), ConversationDeletionError> {
    let conversation_id = conversation_id.to_string();
    let runs = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone())
        .run_store("cancel conversation TaskRuns", move |store| {
            let runs = store.list_runs_for_conversation(&conversation_id)?;
            for run in &runs {
                store.request_cancel(&run.run_id)?;
            }
            Ok(runs)
        })
        .await
        .map_err(|error| ConversationDeletionError::TaskRuntime(error.to_string()))?;
    for run in &runs {
        store.wait_for_run_driver_idle(&run.run_id).await;
    }
    Ok(())
}

fn terminate_active_tools(
    repository: &ToolExecutionRepository,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), ConversationDeletionError> {
    let active = repository
        .summaries_for_conversation(workspace_id, conversation_id)
        .into_iter()
        .filter(|summary| summary.status == ToolExecutionStatus::Running)
        .collect::<Vec<ToolExecutionSummary>>();
    for execution in active {
        repository
            .terminate_orphan(
                workspace_id,
                &execution.owner,
                &execution.call_id,
                ToolExecutionStatus::Interrupted,
            )
            .map_err(|error| ConversationDeletionError::ToolExecution(error.to_string()))?;
    }
    Ok(())
}

fn cleanup_artifacts(
    config: &ToolOutputArtifactConfig,
    conversation_id: &str,
) -> Result<(), ConversationDeletionError> {
    echo_agent::tools::artifact::cleanup_tool_output_scope(config, conversation_id, None)
        .map_err(|error| ConversationDeletionError::Artifacts(error.to_string()))?;
    crate::prepared_turn::cleanup_user_input_scope(
        &config.root_dir.join("user-input"),
        conversation_id,
    )
    .map_err(|error| ConversationDeletionError::Artifacts(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_event_log::ChatEventRetention;
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile};
    use crate::tool_execution::ToolExecutionOwner;
    use echo_agent::agent::ToolInvocation;
    use echo_agent::memory::FileConversationStore;
    use echo_agent::state::{AgentCheckpoint, FileRuntimeStateStore};
    use echo_agent::tools::artifact::{ToolOutputArtifactIdentity, persist_tool_output};
    use std::error::Error;

    fn conversation(id: &str) -> NewConversation {
        NewConversation {
            conversation_id: id.to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("Deletion fixture".to_string()),
        }
    }

    fn file_store(root: &Path) -> Result<Arc<dyn ConversationStore>, Box<dyn Error>> {
        Ok(Arc::new(FileConversationStore::new(root)?))
    }

    #[test]
    fn workspace_scoped_markers_do_not_block_the_same_id_in_another_workspace()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let workspace_a = ConversationDeletionService::new(temp.path().join("a/deletions"));
        let workspace_b = ConversationDeletionService::new(temp.path().join("b/deletions"));
        let id = "shared-conversation-id";
        let tombstone = DeletionTombstone::new(id);
        workspace_a.persist_tombstone(&workspace_a.tombstone_path(id), &tombstone)?;

        assert!(matches!(
            workspace_a.ensure_admission_allowed(id),
            Err(ConversationDeletionError::DeletionPending(_))
        ));
        workspace_b.ensure_admission_allowed(id)?;
        Ok(())
    }

    #[tokio::test]
    async fn commit_started_with_missing_authority_recovers_cleanup_only()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let id = "committed-missing";
        let path = service.tombstone_path(id);
        let mut tombstone = DeletionTombstone::new(id);
        tombstone
            .completed
            .insert(DeletionStep::ConversationCommitStarted);
        service.persist_tombstone(&path, &tombstone)?;

        let receipts = service.recover_committed_deletions(store.as_ref()).await?;
        let receipt = receipts.first().ok_or_else(|| {
            std::io::Error::other("committed deletion did not produce a recovery receipt")
        })?;
        assert_eq!(receipt.conversation_id, id);
        assert!(receipt.resumed);
        assert!(!receipt.cleanup_pending);
        assert!(!path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn commit_started_with_visible_authority_is_ambiguous_and_never_deleted()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let id = "ambiguous-visible";
        store.create_conversation(conversation(id)).await?;
        let path = service.tombstone_path(id);
        let mut tombstone = DeletionTombstone::new(id);
        tombstone
            .completed
            .insert(DeletionStep::ConversationCommitStarted);
        service.persist_tombstone(&path, &tombstone)?;

        let error = service
            .recover_committed_deletions(store.as_ref())
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("ambiguous authority was accepted"))?;
        assert!(matches!(
            error,
            ConversationDeletionError::AmbiguousAuthorityCommit(ref found) if found == id
        ));
        assert!(store.get_conversation(id).await?.is_some());
        assert!(path.is_file());
        Ok(())
    }

    #[tokio::test]
    async fn active_foreground_turn_keeps_transcript_authority_visible()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let tools = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        let turns = ForegroundTurnControl::default();
        let id = "active-conversation";
        store.create_conversation(conversation(id)).await?;
        let lease = turns.begin(ForegroundTurnSurface::Gui, id, "active-turn")?;

        let error = service
            .delete(
                "global",
                id,
                Some(store.clone()),
                None,
                None,
                tools,
                events,
                None,
                &turns,
                None,
            )
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("active conversation was deleted"))?;
        assert!(matches!(
            error,
            ConversationDeletionError::Foreground(
                ForegroundTurnError::ActiveConversationTurns { .. }
            )
        ));
        assert!(store.get_conversation(id).await?.is_some());
        lease.settle(crate::chat_driver::TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_delete_retires_transcript_authority_and_its_marker()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let tools = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        let turns = ForegroundTurnControl::default();
        let id = "delete-complete";
        store.create_conversation(conversation(id)).await?;

        let receipt = service
            .delete(
                "global",
                id,
                Some(store.clone()),
                None,
                None,
                tools,
                events,
                None,
                &turns,
                None,
            )
            .await?;
        assert_eq!(receipt.conversation_id, id);
        assert!(!receipt.resumed);
        assert!(!receipt.cleanup_pending);
        assert!(store.get_conversation(id).await?.is_none());
        assert!(!service.tombstone_path(id).exists());
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_delete_cascades_through_every_real_conversation_authority()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let task_runtime = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            temp.path().join("task-runtime"),
        )?);
        let tools = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        let runtime_state: Arc<dyn RuntimeStateStore> = Arc::new(FileRuntimeStateStore::new(
            temp.path().join("runtime-state"),
        )?);
        let turns = ForegroundTurnControl::default();
        let id = "delete-all-authorities";
        let run_id = "run-delete-all";
        let turn_id = "turn-delete-all";
        store.create_conversation(conversation(id)).await?;

        task_runtime.create_run(
            run_id,
            "workspace-delete-all",
            id,
            turn_id,
            DomainProfile::General,
            "Delete every conversation-owned resource",
            "test",
            AttendedMode::Attended,
        )?;

        let owner = ToolExecutionOwner::Chat {
            message_id: turn_id.to_string(),
        };
        tools.project_start(
            "global",
            owner,
            Some(id),
            Some(run_id),
            "call-delete-all",
            &ToolInvocation {
                requested_name: "read_file".to_string(),
                requested_args: serde_json::json!({"path": "fixture.txt"}),
                name: "read_file".to_string(),
                args: serde_json::json!({"path": "fixture.txt"}),
                rewrites: Vec::new(),
            },
        )?;

        events.append(
            "global",
            Some(id),
            turn_id,
            crate::chat_driver::ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        )?;

        let mut checkpoint = AgentCheckpoint::new(id);
        checkpoint.messages_json = "[]".to_string();
        runtime_state.save_checkpoint(&checkpoint).await?;

        let artifact_config =
            ToolOutputArtifactConfig::new(temp.path().join("artifacts"), "conversation_or_30d")
                .threshold_bytes(1);
        let tool_artifact = persist_tool_output(
            artifact_config.clone(),
            ToolOutputArtifactIdentity {
                conversation_id: Some(id.to_string()),
                run_id: Some(run_id.to_string()),
                call_id: "call-delete-all".to_string(),
                tool_name: "read_file".to_string(),
            },
            "durable tool output",
        )?
        .ok_or_else(|| std::io::Error::other("tool artifact was not persisted"))?;
        let user_input = artifact_config
            .root_dir
            .join("user-input")
            .join(id)
            .join(turn_id)
            .join("input.txt");
        let user_input_parent = user_input
            .parent()
            .ok_or_else(|| std::io::Error::other("user-input fixture has no parent"))?;
        fs::create_dir_all(user_input_parent)?;
        fs::write(&user_input, b"durable user input")?;

        let receipt = service
            .delete(
                "global",
                id,
                Some(store.clone()),
                None,
                Some(task_runtime.clone()),
                tools.clone(),
                events.clone(),
                Some(runtime_state.clone()),
                &turns,
                Some(artifact_config),
            )
            .await?;

        assert!(!receipt.resumed);
        assert!(!receipt.cleanup_pending);
        assert!(store.get_conversation(id).await?.is_none());
        assert!(task_runtime.list_runs_for_conversation(id)?.is_empty());
        assert!(tools.summaries_for_conversation("global", id).is_empty());
        assert!(
            events
                .replay("global", Some(id), turn_id, 0)?
                .events
                .is_empty()
        );
        assert!(runtime_state.get_checkpoint(id).await?.is_none());
        assert!(!tool_artifact.path.exists());
        assert!(!user_input.exists());
        assert!(!service.tombstone_path(id).exists());
        Ok(())
    }
}
