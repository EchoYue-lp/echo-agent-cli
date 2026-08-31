//! Application-owned foreground turn admission and cancellation.
//!
//! The framework owns execution and same-turn steering. EKO owns the product
//! rule that one `(workspace, surface, conversation)` tuple has at most one foreground
//! turn, and that cancellation never releases that ownership before the
//! existing [`crate::chat_driver::TurnOutcome`] has settled.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use echo_agent::agent::{AgentEvent, AgentHandle, CancellationToken};
use echo_agent::runtime::TurnReceipt;
use tokio::sync::watch;

use crate::chat_driver::{ChatDriverEvent, ChatSink, TurnOutcome, drive_chat, drive_chat_turn};
use crate::chat_resources::ChatResources;
use crate::prepared_turn::PreparedUserTurn;

const TERMINAL_RETRY_LIMIT: usize = 3;
const TERMINAL_RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

pub type ForegroundTerminalProjector = Arc<
    dyn Fn(TurnOutcome) -> futures::future::BoxFuture<'static, Result<(), String>> + Send + Sync,
>;

/// Interactive product surface that owns a foreground turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, rename = "ForegroundTurnSurface")]
pub enum ForegroundTurnSurface {
    Gui,
    Tui,
    Cli,
    Channel,
    /// Application-owned cross-workspace inbox delivery.
    Agent,
}

impl fmt::Display for ForegroundTurnSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Channel => "channel",
            Self::Agent => "agent",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ForegroundTurnKey {
    workspace_id: String,
    surface: ForegroundTurnSurface,
    conversation_id: String,
}

/// Read-only identity and cancellation state for an active foreground turn.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export, rename = "ForegroundTurnSnapshot")]
pub struct ForegroundTurnSnapshot {
    pub workspace_id: String,
    pub surface: ForegroundTurnSurface,
    pub conversation_id: String,
    /// Stable root identity used by the surface message and its events.
    pub root_turn_id: String,
    /// Current framework turn identity used for exact steer/cancel requests.
    pub active_turn_id: String,
    pub cancellation_requested: bool,
}

/// Terminal receipt delivered after the foreground execution future settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundTurnSettlement {
    pub workspace_id: String,
    pub surface: ForegroundTurnSurface,
    pub conversation_id: String,
    pub turn_id: String,
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForegroundTurnError {
    #[error("foreground turn workspace id is empty")]
    EmptyWorkspaceId,
    #[error("foreground turn conversation id is empty")]
    EmptyConversationId,
    #[error("foreground turn id is empty")]
    EmptyTurnId,
    #[error(
        "foreground turn is busy for {surface}:{conversation_id}; active turn is {active_turn_id}"
    )]
    Busy {
        surface: ForegroundTurnSurface,
        conversation_id: String,
        active_turn_id: String,
    },
    #[error("no active foreground turn for {surface}:{conversation_id}")]
    NoActiveTurn {
        surface: ForegroundTurnSurface,
        conversation_id: String,
    },
    #[error(
        "foreground turn mismatch for {surface}:{conversation_id}; expected {expected_turn_id}, actual {actual_turn_id}"
    )]
    TurnMismatch {
        surface: ForegroundTurnSurface,
        conversation_id: String,
        expected_turn_id: String,
        actual_turn_id: String,
    },
    #[error("foreground turn admission is suspended for a workspace transition")]
    AdmissionSuspended,
    #[error(
        "foreground turn admission is suspended while conversation {conversation_id} is deleted"
    )]
    ConversationAdmissionSuspended { conversation_id: String },
    #[error("foreground turn control is shutting down")]
    ShuttingDown,
    #[error("a foreground turn is active; workspace transition admission cannot be suspended")]
    ActiveTurns,
    #[error("conversation {conversation_id} has an active foreground turn")]
    ActiveConversationTurns { conversation_id: String },
    #[error("foreground turn control state is unavailable")]
    StateUnavailable,
    #[error("foreground driver supervision requires an active Tokio runtime: {0}")]
    RuntimeUnavailable(String),
    #[error("foreground driver lease belongs to another control")]
    LeaseOwnerMismatch,
    #[error("foreground driver settlement failed: {0}")]
    DriverSettlement(String),
}

struct ActiveForegroundTurn {
    key: ForegroundTurnKey,
    root_turn_id: String,
    active_agent_turn_id: Mutex<String>,
    cancel: CancellationToken,
    settlement_tx: watch::Sender<Option<ForegroundTurnSettlement>>,
    terminal_debt_tx: watch::Sender<Option<ForegroundTerminalDebt>>,
    settlement_owner_started: AtomicBool,
    input_observers: Mutex<ForegroundInputObservers>,
}

#[derive(Debug, Clone)]
struct ForegroundTerminalDebt {
    outcome: TurnOutcome,
    failures: Vec<String>,
}

impl ForegroundTerminalDebt {
    fn error(&self) -> ForegroundTurnError {
        ForegroundTurnError::DriverSettlement(format!(
            "foreground durable terminal debt for {:?}: {}",
            self.outcome,
            self.failures.join("; ")
        ))
    }
}

struct ForegroundInputObservers {
    admission_open: bool,
    tasks: tokio::task::JoinSet<Result<(), String>>,
    terminal_projectors: Vec<ForegroundTerminalProjector>,
}

impl Default for ForegroundInputObservers {
    fn default() -> Self {
        Self {
            admission_open: true,
            tasks: tokio::task::JoinSet::new(),
            terminal_projectors: Vec::new(),
        }
    }
}

impl ActiveForegroundTurn {
    fn active_agent_turn_id(&self) -> String {
        self.active_agent_turn_id
            .lock()
            .map(|turn_id| turn_id.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    fn snapshot(&self) -> ForegroundTurnSnapshot {
        ForegroundTurnSnapshot {
            workspace_id: self.key.workspace_id.clone(),
            surface: self.key.surface,
            conversation_id: self.key.conversation_id.clone(),
            root_turn_id: self.root_turn_id.clone(),
            active_turn_id: self.active_agent_turn_id(),
            cancellation_requested: self.cancel.is_cancelled(),
        }
    }

    async fn close_input_lifecycle(&self) -> (Vec<String>, Vec<ForegroundTerminalProjector>) {
        let (mut tasks, terminal_projectors) = {
            let mut observers = self
                .input_observers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            observers.admission_open = false;
            (
                std::mem::take(&mut observers.tasks),
                std::mem::take(&mut observers.terminal_projectors),
            )
        };
        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(error) => failures.push(error.to_string()),
            }
        }
        (failures, terminal_projectors)
    }

    fn record_terminal_debt(&self, outcome: TurnOutcome, failures: Vec<String>) {
        self.cancel.cancel();
        if let Ok(mut observers) = self.input_observers.lock() {
            observers.admission_open = false;
        }
        self.terminal_debt_tx
            .send_replace(Some(ForegroundTerminalDebt { outcome, failures }));
    }
}

tokio::task_local! {
    static CURRENT_FOREGROUND_TURN: Arc<ActiveForegroundTurn>;
}

/// Cloneable reference to the existing foreground turn authority. It carries
/// the exact admitted entry across supervisor task boundaries without creating
/// a second lookup or lifecycle owner.
#[derive(Clone)]
pub(crate) struct ForegroundTurnProgress(Arc<ActiveForegroundTurn>);

impl ForegroundTurnProgress {
    pub(crate) fn advance(&self, turn_id: &str) {
        if turn_id.trim().is_empty() {
            return;
        }
        let mut current = self
            .0
            .active_agent_turn_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = turn_id.to_string();
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.0.cancel.clone()
    }

    /// Run one finite continuation invocation under the original foreground
    /// entry. This restores the task-local identity across supervisor spawns,
    /// but deliberately carries no settlement capability.
    pub(crate) async fn scope_chat<Execute, ExecuteFuture>(
        &self,
        resources: Arc<ChatResources>,
        execute: Execute,
    ) -> Result<TurnReceipt, String>
    where
        Execute: FnOnce(Arc<ChatResources>) -> ExecuteFuture,
        ExecuteFuture: std::future::Future<Output = Result<TurnReceipt, String>>,
    {
        let cancel = self.cancellation_token();
        let delivery = Arc::new(DownstreamDeliveryState::default());
        let sink: Arc<dyn ChatSink> = Arc::new(CancellationAwareChatSink {
            inner: Arc::clone(&resources.sink),
            cancel: cancel.clone(),
            delivery: Arc::clone(&delivery),
        });
        let controlled_resources = Arc::new(ChatResources {
            execution_scope: resources.execution_scope.clone(),
            workspace_io_receipt: resources.workspace_io_receipt.clone(),
            pool: resources.pool.clone(),
            store: resources.store.clone(),
            sink,
            webhook_emitter: resources.webhook_emitter.clone(),
            conv_id: resources.conv_id.clone(),
            root_message_id: resources.root_message_id.clone(),
            attachments: resources.attachments.clone(),
            cancel,
            review_integration: resources.review_integration.clone(),
            memory_generation: resources.memory_generation.clone(),
            human_loop_provider: resources.human_loop_provider.clone(),
        });
        let memory_generation = resources.memory_generation.clone();
        let result = CURRENT_FOREGROUND_TURN
            .scope(Arc::clone(&self.0), execute(controlled_resources))
            .await;
        if let Some(generation) = memory_generation {
            let receipt = generation.settle_hot_memory_projection().await;
            if receipt.status
                == crate::evolution::review_integration::MemoryProjectionSettlementStatus::Degraded
            {
                tracing::warn!(error = ?receipt.error, "foreground hot-memory projection remains pending");
            }
        }
        if !delivery.terminal_delivery_failed() {
            return result;
        }
        result.map(|mut outcome| {
            outcome.outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "downstream_disconnect",
                "chat event consumer closed before terminal delivery",
            ));
            outcome
        })
    }
}

pub(crate) fn current_foreground_progress() -> Option<ForegroundTurnProgress> {
    CURRENT_FOREGROUND_TURN
        .try_with(|entry| ForegroundTurnProgress(Arc::clone(entry)))
        .ok()
}

type ForegroundShutdownResult = Result<(), ForegroundTurnError>;

#[derive(Default)]
enum ForegroundShutdownState {
    #[default]
    Open,
    Running(watch::Receiver<Option<ForegroundShutdownResult>>),
    Settled(ForegroundShutdownResult),
}

impl ForegroundShutdownState {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }
}

#[derive(Default)]
struct ForegroundTurnState {
    active: HashMap<ForegroundTurnKey, Arc<ActiveForegroundTurn>>,
    drivers: tokio::task::JoinSet<()>,
    driver_failures: Vec<String>,
    shutdown: ForegroundShutdownState,
    shutdown_owner: Option<tokio::task::JoinHandle<()>>,
    admission_suspended: bool,
    suspended_conversations: HashSet<(String, String)>,
    shutting_down: bool,
}

#[derive(Default)]
struct ForegroundTurnControlInner {
    state: Mutex<ForegroundTurnState>,
}

/// Single application authority for foreground turn identity and cancellation.
#[derive(Clone, Default)]
pub struct ForegroundTurnControl {
    inner: Arc<ForegroundTurnControlInner>,
}

/// Ordered generation receipts for one foreground execution. TaskRuntime is
/// retained first; memory evolution is appended by the Memory integration;
/// the pool receipt is held separately and released before this stack.
#[must_use]
pub struct ForegroundExecutionReceipts {
    generations: Vec<Box<dyn Send>>,
}

impl ForegroundExecutionReceipts {
    pub fn new(
        task_runtime: Option<crate::tasks::task_runtime::store::TaskRuntimeGenerationReceipt>,
    ) -> Self {
        let mut receipts = Self {
            generations: Vec::new(),
        };
        if let Some(receipt) = task_runtime {
            receipts.retain(receipt);
        }
        receipts
    }

    pub fn retain<Receipt>(&mut self, receipt: Receipt)
    where
        Receipt: Send + 'static,
    {
        self.generations.push(Box::new(receipt));
    }

    pub fn release_lifo(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        while self.generations.pop().is_some() {}
    }
}

impl Drop for ForegroundExecutionReceipts {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Pauses new foreground turns while an application workspace transition is
/// settling. The active-turn check and suspension bit share one mutex, closing
/// the gap where a turn could enter after a read-only idle snapshot.
#[must_use]
pub(crate) struct ForegroundAdmissionSuspension {
    control: ForegroundTurnControl,
    active: bool,
}

impl Drop for ForegroundAdmissionSuspension {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .control
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.shutting_down {
            state.admission_suspended = false;
        }
        self.active = false;
    }
}

/// Prevents any surface from starting a turn for one conversation while its
/// application-owned resources are being deleted.
#[must_use]
pub(crate) struct ForegroundConversationSuspension {
    control: ForegroundTurnControl,
    workspace_id: String,
    conversation_id: String,
    active: bool,
}

impl Drop for ForegroundConversationSuspension {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .control
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .suspended_conversations
            .remove(&(self.workspace_id.clone(), self.conversation_id.clone()));
        self.active = false;
    }
}

impl ForegroundTurnControl {
    pub fn supervise_input_lifecycle_scoped<Fut>(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
        observer: Fut,
        terminal_projector: ForegroundTerminalProjector,
    ) -> Result<(), ForegroundTurnError>
    where
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let entry =
            self.input_lifecycle_entry(workspace_id, surface, conversation_id, expected_turn_id)?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| ForegroundTurnError::RuntimeUnavailable(error.to_string()))?;
        let mut observers = entry
            .input_observers
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if !observers.admission_open {
            return Err(ForegroundTurnError::DriverSettlement(
                "foreground input lifecycle admission is closed".to_string(),
            ));
        }
        observers.terminal_projectors.push(terminal_projector);
        observers.tasks.spawn_on(observer, &runtime);
        Ok(())
    }

    pub fn supervise_input_observer_scoped<Fut>(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
        observer: Fut,
    ) -> Result<(), ForegroundTurnError>
    where
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let entry =
            self.input_lifecycle_entry(workspace_id, surface, conversation_id, expected_turn_id)?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| ForegroundTurnError::RuntimeUnavailable(error.to_string()))?;
        let mut observers = entry
            .input_observers
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if !observers.admission_open {
            return Err(ForegroundTurnError::DriverSettlement(
                "foreground input observer admission is closed".to_string(),
            ));
        }
        observers.tasks.spawn_on(observer, &runtime);
        Ok(())
    }

    fn input_lifecycle_entry(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<Arc<ActiveForegroundTurn>, ForegroundTurnError> {
        let key = ForegroundTurnKey {
            workspace_id: workspace_id.to_string(),
            surface,
            conversation_id: conversation_id.to_string(),
        };
        let entry = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?
            .active
            .get(&key)
            .cloned()
            .ok_or_else(|| ForegroundTurnError::NoActiveTurn {
                surface,
                conversation_id: conversation_id.to_string(),
            })?;
        let actual_turn_id = entry.active_agent_turn_id();
        if actual_turn_id != expected_turn_id {
            return Err(ForegroundTurnError::TurnMismatch {
                surface,
                conversation_id: conversation_id.to_string(),
                expected_turn_id: expected_turn_id.to_string(),
                actual_turn_id,
            });
        }
        Ok(entry)
    }

    /// Acquire one exact foreground turn. The returned lease owns its token.
    pub fn begin(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<ForegroundTurnLease, ForegroundTurnError> {
        self.begin_scoped("global", surface, conversation_id, turn_id)
    }

    /// Acquire one exact workspace-qualified foreground turn.
    pub fn begin_scoped(
        &self,
        workspace_id: impl Into<String>,
        surface: ForegroundTurnSurface,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<ForegroundTurnLease, ForegroundTurnError> {
        let workspace_id = workspace_id.into();
        if workspace_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyWorkspaceId);
        }
        let conversation_id = conversation_id.into();
        if conversation_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyConversationId);
        }
        let turn_id = turn_id.into();
        if turn_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyTurnId);
        }
        let key = ForegroundTurnKey {
            workspace_id,
            surface,
            conversation_id,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ForegroundTurnError::ShuttingDown);
        }
        if state.admission_suspended {
            return Err(ForegroundTurnError::AdmissionSuspended);
        }
        if state
            .suspended_conversations
            .contains(&(key.workspace_id.clone(), key.conversation_id.clone()))
        {
            return Err(ForegroundTurnError::ConversationAdmissionSuspended {
                conversation_id: key.conversation_id,
            });
        }
        let conflict = state.active.values().find(|existing| {
            existing.key.workspace_id == key.workspace_id
                && existing.key.conversation_id == key.conversation_id
        });
        if let Some(existing) = conflict {
            return Err(ForegroundTurnError::Busy {
                surface,
                conversation_id: key.conversation_id.clone(),
                active_turn_id: existing.active_agent_turn_id(),
            });
        }
        let cancel = CancellationToken::new();
        let (settlement_tx, _) = watch::channel(None);
        let (terminal_debt_tx, _) = watch::channel(None);
        let entry = Arc::new(ActiveForegroundTurn {
            key: key.clone(),
            root_turn_id: turn_id.clone(),
            active_agent_turn_id: Mutex::new(turn_id),
            cancel,
            settlement_tx,
            terminal_debt_tx,
            settlement_owner_started: AtomicBool::new(false),
            input_observers: Mutex::new(ForegroundInputObservers::default()),
        });
        state.active.insert(key, Arc::clone(&entry));
        Ok(ForegroundTurnLease {
            control: self.clone(),
            entry,
            settled: false,
        })
    }

    /// Snapshot the active turn for one exact product scope.
    pub fn snapshot(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
    ) -> Option<ForegroundTurnSnapshot> {
        self.snapshot_scoped("global", surface, conversation_id)
    }

    pub fn snapshot_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
    ) -> Option<ForegroundTurnSnapshot> {
        let state = self.inner.state.lock().ok()?;
        state
            .active
            .get(&ForegroundTurnKey {
                workspace_id: workspace_id.to_string(),
                surface,
                conversation_id: conversation_id.to_string(),
            })
            .map(|entry| entry.snapshot())
    }

    /// Snapshot every active turn for one surface in deterministic order.
    pub fn snapshots(
        &self,
        surface: ForegroundTurnSurface,
    ) -> Result<Vec<ForegroundTurnSnapshot>, ForegroundTurnError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let mut snapshots = state
            .active
            .values()
            .filter(|entry| entry.key.surface == surface)
            .map(|entry| entry.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.workspace_id
                .cmp(&right.workspace_id)
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
                .then_with(|| left.root_turn_id.cmp(&right.root_turn_id))
        });
        Ok(snapshots)
    }

    /// Snapshot every active surface for one workspace.
    pub fn snapshots_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ForegroundTurnSnapshot>, ForegroundTurnError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let mut snapshots = state
            .active
            .values()
            .filter(|entry| entry.key.workspace_id == workspace_id)
            .map(|entry| entry.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.conversation_id
                .cmp(&right.conversation_id)
                .then_with(|| left.root_turn_id.cmp(&right.root_turn_id))
        });
        Ok(snapshots)
    }

    /// Snapshot every active surface for one exact workspace conversation.
    pub fn snapshots_for_conversation_scoped(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<ForegroundTurnSnapshot>, ForegroundTurnError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let mut snapshots = state
            .active
            .values()
            .filter(|entry| {
                entry.key.workspace_id == workspace_id
                    && entry.key.conversation_id == conversation_id
            })
            .map(|entry| entry.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.surface
                .to_string()
                .cmp(&right.surface.to_string())
                .then_with(|| left.root_turn_id.cmp(&right.root_turn_id))
        });
        Ok(snapshots)
    }

    /// Subscribe to settlement without requesting cancellation.
    pub fn settlement_waiter_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_root_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let entry = state
            .active
            .get(&ForegroundTurnKey {
                workspace_id: workspace_id.to_string(),
                surface,
                conversation_id: conversation_id.to_string(),
            })
            .ok_or_else(|| ForegroundTurnError::NoActiveTurn {
                surface,
                conversation_id: conversation_id.to_string(),
            })?;
        if entry.root_turn_id != expected_root_turn_id {
            return Err(ForegroundTurnError::TurnMismatch {
                surface,
                conversation_id: conversation_id.to_string(),
                expected_turn_id: expected_root_turn_id.to_string(),
                actual_turn_id: entry.root_turn_id.clone(),
            });
        }
        Ok(ForegroundTurnSettlementWaiter {
            settlement_rx: entry.settlement_tx.subscribe(),
            terminal_debt_rx: entry.terminal_debt_tx.subscribe(),
        })
    }

    /// True when any surface owns a turn for this conversation.
    pub fn has_active_conversation(&self, conversation_id: &str) -> bool {
        match self.inner.state.lock() {
            Ok(state) => state
                .active
                .keys()
                .any(|key| key.conversation_id == conversation_id),
            Err(_) => true,
        }
    }

    pub fn has_active_turns(&self) -> bool {
        match self.inner.state.lock() {
            Ok(mut state) => {
                Self::collect_finished_drivers(&mut state);
                !state.active.is_empty() || !state.drivers.is_empty() || state.shutdown.is_running()
            }
            Err(_) => true,
        }
    }

    /// Close admission for one conversation only when every surface is idle.
    /// The active-turn check and suspension marker share the same mutex as
    /// [`Self::begin`], so no turn can enter between them.
    #[cfg(test)]
    pub(crate) fn suspend_conversation_admission_if_idle(
        &self,
        conversation_id: &str,
    ) -> Result<ForegroundConversationSuspension, ForegroundTurnError> {
        self.suspend_conversation_admission_if_idle_scoped("global", conversation_id)
    }

    /// Close admission for one exact workspace conversation only when every
    /// surface is idle. A same-id conversation in another workspace remains
    /// independent.
    pub(crate) fn suspend_conversation_admission_if_idle_scoped(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<ForegroundConversationSuspension, ForegroundTurnError> {
        if workspace_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyWorkspaceId);
        }
        if conversation_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyConversationId);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ForegroundTurnError::ShuttingDown);
        }
        if state
            .active
            .keys()
            .any(|key| key.workspace_id == workspace_id && key.conversation_id == conversation_id)
        {
            return Err(ForegroundTurnError::ActiveConversationTurns {
                conversation_id: conversation_id.to_string(),
            });
        }
        state
            .suspended_conversations
            .insert((workspace_id.to_string(), conversation_id.to_string()));
        Ok(ForegroundConversationSuspension {
            control: self.clone(),
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
            active: true,
        })
    }

    fn collect_finished_drivers(state: &mut ForegroundTurnState) {
        while let Some(result) = state.drivers.try_join_next() {
            if let Err(error) = result {
                let message = error.to_string();
                tracing::error!(%error, "foreground driver failed to join");
                state.driver_failures.push(message);
            }
        }
    }

    fn register_terminal_projector(
        entry: &Arc<ActiveForegroundTurn>,
        projector: ForegroundTerminalProjector,
    ) -> Result<(), ForegroundTurnError> {
        let mut observers = entry
            .input_observers
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if !observers.admission_open {
            return Err(ForegroundTurnError::DriverSettlement(
                "foreground terminal projector admission is closed".to_string(),
            ));
        }
        observers.terminal_projectors.push(projector);
        Ok(())
    }

    fn start_settlement_owner(
        &self,
        entry: Arc<ActiveForegroundTurn>,
        outcome: TurnOutcome,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        let waiter = ForegroundTurnSettlementWaiter {
            settlement_rx: entry.settlement_tx.subscribe(),
            terminal_debt_rx: entry.terminal_debt_tx.subscribe(),
        };
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                if entry
                    .settlement_owner_started
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return Ok(waiter);
                }
                let detail = format!("foreground settlement runtime unavailable: {error}");
                entry.record_terminal_debt(outcome, vec![detail.clone()]);
                if let Ok(mut state) = self.inner.state.lock() {
                    state.driver_failures.push(detail);
                }
                return Ok(waiter);
            }
        };
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let owns_entry = state
            .active
            .get(&entry.key)
            .is_some_and(|active| Arc::ptr_eq(active, &entry));
        if !owns_entry {
            return Err(ForegroundTurnError::LeaseOwnerMismatch);
        }
        if entry
            .settlement_owner_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(waiter);
        }
        let control = self.clone();
        state.drivers.spawn_on(
            async move {
                control.settle_entry_owned(entry, outcome).await;
            },
            &runtime,
        );
        Ok(waiter)
    }

    async fn settle_entry_owned(&self, entry: Arc<ActiveForegroundTurn>, outcome: TurnOutcome) {
        let (observer_failures, mut projectors) = entry.close_input_lifecycle().await;
        let outcome = observer_failures.first().map_or(outcome, |_| {
            TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "input_observer",
                observer_failures.join("; "),
            ))
        });
        let mut failures = observer_failures;
        for retry in 0..TERMINAL_RETRY_LIMIT {
            let mut retry_projectors = Vec::new();
            let mut attempt_failures = Vec::new();
            for projector in projectors {
                match projector(outcome.clone()).await {
                    Ok(()) => {}
                    Err(error) => {
                        attempt_failures.push(error);
                        retry_projectors.push(projector);
                    }
                }
            }
            projectors = retry_projectors;
            if projectors.is_empty() {
                self.settle(&entry, outcome);
                return;
            }
            failures = attempt_failures;
            entry.cancel.cancel();
            if retry.saturating_add(1) < TERMINAL_RETRY_LIMIT {
                let multiplier = u32::try_from(retry.saturating_add(1)).unwrap_or(u32::MAX);
                tokio::time::sleep(TERMINAL_RETRY_BASE_DELAY.saturating_mul(multiplier)).await;
            }
        }
        if failures.is_empty() {
            failures.push("foreground terminal projector exhausted retry budget".to_string());
        }
        tracing::error!(errors = %failures.join("; "), "foreground durable terminal debt retained");
        entry.record_terminal_debt(outcome, failures);
    }

    /// Transfer one accepted foreground lease into the canonical application
    /// owner. The operation future may retain pool and memory-generation
    /// receipts; they remain live even if the surface caller drops its stream.
    /// Finished drivers are collected on each admission and shutdown drains the
    /// same `JoinSet`, so no detached cleanup owner is created.
    pub fn supervise<F, Fut>(
        &self,
        lease: ForegroundTurnLease,
        operation: F,
    ) -> Result<(), ForegroundTurnError>
    where
        F: FnOnce(ForegroundTurnLease) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        if !Arc::ptr_eq(&self.inner, &lease.control.inner) {
            return Self::reject_supervision(
                lease,
                operation,
                ForegroundTurnError::LeaseOwnerMismatch,
            );
        }
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                return Self::reject_supervision(
                    lease,
                    operation,
                    ForegroundTurnError::RuntimeUnavailable(error.to_string()),
                );
            }
        };
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Self::reject_supervision(
                    lease,
                    operation,
                    ForegroundTurnError::StateUnavailable,
                );
            }
        };
        if state.shutting_down {
            drop(state);
            return Self::reject_supervision(lease, operation, ForegroundTurnError::ShuttingDown);
        }
        let owns_active_lease = state
            .active
            .get(&lease.entry.key)
            .is_some_and(|entry| Arc::ptr_eq(entry, &lease.entry));
        if !owns_active_lease {
            drop(state);
            return Self::reject_supervision(
                lease,
                operation,
                ForegroundTurnError::LeaseOwnerMismatch,
            );
        }
        Self::collect_finished_drivers(&mut state);
        state.drivers.spawn_on(operation(lease), &runtime);
        Ok(())
    }

    fn reject_supervision<F>(
        mut lease: ForegroundTurnLease,
        operation: F,
        error: ForegroundTurnError,
    ) -> Result<(), ForegroundTurnError> {
        let message = error.to_string();
        drop(operation);
        let outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_supervision",
            message,
        ));
        if let Err(settlement_error) = lease
            .control
            .start_settlement_owner(Arc::clone(&lease.entry), outcome.clone())
        {
            lease
                .entry
                .record_terminal_debt(outcome, vec![settlement_error.to_string()]);
        }
        lease.settled = true;
        Err(error)
    }

    /// Atomically verify idleness and suspend new foreground admission.
    pub(crate) fn suspend_admission_if_idle(
        &self,
    ) -> Result<ForegroundAdmissionSuspension, ForegroundTurnError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ForegroundTurnError::ShuttingDown);
        }
        if state.admission_suspended {
            return Err(ForegroundTurnError::AdmissionSuspended);
        }
        Self::collect_finished_drivers(&mut state);
        if !state.active.is_empty() || !state.drivers.is_empty() {
            return Err(ForegroundTurnError::ActiveTurns);
        }
        state.admission_suspended = true;
        Ok(ForegroundAdmissionSuspension {
            control: self.clone(),
            active: true,
        })
    }

    /// Request cancellation only when the caller's turn id is still current.
    ///
    /// The returned waiter observes settlement; requesting cancellation does
    /// not remove ownership from the registry.
    pub fn request_cancel(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        self.request_cancel_scoped("global", surface, conversation_id, expected_turn_id)
    }

    pub fn request_cancel_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        let key = ForegroundTurnKey {
            workspace_id: workspace_id.to_string(),
            surface,
            conversation_id: conversation_id.to_string(),
        };
        let entry =
            {
                let state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| ForegroundTurnError::StateUnavailable)?;
                state.active.get(&key).cloned().ok_or_else(|| {
                    ForegroundTurnError::NoActiveTurn {
                        surface,
                        conversation_id: conversation_id.to_string(),
                    }
                })?
            };
        let active_turn_id = entry.active_agent_turn_id();
        if active_turn_id != expected_turn_id {
            return Err(ForegroundTurnError::TurnMismatch {
                surface,
                conversation_id: conversation_id.to_string(),
                expected_turn_id: expected_turn_id.to_string(),
                actual_turn_id: active_turn_id,
            });
        }
        let settlement_rx = entry.settlement_tx.subscribe();
        entry.cancel.cancel();
        Ok(ForegroundTurnSettlementWaiter {
            settlement_rx,
            terminal_debt_rx: entry.terminal_debt_tx.subscribe(),
        })
    }

    /// Request cancellation for one exact root surface operation.
    ///
    /// Product Stop actions target the stable root identity. Internal
    /// continuation turns may advance while the user clicks Stop, but they
    /// share the same cancellation token and must not make that request stale.
    pub fn request_root_cancel(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_root_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        self.request_root_cancel_scoped("global", surface, conversation_id, expected_root_turn_id)
    }

    pub fn request_root_cancel_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_root_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        let key = ForegroundTurnKey {
            workspace_id: workspace_id.to_string(),
            surface,
            conversation_id: conversation_id.to_string(),
        };
        let entry =
            {
                let state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| ForegroundTurnError::StateUnavailable)?;
                state.active.get(&key).cloned().ok_or_else(|| {
                    ForegroundTurnError::NoActiveTurn {
                        surface,
                        conversation_id: conversation_id.to_string(),
                    }
                })?
            };
        if entry.root_turn_id != expected_root_turn_id {
            return Err(ForegroundTurnError::TurnMismatch {
                surface,
                conversation_id: conversation_id.to_string(),
                expected_turn_id: expected_root_turn_id.to_string(),
                actual_turn_id: entry.root_turn_id.clone(),
            });
        }
        let settlement_rx = entry.settlement_tx.subscribe();
        entry.cancel.cancel();
        Ok(ForegroundTurnSettlementWaiter {
            settlement_rx,
            terminal_debt_rx: entry.terminal_debt_tx.subscribe(),
        })
    }

    /// Request exact cancellation and wait for the execution future to settle.
    pub async fn cancel_and_wait(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        self.request_cancel(surface, conversation_id, expected_turn_id)?
            .wait()
            .await
    }

    pub async fn cancel_and_wait_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        self.request_cancel_scoped(workspace_id, surface, conversation_id, expected_turn_id)?
            .wait()
            .await
    }

    pub async fn root_cancel_and_wait_scoped(
        &self,
        workspace_id: &str,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_root_turn_id: &str,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        self.request_root_cancel_scoped(
            workspace_id,
            surface,
            conversation_id,
            expected_root_turn_id,
        )?
        .wait()
        .await
    }

    /// Permanently close foreground admission, cancel every exact active turn,
    /// and wait for their existing driver leases to publish settlement.
    ///
    /// The first caller starts the state-owned settlement task. Every caller
    /// observes the same typed result, and dropping any caller future cannot
    /// drop the accepted driver `JoinSet` or its receipts.
    pub fn begin_shutdown(&self) -> Result<(), ForegroundTurnError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        state.shutting_down = true;
        state.admission_suspended = true;
        for entry in state.active.values() {
            entry.cancel.cancel();
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), ForegroundTurnError> {
        let result_rx = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| ForegroundTurnError::StateUnavailable)?;
            if let ForegroundShutdownState::Running(result_rx) = &state.shutdown {
                result_rx.clone()
            } else if let ForegroundShutdownState::Settled(result) = &state.shutdown {
                let result = result.clone();
                if state
                    .shutdown_owner
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    state.shutdown_owner.take();
                }
                return result;
            } else {
                state.shutting_down = true;
                state.admission_suspended = true;
                let runtime = match tokio::runtime::Handle::try_current() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let result =
                            Err(ForegroundTurnError::RuntimeUnavailable(error.to_string()));
                        state.shutdown = ForegroundShutdownState::Settled(result.clone());
                        return result;
                    }
                };
                let waiters = state
                    .active
                    .values()
                    .map(|entry| {
                        let settlement_rx = entry.settlement_tx.subscribe();
                        entry.cancel.cancel();
                        ForegroundTurnSettlementWaiter {
                            settlement_rx,
                            terminal_debt_rx: entry.terminal_debt_tx.subscribe(),
                        }
                    })
                    .collect::<Vec<_>>();
                let mut drivers = std::mem::take(&mut state.drivers);
                let mut failures = std::mem::take(&mut state.driver_failures);
                let (result_tx, result_rx) = watch::channel(None);
                let inner = Arc::clone(&self.inner);
                let owner = runtime.spawn(async move {
                    for waiter in waiters {
                        if let Err(error) = waiter.wait().await {
                            failures.push(error.to_string());
                        }
                    }
                    while let Some(result) = drivers.join_next().await {
                        if let Err(error) = result {
                            tracing::error!(%error, "foreground driver failed during shutdown");
                            failures.push(error.to_string());
                        }
                    }
                    let result = if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(ForegroundTurnError::DriverSettlement(failures.join("; ")))
                    };
                    let mut state = inner
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.driver_failures = failures;
                    state.shutdown = ForegroundShutdownState::Settled(result.clone());
                    result_tx.send_replace(Some(result));
                });
                state.shutdown = ForegroundShutdownState::Running(result_rx.clone());
                state.shutdown_owner = Some(owner);
                result_rx
            }
        };
        Self::wait_for_shutdown(result_rx).await
    }

    async fn wait_for_shutdown(
        mut result_rx: watch::Receiver<Option<ForegroundShutdownResult>>,
    ) -> ForegroundShutdownResult {
        loop {
            if let Some(result) = result_rx.borrow().clone() {
                return result;
            }
            result_rx.changed().await.map_err(|_| {
                ForegroundTurnError::DriverSettlement(
                    "foreground shutdown owner ended without publishing settlement".to_string(),
                )
            })?;
        }
    }

    fn settle(&self, entry: &Arc<ActiveForegroundTurn>, outcome: TurnOutcome) {
        let settlement = ForegroundTurnSettlement {
            workspace_id: entry.key.workspace_id.clone(),
            surface: entry.key.surface,
            conversation_id: entry.key.conversation_id.clone(),
            turn_id: entry.root_turn_id.clone(),
            outcome,
        };
        if let Ok(mut state) = self.inner.state.lock()
            && state
                .active
                .get(&entry.key)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            state.active.remove(&entry.key);
        }
        entry.settlement_tx.send_replace(Some(settlement));
    }
}

/// Wait handle returned by an exact-id cancellation request.
pub struct ForegroundTurnSettlementWaiter {
    settlement_rx: watch::Receiver<Option<ForegroundTurnSettlement>>,
    terminal_debt_rx: watch::Receiver<Option<ForegroundTerminalDebt>>,
}

impl ForegroundTurnSettlementWaiter {
    pub async fn wait(mut self) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        loop {
            if let Some(settlement) = self.settlement_rx.borrow().clone() {
                return Ok(settlement);
            }
            if let Some(debt) = self.terminal_debt_rx.borrow().clone() {
                return Err(debt.error());
            }
            tokio::select! {
                changed = self.settlement_rx.changed() => {
                    if changed.is_err()
                        && self.settlement_rx.borrow().is_none()
                        && self.terminal_debt_rx.borrow().is_none()
                    {
                        return Err(ForegroundTurnError::StateUnavailable);
                    }
                }
                changed = self.terminal_debt_rx.changed() => {
                    if changed.is_err()
                        && self.settlement_rx.borrow().is_none()
                        && self.terminal_debt_rx.borrow().is_none()
                    {
                        return Err(ForegroundTurnError::StateUnavailable);
                    }
                }
            }
        }
    }
}

/// RAII ownership for one foreground turn.
///
/// Normal execution and explicit cancellation call [`Self::settle`] only after
/// the outer driver future returns its existing `TurnOutcome`. Dropping an
/// unfinished lease means that outer future was abandoned. Drop requests token
/// cancellation and transfers the exact entry to the control-owned settlement
/// task; that owner joins accepted input observers and terminal projectors
/// before publishing `Cancelled`. Exhausted durable projection keeps the entry
/// active as debt rather than publishing a false terminal.
pub struct ForegroundTurnLease {
    control: ForegroundTurnControl,
    entry: Arc<ActiveForegroundTurn>,
    settled: bool,
}

impl ForegroundTurnLease {
    pub fn workspace_id(&self) -> &str {
        &self.entry.key.workspace_id
    }

    pub fn surface(&self) -> ForegroundTurnSurface {
        self.entry.key.surface
    }

    pub fn conversation_id(&self) -> &str {
        &self.entry.key.conversation_id
    }

    pub fn turn_id(&self) -> &str {
        &self.entry.root_turn_id
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.entry.cancel.clone()
    }

    #[cfg(test)]
    pub(crate) fn settle(mut self, outcome: TurnOutcome) -> ForegroundTurnSettlement {
        let settlement = ForegroundTurnSettlement {
            workspace_id: self.workspace_id().to_string(),
            surface: self.surface(),
            conversation_id: self.conversation_id().to_string(),
            turn_id: self.turn_id().to_string(),
            outcome: outcome.clone(),
        };
        self.control.settle(&self.entry, outcome);
        self.settled = true;
        settlement
    }

    pub async fn settle_after<F, Fut>(
        mut self,
        outcome: TurnOutcome,
        durable_terminal: F,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError>
    where
        F: Fn(TurnOutcome) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let projector: ForegroundTerminalProjector =
            Arc::new(move |outcome| Box::pin(durable_terminal(outcome)));
        ForegroundTurnControl::register_terminal_projector(&self.entry, projector)?;
        let waiter = self
            .control
            .start_settlement_owner(Arc::clone(&self.entry), outcome)?;
        self.settled = true;
        waiter.wait().await
    }

    pub async fn settle_after_observers(
        mut self,
        outcome: TurnOutcome,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        let waiter = self
            .control
            .start_settlement_owner(Arc::clone(&self.entry), outcome)?;
        self.settled = true;
        waiter.wait().await
    }
}

impl Drop for ForegroundTurnLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.entry.cancel.cancel();
        if let Err(error) = self
            .control
            .start_settlement_owner(Arc::clone(&self.entry), TurnOutcome::Cancelled)
        {
            self.entry.record_terminal_debt(
                TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "foreground_settlement",
                    error.to_string(),
                )),
                vec![error.to_string()],
            );
        }
        self.settled = true;
    }
}

struct CancellationAwareChatSink {
    inner: Arc<dyn ChatSink>,
    cancel: CancellationToken,
    delivery: Arc<DownstreamDeliveryState>,
}

#[derive(Default)]
struct DownstreamDeliveryState {
    rejected: AtomicBool,
    terminal_delivered: AtomicBool,
}

impl DownstreamDeliveryState {
    fn terminal_was_delivered(&self) -> bool {
        self.terminal_delivered.load(Ordering::Acquire)
    }

    fn terminal_delivery_failed(&self) -> bool {
        self.rejected.load(Ordering::Acquire) && !self.terminal_was_delivered()
    }
}

impl ChatSink for CancellationAwareChatSink {
    fn on_event(&self, event: ChatDriverEvent) -> bool {
        if self.delivery.rejected.load(Ordering::Acquire) {
            return false;
        }
        let terminal = matches!(
            &event,
            ChatDriverEvent::Agent(envelope)
                if matches!(
                    &envelope.payload,
                    AgentEvent::FinalAnswer(_) | AgentEvent::Cancelled | AgentEvent::Error { .. }
                )
        );
        let accepted = self.inner.on_event(event);
        if accepted && terminal {
            self.delivery
                .terminal_delivered
                .store(true, Ordering::Release);
        } else if !accepted {
            self.delivery.rejected.store(true, Ordering::Release);
            self.cancel.cancel();
        }
        accepted
    }

    fn continuation_sink(&self) -> Option<Arc<dyn ChatSink>> {
        Some(Arc::clone(&self.inner))
    }

    fn deferred_continuation_sink(&self) -> Option<Arc<dyn ChatSink>> {
        self.inner.deferred_continuation_sink()
    }
}

fn normalize_downstream_outcome(
    result: Result<TurnOutcome, String>,
    delivery: &DownstreamDeliveryState,
) -> Result<TurnOutcome, String> {
    if delivery.terminal_delivery_failed() {
        // Events already accepted by the consumer remain delivered; only the
        // authoritative terminal result is replaced when that consumer closed
        // before accepting a terminal event.
        return Ok(TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message(
                "downstream_disconnect",
                "chat event consumer closed before terminal delivery",
            ),
        ));
    }
    result
}

/// Run the existing shared chat driver under one application foreground lease.
///
/// This adds no second execution state machine. It binds the driver's existing
/// `TurnOutcome` to the product owner, and wraps the downstream sink so a closed
/// renderer cancels the exact same token before the driver settles.
pub async fn drive_foreground_chat(
    lease: ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
) -> Result<TurnOutcome, String> {
    let (result, settlement_outcome) = run_foreground_chat(&lease, agent, turn, resources).await;
    let settlement = lease
        .settle_after_observers(settlement_outcome)
        .await
        .map_err(|error| error.to_string())?;
    let _ = result;
    Ok(settlement.outcome)
}

/// Run a foreground turn while one application owner observes the framework's
/// initial-input receipt. The callback is carried through the existing chat
/// driver; foreground settlement remains owned by this function.
pub async fn drive_foreground_chat_with_input_observer(
    lease: ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
    input_observer: crate::chat_driver::InputReceiptObserver,
) -> Result<TurnOutcome, String> {
    let (result, settlement_outcome) =
        run_foreground_chat_with(&lease, resources, |controlled_resources| async move {
            crate::chat_driver::drive_chat_turn_with_input_observer(
                agent,
                turn,
                controlled_resources,
                None,
                Some(input_observer),
            )
            .await
            .map(|receipt| receipt.outcome)
        })
        .await;
    let settlement = lease
        .settle_after_observers(settlement_outcome)
        .await
        .map_err(|error| error.to_string())?;
    let _ = result;
    Ok(settlement.outcome)
}

pub async fn drive_foreground_chat_with_ingress<Settle, SettleFuture>(
    lease: ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
    input_observer: crate::chat_driver::InputReceiptObserver,
    durable_terminal: Settle,
) -> Result<TurnOutcome, String>
where
    Settle: Fn(TurnOutcome) -> SettleFuture + Send + Sync + 'static,
    SettleFuture: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let (result, settlement_outcome) =
        run_foreground_chat_with(&lease, resources, |controlled_resources| async move {
            crate::chat_driver::drive_chat_turn_with_input_observer(
                agent,
                turn,
                controlled_resources,
                None,
                Some(input_observer),
            )
            .await
            .map(|receipt| receipt.outcome)
        })
        .await;
    let settlement = lease
        .settle_after(settlement_outcome, durable_terminal)
        .await
        .map_err(|error| error.to_string())?;
    let _ = result;
    Ok(settlement.outcome)
}

/// Resume or recover an existing TaskRun through the same foreground owner.
pub async fn drive_foreground_chat_turn(
    lease: ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
    binding: crate::tasks::task_runtime::types::RunTurnBinding,
) -> Result<TurnOutcome, String> {
    let (result, settlement_outcome) =
        run_foreground_chat_with(&lease, resources, |controlled_resources| async move {
            drive_chat_turn(agent, turn, controlled_resources, Some(binding))
                .await
                .map(|receipt| receipt.outcome)
        })
        .await;
    let settlement = lease
        .settle_after_observers(settlement_outcome)
        .await
        .map_err(|error| error.to_string())?;
    let _ = result;
    Ok(settlement.outcome)
}

/// Drive a pooled foreground chat while the existing foreground owner retains
/// every subsystem receipt through outer settlement. The shared chat driver
/// acquires them in `Foreground -> TaskRuntime -> Memory -> pool` order and
/// releases `pool -> Memory -> TaskRuntime` before this function publishes the
/// foreground terminal receipt.
pub async fn drive_foreground_pooled_chat<Configure, ConfigureFuture>(
    lease: ForegroundTurnLease,
    pool: Arc<crate::agent_pool::AgentPool>,
    pool_key: String,
    configure: Configure,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
) -> TurnOutcome
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    drive_foreground_pooled_chat_with_binding(
        lease, pool, pool_key, configure, turn, resources, None,
    )
    .await
}

/// Resume one exact TaskRun through a pooled foreground owner. The explicit
/// binding prevents a conversation with multiple paused runs from selecting a
/// different goal during the prepare/claim transaction.
pub async fn drive_foreground_pooled_chat_turn<Configure, ConfigureFuture>(
    lease: ForegroundTurnLease,
    pool: Arc<crate::agent_pool::AgentPool>,
    pool_key: String,
    configure: Configure,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
    binding: crate::tasks::task_runtime::types::RunTurnBinding,
) -> TurnOutcome
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    drive_foreground_pooled_chat_with_binding(
        lease,
        pool,
        pool_key,
        configure,
        turn,
        resources,
        Some(binding),
    )
    .await
}

async fn drive_foreground_pooled_chat_with_binding<Configure, ConfigureFuture>(
    lease: ForegroundTurnLease,
    pool: Arc<crate::agent_pool::AgentPool>,
    pool_key: String,
    configure: Configure,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
    binding: Option<crate::tasks::task_runtime::types::RunTurnBinding>,
) -> TurnOutcome
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    let (_result, settlement_outcome) =
        run_foreground_chat_with(&lease, resources, move |controlled_resources| async move {
            crate::chat_driver::drive_pooled_chat_turn(
                pool,
                &pool_key,
                configure,
                turn,
                controlled_resources,
                binding,
            )
            .await
            .map(|receipt| receipt.outcome)
        })
        .await;
    match lease
        .settle_after_observers(settlement_outcome.clone())
        .await
    {
        Ok(_) => settlement_outcome,
        Err(error) => TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "input_observer",
            error.to_string(),
        )),
    }
}

async fn run_foreground_chat(
    lease: &ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
) -> (Result<TurnOutcome, String>, TurnOutcome) {
    run_foreground_chat_with(lease, resources, |controlled_resources| {
        drive_chat(agent, turn, controlled_resources)
    })
    .await
}

async fn run_foreground_chat_with<Execute, ExecuteFuture>(
    lease: &ForegroundTurnLease,
    resources: Arc<ChatResources>,
    execute: Execute,
) -> (Result<TurnOutcome, String>, TurnOutcome)
where
    Execute: FnOnce(Arc<ChatResources>) -> ExecuteFuture,
    ExecuteFuture: std::future::Future<Output = Result<TurnOutcome, String>>,
{
    let identity_error = if resources.conv_id.as_deref() != Some(lease.conversation_id()) {
        Some("foreground conversation id does not match chat resources".to_string())
    } else if resources.root_message_id != lease.turn_id() {
        Some("foreground turn id does not match chat resources".to_string())
    } else {
        None
    };
    if let Some(error) = identity_error {
        let outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_turn",
            error.clone(),
        ));
        return (Err(error), outcome);
    }

    let cancel = lease.cancellation_token();
    let delivery = Arc::new(DownstreamDeliveryState::default());
    let sink: Arc<dyn ChatSink> = Arc::new(CancellationAwareChatSink {
        inner: Arc::clone(&resources.sink),
        cancel: cancel.clone(),
        delivery: Arc::clone(&delivery),
    });
    // Memory merge copies `review_integration` and the exact caller-pinned
    // `memory_generation` into this controlled view. This wrapper must never
    // reacquire a generation while decorating the sink.
    let controlled_resources = Arc::new(ChatResources {
        execution_scope: resources.execution_scope.clone(),
        workspace_io_receipt: resources.workspace_io_receipt.clone(),
        pool: resources.pool.clone(),
        store: resources.store.clone(),
        sink,
        webhook_emitter: resources.webhook_emitter.clone(),
        conv_id: resources.conv_id.clone(),
        root_message_id: resources.root_message_id.clone(),
        attachments: resources.attachments.clone(),
        cancel,
        review_integration: resources.review_integration.clone(),
        memory_generation: resources.memory_generation.clone(),
        human_loop_provider: resources.human_loop_provider.clone(),
    });
    let memory_generation = resources.memory_generation.clone();
    let result = normalize_downstream_outcome(
        CURRENT_FOREGROUND_TURN
            .scope(Arc::clone(&lease.entry), execute(controlled_resources))
            .await,
        delivery.as_ref(),
    );
    if let Some(generation) = memory_generation {
        let receipt = generation.settle_hot_memory_projection().await;
        if receipt.status
            == crate::evolution::review_integration::MemoryProjectionSettlementStatus::Degraded
        {
            tracing::warn!(error = ?receipt.error, "foreground hot-memory projection remains pending");
        }
    }
    let settlement_outcome = result.clone().unwrap_or_else(|error| {
        TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_turn",
            error,
        ))
    });
    (result, settlement_outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropReceipt(Arc<AtomicBool>);

    impl Drop for DropReceipt {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct OrderedReceipt {
        name: &'static str,
        releases: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for OrderedReceipt {
        fn drop(&mut self) {
            self.releases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(self.name);
        }
    }

    struct ClosedSink;

    #[tokio::test]
    async fn foreground_settlement_waits_for_observer_and_retries_durable_terminal()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-observer",
                ForegroundTurnSurface::Cli,
                "conversation-observer",
                "turn-observer",
            )
            .map_err(|error| error.to_string())?;
        let (observer_entered, observer_wait) = tokio::sync::oneshot::channel();
        let (observer_release, observer_released) = tokio::sync::oneshot::channel();
        control
            .supervise_input_observer_scoped(
                "workspace-observer",
                ForegroundTurnSurface::Cli,
                "conversation-observer",
                "turn-observer",
                async move {
                    let _ = observer_entered.send(());
                    observer_released.await.map_err(|error| error.to_string())?;
                    Ok(())
                },
            )
            .map_err(|error| error.to_string())?;
        observer_wait.await.map_err(|error| error.to_string())?;
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_settle = Arc::clone(&attempts);
        let settlement = tokio::spawn(async move {
            lease
                .settle_after(TurnOutcome::Completed, move |_| {
                    let attempts = Arc::clone(&attempts_for_settle);
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        (attempt > 0)
                            .then_some(())
                            .ok_or_else(|| "injected durable append failure".to_string())
                    }
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(!settlement.is_finished());
        observer_release
            .send(())
            .map_err(|_| "observer receiver closed".to_string())?;
        let receipt = settlement
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.outcome, TurnOutcome::Completed);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            control
                .snapshot_scoped(
                    "workspace-observer",
                    ForegroundTurnSurface::Cli,
                    "conversation-observer",
                )
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn supervised_durable_debt_survives_caller_drop_and_shutdown_waits_for_retry()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-debt",
                ForegroundTurnSurface::Cli,
                "conversation-debt",
                "turn-debt",
            )
            .map_err(|error| error.to_string())?;
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_owner = Arc::clone(&attempts);
        control
            .supervise(lease, move |lease| async move {
                let _ = lease
                    .settle_after(TurnOutcome::Completed, move |_| {
                        let attempts = Arc::clone(&attempts_for_owner);
                        async move {
                            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                            (attempt > 0)
                                .then_some(())
                                .ok_or_else(|| "injected append debt".to_string())
                        }
                    })
                    .await;
            })
            .map_err(|error| error.to_string())?;
        control
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_terminal_projector_retains_exact_debt_and_shutdown_returns_error()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-permanent-debt",
                ForegroundTurnSurface::Tui,
                "conversation-permanent-debt",
                "turn-permanent-debt",
            )
            .map_err(|error| error.to_string())?;
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_projector = Arc::clone(&attempts);
        let projector: ForegroundTerminalProjector = Arc::new(move |_| {
            let attempts = Arc::clone(&attempts_for_projector);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err("permanent terminal projection failure".to_string())
            })
        });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-permanent-debt",
                ForegroundTurnSurface::Tui,
                "conversation-permanent-debt",
                "turn-permanent-debt",
                async { Ok(()) },
                projector,
            )
            .map_err(|error| error.to_string())?;

        let settlement = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lease.settle_after_observers(TurnOutcome::Completed),
        )
        .await
        .map_err(|_| "permanent terminal projector exhausted without returning debt".to_string())?;
        assert!(matches!(
            settlement,
            Err(ForegroundTurnError::DriverSettlement(ref message))
                if message.contains("permanent terminal projection failure")
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), TERMINAL_RETRY_LIMIT);
        assert!(matches!(
            control.supervise_input_observer_scoped(
                "workspace-permanent-debt",
                ForegroundTurnSurface::Tui,
                "conversation-permanent-debt",
                "turn-permanent-debt",
                async { Ok(()) },
            ),
            Err(ForegroundTurnError::DriverSettlement(_))
        ));
        assert!(matches!(
            control.begin_scoped(
                "workspace-permanent-debt",
                ForegroundTurnSurface::Tui,
                "conversation-permanent-debt",
                "next-turn",
            ),
            Err(ForegroundTurnError::Busy { .. })
        ));

        let shutdown = tokio::time::timeout(std::time::Duration::from_secs(1), control.shutdown())
            .await
            .map_err(|_| "shutdown waited forever on permanent terminal debt".to_string())?;
        assert!(matches!(
            shutdown,
            Err(ForegroundTurnError::DriverSettlement(ref message))
                if message.contains("permanent terminal projection failure")
        ));
        assert!(control.has_active_turns());
        Ok(())
    }

    #[tokio::test]
    async fn accepted_lease_task_abort_runs_cancelled_terminal_projector_before_release()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-abort",
                ForegroundTurnSurface::Tui,
                "conversation-abort",
                "turn-abort",
            )
            .map_err(|error| error.to_string())?;
        let waiter = control
            .settlement_waiter_scoped(
                "workspace-abort",
                ForegroundTurnSurface::Tui,
                "conversation-abort",
                "turn-abort",
            )
            .map_err(|error| error.to_string())?;
        let projected = Arc::new(Mutex::new(None));
        let projected_terminal = Arc::clone(&projected);
        let projector: ForegroundTerminalProjector = Arc::new(move |outcome| {
            let projected = Arc::clone(&projected_terminal);
            Box::pin(async move {
                *projected
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
                Ok(())
            })
        });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-abort",
                ForegroundTurnSurface::Tui,
                "conversation-abort",
                "turn-abort",
                async { Ok(()) },
                projector,
            )
            .map_err(|error| error.to_string())?;
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _lease = lease;
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        accepted_rx
            .await
            .map_err(|_| "abort fixture did not accept the lease".to_string())?;
        task.abort();
        let _ = task.await;

        let settlement = tokio::time::timeout(std::time::Duration::from_secs(1), waiter.wait())
            .await
            .map_err(|_| "aborted lease did not reach control-owned settlement".to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(settlement.outcome, TurnOutcome::Cancelled);
        assert!(matches!(
            projected
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref(),
            Some(TurnOutcome::Cancelled)
        ));
        assert!(!control.has_active_turns());
        Ok(())
    }

    #[tokio::test]
    async fn registered_live_lifecycles_project_before_taskrun_like_lease_release()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-live",
                ForegroundTurnSurface::Channel,
                "conversation-live",
                "turn-live",
            )
            .map_err(|error| error.to_string())?;
        let waiter = control
            .settlement_waiter_scoped(
                "workspace-live",
                ForegroundTurnSurface::Channel,
                "conversation-live",
                "turn-live",
            )
            .map_err(|error| error.to_string())?;
        let projected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..2 {
            let projected_for_terminal = Arc::clone(&projected);
            let projector: ForegroundTerminalProjector = Arc::new(move |_| {
                let projected = Arc::clone(&projected_for_terminal);
                Box::pin(async move {
                    projected.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            });
            control
                .supervise_input_lifecycle_scoped(
                    "workspace-live",
                    ForegroundTurnSurface::Channel,
                    "conversation-live",
                    "turn-live",
                    async { Ok(()) },
                    projector,
                )
                .map_err(|error| error.to_string())?;
        }
        lease
            .settle_after_observers(TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(projected.load(Ordering::SeqCst), 2);
        let settlement = waiter.wait().await.map_err(|error| error.to_string())?;
        assert_eq!(settlement.outcome, TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn observer_error_projects_failed_terminal_and_closes_registration_race()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-error",
                ForegroundTurnSurface::Tui,
                "conversation-error",
                "turn-error",
            )
            .map_err(|error| error.to_string())?;
        let (release, released) = tokio::sync::oneshot::channel();
        let observed_outcome = Arc::new(Mutex::new(None));
        let terminal_outcome = Arc::clone(&observed_outcome);
        let projector: ForegroundTerminalProjector = Arc::new(move |outcome| {
            let observed = Arc::clone(&terminal_outcome);
            Box::pin(async move {
                *observed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
                Ok(())
            })
        });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-error",
                ForegroundTurnSurface::Tui,
                "conversation-error",
                "turn-error",
                async move {
                    released.await.map_err(|error| error.to_string())?;
                    Err("observer append failed".to_string())
                },
                projector,
            )
            .map_err(|error| error.to_string())?;
        let exact_entry = Arc::clone(&lease.entry);
        let settling =
            tokio::spawn(async move { lease.settle_after_observers(TurnOutcome::Completed).await });
        loop {
            let admission_open = exact_entry
                .input_observers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .admission_open;
            if !admission_open {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            control.supervise_input_observer_scoped(
                "workspace-error",
                ForegroundTurnSurface::Tui,
                "conversation-error",
                "turn-error",
                async { Ok(()) },
            ),
            Err(ForegroundTurnError::DriverSettlement(_))
        ));
        release
            .send(())
            .map_err(|_| "observer release receiver closed".to_string())?;
        let settlement = settling
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(settlement.outcome, TurnOutcome::Failed(_)));
        assert!(matches!(
            observed_outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref(),
            Some(TurnOutcome::Failed(_))
        ));
        Ok(())
    }

    impl ChatSink for ClosedSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }
    }

    #[test]
    fn generation_receipts_release_in_reverse_acquisition_order() {
        let releases = Arc::new(Mutex::new(Vec::new()));
        let mut receipts = ForegroundExecutionReceipts::new(None);
        receipts.retain(OrderedReceipt {
            name: "task-runtime",
            releases: Arc::clone(&releases),
        });
        receipts.retain(OrderedReceipt {
            name: "memory",
            releases: Arc::clone(&releases),
        });

        receipts.release_lifo();

        let observed = releases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed, ["memory", "task-runtime"]);
    }

    #[test]
    fn generation_receipt_drop_uses_the_same_reverse_order() {
        let releases = Arc::new(Mutex::new(Vec::new()));
        let mut receipts = ForegroundExecutionReceipts::new(None);
        receipts.retain(OrderedReceipt {
            name: "task-runtime",
            releases: Arc::clone(&releases),
        });
        receipts.retain(OrderedReceipt {
            name: "memory",
            releases: Arc::clone(&releases),
        });

        drop(receipts);

        let observed = releases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed, ["memory", "task-runtime"]);
    }

    #[test]
    fn supervision_rejection_releases_operation_before_typed_terminal() -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin(ForegroundTurnSurface::Channel, "conversation", "turn")
            .map_err(|error| error.to_string())?;
        let waiter = control
            .request_cancel(ForegroundTurnSurface::Channel, "conversation", "turn")
            .map_err(|error| error.to_string())?;
        let dropped = Arc::new(AtomicBool::new(false));
        let receipt = DropReceipt(Arc::clone(&dropped));

        let error = control
            .supervise(lease, move |_lease| {
                let _receipt = receipt;
                async move {}
            })
            .err()
            .ok_or_else(|| "supervision unexpectedly succeeded without a runtime".to_string())?;
        assert!(matches!(error, ForegroundTurnError::RuntimeUnavailable(_)));
        assert!(
            dropped.load(Ordering::SeqCst),
            "operation receipts must release before rejection is returned"
        );

        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        let settlement = runtime.block_on(waiter.wait());
        assert!(matches!(
            settlement,
            Err(ForegroundTurnError::DriverSettlement(ref detail))
                if detail.contains("runtime unavailable")
        ));
        Ok(())
    }

    #[test]
    fn conversation_admission_is_exclusive_across_surfaces() -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let gui = control.begin(ForegroundTurnSurface::Gui, "conversation", "gui-turn")?;
        let busy = control.begin(ForegroundTurnSurface::Gui, "conversation", "second");
        assert!(matches!(
            busy,
            Err(ForegroundTurnError::Busy {
                active_turn_id,
                ..
            }) if active_turn_id == "gui-turn"
        ));
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Tui, "conversation", "tui-turn"),
            Err(ForegroundTurnError::Busy {
                active_turn_id,
                ..
            }) if active_turn_id == "gui-turn"
        ));
        assert_eq!(
            control.snapshots(ForegroundTurnSurface::Gui)?,
            vec![ForegroundTurnSnapshot {
                workspace_id: "global".to_string(),
                surface: ForegroundTurnSurface::Gui,
                conversation_id: "conversation".to_string(),
                root_turn_id: "gui-turn".to_string(),
                active_turn_id: "gui-turn".to_string(),
                cancellation_requested: false,
            }]
        );
        gui.settle(TurnOutcome::Completed);
        assert!(!control.has_active_turns());
        Ok(())
    }

    #[test]
    fn agent_delivery_is_conversation_exclusive_across_surfaces() -> Result<(), ForegroundTurnError>
    {
        let control = ForegroundTurnControl::default();
        let gui = control.begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Gui,
            "conversation",
            "gui-turn",
        )?;
        assert!(matches!(
            control.begin_scoped(
                "workspace-a",
                ForegroundTurnSurface::Agent,
                "conversation",
                "Agent-turn",
            ),
            Err(ForegroundTurnError::Busy { .. })
        ));
        gui.settle(TurnOutcome::Completed);

        let agent = control.begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Agent,
            "conversation",
            "Agent-turn",
        )?;
        assert!(matches!(
            control.begin_scoped(
                "workspace-a",
                ForegroundTurnSurface::Tui,
                "conversation",
                "tui-turn",
            ),
            Err(ForegroundTurnError::Busy { .. })
        ));
        agent.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn scopes_same_conversation_identity_by_workspace() -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let workspace_a = control.begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Gui,
            "conversation",
            "turn-a",
        )?;
        let workspace_b = control.begin_scoped(
            "workspace-b",
            ForegroundTurnSurface::Gui,
            "conversation",
            "turn-b",
        )?;

        assert_eq!(
            control
                .snapshot_scoped("workspace-a", ForegroundTurnSurface::Gui, "conversation")
                .map(|snapshot| snapshot.active_turn_id),
            Some("turn-a".to_string())
        );
        assert_eq!(
            control
                .snapshot_scoped("workspace-b", ForegroundTurnSurface::Gui, "conversation")
                .map(|snapshot| snapshot.active_turn_id),
            Some("turn-b".to_string())
        );
        workspace_a.settle(TurnOutcome::Completed);
        workspace_b.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn workspace_transition_suspension_is_atomic_and_reopens_admission()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let active = control.begin(ForegroundTurnSurface::Gui, "conversation", "active")?;
        assert!(matches!(
            control.suspend_admission_if_idle(),
            Err(ForegroundTurnError::ActiveTurns)
        ));
        active.settle(TurnOutcome::Completed);

        let transition = control.suspend_admission_if_idle()?;
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Tui, "blocked", "turn"),
            Err(ForegroundTurnError::AdmissionSuspended)
        ));
        drop(transition);

        let reopened = control.begin(ForegroundTurnSurface::Tui, "reopened", "turn")?;
        reopened.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn conversation_suspension_blocks_every_surface_and_reopens_only_its_identity()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let suspension = control.suspend_conversation_admission_if_idle("conversation-a")?;

        for surface in [
            ForegroundTurnSurface::Gui,
            ForegroundTurnSurface::Tui,
            ForegroundTurnSurface::Cli,
            ForegroundTurnSurface::Channel,
        ] {
            assert!(matches!(
                control.begin(surface, "conversation-a", "blocked-turn"),
                Err(ForegroundTurnError::ConversationAdmissionSuspended {
                    ref conversation_id
                }) if conversation_id == "conversation-a"
            ));
        }
        let other = control.begin(ForegroundTurnSurface::Gui, "conversation-b", "allowed-turn")?;
        other.settle(TurnOutcome::Completed);

        drop(suspension);
        let reopened = control.begin(
            ForegroundTurnSurface::Channel,
            "conversation-a",
            "reopened-turn",
        )?;
        reopened.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn conversation_suspension_refuses_an_active_identity_without_blocking_others()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let active = control.begin(ForegroundTurnSurface::Tui, "conversation-a", "active-turn")?;

        assert!(matches!(
            control.suspend_conversation_admission_if_idle("conversation-a"),
            Err(ForegroundTurnError::ActiveConversationTurns {
                ref conversation_id
            }) if conversation_id == "conversation-a"
        ));
        let other = control.suspend_conversation_admission_if_idle("conversation-b")?;
        assert!(matches!(
            control.begin(
                ForegroundTurnSurface::Channel,
                "conversation-b",
                "blocked-turn"
            ),
            Err(ForegroundTurnError::ConversationAdmissionSuspended { .. })
        ));

        drop(other);
        active.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn exact_cancel_rejects_stale_and_cross_conversation_ids()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-a")?;
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-b", "turn-a"),
            Err(ForegroundTurnError::NoActiveTurn { .. })
        ));
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "stale"),
            Err(ForegroundTurnError::TurnMismatch { .. })
        ));
        assert!(!lease.cancellation_token().is_cancelled());

        let waiter =
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "turn-a")?;
        assert!(lease.cancellation_token().is_cancelled());
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation-a")
                .is_some()
        );
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-b"),
            Err(ForegroundTurnError::Busy { .. })
        ));
        lease.settle(TurnOutcome::Cancelled);
        let settlement = waiter.wait().await?;
        assert_eq!(settlement.outcome, TurnOutcome::Cancelled);
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation-a")
                .is_none()
        );
        let next = control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-b")?;
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "turn-a"),
            Err(ForegroundTurnError::TurnMismatch { .. })
        ));
        assert!(!next.cancellation_token().is_cancelled());
        next.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn foreground_control_tracks_the_current_continuation_turn_id()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "root-turn")?;
        CURRENT_FOREGROUND_TURN
            .scope(Arc::clone(&lease.entry), async {
                current_foreground_progress()
                    .ok_or(ForegroundTurnError::StateUnavailable)?
                    .advance("continuation-turn");
                let snapshot = control
                    .snapshot(ForegroundTurnSurface::Gui, "conversation")
                    .ok_or(ForegroundTurnError::StateUnavailable)?;
                assert_eq!(snapshot.root_turn_id, "root-turn");
                assert_eq!(snapshot.active_turn_id, "continuation-turn");
                assert!(matches!(
                    control
                        .request_cancel(ForegroundTurnSurface::Gui, "conversation", "root-turn",),
                    Err(ForegroundTurnError::TurnMismatch { .. })
                ));
                let waiter = control.request_cancel(
                    ForegroundTurnSurface::Gui,
                    "conversation",
                    "continuation-turn",
                )?;
                assert!(lease.cancellation_token().is_cancelled());
                lease.settle(TurnOutcome::Cancelled);
                assert_eq!(waiter.wait().await?.turn_id, "root-turn");
                Ok::<(), ForegroundTurnError>(())
            })
            .await
    }

    #[tokio::test]
    async fn foreground_progress_handle_survives_supervisor_spawn()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Channel, "conversation", "root-turn")?;
        let progress = CURRENT_FOREGROUND_TURN
            .scope(Arc::clone(&lease.entry), async {
                current_foreground_progress().ok_or(ForegroundTurnError::StateUnavailable)
            })
            .await?;

        tokio::spawn(async move {
            progress.advance("continuation-turn");
        })
        .await
        .map_err(|error| ForegroundTurnError::DriverSettlement(error.to_string()))?;
        let snapshot = control
            .snapshot(ForegroundTurnSurface::Channel, "conversation")
            .ok_or(ForegroundTurnError::StateUnavailable)?;
        assert_eq!(snapshot.root_turn_id, "root-turn");
        assert_eq!(snapshot.active_turn_id, "continuation-turn");
        lease.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn root_cancel_survives_a_continuation_identity_change() -> Result<(), ForegroundTurnError>
    {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "root-turn")?;
        CURRENT_FOREGROUND_TURN
            .scope(Arc::clone(&lease.entry), async {
                current_foreground_progress()
                    .ok_or(ForegroundTurnError::StateUnavailable)?
                    .advance("continuation-turn");
                assert!(matches!(
                    control.request_root_cancel(
                        ForegroundTurnSurface::Gui,
                        "conversation",
                        "another-root",
                    ),
                    Err(ForegroundTurnError::TurnMismatch { .. })
                ));
                let waiter = control.request_root_cancel(
                    ForegroundTurnSurface::Gui,
                    "conversation",
                    "root-turn",
                )?;
                assert!(lease.cancellation_token().is_cancelled());
                lease.settle(TurnOutcome::Cancelled);
                assert_eq!(waiter.wait().await?.turn_id, "root-turn");
                Ok::<(), ForegroundTurnError>(())
            })
            .await
    }

    #[tokio::test]
    async fn closed_sink_cancels_same_token_and_waiter_blocks_until_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Channel, "sender", "turn")?;
        let token = lease.cancellation_token();
        let sink = CancellationAwareChatSink {
            inner: Arc::new(ClosedSink),
            cancel: token.clone(),
            delivery: Arc::new(DownstreamDeliveryState::default()),
        };
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert!(token.is_cancelled());

        let waiter = control.request_cancel(ForegroundTurnSurface::Channel, "sender", "turn")?;
        let mut wait_task = tokio::spawn(waiter.wait());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait_task)
                .await
                .is_err()
        );
        let outcome =
            normalize_downstream_outcome(Ok(TurnOutcome::Cancelled), sink.delivery.as_ref())
                .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        lease.settle(outcome);
        let settlement = wait_task
            .await
            .map_err(|_| ForegroundTurnError::StateUnavailable)??;
        assert_eq!(settlement.turn_id, "turn");
        assert!(matches!(
            settlement.outcome,
            TurnOutcome::Failed(failure) if failure.code == "downstream_disconnect"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dropped_lease_cancels_and_settles() -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Cli, "conversation", "turn")?;
        let token = lease.cancellation_token();
        let waiter = control.request_cancel(ForegroundTurnSurface::Cli, "conversation", "turn")?;
        drop(lease);
        assert!(token.is_cancelled());
        let settlement = waiter.wait().await?;
        assert_eq!(settlement.outcome, TurnOutcome::Cancelled);
        assert!(!control.has_active_turns());
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_closes_admission_and_waits_for_exact_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let token = lease.cancellation_token();
        let shutdown = control.shutdown();
        let settlement = async move {
            token.cancelled().await;
            lease.settle(TurnOutcome::Cancelled);
        };
        let (shutdown_result, ()) = tokio::join!(shutdown, settlement);
        shutdown_result?;
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Gui, "conversation", "next"),
            Err(ForegroundTurnError::ShuttingDown)
        ));
        control.shutdown().await
    }

    #[tokio::test]
    async fn supervised_driver_owns_receipt_until_outer_settlement() -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin(ForegroundTurnSurface::Channel, "conversation", "turn")
            .map_err(|error| error.to_string())?;
        let token = lease.cancellation_token();
        let dropped = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let receipt = DropReceipt(Arc::clone(&dropped));
        control
            .supervise(lease, move |lease| async move {
                let _receipt = receipt;
                token.cancelled().await;
                let _released = release_rx.await;
                lease.settle(TurnOutcome::Cancelled);
            })
            .map_err(|error| error.to_string())?;

        let shutdown_control = control.clone();
        let mut shutdown = tokio::spawn(async move { shutdown_control.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let cancellation_requested = control
                    .snapshot(ForegroundTurnSurface::Channel, "conversation")
                    .is_some_and(|snapshot| snapshot.cancellation_requested);
                if cancellation_requested {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "shutdown did not cancel the supervised driver".to_string())?;
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut shutdown)
                .await
                .is_err(),
            "shutdown must await the owner task, not only the foreground watch receipt"
        );

        release_tx
            .send(())
            .map_err(|_| "supervised driver release receiver closed".to_string())?;
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(dropped.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_owner_survives_waiter_abort_and_shares_driver_join_error()
    -> Result<(), String> {
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin(ForegroundTurnSurface::Channel, "conversation", "turn")
            .map_err(|error| error.to_string())?;
        let token = lease.cancellation_token();
        let dropped = Arc::new(AtomicBool::new(false));
        let receipt = DropReceipt(Arc::clone(&dropped));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (panic_tx, panic_rx) = tokio::sync::oneshot::channel();
        control
            .supervise(lease, move |lease| async move {
                let _receipt = receipt;
                let _started = started_tx.send(());
                lease.cancellation_token().cancelled().await;
                let trigger = panic_rx.await;
                assert!(
                    trigger.is_err(),
                    "injected foreground driver panic after cancellation"
                );
                lease.settle(TurnOutcome::Cancelled);
            })
            .map_err(|error| error.to_string())?;
        started_rx
            .await
            .map_err(|_| "foreground driver did not start".to_string())?;

        let first_control = control.clone();
        let first = tokio::spawn(async move { first_control.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), token.cancelled())
            .await
            .map_err(|_| "shutdown did not cancel the driver".to_string())?;
        let second_control = control.clone();
        let second = tokio::spawn(async move { second_control.shutdown().await });
        let third_control = control.clone();
        let third = tokio::spawn(async move { third_control.shutdown().await });
        first.abort();
        let _first_join = first.await;
        assert!(
            !dropped.load(Ordering::SeqCst),
            "aborting one shutdown waiter must not drop the owned driver receipt"
        );

        panic_tx
            .send(())
            .map_err(|_| "foreground panic trigger receiver closed".to_string())?;
        let second_result = second.await.map_err(|error| error.to_string())?;
        let third_result = third.await.map_err(|error| error.to_string())?;
        assert_eq!(second_result, third_result);
        assert!(matches!(
            &second_result,
            Err(ForegroundTurnError::DriverSettlement(message)) if message.contains("panicked")
        ));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(control.shutdown().await, second_result);
        Ok(())
    }

    #[tokio::test]
    async fn explicit_cancel_and_wait_remains_cancelled_after_driver_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Tui, "conversation", "turn")?;
        let cancellation =
            control.cancel_and_wait(ForegroundTurnSurface::Tui, "conversation", "turn");
        let settlement = async move {
            tokio::task::yield_now().await;
            lease.settle(TurnOutcome::Cancelled);
        };
        let (result, _) = tokio::join!(cancellation, settlement);
        assert_eq!(result?.outcome, TurnOutcome::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn settlement_publishes_one_existing_failed_outcome_to_every_waiter()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let first = control.request_cancel(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let second = control.request_cancel(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "test",
            "terminal failure",
        ));
        let receipt = lease.settle(outcome.clone());
        assert_eq!(receipt.outcome, outcome);
        assert_eq!(first.wait().await?.outcome, outcome);
        assert_eq!(second.wait().await?.outcome, outcome);
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation")
                .is_none()
        );
        Ok(())
    }
}
