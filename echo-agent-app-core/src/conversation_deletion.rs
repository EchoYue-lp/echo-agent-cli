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
    #[error("conversation deletion product-data I/O failed: {0}")]
    ProductDataIo(String),
}

impl ConversationDeletionError {
    fn is_durable_settlement_debt(&self) -> bool {
        !matches!(
            self,
            Self::EmptyConversationId
                | Self::StoreUnavailable
                | Self::NotFound(_)
                | Self::DeletionPending(_)
                | Self::Foreground(_)
        )
    }
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
#[derive(Clone)]
pub struct ConversationDeletionService {
    root: Arc<PathBuf>,
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    product_data_io: crate::product_data_io::ProductDataIoService,
    #[cfg(test)]
    before_lineage_barrier: Arc<std::sync::Mutex<Option<DeletionTestBarrier>>>,
    #[cfg(test)]
    io_fault: Arc<std::sync::Mutex<Option<DeletionIoFault>>>,
}

#[cfg(test)]
struct DeletionTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeletionIoFault {
    CreateRootBarrier,
    RemoveFileBarrier,
}

struct ConversationLockRegistration<'a> {
    locks: &'a DashMap<String, Arc<Mutex<()>>>,
    key: String,
    lock: Arc<Mutex<()>>,
}

#[derive(Clone, Copy)]
enum DeletionIo<'a> {
    Service(&'a crate::product_data_io::ProductDataIoService),
    Flow(&'a crate::product_data_io::ProductDataIoFlow),
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
    #[cfg(test)]
    pub fn at_default_root() -> Self {
        Self::new(crate::data_root::user_data_path("conversation-deletions"))
    }

    pub fn at_default_root_with_product_data_io(
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> Self {
        Self::new_with_product_data_io(
            crate::data_root::user_data_path("conversation-deletions"),
            product_data_io,
        )
    }

    #[cfg(test)]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::new_with_product_data_io(root, crate::product_data_io::ProductDataIoService::new())
    }

    pub fn new_with_product_data_io(
        root: impl Into<PathBuf>,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> Self {
        Self {
            root: Arc::new(root.into()),
            locks: Arc::new(DashMap::new()),
            product_data_io,
            #[cfg(test)]
            before_lineage_barrier: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            io_fault: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn install_before_lineage_barrier(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self
            .before_lineage_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(DeletionTestBarrier { entered, release });
    }

    #[cfg(test)]
    async fn wait_before_lineage_barrier(&self) {
        let barrier = self
            .before_lineage_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _entered = barrier.entered.send(());
            let _released = barrier.release.await;
        }
    }

    #[cfg(test)]
    fn fail_next_io(&self, fault: DeletionIoFault) {
        *self
            .io_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fault);
    }

    #[cfg(test)]
    fn take_io_fault(&self, expected: DeletionIoFault) -> bool {
        let mut fault = self
            .io_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let matches = fault.as_ref() == Some(&expected);
        if matches {
            fault.take();
            true
        } else {
            false
        }
    }

    async fn run_io<T, F>(
        &self,
        io: DeletionIo<'_>,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
        operation: &'static str,
        function: F,
    ) -> Result<T, ConversationDeletionError>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Result<T, ConversationDeletionError> + Send + 'static,
    {
        let service = self.clone();
        let function = move || {
            let _workspace = workspace;
            function(service)
        };
        match io {
            DeletionIo::Service(io) => io.run(operation, function).await,
            DeletionIo::Flow(io) => io.run(operation, function).await,
        }
        .map_err(|error| ConversationDeletionError::ProductDataIo(error.to_string()))?
    }

    pub async fn ensure_admission_allowed(
        &self,
        conversation_id: &str,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<(), ConversationDeletionError> {
        let conversation_id = validated_id(conversation_id)?.to_string();
        let path = self.tombstone_path(&conversation_id);
        let load_id = conversation_id.clone();
        if self
            .run_io(
                DeletionIo::Service(&self.product_data_io),
                workspace,
                "inspect conversation deletion admission",
                move |service| service.load_tombstone(&path, &load_id),
            )
            .await?
            .is_some()
        {
            return Err(ConversationDeletionError::DeletionPending(conversation_id));
        }
        Ok(())
    }

    pub async fn create_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Conversation, ConversationDeletionError> {
        self.write_conversation(store, conversation, false, workspace)
            .await
    }

    pub async fn ensure_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Conversation, ConversationDeletionError> {
        self.write_conversation(store, conversation, true, workspace)
            .await
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
            None,
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
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<ForegroundTurnLease, ConversationDeletionError> {
        let conversation_id = validated_id(conversation_id)?.to_string();
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        self.ensure_admission_allowed(&conversation_id, workspace)
            .await?;
        foreground_turns
            .begin_scoped(workspace_id, surface, conversation_id, turn_id)
            .map_err(ConversationDeletionError::Foreground)
    }

    pub async fn recover_committed_deletions(
        &self,
        conversation_store: Arc<dyn ConversationStore>,
        runtime_state: Option<Arc<dyn RuntimeStateStore>>,
        agent_pool: Option<Arc<crate::agent_pool::AgentPool>>,
        workspace_io_receipt: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Vec<ConversationDeletionReceipt>, ConversationDeletionError> {
        let flow = self
            .product_data_io
            .begin_owned_flow("recover committed conversation deletions")
            .map_err(|error| ConversationDeletionError::ProductDataIo(error.to_string()))?;
        let service = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = service
                .recover_committed_deletions_owned(
                    &flow,
                    conversation_store,
                    runtime_state,
                    agent_pool,
                    workspace_io_receipt,
                )
                .await;
            let durable_failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_durable_settlement_debt())
                .map(ToString::to_string);
            match result_tx.send(result) {
                Ok(()) => flow.settle(durable_failure),
                Err(result) => {
                    let failure =
                        durable_failure.or_else(|| result.err().map(|error| error.to_string()));
                    flow.settle(failure);
                }
            }
        });
        result_rx.await.map_err(|_| {
            ConversationDeletionError::ProductDataIo(
                "conversation deletion recovery owner ended without a typed result".to_string(),
            )
        })?
    }

    async fn recover_committed_deletions_owned(
        &self,
        flow: &crate::product_data_io::ProductDataIoFlow,
        conversation_store: Arc<dyn ConversationStore>,
        runtime_state: Option<Arc<dyn RuntimeStateStore>>,
        agent_pool: Option<Arc<crate::agent_pool::AgentPool>>,
        workspace_io_receipt: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Vec<ConversationDeletionReceipt>, ConversationDeletionError> {
        let discovered = self
            .run_io(
                DeletionIo::Flow(flow),
                workspace_io_receipt.clone(),
                "discover conversation deletion tombstones",
                |service| service.discover_tombstones_sync(),
            )
            .await?;

        let mut recovered = Vec::new();
        let mut first_error = None;
        for (path, discovered) in discovered {
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
            let load_path = path.clone();
            let load_conversation_id = conversation_id.clone();
            let tombstone = match self
                .run_io(
                    DeletionIo::Flow(flow),
                    workspace_io_receipt.clone(),
                    "load conversation deletion tombstone",
                    move |service| service.load_tombstone(&load_path, &load_conversation_id),
                )
                .await
            {
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
            let _retirement_receipts = match begin_runtime_lineage_retirements(
                agent_pool.as_ref(),
                runtime_state.as_ref(),
                &conversation_id,
            )
            .await
            {
                Ok(receipts) => receipts,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            };
            match self
                .finish_committed_cleanup(
                    &path,
                    &conversation_id,
                    tombstone,
                    conversation_store.as_ref(),
                    runtime_state.as_deref(),
                    DeletionIo::Flow(flow),
                    workspace_io_receipt.clone(),
                )
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

    pub(crate) async fn recover_committed_deletions_in_flow(
        &self,
        flow: &crate::product_data_io::ProductDataIoFlow,
        conversation_store: Arc<dyn ConversationStore>,
        runtime_state: Option<Arc<dyn RuntimeStateStore>>,
        agent_pool: Option<Arc<crate::agent_pool::AgentPool>>,
        workspace_io_receipt: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Vec<ConversationDeletionReceipt>, ConversationDeletionError> {
        self.recover_committed_deletions_owned(
            flow,
            conversation_store,
            runtime_state,
            agent_pool,
            workspace_io_receipt,
        )
        .await
    }

    async fn write_conversation(
        &self,
        store: &dyn ConversationStore,
        conversation: NewConversation,
        ensure: bool,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<Conversation, ConversationDeletionError> {
        let conversation_id = validated_id(&conversation.conversation_id)?.to_string();
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        self.ensure_admission_allowed(&conversation_id, workspace)
            .await?;
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
        workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
    ) -> Result<ConversationDeletionReceipt, ConversationDeletionError> {
        let workspace_id = workspace_id.to_string();
        let conversation_id = validated_id(conversation_id)?.to_string();
        let foreground_turns = foreground_turns.clone();
        let flow = self
            .product_data_io
            .begin_owned_flow("delete conversation aggregate")
            .map_err(|error| ConversationDeletionError::ProductDataIo(error.to_string()))?;
        let service = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = service
                .delete_owned(
                    &flow,
                    workspace_id,
                    conversation_id,
                    conversation_store,
                    agent_pool,
                    task_runtime,
                    tool_executions,
                    chat_events,
                    runtime_state,
                    foreground_turns,
                    artifact_config,
                    workspace_io_receipt,
                )
                .await;
            let durable_failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_durable_settlement_debt())
                .map(ToString::to_string);
            match result_tx.send(result) {
                Ok(()) => flow.settle(durable_failure),
                Err(result) => {
                    let failure =
                        durable_failure.or_else(|| result.err().map(|error| error.to_string()));
                    flow.settle(failure);
                }
            }
        });
        result_rx.await.map_err(|_| {
            ConversationDeletionError::ProductDataIo(
                "conversation deletion owner ended without a typed result".to_string(),
            )
        })?
    }

    #[allow(clippy::too_many_arguments)]
    async fn delete_owned(
        &self,
        flow: &crate::product_data_io::ProductDataIoFlow,
        workspace_id: String,
        conversation_id: String,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        agent_pool: Option<Arc<crate::agent_pool::AgentPool>>,
        task_runtime: Option<Arc<TaskRuntimeStore>>,
        tool_executions: Arc<ToolExecutionRepository>,
        chat_events: Arc<ChatEventLog>,
        runtime_state: Option<Arc<dyn RuntimeStateStore>>,
        foreground_turns: ForegroundTurnControl,
        artifact_config: Option<ToolOutputArtifactConfig>,
        workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
    ) -> Result<ConversationDeletionReceipt, ConversationDeletionError> {
        let registration = self.lock_registration(&conversation_id);
        let _identity_lock = registration.lock.lock().await;
        let _foreground_suspension = foreground_turns
            .suspend_conversation_admission_if_idle_scoped(&workspace_id, &conversation_id)?;
        let tombstone_path = self.tombstone_path(&conversation_id);
        let load_path = tombstone_path.clone();
        let load_id = conversation_id.clone();
        let loaded = self
            .run_io(
                DeletionIo::Flow(flow),
                Some(workspace_io_receipt.clone()),
                "load aggregate deletion tombstone",
                move |service| service.load_tombstone(&load_path, &load_id),
            )
            .await?;
        let (mut tombstone, resumed) = match loaded {
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
                let persist_path = tombstone_path.clone();
                let persist_tombstone = tombstone.clone();
                self.run_io(
                    DeletionIo::Flow(flow),
                    Some(workspace_io_receipt.clone()),
                    "persist aggregate deletion tombstone",
                    move |service| service.persist_tombstone(&persist_path, &persist_tombstone),
                )
                .await?;
                (tombstone, false)
            }
        };

        if authority_commit_started(&tombstone) {
            let store = conversation_store
                .as_ref()
                .ok_or(ConversationDeletionError::StoreUnavailable)?;
            let _retirement_receipts = begin_runtime_lineage_retirements(
                agent_pool.as_ref(),
                runtime_state.as_ref(),
                &conversation_id,
            )
            .await?;
            return self
                .finish_committed_cleanup(
                    &tombstone_path,
                    &conversation_id,
                    tombstone,
                    store.as_ref(),
                    runtime_state.as_deref(),
                    DeletionIo::Flow(flow),
                    Some(workspace_io_receipt.clone()),
                )
                .await;
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
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::TaskRuntime,
        )
        .await?;

        let tool_workspace_id = workspace_id.to_string();
        let tool_conversation_id = conversation_id.clone();
        self.run_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            "remove conversation tool executions",
            move |_service| {
                terminate_active_tools(
                    &tool_executions,
                    &tool_workspace_id,
                    &tool_conversation_id,
                )?;
                tool_executions
                    .remove_conversation(&tool_workspace_id, &tool_conversation_id)
                    .map_err(|error| ConversationDeletionError::ToolExecution(error.to_string()))
            },
        )
        .await?;
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::ToolExecutions,
        )
        .await?;

        let event_workspace_id = workspace_id.to_string();
        let event_conversation_id = conversation_id.clone();
        self.run_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            "remove conversation chat events",
            move |_service| {
                chat_events
                    .remove_conversation(&event_workspace_id, &event_conversation_id)
                    .map_err(|error| ConversationDeletionError::ChatEvents(error.to_string()))
            },
        )
        .await?;
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::ChatEvents,
        )
        .await?;

        if let Some(config) = artifact_config {
            let id = conversation_id.clone();
            self.run_io(
                DeletionIo::Flow(flow),
                Some(workspace_io_receipt.clone()),
                "remove conversation artifacts",
                move |_service| cleanup_artifacts(&config, &id),
            )
            .await?;
        }
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::Artifacts,
        )
        .await?;

        // Foreground admission has been suspended since entry and every
        // TaskRun/tool owner is now quiescent. Enumerate the authoritative
        // lineage only at this cut, then retain each closed pool key through
        // the framework aggregate delete commit.
        #[cfg(test)]
        self.wait_before_lineage_barrier().await;
        let _retirement_receipts = begin_runtime_lineage_retirements(
            agent_pool.as_ref(),
            runtime_state.as_ref(),
            &conversation_id,
        )
        .await?;
        let store = conversation_store
            .as_ref()
            .ok_or(ConversationDeletionError::StoreUnavailable)?;
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::ConversationCommitStarted,
        )
        .await?;
        match runtime_state.as_deref() {
            Some(runtime_state) => {
                echo_agent::state::delete_persisted_conversation(
                    store.as_ref(),
                    runtime_state,
                    &conversation_id,
                )
                .await
                .map_err(|error| ConversationDeletionError::RuntimeState(error.to_string()))?;
            }
            None => {
                store
                    .delete_conversation(&conversation_id)
                    .await
                    .map_err(|error| {
                        ConversationDeletionError::ConversationStore(error.to_string())
                    })?;
            }
        }
        self.complete_step_io(
            DeletionIo::Flow(flow),
            Some(workspace_io_receipt.clone()),
            &tombstone_path,
            &mut tombstone,
            DeletionStep::RuntimeState,
        )
        .await?;

        tombstone.completed.insert(DeletionStep::Conversation);
        let persist_path = tombstone_path.clone();
        let persisted_tombstone = tombstone.clone();
        let persisted = self
            .run_io(
                DeletionIo::Flow(flow),
                Some(workspace_io_receipt.clone()),
                "persist aggregate deletion completion",
                move |service| service.persist_tombstone(&persist_path, &persisted_tombstone),
            )
            .await;
        let cleanup_pending = if let Err(error) = persisted {
            tracing::warn!(conversation_id, %error, "conversation deletion committed but its marker could not record completion");
            true
        } else {
            let retire_path = tombstone_path.clone();
            let retire_id = conversation_id.clone();
            self.run_io(
                DeletionIo::Flow(flow),
                Some(workspace_io_receipt),
                "retire aggregate deletion tombstone",
                move |service| {
                    service.remove_tombstone(&retire_path)?;
                    tracing::debug!(conversation_id = %retire_id, "conversation deletion tombstone retired");
                    Ok(())
                },
            )
            .await
            .is_err()
        };
        Ok(ConversationDeletionReceipt {
            conversation_id,
            resumed,
            cleanup_pending,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_committed_cleanup(
        &self,
        path: &Path,
        conversation_id: &str,
        mut tombstone: DeletionTombstone,
        store: &dyn ConversationStore,
        runtime_state: Option<&dyn RuntimeStateStore>,
        io: DeletionIo<'_>,
        workspace_io_receipt: Option<crate::state::ScopedWorkspaceIoReceipt>,
    ) -> Result<ConversationDeletionReceipt, ConversationDeletionError> {
        let conversation_visible = store
            .get_conversation(conversation_id)
            .await
            .map_err(|error| ConversationDeletionError::ConversationStore(error.to_string()))?
            .is_some();
        if conversation_visible && tombstone.completed.contains(&DeletionStep::Conversation) {
            return Err(ConversationDeletionError::CommittedIdentityReappeared(
                conversation_id.to_string(),
            ));
        }
        match runtime_state {
            Some(runtime_state) => {
                echo_agent::state::delete_persisted_conversation(
                    store,
                    runtime_state,
                    conversation_id,
                )
                .await
                .map_err(|error| ConversationDeletionError::RuntimeState(error.to_string()))?;
            }
            None if conversation_visible => {
                return Err(ConversationDeletionError::AmbiguousAuthorityCommit(
                    conversation_id.to_string(),
                ));
            }
            None => {}
        }
        if !tombstone.completed.contains(&DeletionStep::RuntimeState) {
            self.complete_step_io(
                io,
                workspace_io_receipt.clone(),
                path,
                &mut tombstone,
                DeletionStep::RuntimeState,
            )
            .await?;
        }

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

        if !tombstone.completed.contains(&DeletionStep::Conversation) {
            tombstone.completed.insert(DeletionStep::Conversation);
        }
        let persist_path = path.to_path_buf();
        let persist_tombstone = tombstone.clone();
        let persisted = self
            .run_io(
                io,
                workspace_io_receipt.clone(),
                "persist recovered conversation deletion",
                move |service| service.persist_tombstone(&persist_path, &persist_tombstone),
            )
            .await;
        let cleanup_pending = if let Err(error) = persisted {
            tracing::warn!(conversation_id, %error, "recovered deletion could not record completion");
            true
        } else {
            let retire_path = path.to_path_buf();
            self.run_io(
                io,
                workspace_io_receipt,
                "retire recovered conversation deletion tombstone",
                move |service| service.remove_tombstone(&retire_path),
            )
            .await
            .is_err()
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

    fn discover_tombstones_sync(
        &self,
    ) -> Result<Vec<(PathBuf, DeletionTombstone)>, ConversationDeletionError> {
        self.ensure_tombstone_root()?;
        let entries =
            fs::read_dir(self.root.as_ref()).map_err(|source| ConversationDeletionError::Io {
                path: self.root.as_ref().clone(),
                source,
            })?;
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ConversationDeletionError::Io {
                path: self.root.as_ref().clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json") {
                paths.push(path);
            }
        }
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                self.read_tombstone(&path)
                    .map(|tombstone| (path, tombstone))
            })
            .collect()
    }

    async fn complete_step_io(
        &self,
        io: DeletionIo<'_>,
        workspace: Option<crate::state::ScopedWorkspaceIoReceipt>,
        path: &Path,
        tombstone: &mut DeletionTombstone,
        step: DeletionStep,
    ) -> Result<(), ConversationDeletionError> {
        let mut next = tombstone.clone();
        next.completed.insert(step);
        let persist_path = path.to_path_buf();
        let persist_next = next.clone();
        self.run_io(
            io,
            workspace,
            "persist conversation deletion step",
            move |service| service.persist_tombstone(&persist_path, &persist_next),
        )
        .await?;
        *tombstone = next;
        Ok(())
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
        #[cfg(test)]
        if self.take_io_fault(DeletionIoFault::RemoveFileBarrier) {
            return Err(ConversationDeletionError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other(
                    "injected tombstone unlink durability barrier failure",
                ),
            });
        }
        echo_agent::utils::fs::remove_file_durable(path)
            .map(|_removed| ())
            .map_err(|source| ConversationDeletionError::Io {
                path: path.to_path_buf(),
                source,
            })
    }

    fn ensure_tombstone_root(&self) -> Result<(), ConversationDeletionError> {
        match fs::symlink_metadata(self.root.as_ref()) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(ConversationDeletionError::UnsafeTombstoneRoot {
                path: self.root.as_ref().clone(),
                message: "coordinator root is not a real directory".to_string(),
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(test)]
                if self.take_io_fault(DeletionIoFault::CreateRootBarrier) {
                    return Err(ConversationDeletionError::Io {
                        path: self.root.as_ref().clone(),
                        source: std::io::Error::other(
                            "injected tombstone mkdir durability barrier failure",
                        ),
                    });
                }
                echo_agent::utils::fs::create_dir_all_durable(self.root.as_ref()).map_err(
                    |source| ConversationDeletionError::Io {
                        path: self.root.as_ref().clone(),
                        source,
                    },
                )?;
                let metadata = fs::symlink_metadata(self.root.as_ref()).map_err(|source| {
                    ConversationDeletionError::Io {
                        path: self.root.as_ref().clone(),
                        source,
                    }
                })?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    Ok(())
                } else {
                    Err(ConversationDeletionError::UnsafeTombstoneRoot {
                        path: self.root.as_ref().clone(),
                        message: "new coordinator root is not a real directory".to_string(),
                    })
                }
            }
            Err(source) => Err(ConversationDeletionError::Io {
                path: self.root.as_ref().clone(),
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

async fn begin_runtime_lineage_retirements(
    pool: Option<&Arc<crate::agent_pool::AgentPool>>,
    runtime_state: Option<&Arc<dyn RuntimeStateStore>>,
    conversation_id: &str,
) -> Result<Vec<crate::agent_pool::AgentPoolConversationRetirement>, ConversationDeletionError> {
    let Some(pool) = pool else {
        return Ok(Vec::new());
    };
    let mut keys = BTreeSet::from([conversation_id.to_string()]);
    if let Some(runtime_state) = runtime_state {
        keys.extend(
            runtime_state
                .runtime_state_ids(conversation_id)
                .await
                .map_err(|error| ConversationDeletionError::RuntimeState(error.to_string()))?,
        );
    }
    let mut receipts = Vec::with_capacity(keys.len());
    for key in keys {
        let receipt = pool
            .begin_conversation_retirement(&key)
            .map_err(|error| ConversationDeletionError::AgentPool(error.to_string()))?;
        receipts.push(receipt);
    }
    for receipt in &receipts {
        pool.drain_conversation_retirement(receipt)
            .await
            .map_err(|error| ConversationDeletionError::AgentPool(error.to_string()))?;
    }
    Ok(receipts)
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
    use crate::agent_handle::AgentHandle;
    use crate::chat_event_log::ChatEventRetention;
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile};
    use crate::tool_execution::ToolExecutionOwner;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::agent::ToolInvocation;
    use echo_agent::memory::FileConversationStore;
    use echo_agent::state::{AgentCheckpoint, FileRuntimeStateStore};
    use echo_agent::testing::MockLlmClient;
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
    fn tombstone_durability_barrier_failures_are_retryable() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let id = "durability-barrier";
        let path = service.tombstone_path(id);
        let tombstone = DeletionTombstone::new(id);

        service.fail_next_io(DeletionIoFault::CreateRootBarrier);
        let create_error = service.persist_tombstone(&path, &tombstone).err();
        assert!(create_error.is_some());
        assert!(!path.exists());
        service.persist_tombstone(&path, &tombstone)?;
        assert!(path.is_file());

        service.fail_next_io(DeletionIoFault::RemoveFileBarrier);
        let remove_error = service.remove_tombstone(&path).err();
        assert!(remove_error.is_some());
        assert!(path.is_file());
        service.remove_tombstone(&path)?;
        assert!(!path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn workspace_scoped_markers_do_not_block_the_same_id_in_another_workspace()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let workspace_a = ConversationDeletionService::new(temp.path().join("a/deletions"));
        let workspace_b = ConversationDeletionService::new(temp.path().join("b/deletions"));
        let id = "shared-conversation-id";
        let tombstone = DeletionTombstone::new(id);
        workspace_a.persist_tombstone(&workspace_a.tombstone_path(id), &tombstone)?;

        assert!(matches!(
            workspace_a.ensure_admission_allowed(id, None).await,
            Err(ConversationDeletionError::DeletionPending(_))
        ));
        workspace_b.ensure_admission_allowed(id, None).await?;
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

        let receipts = service
            .recover_committed_deletions(store.clone(), None, None, None)
            .await?;
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
            .recover_committed_deletions(store.clone(), None, None, None)
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
    async fn commit_started_recovery_retries_runtime_lineage_and_transcript_delete()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let runtime_state: Arc<dyn RuntimeStateStore> = Arc::new(FileRuntimeStateStore::new(
            temp.path().join("runtime-state"),
        )?);
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let id = "recover-runtime-lineage";
        let runtime_id = "recover-runtime-lineage:incarnation-a";
        store.create_conversation(conversation(id)).await?;
        store.create_conversation(conversation(runtime_id)).await?;
        let mut checkpoint = AgentCheckpoint::new(runtime_id);
        checkpoint.messages_json = "[]".to_string();
        runtime_state
            .save_checkpoint_for_scope(id, &checkpoint)
            .await?;
        let path = service.tombstone_path(id);
        let mut tombstone = DeletionTombstone::new(id);
        tombstone
            .completed
            .insert(DeletionStep::ConversationCommitStarted);
        service.persist_tombstone(&path, &tombstone)?;

        let receipts = service
            .recover_committed_deletions(store.clone(), Some(runtime_state.clone()), None, None)
            .await?;
        assert_eq!(receipts.len(), 1);
        assert!(store.get_conversation(id).await?.is_none());
        assert!(store.get_conversation(runtime_id).await?.is_none());
        assert!(runtime_state.get_checkpoint(runtime_id).await?.is_none());
        assert!(runtime_state.runtime_state_ids(id).await?.is_empty());
        assert!(!path.exists());
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
                crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path()),
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
                crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path()),
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
    async fn reopened_owner_retries_committed_cleanup_after_unlink_debt()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let first_io = crate::product_data_io::ProductDataIoService::new();
        let first = ConversationDeletionService::new_with_product_data_io(
            temp.path().join("deletions"),
            first_io.clone(),
        );
        let tools = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        let turns = ForegroundTurnControl::default();
        let id = "delete-reopen-retry";
        store.create_conversation(conversation(id)).await?;
        first.fail_next_io(DeletionIoFault::RemoveFileBarrier);
        let first_receipt = first
            .delete(
                "global",
                id,
                Some(store.clone()),
                None,
                None,
                tools.clone(),
                events.clone(),
                None,
                &turns,
                None,
                crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path()),
            )
            .await?;
        assert!(first_receipt.cleanup_pending);
        assert!(first.tombstone_path(id).is_file());
        first_io
            .join_shutdown()
            .await
            .map_err(std::io::Error::other)?;

        let reopened = ConversationDeletionService::new_with_product_data_io(
            temp.path().join("deletions"),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let reopened_receipt = reopened
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
                crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path()),
            )
            .await?;
        assert!(reopened_receipt.resumed);
        assert!(!reopened_receipt.cleanup_pending);
        assert!(!reopened.tombstone_path(id).exists());
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_and_shutdown_join_preserve_the_final_lineage_cut()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let store = file_store(&temp.path().join("state"))?;
        let service = ConversationDeletionService::new(temp.path().join("deletions"));
        let runtime_state: Arc<dyn RuntimeStateStore> = Arc::new(FileRuntimeStateStore::new(
            temp.path().join("runtime-state"),
        )?);
        let tools = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            ChatEventRetention::default(),
        )?);
        let primary = ReactAgentBuilder::new()
            .model("conversation-delete-aba-test")
            .llm_client(Arc::new(MockLlmClient::new()))
            .build()?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                8,
                false,
            )
            .await,
        );
        let turns = Arc::new(ForegroundTurnControl::default());
        let product_id = "delete-final-lineage";
        let initial_runtime_id = "delete-final-lineage:initial";
        let late_runtime_id = "delete-final-lineage:late";
        store.create_conversation(conversation(product_id)).await?;
        store
            .create_conversation(conversation(initial_runtime_id))
            .await?;
        let mut initial_checkpoint = AgentCheckpoint::new(initial_runtime_id);
        initial_checkpoint.messages_json = "[]".to_string();
        runtime_state
            .save_checkpoint_for_scope(product_id, &initial_checkpoint)
            .await?;
        let initial_execution = pool.acquire(initial_runtime_id).await?;
        drop(initial_execution);

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        service.install_before_lineage_barrier(entered_tx, release_rx);
        let delete_service = service.clone();
        let delete_store = store.clone();
        let delete_pool = pool.clone();
        let delete_runtime_state = runtime_state.clone();
        let delete_tools = tools.clone();
        let delete_events = events.clone();
        let delete_turns = turns.clone();
        let workspace_receipt =
            crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path());
        let deletion = tokio::spawn(async move {
            delete_service
                .delete(
                    "global",
                    product_id,
                    Some(delete_store),
                    Some(delete_pool),
                    None,
                    delete_tools,
                    delete_events,
                    Some(delete_runtime_state),
                    delete_turns.as_ref(),
                    None,
                    workspace_receipt,
                )
                .await
        });
        entered_rx.await?;
        deletion.abort();
        let _cancelled_waiter = deletion.await;

        store
            .create_conversation(conversation(late_runtime_id))
            .await?;
        let mut late_checkpoint = AgentCheckpoint::new(late_runtime_id);
        late_checkpoint.messages_json = "[]".to_string();
        runtime_state
            .save_checkpoint_for_scope(product_id, &late_checkpoint)
            .await?;
        let late_execution = pool.acquire(late_runtime_id).await?;
        drop(late_execution);
        let shutdown_service = service.product_data_io.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.join_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release_tx
            .send(())
            .map_err(|_| std::io::Error::other("deletion barrier receiver closed"))?;
        shutdown.await?.map_err(std::io::Error::other)?;

        assert!(store.get_conversation(product_id).await?.is_none());
        for runtime_id in [initial_runtime_id, late_runtime_id] {
            assert!(runtime_state.get_checkpoint(runtime_id).await?.is_none());
            assert!(store.get_conversation(runtime_id).await?.is_none());
            assert!(pool.lease_existing(runtime_id).await?.is_none());
        }
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

        let runtime_ids = [
            "delete-all-authorities:incarnation-a",
            "delete-all-authorities:incarnation-b",
        ];
        let primary = ReactAgentBuilder::new()
            .model("conversation-delete-test")
            .llm_client(Arc::new(MockLlmClient::new()))
            .build()?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                8,
                false,
            )
            .await,
        );
        for runtime_id in runtime_ids {
            store.create_conversation(conversation(runtime_id)).await?;
            let mut checkpoint = AgentCheckpoint::new(runtime_id);
            checkpoint.messages_json = "[]".to_string();
            runtime_state
                .save_checkpoint_for_scope(id, &checkpoint)
                .await?;
            let execution = pool.acquire(runtime_id).await?;
            drop(execution);
        }

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
                Some(pool.clone()),
                Some(task_runtime.clone()),
                tools.clone(),
                events.clone(),
                Some(runtime_state.clone()),
                &turns,
                Some(artifact_config),
                crate::state::ScopedWorkspaceIoReceipt::global_for_test(temp.path()),
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
        for runtime_id in runtime_ids {
            assert!(runtime_state.get_checkpoint(runtime_id).await?.is_none());
            assert!(store.get_conversation(runtime_id).await?.is_none());
            assert!(pool.lease_existing(runtime_id).await?.is_none());
        }
        assert!(runtime_state.runtime_state_ids(id).await?.is_empty());
        assert!(!tool_artifact.path.exists());
        assert!(!user_input.exists());
        assert!(!service.tombstone_path(id).exists());
        Ok(())
    }
}
