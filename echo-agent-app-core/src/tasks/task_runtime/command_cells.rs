//! Thin EKO adapter over the framework command-cell registry.
//!
//! Process execution, output retention, waiting, cancellation and sandboxing
//! remain authoritative in `BackgroundCommandManager`. This adapter only
//! projects lifecycle facts into the owning TaskRuntime event stream.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use echo_agent::sandbox::{SandboxExecutor, SandboxManager};
use echo_agent::tasks::{BackgroundCommandManager, BackgroundCommandManagerConfig};
use echo_agent::tools::cell::{
    CommandCellDelta, CommandCellError, CommandCellLaunchReceipt, CommandCellObservationLease,
    CommandCellRegistry, CommandCellRequest, CommandCellSnapshot,
};
use echo_agent::tools::{Tool, ToolContext, ToolParameters, ToolResult};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::store::TaskRuntimeStore;
use super::types::{
    BackgroundCellArtifactStatus, BackgroundCellPhase, BackgroundCellState,
    BackgroundCellTerminalCause,
};

const OBSERVER_YIELD_MS: u64 = 30_000;
const OUTPUT_EXCERPT_CHARS: usize = 1_000;
const MAX_ACTIVE_AWAITERS: usize = 64;
const SETTLED_AWAITER_RETENTION: usize = 256;
const MAX_PROJECTION_REPAIR_ATTEMPTS: u64 = 8;
const OBSERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
type FrameworkShutdownSettlement = Shared<BoxFuture<'static, Result<(), String>>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RunCellScope {
    workspace_id: String,
    run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChatCellScope {
    workspace_id: String,
    conversation_id: String,
    root_turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AwaiterWatchKey {
    workspace_id: String,
    conversation_id: String,
    run_id: Option<String>,
    root_turn_id: String,
    cell_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaiterWatchState {
    Started,
    Settled,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AwaiterWatchReceipt {
    pub execution_id: String,
    pub control_task_id: String,
    pub attempt: u32,
    pub watch_generation: u64,
    pub cell_id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub run_id: Option<String>,
    pub root_turn_id: String,
    pub state: AwaiterWatchState,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub settled_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaiterSummaryStatus {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AwaiterResult {
    pub receipt: AwaiterWatchReceipt,
    pub cell: BackgroundCellState,
    pub awaiter_status: AwaiterSummaryStatus,
    pub awaiter_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaiterDeliveryOutcome {
    Drained,
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AwaiterResultAcknowledgement {
    pub execution_id: String,
    pub attempt: u32,
    pub watch_generation: u64,
    pub cell_id: String,
    pub acknowledged_turn_id: String,
    pub outcome: AwaiterDeliveryOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AwaiterSurfaceProjection {
    Ready {
        execution_id: String,
        cell_id: String,
        phase: BackgroundCellPhase,
        terminal_cause: Option<BackgroundCellTerminalCause>,
        exit_code: Option<i32>,
        artifact_status: BackgroundCellArtifactStatus,
    },
    Acknowledged {
        execution_id: String,
        cell_id: String,
        acknowledged_turn_id: String,
        outcome: AwaiterDeliveryOutcome,
    },
}

impl AwaiterSurfaceProjection {
    pub fn display_message(&self) -> String {
        match self {
            Self::Ready {
                execution_id,
                cell_id,
                phase,
                ..
            } => format!("Awaiter {execution_id} ready: cell {cell_id} {phase}"),
            Self::Acknowledged {
                execution_id,
                acknowledged_turn_id,
                outcome,
                ..
            } => match outcome {
                AwaiterDeliveryOutcome::Drained => {
                    format!("Awaiter {execution_id} delivered to turn {acknowledged_turn_id}")
                }
                AwaiterDeliveryOutcome::OutcomeUnknown => format!(
                    "Awaiter {execution_id} delivery to turn {acknowledged_turn_id} is indeterminate"
                ),
            },
        }
    }
}

pub fn project_awaiter_surface_event(
    event: &crate::chat_driver::ChatDriverEvent,
) -> Option<AwaiterSurfaceProjection> {
    match event {
        crate::chat_driver::ChatDriverEvent::AwaiterResultReady { result } => {
            Some(AwaiterSurfaceProjection::Ready {
                execution_id: result.receipt.execution_id.clone(),
                cell_id: result.cell.cell_id.clone(),
                phase: result.cell.phase,
                terminal_cause: result.cell.terminal_cause,
                exit_code: result.cell.exit_code,
                artifact_status: result.cell.artifact_status,
            })
        }
        crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement } => {
            Some(AwaiterSurfaceProjection::Acknowledged {
                execution_id: acknowledgement.execution_id.clone(),
                cell_id: acknowledgement.cell_id.clone(),
                acknowledged_turn_id: acknowledgement.acknowledged_turn_id.clone(),
                outcome: acknowledgement.outcome.clone(),
            })
        }
        _ => None,
    }
}

struct ActiveAwaiterWatch {
    receipt: AwaiterWatchReceipt,
    executor: Arc<echo_agent::agent::subagent::SubagentExecutor>,
    handle: Option<echo_agent::agent::subagent::BackgroundSubagentHandle>,
    cancel: echo_agent::agent::CancellationToken,
}

#[derive(Default)]
struct AwaiterRuntimeState {
    active: HashMap<AwaiterWatchKey, ActiveAwaiterWatch>,
    latest: HashMap<AwaiterWatchKey, AwaiterWatchReceipt>,
    settled_order: VecDeque<(AwaiterWatchKey, u64)>,
}

#[derive(Default)]
struct AwaiterRecoveryState {
    completed: bool,
    running: Option<tokio::sync::watch::Receiver<Option<Result<(), String>>>>,
}

#[cfg(test)]
struct AwaiterRecoveryTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
struct AwaiterStartedTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AwaiterAgentKey {
    workspace_id: String,
    conversation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCellProjectionDiagnostic {
    pub cell_id: String,
    pub message: String,
}

pub struct CommandCellRuntimeService {
    inner: Arc<BackgroundCommandManager>,
    run_cells: RwLock<HashMap<RunCellScope, HashSet<String>>>,
    chat_cells: RwLock<HashMap<ChatCellScope, HashSet<String>>>,
    cell_deadlines: RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>,
    stores_by_workspace: RwLock<HashMap<String, Weak<TaskRuntimeStore>>>,
    projection_degraded: RwLock<HashMap<String, CommandCellProjectionDiagnostic>>,
    governor: Arc<super::executor::ProcessExecutionGovernor>,
    observers: TaskTracker,
    shutdown: CancellationToken,
    chat_events: Arc<crate::chat_event_log::ChatEventLog>,
    product_data_flow: crate::product_data_io::ProductDataIoFlow,
    awaiters: Mutex<AwaiterRuntimeState>,
    awaiter_agents: RwLock<
        HashMap<
            AwaiterAgentKey,
            std::sync::Weak<tokio::sync::RwLock<echo_agent::agent::ReactAgent>>,
        >,
    >,
    foreground_turns: RwLock<Option<crate::foreground_turn::ForegroundTurnControl>>,
    framework_shutdown: Mutex<Option<FrameworkShutdownSettlement>>,
    awaiter_recovery: tokio::sync::Mutex<AwaiterRecoveryState>,
    #[cfg(test)]
    awaiter_recovery_barrier: Mutex<Option<AwaiterRecoveryTestBarrier>>,
    #[cfg(test)]
    awaiter_recovery_failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    awaiter_started_barrier: Mutex<Option<AwaiterStartedTestBarrier>>,
}

impl CommandCellRuntimeService {
    pub fn new(
        sandbox: Arc<SandboxManager>,
        chat_events: Arc<crate::chat_event_log::ChatEventLog>,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> Result<Arc<Self>, String> {
        let executor: Arc<dyn SandboxExecutor> = sandbox;
        let product_data_flow = product_data_io
            .begin_owned_flow("command-cell product-data projection")
            .map_err(|error| error.to_string())?;
        let service = Arc::new(Self {
            inner: Arc::new(BackgroundCommandManager::new_with_sandbox(
                BackgroundCommandManagerConfig::default(),
                executor,
            )?),
            run_cells: RwLock::new(HashMap::new()),
            chat_cells: RwLock::new(HashMap::new()),
            cell_deadlines: RwLock::new(HashMap::new()),
            stores_by_workspace: RwLock::new(HashMap::new()),
            projection_degraded: RwLock::new(HashMap::new()),
            governor: super::executor::process_execution_governor(),
            observers: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            chat_events,
            product_data_flow,
            awaiters: Mutex::new(AwaiterRuntimeState::default()),
            awaiter_agents: RwLock::new(HashMap::new()),
            foreground_turns: RwLock::new(None),
            framework_shutdown: Mutex::new(None),
            awaiter_recovery: tokio::sync::Mutex::new(AwaiterRecoveryState::default()),
            #[cfg(test)]
            awaiter_recovery_barrier: Mutex::new(None),
            #[cfg(test)]
            awaiter_recovery_failures: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            awaiter_started_barrier: Mutex::new(None),
        });
        service.spawn_started_awaiter_recovery();
        Ok(service)
    }

    fn spawn_started_awaiter_recovery(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let service = Arc::clone(self);
        let observers = self.observers.clone();
        drop(observers.spawn_on(
            async move {
                if let Err(error) = service.ensure_awaiter_recovery().await {
                    tracing::warn!(%error, "Awaiter DeliveryStarted recovery remains pending");
                }
            },
            &runtime,
        ));
    }

    async fn ensure_awaiter_recovery(self: &Arc<Self>) -> Result<(), String> {
        let mut receipt = {
            let mut recovery = self.awaiter_recovery.lock().await;
            if recovery.completed {
                return Ok(());
            }
            match recovery.running.as_ref() {
                Some(running) => running.clone(),
                None => {
                    if self.shutdown.is_cancelled() {
                        return Err(
                            "Awaiter boot recovery is unavailable during shutdown".to_string()
                        );
                    }
                    let (settled, receipt) = tokio::sync::watch::channel(None);
                    recovery.running = Some(receipt.clone());
                    let owner = Arc::downgrade(self);
                    let chat_events = self.chat_events.clone();
                    let observers = self.observers.clone();
                    drop(observers.spawn(async move {
                        #[cfg(test)]
                        if let Some(owner) = owner.upgrade() {
                            let barrier = owner
                                .awaiter_recovery_barrier
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take();
                            if let Some(barrier) = barrier {
                                let _ = barrier.entered.send(());
                                let _ = barrier.release.await;
                            }
                        }
                        #[cfg(test)]
                        let injected_failure = owner.upgrade().is_some_and(|owner| {
                            owner
                                .awaiter_recovery_failures
                                .fetch_update(
                                    std::sync::atomic::Ordering::AcqRel,
                                    std::sync::atomic::Ordering::Acquire,
                                    |remaining| remaining.checked_sub(1),
                                )
                                .is_ok()
                        });
                        #[cfg(not(test))]
                        let injected_failure = false;
                        let result = if injected_failure {
                            Err("injected Awaiter boot recovery failure".to_string())
                        } else {
                            chat_events
                                .settle_all_started_awaiter_deliveries_async()
                                .await
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        };
                        if let Some(owner) = owner.upgrade() {
                            let mut recovery = owner.awaiter_recovery.lock().await;
                            recovery.completed = result.is_ok();
                            recovery.running = None;
                            match &result {
                                Ok(()) => owner.clear_projection_degraded("awaiter-recovery"),
                                Err(error) => owner
                                    .mark_projection_degraded("awaiter-recovery", error.clone()),
                            }
                        }
                        let _ = settled.send(Some(result));
                    }));
                    receipt
                }
            }
        };
        loop {
            if let Some(result) = receipt.borrow().clone() {
                return result;
            }
            receipt.changed().await.map_err(|_| {
                "Awaiter boot recovery owner closed without a typed receipt".to_string()
            })?;
        }
    }

    #[cfg(test)]
    async fn reset_awaiter_recovery_for_test(&self, barrier: Option<AwaiterRecoveryTestBarrier>) {
        let mut recovery = self.awaiter_recovery.lock().await;
        recovery.completed = false;
        recovery.running = None;
        *self
            .awaiter_recovery_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = barrier;
    }

    #[cfg(test)]
    fn fail_next_awaiter_recovery_for_test(&self) {
        self.awaiter_recovery_failures
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    async fn wait_after_awaiter_started_for_test(&self) {
        let barrier = self
            .awaiter_started_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.entered.send(());
            let _ = barrier.release.await;
        }
    }

    pub fn chat_events(&self) -> Arc<crate::chat_event_log::ChatEventLog> {
        self.chat_events.clone()
    }

    pub fn bind_agent(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        agent: &crate::agent_handle::AgentHandle,
    ) {
        if workspace_id.trim().is_empty() || conversation_id.trim().is_empty() {
            return;
        }
        self.awaiter_agents
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                AwaiterAgentKey {
                    workspace_id: workspace_id.to_string(),
                    conversation_id: conversation_id.to_string(),
                },
                Arc::downgrade(agent.inner()),
            );
    }

    pub fn bind_foreground_turns(
        &self,
        foreground_turns: crate::foreground_turn::ForegroundTurnControl,
    ) {
        *self
            .foreground_turns
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(foreground_turns);
    }

    fn agent_for(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Option<crate::agent_handle::AgentHandle> {
        let mut agents = self
            .awaiter_agents
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let key = AwaiterAgentKey {
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
        };
        let agent = agents
            .get(&key)
            .and_then(std::sync::Weak::upgrade)
            .map(crate::agent_handle::AgentHandle::from_arc);
        if agent.is_none() {
            agents.remove(&key);
        }
        agent
    }

    pub fn projection_diagnostics(&self) -> Vec<CommandCellProjectionDiagnostic> {
        self.projection_degraded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn mark_projection_degraded(&self, cell_id: &str, message: impl Into<String>) {
        self.projection_degraded
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                cell_id.to_string(),
                CommandCellProjectionDiagnostic {
                    cell_id: cell_id.to_string(),
                    message: message.into(),
                },
            );
    }

    fn clear_projection_degraded(&self, cell_id: &str) {
        self.projection_degraded
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(cell_id);
    }

    pub fn scoped(
        self: &Arc<Self>,
        execution_scope: crate::workspace::WorkspaceExecutionScope,
        store: Option<Arc<TaskRuntimeStore>>,
    ) -> Arc<dyn CommandCellRegistry> {
        if let Some(store) = store.as_ref() {
            self.bind_store(store);
        }
        Arc::new(ScopedCommandCellRegistry {
            service: self.clone(),
            execution_scope,
        })
    }

    pub fn bind_store(self: &Arc<Self>, store: &Arc<TaskRuntimeStore>) {
        store.bind_command_cell_runtime(Arc::downgrade(self));
        self.stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(store.active_workspace_id(), Arc::downgrade(store));
    }

    fn store_for_workspace(&self, workspace_id: &str) -> Option<Arc<TaskRuntimeStore>> {
        let mut stores = self
            .stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let store = stores.get(workspace_id).and_then(Weak::upgrade);
        if store.is_none() {
            stores.remove(workspace_id);
        }
        store
    }

    pub(crate) fn rebind_store_workspace(&self, previous: &str, current: &str) {
        let mut stores = self
            .stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(store) = stores.remove(previous) {
            stores.insert(current.to_string(), store);
        }
    }

    fn track(&self, scope: &RunCellScope, cell_id: &str, deadline: chrono::DateTime<chrono::Utc>) {
        self.run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(scope.clone())
            .or_default()
            .insert(cell_id.to_string());
        self.cell_deadlines
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(cell_id.to_string(), deadline);
    }

    fn forget(&self, scope: &RunCellScope, cell_id: &str) {
        let mut run_cells = self
            .run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let remove_run = run_cells.get_mut(scope).is_some_and(|cells| {
            cells.remove(cell_id);
            cells.is_empty()
        });
        if remove_run {
            run_cells.remove(scope);
        }
        self.cell_deadlines
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(cell_id);
    }

    fn track_chat(
        &self,
        scope: &ChatCellScope,
        cell_id: &str,
        deadline: chrono::DateTime<chrono::Utc>,
    ) {
        self.chat_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(scope.clone())
            .or_default()
            .insert(cell_id.to_string());
        self.cell_deadlines
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(cell_id.to_string(), deadline);
    }

    fn forget_chat(&self, scope: &ChatCellScope, cell_id: &str) {
        let mut chat_cells = self
            .chat_cells
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let remove_scope = chat_cells.get_mut(scope).is_some_and(|cells| {
            cells.remove(cell_id);
            cells.is_empty()
        });
        if remove_scope {
            chat_cells.remove(scope);
        }
        self.cell_deadlines
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(cell_id);
    }

    fn append_chat_cell_fact_sync(
        chat_events: &crate::chat_event_log::ChatEventLog,
        scope: &ChatCellScope,
        cell: &BackgroundCellState,
        settled: bool,
    ) -> Result<(), String> {
        let replay = chat_events
            .replay(
                &scope.workspace_id,
                Some(&scope.conversation_id),
                &scope.root_turn_id,
                0,
            )
            .map_err(|error| error.to_string())?;
        if let Some(existing) = find_chat_cell_fact(&replay.events, &cell.cell_id, settled) {
            return if existing == cell {
                Ok(())
            } else {
                Err(format!(
                    "conflicting ordinary-chat command-cell fact for {}",
                    cell.cell_id
                ))
            };
        }
        let event = if settled {
            crate::chat_driver::ChatDriverEvent::CommandCellSettled {
                cell: Box::new(cell.clone()),
            }
        } else {
            crate::chat_driver::ChatDriverEvent::CommandCellStarted {
                cell: Box::new(cell.clone()),
            }
        };
        match chat_events.append(
            &scope.workspace_id,
            Some(&scope.conversation_id),
            &scope.root_turn_id,
            event,
        ) {
            Ok(_) => Ok(()),
            Err(append_error) => {
                let repaired = chat_events
                    .replay(
                        &scope.workspace_id,
                        Some(&scope.conversation_id),
                        &scope.root_turn_id,
                        0,
                    )
                    .map_err(|repair_error| {
                        format!("{append_error}; journal repair failed: {repair_error}")
                    })?;
                match find_chat_cell_fact(&repaired.events, &cell.cell_id, settled) {
                    Some(existing) if existing == cell => Ok(()),
                    Some(_) => Err(format!(
                        "conflicting ordinary-chat command-cell fact after repair for {}",
                        cell.cell_id
                    )),
                    None => Err(append_error.to_string()),
                }
            }
        }
    }

    async fn append_chat_cell_fact(
        &self,
        scope: &ChatCellScope,
        cell: &BackgroundCellState,
        settled: bool,
    ) -> Result<(), String> {
        let chat_events = self.chat_events.clone();
        let scope = scope.clone();
        let cell = cell.clone();
        self.product_data_flow
            .run("persist ordinary-chat command cell", move || {
                Self::append_chat_cell_fact_sync(&chat_events, &scope, &cell, settled)
            })
            .await
            .map_err(|error| error.to_string())?
    }

    async fn cell_state_for_watch(
        &self,
        key: &AwaiterWatchKey,
    ) -> Result<BackgroundCellState, CommandCellError> {
        if let Some(run_id) = key.run_id.as_deref() {
            let store = self.store_for_workspace(&key.workspace_id).ok_or_else(|| {
                CommandCellError::Validation {
                    message: "Awaiter requires the exact TaskRuntimeStore".to_string(),
                }
            })?;
            let run_id = run_id.to_string();
            let cell_id = key.cell_id.clone();
            return super::executor::TaskRuntimeBlockingAdapter::new(store)
                .run_store("load Awaiter TaskRun cell", move |store| {
                    store
                        .list_background_cells(&run_id)
                        .map(|cells| cells.into_iter().find(|cell| cell.cell_id == cell_id))
                })
                .await
                .map_err(|error| CommandCellError::Runtime {
                    message: error.to_string(),
                })?
                .ok_or_else(|| CommandCellError::Validation {
                    message: "cell does not belong to the exact TaskRun scope".to_string(),
                });
        }
        let chat_events = self.chat_events.clone();
        let key = key.clone();
        self.product_data_flow
            .run("load Awaiter ordinary-chat cell", move || {
                let replay = chat_events.replay(
                    &key.workspace_id,
                    Some(&key.conversation_id),
                    &key.root_turn_id,
                    0,
                )?;
                Ok::<Option<BackgroundCellState>, crate::chat_event_log::ChatEventLogError>(
                    find_chat_cell_fact(&replay.events, &key.cell_id, false)
                        .or_else(|| find_chat_cell_fact(&replay.events, &key.cell_id, true))
                        .cloned(),
                )
            })
            .await
            .map_err(|error| CommandCellError::Runtime {
                message: error.to_string(),
            })?
            .map_err(|error| CommandCellError::Runtime {
                message: error.to_string(),
            })?
            .ok_or_else(|| CommandCellError::Validation {
                message: "cell does not belong to the exact ordinary Chat scope".to_string(),
            })
    }

    fn owns_active_cell(&self, key: &AwaiterWatchKey) -> bool {
        match key.run_id.as_deref() {
            Some(run_id) => self
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&RunCellScope {
                    workspace_id: key.workspace_id.clone(),
                    run_id: run_id.to_string(),
                })
                .is_some_and(|cells| cells.contains(&key.cell_id)),
            None => self
                .chat_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&ChatCellScope {
                    workspace_id: key.workspace_id.clone(),
                    conversation_id: key.conversation_id.clone(),
                    root_turn_id: key.root_turn_id.clone(),
                })
                .is_some_and(|cells| cells.contains(&key.cell_id)),
        }
    }

    async fn watch_cell(
        self: &Arc<Self>,
        registry: Arc<dyn CommandCellRegistry>,
        executor: Arc<echo_agent::agent::subagent::SubagentExecutor>,
        execution_scope: &crate::workspace::WorkspaceExecutionScope,
        context: &ToolContext,
        cell_id: &str,
        new_generation: bool,
    ) -> Result<AwaiterWatchReceipt, CommandCellError> {
        let conversation_id =
            context
                .conversation_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "watch_cell requires conversation identity".to_string(),
                })?;
        let root_turn_id =
            context
                .message_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "watch_cell requires root message identity".to_string(),
                })?;
        let key = AwaiterWatchKey {
            workspace_id: execution_scope.workspace_id().to_string(),
            conversation_id,
            run_id: context.run_id.clone(),
            root_turn_id,
            cell_id: cell_id.to_string(),
        };
        if !self.owns_active_cell(&key) {
            return Err(CommandCellError::Validation {
                message: "watch_cell cannot cross its exact cell owner scope".to_string(),
            });
        }
        let snapshot = registry.wait(cell_id, 0, 0).await?;
        if snapshot.snapshot.phase.is_terminal() {
            return Err(CommandCellError::Validation {
                message: format!(
                    "cell {cell_id} is already {}",
                    snapshot.snapshot.phase.as_str()
                ),
            });
        }
        let base_cell = self.cell_state_for_watch(&key).await?;
        let observation = registry.observe(cell_id)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| CommandCellError::Runtime {
                message: format!("Tokio runtime is unavailable: {error}"),
            })?;
        let cancel = echo_agent::agent::CancellationToken::new();
        let receipt = {
            let mut state = self
                .awaiters
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(active) = state.active.get(&key) {
                return Ok(active.receipt.clone());
            }
            if !new_generation && let Some(latest) = state.latest.get(&key) {
                return Ok(latest.clone());
            }
            if state.active.len() >= MAX_ACTIVE_AWAITERS {
                return Err(CommandCellError::Runtime {
                    message: "process Awaiter capacity is full".to_string(),
                });
            }
            let watch_generation = state
                .latest
                .get(&key)
                .map(|receipt| receipt.watch_generation.saturating_add(1))
                .unwrap_or(1);
            let control_task_id = format!("awaiter:{cell_id}:{watch_generation}");
            let receipt = AwaiterWatchReceipt {
                execution_id: format!("awaiter-{}", uuid::Uuid::new_v4()),
                control_task_id,
                attempt: 1,
                watch_generation,
                cell_id: cell_id.to_string(),
                workspace_id: key.workspace_id.clone(),
                conversation_id: key.conversation_id.clone(),
                run_id: key.run_id.clone(),
                root_turn_id: key.root_turn_id.clone(),
                state: AwaiterWatchState::Started,
                started_at: chrono::Utc::now(),
                settled_at: None,
            };
            state.latest.insert(key.clone(), receipt.clone());
            state.active.insert(
                key.clone(),
                ActiveAwaiterWatch {
                    receipt: receipt.clone(),
                    executor: executor.clone(),
                    handle: None,
                    cancel: cancel.clone(),
                },
            );
            receipt
        };

        let deadline = self
            .cell_deadlines
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(cell_id)
            .copied()
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::seconds(30));
        let wait_duration = (deadline - chrono::Utc::now()).to_std().unwrap_or_default();
        let permit = tokio::select! {
            _ = self.shutdown.cancelled() => Err(CommandCellError::Shutdown),
            _ = cancel.cancelled() => Err(CommandCellError::Cancelled),
            result = tokio::time::timeout(
                wait_duration,
                self.governor.subagent_semaphore().acquire_owned(),
            ) => result
                .map_err(|_| CommandCellError::CapacityDeadline)
                .and_then(|permit| permit.map_err(|_| CommandCellError::Shutdown)),
        };
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => {
                self.fail_awaiter_start(&key, &receipt, error.to_string());
                return Err(error);
            }
        };
        let identity = match echo_agent::agent::subagent::SubagentAttemptIdentity::new(
            receipt.control_task_id.clone(),
            receipt.execution_id.clone(),
            receipt.attempt,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                self.fail_awaiter_start(&key, &receipt, error.to_string());
                return Err(CommandCellError::Runtime {
                    message: error.to_string(),
                });
            }
        };
        let request = echo_agent::agent::subagent::DispatchRequest {
            agent_name: "awaiter".to_string(),
            task: format!(
                "Watch command cell {cell_id} until it reaches a terminal state and all output is drained. Report only typed runtime fields."
            ),
            mode_override: None,
            cancel: cancel.clone(),
            parent_agent: "eko".to_string(),
            parent_context: None,
            delegation_policy: echo_agent::tasks::NestedDelegationPolicy {
                can_spawn_subagents: false,
                delegate_depth: 0,
                max_delegate_depth: 0,
            },
            runtime_context: Some(echo_agent::tools::ExternalRunContext {
                conversation_id: Some(key.conversation_id.clone()),
                run_id: key.run_id.clone(),
                turn_id: context.turn_id.clone(),
                execution_id: Some(receipt.execution_id.clone()),
                isolation_id: None,
                message_id: Some(key.root_turn_id.clone()),
                cancel: Some(Arc::new(cancel.clone())),
                trace_sink: context.trace_sink.clone(),
                delegation_policy: Some(echo_agent::tasks::NestedDelegationPolicy {
                    can_spawn_subagents: false,
                    delegate_depth: 0,
                    max_delegate_depth: 0,
                }),
                resource_guards: context.resource_guards.clone(),
            }),
            message: None,
            prompt_payload: None,
            constraints: vec![
                "Observe only the assigned command cell".to_string(),
                "Do not create or mutate TaskRun state".to_string(),
            ],
            background: true,
        };
        let handle = match executor
            .dispatch_background_attempt(request, identity)
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                self.fail_awaiter_start(&key, &receipt, error.to_string());
                return Err(CommandCellError::Runtime {
                    message: error.to_string(),
                });
            }
        };
        let cancelled_while_starting = {
            let mut state = self
                .awaiters
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match state.active.get_mut(&key) {
                Some(active) if active.receipt.execution_id == receipt.execution_id => {
                    active.handle = Some(handle.clone());
                    active.cancel.is_cancelled()
                }
                _ => true,
            }
        };
        if cancelled_while_starting {
            handle.cancel();
        }
        let service = self.clone();
        let observers = self.observers.clone();
        let receipt_for_task = receipt.clone();
        drop(observers.spawn_on(
            async move {
                let _observation = observation;
                let join_result = handle.join().await;
                drop(permit);
                let settled_receipt =
                    service.mark_awaiter_joined(&key, &receipt_for_task, &join_result);
                let settled_execution_id = settled_receipt.execution_id.clone();
                match observe_awaiter_cell_truth(registry, base_cell, true, &service.shutdown).await
                {
                    Ok(Some(cell)) => {
                        let (awaiter_status, awaiter_summary) = awaiter_summary(join_result);
                        if let Err(error) = service
                            .publish_awaiter_result(AwaiterResult {
                                receipt: settled_receipt,
                                cell,
                                awaiter_status,
                                awaiter_summary,
                            })
                            .await
                        {
                            service.mark_projection_degraded(&settled_execution_id, error.clone());
                            tracing::error!(%error, "Awaiter result publication remained degraded");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        service.mark_projection_degraded(&settled_execution_id, error.clone());
                        tracing::warn!(
                            execution_id = settled_execution_id,
                            %error,
                            "Awaiter joined but runtime cell truth could not be observed"
                        );
                    }
                }
            },
            &runtime,
        ));
        Ok(receipt)
    }

    fn fail_awaiter_start(
        &self,
        key: &AwaiterWatchKey,
        receipt: &AwaiterWatchReceipt,
        message: String,
    ) {
        let mut state = self
            .awaiters
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active.remove(key);
        let mut failed = receipt.clone();
        failed.state = AwaiterWatchState::Failed;
        failed.settled_at = Some(chrono::Utc::now());
        state.latest.insert(key.clone(), failed);
        tracing::warn!(execution_id = receipt.execution_id, %message, "Awaiter start failed");
    }

    fn mark_awaiter_joined(
        &self,
        key: &AwaiterWatchKey,
        receipt: &AwaiterWatchReceipt,
        result: &echo_agent::error::Result<echo_agent::agent::subagent::SubagentResult>,
    ) -> AwaiterWatchReceipt {
        let mut settled = receipt.clone();
        settled.state = match result {
            Ok(result) => match result.outcome.status {
                echo_agent::agent::subagent::SubagentStatus::Completed => {
                    AwaiterWatchState::Settled
                }
                echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                    AwaiterWatchState::Cancelled
                }
                echo_agent::agent::subagent::SubagentStatus::Failed
                | echo_agent::agent::subagent::SubagentStatus::TimedOut => {
                    AwaiterWatchState::Failed
                }
            },
            Err(error) => match echo_agent::agent::subagent::subagent_status_from_error(error) {
                echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                    AwaiterWatchState::Cancelled
                }
                _ => AwaiterWatchState::Failed,
            },
        };
        settled.settled_at = Some(chrono::Utc::now());
        let mut state = self
            .awaiters
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active.remove(key);
        state.latest.insert(key.clone(), settled.clone());
        state
            .settled_order
            .push_back((key.clone(), settled.watch_generation));
        while state.settled_order.len() > SETTLED_AWAITER_RETENTION {
            let Some((old_key, old_generation)) = state.settled_order.pop_front() else {
                break;
            };
            let remove = state
                .latest
                .get(&old_key)
                .is_some_and(|latest| latest.watch_generation == old_generation)
                && !state.active.contains_key(&old_key);
            if remove {
                state.latest.remove(&old_key);
            }
        }
        settled
    }

    async fn interrupt_awaiter(
        &self,
        execution_id: &str,
        expected_attempt: u32,
    ) -> Result<AwaiterWatchReceipt, String> {
        let (executor, cancel, handle, receipt) = {
            let state = self
                .awaiters
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let active = state
                .active
                .values()
                .find(|active| active.receipt.execution_id == execution_id)
                .ok_or_else(|| format!("Awaiter execution '{execution_id}' is not active"))?;
            if active.receipt.attempt != expected_attempt {
                return Err(format!(
                    "Awaiter attempt mismatch: expected {expected_attempt}, active {}",
                    active.receipt.attempt
                ));
            }
            (
                active.executor.clone(),
                active.cancel.clone(),
                active.handle.clone(),
                active.receipt.clone(),
            )
        };
        cancel.cancel();
        if let Some(handle) = handle {
            handle.cancel();
            executor
                .interrupt_subagent(execution_id, expected_attempt)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(receipt)
    }

    async fn publish_awaiter_result(self: &Arc<Self>, result: AwaiterResult) -> Result<(), String> {
        self.ensure_awaiter_recovery().await?;
        let mut delay = Duration::from_millis(50);
        let mut attempt = 0_u64;
        loop {
            attempt = attempt.saturating_add(1);
            let chat_events = self.chat_events.clone();
            let append_result = result.clone();
            let persisted = self
                .product_data_flow
                .run("persist Awaiter Ready fact", move || {
                    chat_events.append(
                        &append_result.receipt.workspace_id,
                        Some(&append_result.receipt.conversation_id),
                        &append_result.receipt.root_turn_id,
                        crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
                            result: Box::new(append_result.clone()),
                        },
                    )
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map(|_| ()).map_err(|error| error.to_string()));
            match persisted {
                Ok(()) => {
                    self.clear_projection_degraded(&result.receipt.execution_id);
                    break;
                }
                Err(error) => {
                    self.mark_projection_degraded(&result.receipt.execution_id, error.clone());
                    tracing::warn!(
                        execution_id = result.receipt.execution_id,
                        attempt,
                        %error,
                        "retrying Awaiter Ready persistence"
                    );
                    if attempt >= MAX_PROJECTION_REPAIR_ATTEMPTS {
                        return Err(format!(
                            "Awaiter '{}' Ready persistence exhausted its repair budget: {error}",
                            result.receipt.execution_id
                        ));
                    }
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2).min(Duration::from_secs(1));
                }
            }
        }

        let instruction = render_awaiter_handoff(&result);
        let foreground_turns = self
            .foreground_turns
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let snapshots = foreground_turns
            .as_ref()
            .and_then(|turns| {
                turns
                    .snapshots_for_conversation_scoped(
                        &result.receipt.workspace_id,
                        &result.receipt.conversation_id,
                    )
                    .ok()
            })
            .unwrap_or_default();
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.root_turn_id == result.receipt.root_turn_id)
            .or_else(|| snapshots.first());
        if let (Some(snapshot), Some(agent)) = (
            snapshot,
            self.agent_for(
                &result.receipt.workspace_id,
                &result.receipt.conversation_id,
            ),
        ) {
            let mut acknowledgement = AwaiterResultAcknowledgement {
                execution_id: result.receipt.execution_id.clone(),
                attempt: result.receipt.attempt,
                watch_generation: result.receipt.watch_generation,
                cell_id: result.receipt.cell_id.clone(),
                acknowledged_turn_id: snapshot.active_turn_id.clone(),
                outcome: AwaiterDeliveryOutcome::OutcomeUnknown,
            };
            self.persist_awaiter_delivery_fact(&result, "DeliveryStarted", || {
                crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                    acknowledgement: acknowledgement.clone(),
                }
            })
            .await?;
            #[cfg(test)]
            self.wait_after_awaiter_started_for_test().await;
            let mut steer = match agent
                .steer_input_tracked(
                    Some(&snapshot.active_turn_id),
                    echo_agent::llm::types::Message::user(instruction),
                )
                .await
            {
                Ok(steer) => steer,
                Err(_) => {
                    self.persist_awaiter_delivery_fact(&result, "Acknowledged", || {
                        crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
                            acknowledgement: acknowledgement.clone(),
                        }
                    })
                    .await?;
                    return Ok(());
                }
            };
            let state = tokio::select! {
                _ = self.shutdown.cancelled() => None,
                state = steer.wait_for_drained() => Some(state),
            };
            if state.is_some_and(|state| state.was_drained()) {
                acknowledgement.acknowledged_turn_id = steer.turn_id().to_string();
                acknowledgement.outcome = AwaiterDeliveryOutcome::Drained;
            }
            self.persist_awaiter_delivery_fact(&result, "Acknowledged", || {
                crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
                    acknowledgement: acknowledgement.clone(),
                }
            })
            .await?;
        }
        Ok(())
    }

    async fn persist_awaiter_delivery_fact<F>(
        &self,
        result: &AwaiterResult,
        fact: &'static str,
        event: F,
    ) -> Result<(), String>
    where
        F: Fn() -> crate::chat_driver::ChatDriverEvent,
    {
        let mut delay = Duration::from_millis(50);
        for attempt in 1..=MAX_PROJECTION_REPAIR_ATTEMPTS {
            let chat_events = self.chat_events.clone();
            let workspace_id = result.receipt.workspace_id.clone();
            let conversation_id = result.receipt.conversation_id.clone();
            let root_turn_id = result.receipt.root_turn_id.clone();
            let durable_event = event();
            let persisted = self
                .product_data_flow
                .run("persist Awaiter delivery fact", move || {
                    chat_events.append(
                        &workspace_id,
                        Some(&conversation_id),
                        &root_turn_id,
                        durable_event,
                    )
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map(|_| ()).map_err(|error| error.to_string()));
            match persisted {
                Ok(()) => {
                    self.clear_projection_degraded(&result.receipt.execution_id);
                    return Ok(());
                }
                Err(error) => {
                    self.mark_projection_degraded(&result.receipt.execution_id, error.clone());
                    tracing::warn!(
                        execution_id = result.receipt.execution_id,
                        attempt,
                        %error,
                        fact,
                        "retrying Awaiter delivery fact persistence"
                    );
                    if attempt >= MAX_PROJECTION_REPAIR_ATTEMPTS {
                        return Err(format!(
                            "Awaiter '{}' {fact} persistence exhausted its repair budget: {error}",
                            result.receipt.execution_id
                        ));
                    }
                    tokio::select! {
                        _ = self.shutdown.cancelled() => {
                            return Err(format!("Awaiter {fact} persistence stopped during shutdown"));
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                    delay = delay.saturating_mul(2).min(Duration::from_secs(1));
                }
            }
        }
        Err(format!("Awaiter {fact} persistence did not settle"))
    }

    pub(crate) async fn project_pending_awaiter_results(
        self: &Arc<Self>,
        workspace_id: &str,
        conversation_id: &str,
        turn_id: &str,
    ) -> Result<Option<String>, String> {
        self.ensure_awaiter_recovery().await?;
        let chat_events = self.chat_events.clone();
        let pending_workspace_id = workspace_id.to_string();
        let pending_conversation_id = conversation_id.to_string();
        let pending = self
            .product_data_flow
            .run("project pending Awaiter results", move || {
                chat_events
                    .pending_awaiter_results_for_conversation(
                        &pending_workspace_id,
                        &pending_conversation_id,
                    )
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
        if pending.is_empty() {
            return Ok(None);
        }
        let mut rendered = String::from("[pending_awaiter_results]\n");
        for result in &pending {
            rendered.push_str(&render_awaiter_handoff(result));
            rendered.push('\n');
            let acknowledgement = AwaiterResultAcknowledgement {
                execution_id: result.receipt.execution_id.clone(),
                attempt: result.receipt.attempt,
                watch_generation: result.receipt.watch_generation,
                cell_id: result.receipt.cell_id.clone(),
                acknowledged_turn_id: turn_id.to_string(),
                outcome: AwaiterDeliveryOutcome::OutcomeUnknown,
            };
            self.persist_awaiter_delivery_fact(result, "DeliveryStarted", || {
                crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                    acknowledgement: acknowledgement.clone(),
                }
            })
            .await?;
            self.persist_awaiter_delivery_fact(result, "Acknowledged", || {
                crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
                    acknowledgement: acknowledgement.clone(),
                }
            })
            .await?;
        }
        rendered.push_str("[/pending_awaiter_results]");
        Ok(Some(rendered))
    }

    pub(crate) fn stop_run(&self, workspace_id: &str, run_id: &str) -> usize {
        let scope = RunCellScope {
            workspace_id: workspace_id.to_string(),
            run_id: run_id.to_string(),
        };
        let cells = self
            .run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&scope)
            .unwrap_or_default();
        cells
            .into_iter()
            .filter(|cell_id| self.inner.stop(cell_id))
            .count()
    }

    pub fn begin_shutdown(&self) -> Result<(), String> {
        self.shutdown.cancel();
        self.observers.close();
        let active_cells = {
            let run_cells = self
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let chat_cells = self
                .chat_cells
                .read()
                .unwrap_or_else(|error| error.into_inner());
            run_cells
                .values()
                .chain(chat_cells.values())
                .flat_map(|cells| cells.iter().cloned())
                .collect::<HashSet<_>>()
        };
        for cell_id in active_cells {
            let _ = self.inner.stop(&cell_id);
        }
        let active_awaiters = self
            .awaiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .values()
            .map(|active| (active.cancel.clone(), active.handle.clone()))
            .collect::<Vec<_>>();
        for (cancel, handle) in active_awaiters {
            cancel.cancel();
            if let Some(handle) = handle {
                handle.cancel();
            }
        }
        let mut owner = self
            .framework_shutdown
            .lock()
            .map_err(|_| "command-cell framework shutdown owner lock is poisoned".to_string())?;
        if owner.is_none() {
            let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
                format!("Tokio runtime is unavailable during command-cell shutdown: {error}")
            })?;
            let inner = self.inner.clone();
            let shutdown = async move {
                match std::panic::AssertUnwindSafe(inner.shutdown())
                    .catch_unwind()
                    .await
                {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("command-cell framework shutdown panicked".to_string()),
                }
            }
            .boxed()
            .shared();
            drop(runtime.spawn(shutdown.clone()));
            *owner = Some(shutdown);
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Err(error) = self.begin_shutdown() {
            failures.push(error);
        }
        let owned_shutdown = self
            .framework_shutdown
            .lock()
            .map_err(|_| "command-cell framework shutdown owner lock is poisoned".to_string())?
            .clone();
        let settlement = tokio::time::timeout(OBSERVER_SHUTDOWN_TIMEOUT, async {
            let shutdown_result = match owned_shutdown {
                Some(shutdown) => shutdown.await,
                None => Err("command-cell framework shutdown owner is unavailable".to_string()),
            };
            self.observers.wait().await;
            shutdown_result
        })
        .await;
        match settlement {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(error),
            Err(_) => failures.push(format!(
                "command-cell framework and observer shutdown did not settle within {} seconds",
                OBSERVER_SHUTDOWN_TIMEOUT.as_secs()
            )),
        }
        let degraded = self
            .projection_degraded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .map(|diagnostic| format!("{}: {}", diagnostic.cell_id, diagnostic.message))
            .collect::<Vec<_>>();
        if !degraded.is_empty() {
            failures.push(format!(
                "command-cell projection debt remained at shutdown: {}",
                degraded.join("; ")
            ));
        }
        let product_data_debt = (!failures.is_empty()).then(|| failures.join("; "));
        self.product_data_flow.settle(product_data_debt);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

struct ScopedCommandCellRegistry {
    service: Arc<CommandCellRuntimeService>,
    execution_scope: crate::workspace::WorkspaceExecutionScope,
}

impl ScopedCommandCellRegistry {
    #[allow(clippy::too_many_arguments)]
    fn spawn_observer(
        &self,
        observation: CommandCellObservationLease,
        store: Arc<TaskRuntimeStore>,
        scope: RunCellScope,
        cell_id: String,
        name: String,
        call_id: Option<String>,
        shell_permit: Option<OwnedSemaphorePermit>,
        operation_reservation: super::executor::TaskRuntimeSettlementReservation,
    ) -> Result<(), CommandCellError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| CommandCellError::Runtime {
                message: format!("Tokio runtime is unavailable: {error}"),
            })?;
        let service = self.service.clone();
        let registry = service.inner.clone();
        let observers = service.observers.clone();
        let operation_store = store.clone();
        let operation_adapter = super::executor::TaskRuntimeBlockingAdapter::new(store);
        let debt_adapter = operation_adapter.clone();
        let operation = operation_adapter.spawn_reserved_settlement(
            "observe command-cell terminal",
            operation_reservation,
            async move {
                let _observation = observation;
                let _shell_permit = shell_permit;
                let persisted = observe_terminal_cell(
                    registry,
                    operation_store,
                    scope.run_id.clone(),
                    cell_id.clone(),
                    name,
                    call_id,
                    service.clone(),
                )
                .await;
                service.forget(&scope, &cell_id);
                if persisted {
                    Ok(())
                } else {
                    let error = super::store::StoreError::InvalidPlan(format!(
                        "command-cell '{cell_id}' terminal persistence exhausted its repair budget"
                    ));
                    debt_adapter.record_lifecycle_debt("observe command-cell terminal", &error);
                    Err(error)
                }
            },
        );
        drop(observers.spawn_on(
            async move {
                match operation.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::error!(%error, "command-cell observer ownership failed");
                    }
                    Err(error) => {
                        tracing::error!(%error, "command-cell observer receipt was lost");
                    }
                }
            },
            &runtime,
        ));
        Ok(())
    }

    fn spawn_chat_observer(
        &self,
        observation: CommandCellObservationLease,
        scope: ChatCellScope,
        started: BackgroundCellState,
        shell_permit: Option<OwnedSemaphorePermit>,
        operation_reservation: super::executor::TaskRuntimeSettlementReservation,
    ) -> Result<(), CommandCellError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| CommandCellError::Runtime {
                message: format!("Tokio runtime is unavailable: {error}"),
            })?;
        let service = self.service.clone();
        let registry = service.inner.clone();
        let observers = service.observers.clone();
        let store = service
            .store_for_workspace(&scope.workspace_id)
            .ok_or_else(|| CommandCellError::Validation {
                message: "ordinary Chat cell requires the scoped TaskRuntimeStore".to_string(),
            })?;
        let operation_adapter = super::executor::TaskRuntimeBlockingAdapter::new(store);
        let debt_adapter = operation_adapter.clone();
        let operation = operation_adapter.spawn_reserved_settlement(
                "observe ordinary-chat command-cell terminal",
                operation_reservation,
                async move {
                    let _observation = observation;
                    let _shell_permit = shell_permit;
                    let cell_id = started.cell_id.clone();
                    let persisted = observe_chat_terminal_cell(
                        registry,
                        service.clone(),
                        scope.clone(),
                        started,
                    )
                    .await;
                    service.forget_chat(&scope, &cell_id);
                    if persisted {
                        Ok(())
                    } else {
                        let error = super::store::StoreError::InvalidPlan(format!(
                            "ordinary-chat command-cell '{cell_id}' terminal persistence exhausted its repair budget"
                        ));
                        debt_adapter.record_lifecycle_debt(
                            "observe ordinary-chat command-cell terminal",
                            &error,
                        );
                        Err(error)
                    }
                },
            );
        drop(observers.spawn_on(
            async move {
                match operation.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::error!(%error, "ordinary-chat command-cell observer failed");
                    }
                    Err(error) => {
                        tracing::error!(%error, "ordinary-chat command-cell receipt was lost");
                    }
                }
            },
            &runtime,
        ));
        Ok(())
    }

    async fn launch_chat(
        &self,
        request: CommandCellRequest,
        name: String,
        command_hash: String,
    ) -> Result<CommandCellLaunchReceipt, CommandCellError> {
        let owner = request.owner.clone();
        let conversation_id =
            owner
                .conversation_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "ordinary Chat cell requires conversation identity".to_string(),
                })?;
        let root_turn_id =
            owner
                .message_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "ordinary Chat cell requires root message identity".to_string(),
                })?;
        let operation_store = self
            .service
            .store_for_workspace(self.execution_scope.workspace_id())
            .ok_or_else(|| CommandCellError::Validation {
                message: "ordinary Chat cell requires the scoped TaskRuntimeStore".to_string(),
            })?;
        let operation_adapter = super::executor::TaskRuntimeBlockingAdapter::new(operation_store);
        let operation_reservation = operation_adapter
            .reserve_settlement("observe ordinary-chat command-cell terminal")
            .map_err(|error| CommandCellError::Runtime {
                message: error.to_string(),
            })?;
        let reservation = self.service.inner.prepare_launch(request).await?;
        let receipt = reservation.receipt().clone();
        let cell_id = receipt.cell_id.clone();
        let observation = self.service.inner.observe(&cell_id)?;
        let scope = ChatCellScope {
            workspace_id: self.execution_scope.workspace_id().to_string(),
            conversation_id,
            root_turn_id,
        };
        let started = BackgroundCellState {
            cell_id: cell_id.clone(),
            name,
            command_hash,
            turn_id: owner.turn_id.clone(),
            execution_id: owner.execution_id.clone(),
            call_id: owner.call_id.clone(),
            phase: BackgroundCellPhase::Prepared,
            terminal_cause: None,
            terminal_message: None,
            exit_code: None,
            artifact_status: BackgroundCellArtifactStatus::NotRequested,
            artifact_message: None,
            total_output_bytes: 0,
            output_truncated: false,
            output_excerpt: None,
            artifact_path: None,
            artifact_sha256: None,
            started_at: receipt.accepted_at,
            finished_at: None,
        };
        if let Err(error) = self
            .service
            .append_chat_cell_fact(&scope, &started, false)
            .await
        {
            let _ = self
                .service
                .inner
                .abort_prepared(reservation, format!("Started persistence failed: {error}"));
            return Err(CommandCellError::Runtime {
                message: format!("ordinary Chat cell start could not be persisted: {error}"),
            });
        }
        self.service.track_chat(&scope, &cell_id, receipt.deadline);

        let deadline = (receipt.deadline - chrono::Utc::now())
            .to_std()
            .unwrap_or_default();
        let shell = self.service.governor.shell_semaphore().acquire_owned();
        let shell_permit = tokio::select! {
            _ = self.service.shutdown.cancelled() => Err(CommandCellError::Shutdown),
            result = tokio::time::timeout(deadline, shell) => result
                .map_err(|_| CommandCellError::CapacityDeadline)
                .and_then(|permit| permit.map_err(|_| CommandCellError::Shutdown)),
        };
        let shell_permit = match shell_permit {
            Ok(permit) => permit,
            Err(error) => {
                let _ = self.service.inner.abort_prepared(
                    reservation,
                    format!("process shell admission failed: {error}"),
                );
                self.spawn_chat_observer(observation, scope, started, None, operation_reservation)?;
                return Err(error);
            }
        };
        let start_result = self.service.inner.start_prepared(reservation).await;
        if let Err(error) = start_result {
            self.spawn_chat_observer(
                observation,
                scope,
                started,
                Some(shell_permit),
                operation_reservation,
            )?;
            return Err(error);
        }
        self.spawn_chat_observer(
            observation,
            scope,
            started,
            Some(shell_permit),
            operation_reservation,
        )?;
        Ok(receipt)
    }
}

/// Install the Task/Auto-safe Awaiter control surface. Dispatch goes directly
/// through the framework executor while EKO retains the exact attempt handle.
pub(crate) fn install_watch_cell_tool(
    agent: &mut echo_agent::agent::ReactAgent,
    registry: Arc<dyn CommandCellRegistry>,
    service: Arc<CommandCellRuntimeService>,
    execution_scope: crate::workspace::WorkspaceExecutionScope,
) {
    let executor = agent.subagent_executor().clone();
    agent.add_tool(Box::new(WatchCellTool {
        registry,
        service: service.clone(),
        executor,
        execution_scope,
    }));
    agent.add_tool(Box::new(InterruptAwaiterTool { service }));
}

struct WatchCellTool {
    registry: Arc<dyn CommandCellRegistry>,
    service: Arc<CommandCellRuntimeService>,
    executor: Arc<echo_agent::agent::subagent::SubagentExecutor>,
    execution_scope: crate::workspace::WorkspaceExecutionScope,
}

impl Tool for WatchCellTool {
    fn name(&self) -> &str {
        "watch_cell"
    }

    fn description(&self) -> &str {
        "Dispatch the dedicated low-reasoning awaiter Subagent to watch one running background command cell. Returns immediately; the current agent can continue other work."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "cell_id": {
                    "type": "string",
                    "description": "Running cell ID returned by shell(background=true)"
                },
                "new_generation": {
                    "type": "boolean",
                    "description": "Start a new watch generation after the previous one settled"
                }
            },
            "required": ["cell_id"]
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        let registry = Arc::clone(&self.registry);
        let service = self.service.clone();
        let executor = self.executor.clone();
        let execution_scope = self.execution_scope.clone();
        Box::pin(async move {
            let cell_id = parameters
                .get("cell_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    echo_agent::error::ToolError::MissingParameter("cell_id".to_string())
                })?;
            let new_generation = parameters
                .get("new_generation")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            match service
                .watch_cell(
                    registry,
                    executor,
                    &execution_scope,
                    context,
                    cell_id,
                    new_generation,
                )
                .await
            {
                Ok(receipt) => serde_json::to_string(&receipt)
                    .map(ToolResult::success)
                    .map_err(|error| {
                        echo_agent::error::ToolError::ExecutionFailed {
                            tool: "watch_cell".to_string(),
                            message: error.to_string(),
                        }
                        .into()
                    }),
                Err(error) => Ok(ToolResult::error(error.to_string())),
            }
        })
    }
}

struct InterruptAwaiterTool {
    service: Arc<CommandCellRuntimeService>,
}

impl Tool for InterruptAwaiterTool {
    fn name(&self) -> &str {
        "interrupt_awaiter"
    }

    fn description(&self) -> &str {
        "Interrupt one exact Awaiter attempt without stopping its command cell."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "execution_id": { "type": "string" },
                "expected_attempt": { "type": "integer", "minimum": 1 }
            },
            "required": ["execution_id", "expected_attempt"]
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        _context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        let service = self.service.clone();
        Box::pin(async move {
            let execution_id = parameters
                .get("execution_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    echo_agent::error::ToolError::MissingParameter("execution_id".to_string())
                })?;
            let expected_attempt = parameters
                .get("expected_attempt")
                .and_then(serde_json::Value::as_u64)
                .and_then(|attempt| u32::try_from(attempt).ok())
                .filter(|attempt| *attempt > 0)
                .ok_or_else(|| echo_agent::error::ToolError::InvalidParameter {
                    name: "expected_attempt".to_string(),
                    message: "must be a positive u32".to_string(),
                })?;
            match service
                .interrupt_awaiter(execution_id, expected_attempt)
                .await
            {
                Ok(receipt) => serde_json::to_string(&receipt)
                    .map(ToolResult::success)
                    .map_err(|error| {
                        echo_agent::error::ToolError::ExecutionFailed {
                            tool: "interrupt_awaiter".to_string(),
                            message: error.to_string(),
                        }
                        .into()
                    }),
                Err(error) => Ok(ToolResult::error(error)),
            }
        })
    }
}

impl CommandCellRegistry for ScopedCommandCellRegistry {
    fn launch(
        &self,
        request: CommandCellRequest,
    ) -> BoxFuture<'_, Result<CommandCellLaunchReceipt, CommandCellError>> {
        Box::pin(async move {
            let owner = request.owner.clone();
            let name = request.command.chars().take(80).collect::<String>();
            let command_hash = format!("{:x}", Sha256::digest(request.command.as_bytes()));
            let Some(run_id) = owner.run_id.clone() else {
                return self.launch_chat(request, name, command_hash).await;
            };
            let store = self
                .service
                .store_for_workspace(self.execution_scope.workspace_id())
                .ok_or_else(|| CommandCellError::Validation {
                    message: "run-owned cell requires the scoped TaskRuntimeStore".to_string(),
                })?;
            let lookup_run_id = run_id.clone();
            let run = super::executor::TaskRuntimeBlockingAdapter::new(store.clone())
                .run_store("load command-cell TaskRun", move |store| {
                    store.get_run(&lookup_run_id)
                })
                .await
                .map_err(|error| CommandCellError::Runtime {
                    message: error.to_string(),
                })?
                .ok_or_else(|| CommandCellError::Validation {
                    message: format!("run '{run_id}' does not exist in the scoped store"),
                })?;
            if run.workspace_id != self.execution_scope.workspace_id() {
                return Err(CommandCellError::Validation {
                    message: format!(
                        "run workspace '{}' does not match scoped workspace '{}'",
                        run.workspace_id,
                        self.execution_scope.workspace_id()
                    ),
                });
            }
            if owner.conversation_id.as_deref() != Some(run.conversation_id.as_str()) {
                return Err(CommandCellError::Validation {
                    message: "cell conversation does not match its TaskRun".to_string(),
                });
            }

            let operation_adapter = super::executor::TaskRuntimeBlockingAdapter::new(store.clone());
            let operation_reservation = operation_adapter
                .reserve_settlement("observe command-cell terminal")
                .map_err(|error| CommandCellError::Runtime {
                    message: error.to_string(),
                })?;
            let reservation = self.service.inner.prepare_launch(request).await?;
            let receipt = reservation.receipt().clone();
            let cell_id = receipt.cell_id.clone();
            let observation = self.service.inner.observe(&cell_id)?;
            let scope = RunCellScope {
                workspace_id: run.workspace_id.clone(),
                run_id: run_id.clone(),
            };
            let start_run_id = run_id.clone();
            let start_cell_id = cell_id.clone();
            let start_name = name.clone();
            let start_command_hash = command_hash.clone();
            let start_turn_id = owner.turn_id.clone();
            let start_execution_id = owner.execution_id.clone();
            let start_call_id = owner.call_id.clone();
            let start_commit = super::executor::TaskRuntimeBlockingAdapter::new(store.clone())
                .run_store("record command-cell start", move |store| {
                    store.record_background_cell_started(
                        &start_run_id,
                        &start_cell_id,
                        &start_name,
                        &start_command_hash,
                        start_turn_id.as_deref(),
                        start_execution_id.as_deref(),
                        start_call_id.as_deref(),
                    )
                })
                .await;
            match start_commit {
                Ok(super::store::ProjectionCommitReceipt::Durable { .. }) => {
                    self.service.track(&scope, &cell_id, receipt.deadline);
                }
                Ok(super::store::ProjectionCommitReceipt::CommittedProjectionDegraded {
                    seq,
                    detail,
                }) => {
                    self.service.track(&scope, &cell_id, receipt.deadline);
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("Started event {seq} projection degraded: {detail}"),
                    );
                    self.spawn_observer(
                        observation,
                        store,
                        scope,
                        cell_id,
                        name,
                        owner.call_id,
                        None,
                        operation_reservation,
                    )?;
                    return Err(CommandCellError::Runtime {
                        message: format!(
                            "cell start event {seq} committed but projection degraded: {detail}"
                        ),
                    });
                }
                Err(error) => {
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("Started persistence failed: {error}"),
                    );
                    return Err(CommandCellError::Runtime {
                        message: format!("cell start event could not be persisted: {error}"),
                    });
                }
            }

            let deadline = (receipt.deadline - chrono::Utc::now())
                .to_std()
                .unwrap_or_default();
            let shell = self.service.governor.shell_semaphore().acquire_owned();
            let shell_permit = tokio::select! {
                _ = self.service.shutdown.cancelled() => Err(CommandCellError::Shutdown),
                result = tokio::time::timeout(deadline, shell) => result
                    .map_err(|_| CommandCellError::CapacityDeadline)
                    .and_then(|permit| permit.map_err(|_| CommandCellError::Shutdown)),
            };
            let shell_permit = match shell_permit {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("process shell admission failed: {error}"),
                    );
                    self.spawn_observer(
                        observation,
                        store,
                        scope,
                        cell_id,
                        name,
                        owner.call_id,
                        None,
                        operation_reservation,
                    )?;
                    return Err(error);
                }
            };
            let start_result = self.service.inner.start_prepared(reservation).await;
            if let Err(error) = start_result {
                self.spawn_observer(
                    observation,
                    store,
                    scope,
                    cell_id,
                    name,
                    owner.call_id,
                    Some(shell_permit),
                    operation_reservation,
                )?;
                return Err(error);
            }
            self.spawn_observer(
                observation,
                store,
                scope,
                cell_id,
                name,
                owner.call_id,
                Some(shell_permit),
                operation_reservation,
            )?;

            Ok(receipt)
        })
    }

    fn wait(
        &self,
        cell_id: &str,
        cursor: u64,
        yield_ms: u64,
    ) -> BoxFuture<'_, Result<CommandCellDelta, CommandCellError>> {
        self.service.inner.wait(cell_id, cursor, yield_ms)
    }

    fn observe(&self, cell_id: &str) -> Result<CommandCellObservationLease, CommandCellError> {
        self.service.inner.observe(cell_id)
    }

    fn stop(&self, cell_id: &str) -> bool {
        self.service.inner.stop(cell_id)
    }

    fn list(&self) -> BoxFuture<'_, Vec<CommandCellSnapshot>> {
        self.service.inner.list()
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), CommandCellError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn observe_awaiter_cell_truth(
    registry: Arc<dyn CommandCellRegistry>,
    mut cell: BackgroundCellState,
    wait_for_terminal: bool,
    shutdown: &CancellationToken,
) -> Result<Option<BackgroundCellState>, String> {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    let mut shutdown_seen = false;
    loop {
        let yield_ms = if shutdown_seen {
            100
        } else if wait_for_terminal {
            OBSERVER_YIELD_MS
        } else {
            0
        };
        let delta = if shutdown_seen {
            registry
                .wait(&cell.cell_id, cursor, yield_ms)
                .await
                .map_err(|error| error.to_string())?
        } else {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    shutdown_seen = true;
                    continue;
                }
                delta = registry.wait(&cell.cell_id, cursor, yield_ms) => {
                    delta.map_err(|error| error.to_string())?
                }
            }
        };
        push_tail(&mut excerpt, &delta.new_output, OUTPUT_EXCERPT_CHARS);
        cursor = delta.next_cursor;
        if !delta.snapshot.phase.is_terminal() {
            if wait_for_terminal {
                continue;
            }
            return Ok(None);
        }
        if cursor < delta.snapshot.total_output_bytes {
            continue;
        }
        cell.phase = project_phase(delta.snapshot.phase);
        cell.terminal_cause = delta.snapshot.terminal_cause.map(project_terminal_cause);
        cell.terminal_message = delta.snapshot.terminal_message;
        cell.exit_code = delta.snapshot.exit_code;
        cell.artifact_status = project_artifact_status(&delta.snapshot.artifact_status);
        cell.artifact_message = delta.snapshot.artifact_message;
        cell.total_output_bytes = delta.snapshot.total_output_bytes;
        cell.output_truncated = delta.snapshot.output_truncated;
        cell.output_excerpt = (!excerpt.is_empty()).then_some(excerpt);
        cell.artifact_path = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string());
        cell.artifact_sha256 = delta
            .snapshot
            .output_artifact
            .map(|artifact| artifact.sha256);
        cell.finished_at = Some(chrono::Utc::now());
        return Ok(Some(cell));
    }
}

fn awaiter_summary(
    result: echo_agent::error::Result<echo_agent::agent::subagent::SubagentResult>,
) -> (AwaiterSummaryStatus, Option<String>) {
    match result {
        Ok(result) => {
            let status = match result.outcome.status {
                echo_agent::agent::subagent::SubagentStatus::Completed => {
                    AwaiterSummaryStatus::Completed
                }
                echo_agent::agent::subagent::SubagentStatus::Failed => AwaiterSummaryStatus::Failed,
                echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                    AwaiterSummaryStatus::Cancelled
                }
                echo_agent::agent::subagent::SubagentStatus::TimedOut => {
                    AwaiterSummaryStatus::TimedOut
                }
            };
            let summary = (!result.outcome.summary.trim().is_empty())
                .then(|| {
                    result
                        .outcome
                        .summary
                        .chars()
                        .take(1_200)
                        .collect::<String>()
                })
                .or_else(|| {
                    (!result.output.trim().is_empty())
                        .then(|| result.output.chars().take(1_200).collect::<String>())
                });
            (status, summary)
        }
        Err(error) => {
            let status = match echo_agent::agent::subagent::subagent_status_from_error(&error) {
                echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                    AwaiterSummaryStatus::Cancelled
                }
                echo_agent::agent::subagent::SubagentStatus::TimedOut => {
                    AwaiterSummaryStatus::TimedOut
                }
                echo_agent::agent::subagent::SubagentStatus::Completed => {
                    AwaiterSummaryStatus::Completed
                }
                echo_agent::agent::subagent::SubagentStatus::Failed => AwaiterSummaryStatus::Failed,
            };
            (status, Some(error.to_string().chars().take(500).collect()))
        }
    }
}

fn render_awaiter_handoff(result: &AwaiterResult) -> String {
    let encoded = serde_json::to_string(result).unwrap_or_else(|error| {
        format!(
            "{{\"execution_id\":\"{}\",\"serialization_error\":\"{}\"}}",
            result.receipt.execution_id,
            error.to_string().chars().take(200).collect::<String>()
        )
    });
    format!(
        "[awaiter_result]\n{encoded}\n[/awaiter_result]\nUse the runtime cell fields as terminal truth. The Awaiter summary is diagnostic only."
    )
}

async fn observe_terminal_cell(
    registry: Arc<BackgroundCommandManager>,
    store: Arc<TaskRuntimeStore>,
    run_id: String,
    cell_id: String,
    name: String,
    call_id: Option<String>,
    service: Arc<CommandCellRuntimeService>,
) -> bool {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    loop {
        let delta = match registry.wait(&cell_id, cursor, OBSERVER_YIELD_MS).await {
            Ok(delta) => delta,
            Err(error) => {
                let error_message = error.to_string();
                let persisted = persist_terminal_with_retry(
                    &store,
                    &run_id,
                    &cell_id,
                    &name,
                    BackgroundCellPhase::Failed,
                    Some(BackgroundCellTerminalCause::ObserverFailed),
                    Some(&error_message),
                    None,
                    BackgroundCellArtifactStatus::NotRequested,
                    None,
                    0,
                    false,
                    None,
                    None,
                    None,
                    call_id.as_deref(),
                    &service,
                )
                .await;
                return persisted;
            }
        };
        push_tail(&mut excerpt, &delta.new_output, OUTPUT_EXCERPT_CHARS);
        cursor = delta.next_cursor;
        if !delta.snapshot.phase.is_terminal() || cursor < delta.snapshot.total_output_bytes {
            continue;
        }
        let artifact_path = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string());
        let artifact_sha256 = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.sha256.clone());
        let persisted = persist_terminal_with_retry(
            &store,
            &run_id,
            &cell_id,
            &name,
            project_phase(delta.snapshot.phase),
            delta.snapshot.terminal_cause.map(project_terminal_cause),
            delta.snapshot.terminal_message.as_deref(),
            delta.snapshot.exit_code,
            project_artifact_status(&delta.snapshot.artifact_status),
            delta.snapshot.artifact_message.as_deref(),
            delta.snapshot.total_output_bytes,
            delta.snapshot.output_truncated,
            (!excerpt.is_empty()).then_some(excerpt.as_str()),
            artifact_path.as_deref(),
            artifact_sha256.as_deref(),
            call_id.as_deref(),
            &service,
        )
        .await;
        return persisted;
    }
}

async fn observe_chat_terminal_cell(
    registry: Arc<BackgroundCommandManager>,
    service: Arc<CommandCellRuntimeService>,
    scope: ChatCellScope,
    mut cell: BackgroundCellState,
) -> bool {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    loop {
        let delta = match registry
            .wait(&cell.cell_id, cursor, OBSERVER_YIELD_MS)
            .await
        {
            Ok(delta) => delta,
            Err(error) => {
                cell.phase = BackgroundCellPhase::Failed;
                cell.terminal_cause = Some(BackgroundCellTerminalCause::ObserverFailed);
                cell.terminal_message = Some(error.to_string());
                cell.finished_at = Some(chrono::Utc::now());
                return persist_chat_terminal_with_retry(&service, &scope, &cell).await;
            }
        };
        push_tail(&mut excerpt, &delta.new_output, OUTPUT_EXCERPT_CHARS);
        cursor = delta.next_cursor;
        if !delta.snapshot.phase.is_terminal() || cursor < delta.snapshot.total_output_bytes {
            continue;
        }
        cell.phase = project_phase(delta.snapshot.phase);
        cell.terminal_cause = delta.snapshot.terminal_cause.map(project_terminal_cause);
        cell.terminal_message = delta.snapshot.terminal_message;
        cell.exit_code = delta.snapshot.exit_code;
        cell.artifact_status = project_artifact_status(&delta.snapshot.artifact_status);
        cell.artifact_message = delta.snapshot.artifact_message;
        cell.total_output_bytes = delta.snapshot.total_output_bytes;
        cell.output_truncated = delta.snapshot.output_truncated;
        cell.output_excerpt = (!excerpt.is_empty()).then_some(excerpt);
        cell.artifact_path = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string());
        cell.artifact_sha256 = delta
            .snapshot
            .output_artifact
            .map(|artifact| artifact.sha256);
        cell.finished_at = Some(chrono::Utc::now());
        return persist_chat_terminal_with_retry(&service, &scope, &cell).await;
    }
}

async fn persist_chat_terminal_with_retry(
    service: &CommandCellRuntimeService,
    scope: &ChatCellScope,
    cell: &BackgroundCellState,
) -> bool {
    let mut delay = Duration::from_millis(50);
    let mut attempt = 0_u64;
    loop {
        attempt = attempt.saturating_add(1);
        match service.append_chat_cell_fact(scope, cell, true).await {
            Ok(()) => {
                service.clear_projection_degraded(&cell.cell_id);
                return true;
            }
            Err(error) => {
                service.mark_projection_degraded(&cell.cell_id, error.clone());
                tracing::warn!(
                    workspace_id = scope.workspace_id,
                    conversation_id = scope.conversation_id,
                    root_turn_id = scope.root_turn_id,
                    cell_id = cell.cell_id,
                    attempt,
                    %error,
                    "retrying ordinary-chat command-cell terminal persistence"
                );
                if attempt >= MAX_PROJECTION_REPAIR_ATTEMPTS {
                    return false;
                }
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(Duration::from_secs(1));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_terminal_with_retry(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    cell_id: &str,
    name: &str,
    phase: BackgroundCellPhase,
    terminal_cause: Option<BackgroundCellTerminalCause>,
    terminal_message: Option<&str>,
    exit_code: Option<i32>,
    artifact_status: BackgroundCellArtifactStatus,
    artifact_message: Option<&str>,
    total_output_bytes: u64,
    output_truncated: bool,
    output_excerpt: Option<&str>,
    artifact_path: Option<&str>,
    artifact_sha256: Option<&str>,
    call_id: Option<&str>,
    service: &CommandCellRuntimeService,
) -> bool {
    let run_id = run_id.to_string();
    let cell_id = cell_id.to_string();
    let name = name.to_string();
    let terminal_message = terminal_message.map(str::to_string);
    let artifact_message = artifact_message.map(str::to_string);
    let output_excerpt = output_excerpt.map(str::to_string);
    let artifact_path = artifact_path.map(str::to_string);
    let artifact_sha256 = artifact_sha256.map(str::to_string);
    let call_id = call_id.map(str::to_string);
    let blocking = super::executor::TaskRuntimeBlockingAdapter::new(store.clone());
    let mut delay = Duration::from_millis(50);
    let mut attempt = 0_u64;
    loop {
        attempt = attempt.saturating_add(1);
        let operation_run_id = run_id.clone();
        let operation_cell_id = cell_id.clone();
        let operation_name = name.clone();
        let operation_terminal_message = terminal_message.clone();
        let operation_artifact_message = artifact_message.clone();
        let operation_output_excerpt = output_excerpt.clone();
        let operation_artifact_path = artifact_path.clone();
        let operation_artifact_sha256 = artifact_sha256.clone();
        let operation_call_id = call_id.clone();
        match blocking
            .run_store("record command-cell terminal", move |store| {
                store.record_background_cell_finished(
                    &operation_run_id,
                    &operation_cell_id,
                    &operation_name,
                    phase,
                    terminal_cause,
                    operation_terminal_message.as_deref(),
                    exit_code,
                    artifact_status,
                    operation_artifact_message.as_deref(),
                    total_output_bytes,
                    output_truncated,
                    operation_output_excerpt.as_deref(),
                    operation_artifact_path.as_deref(),
                    operation_artifact_sha256.as_deref(),
                    operation_call_id.as_deref(),
                )?;
                super::continuation::wake_after_cell_terminal(&store, &operation_run_id);
                Ok(())
            })
            .await
        {
            Ok(()) => {
                service.clear_projection_degraded(&cell_id);
                return true;
            }
            Err(error) => {
                service.mark_projection_degraded(&cell_id, error.to_string());
                tracing::warn!(
                    run_id,
                    cell_id,
                    attempt,
                    %error,
                    "retrying terminal command-cell event persistence"
                );
                if attempt >= MAX_PROJECTION_REPAIR_ATTEMPTS {
                    return false;
                }
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(Duration::from_secs(1));
            }
        }
    }
}

fn project_phase(phase: echo_agent::tools::cell::CommandCellPhase) -> BackgroundCellPhase {
    use echo_agent::tools::cell::CommandCellPhase;
    match phase {
        CommandCellPhase::Prepared => BackgroundCellPhase::Prepared,
        CommandCellPhase::Queued => BackgroundCellPhase::Queued,
        CommandCellPhase::Running => BackgroundCellPhase::Running,
        CommandCellPhase::Succeeded => BackgroundCellPhase::Succeeded,
        CommandCellPhase::Failed => BackgroundCellPhase::Failed,
        CommandCellPhase::Cancelled => BackgroundCellPhase::Cancelled,
        CommandCellPhase::LaunchFailed => BackgroundCellPhase::LaunchFailed,
    }
}

fn project_terminal_cause(
    cause: echo_agent::tools::cell::CommandCellTerminalCause,
) -> BackgroundCellTerminalCause {
    use echo_agent::tools::cell::CommandCellTerminalCause;
    match cause {
        CommandCellTerminalCause::Exited => BackgroundCellTerminalCause::Exited,
        CommandCellTerminalCause::TimedOut => BackgroundCellTerminalCause::TimedOut,
        CommandCellTerminalCause::Cancelled => BackgroundCellTerminalCause::Cancelled,
        CommandCellTerminalCause::LaunchFailed => BackgroundCellTerminalCause::LaunchFailed,
        CommandCellTerminalCause::WaitFailed => BackgroundCellTerminalCause::WaitFailed,
        CommandCellTerminalCause::OutputDrainFailed => {
            BackgroundCellTerminalCause::OutputDrainFailed
        }
    }
}

fn project_artifact_status(
    status: &echo_agent::tools::cell::CommandCellArtifactStatus,
) -> BackgroundCellArtifactStatus {
    use echo_agent::tools::cell::CommandCellArtifactStatus;
    match status {
        CommandCellArtifactStatus::NotRequested => BackgroundCellArtifactStatus::NotRequested,
        CommandCellArtifactStatus::Writing => BackgroundCellArtifactStatus::Writing,
        CommandCellArtifactStatus::BelowThreshold => BackgroundCellArtifactStatus::BelowThreshold,
        CommandCellArtifactStatus::Available => BackgroundCellArtifactStatus::Available,
        CommandCellArtifactStatus::Failed => BackgroundCellArtifactStatus::Failed,
    }
}

fn find_chat_cell_fact<'a>(
    events: &'a [crate::chat_event_log::ChatEventEnvelope],
    cell_id: &str,
    settled: bool,
) -> Option<&'a BackgroundCellState> {
    events.iter().find_map(|event| match &event.payload {
        crate::chat_driver::ChatDriverEvent::CommandCellStarted { cell }
            if !settled && cell.cell_id == cell_id =>
        {
            Some(cell.as_ref())
        }
        crate::chat_driver::ChatDriverEvent::CommandCellSettled { cell }
            if settled && cell.cell_id == cell_id =>
        {
            Some(cell.as_ref())
        }
        _ => None,
    })
}

fn push_tail(target: &mut String, chunk: &str, max_chars: usize) {
    target.push_str(chunk);
    if target.chars().count() <= max_chars {
        return;
    }
    let mut tail = target.chars().rev().take(max_chars).collect::<Vec<_>>();
    tail.reverse();
    *target = tail.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::cell::CommandCellOwner;

    use crate::tasks::task_runtime::compact_context::build_runtime_recovery_capsule;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, RuntimeEventKind, TaskRunStatus,
    };

    fn test_service(root: &std::path::Path) -> Result<Arc<CommandCellRuntimeService>, String> {
        test_service_with_retention(root, crate::chat_event_log::ChatEventRetention::default())
    }

    fn test_service_with_retention(
        root: &std::path::Path,
        retention: crate::chat_event_log::ChatEventRetention,
    ) -> Result<Arc<CommandCellRuntimeService>, String> {
        let chat_events =
            crate::chat_event_log::ChatEventLog::open(root.join("chat-events"), retention)
                .map_err(|error| error.to_string())?;
        let product_data_flow = crate::product_data_io::ProductDataIoService::new()
            .begin_owned_flow("command-cell test projection")
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(CommandCellRuntimeService {
            inner: Arc::new(BackgroundCommandManager::default()),
            run_cells: RwLock::new(HashMap::new()),
            chat_cells: RwLock::new(HashMap::new()),
            cell_deadlines: RwLock::new(HashMap::new()),
            stores_by_workspace: RwLock::new(HashMap::new()),
            projection_degraded: RwLock::new(HashMap::new()),
            governor: super::super::executor::process_execution_governor(),
            observers: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            chat_events: Arc::new(chat_events),
            product_data_flow,
            awaiters: Mutex::new(AwaiterRuntimeState::default()),
            awaiter_agents: RwLock::new(HashMap::new()),
            foreground_turns: RwLock::new(None),
            framework_shutdown: Mutex::new(None),
            awaiter_recovery: tokio::sync::Mutex::new(AwaiterRecoveryState::default()),
            awaiter_recovery_barrier: Mutex::new(None),
            awaiter_recovery_failures: std::sync::atomic::AtomicUsize::new(0),
            awaiter_started_barrier: Mutex::new(None),
        }))
    }

    fn terminal_cell(cell_id: &str) -> BackgroundCellState {
        BackgroundCellState {
            cell_id: cell_id.to_string(),
            name: "test cell".to_string(),
            command_hash: "sha256:test".to_string(),
            turn_id: Some("turn".to_string()),
            execution_id: Some("cell-execution".to_string()),
            call_id: Some("call".to_string()),
            phase: BackgroundCellPhase::Succeeded,
            terminal_cause: Some(BackgroundCellTerminalCause::Exited),
            terminal_message: None,
            exit_code: Some(0),
            artifact_status: BackgroundCellArtifactStatus::BelowThreshold,
            artifact_message: None,
            total_output_bytes: 2,
            output_truncated: false,
            output_excerpt: Some("ok".to_string()),
            artifact_path: None,
            artifact_sha256: None,
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
        }
    }

    fn awaiter_result(execution_id: &str, cell_id: &str) -> AwaiterResult {
        AwaiterResult {
            receipt: AwaiterWatchReceipt {
                execution_id: execution_id.to_string(),
                control_task_id: format!("awaiter:{cell_id}:1"),
                attempt: 1,
                watch_generation: 1,
                cell_id: cell_id.to_string(),
                workspace_id: "global".to_string(),
                conversation_id: "conversation".to_string(),
                run_id: None,
                root_turn_id: "root-message".to_string(),
                state: AwaiterWatchState::Settled,
                started_at: chrono::Utc::now(),
                settled_at: Some(chrono::Utc::now()),
            },
            cell: terminal_cell(cell_id),
            awaiter_status: AwaiterSummaryStatus::Completed,
            awaiter_summary: Some("prose says failed but runtime truth succeeded".to_string()),
        }
    }

    fn recovery_barrier() -> (
        AwaiterRecoveryTestBarrier,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            AwaiterRecoveryTestBarrier {
                entered: entered_tx,
                release: release_rx,
            },
            entered_rx,
            release_tx,
        )
    }

    fn started_barrier() -> (
        AwaiterStartedTestBarrier,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            AwaiterStartedTestBarrier {
                entered: entered_tx,
                release: release_rx,
            },
            entered_rx,
            release_tx,
        )
    }

    async fn accepted_awaiter_fixture(
        root: &std::path::Path,
    ) -> Result<
        (
            Arc<CommandCellRuntimeService>,
            AwaiterWatchReceipt,
            Arc<TaskRuntimeStore>,
        ),
        String,
    > {
        let service = test_service(root)?;
        service.ensure_awaiter_recovery().await?;
        let scope = crate::workspace::WorkspaceExecutionScope::global(root);
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(root.join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(scope.clone(), Some(store.clone()));
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 30".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        let mut parent = echo_agent::agent::ReactAgentBuilder::new()
            .model("awaiter-shutdown-fixture")
            .build()
            .map_err(|error| error.to_string())?;
        parent.register_subagent_with_definition(
            echo_agent::agent::subagent::SubagentBuilder::new("awaiter")
                .description("shutdown fixture")
                .background()
                .build(),
            Box::new(echo_agent::testing::MockAgent::new("awaiter").with_response("joined")),
        );
        let receipt = service
            .watch_cell(
                registry,
                parent.subagent_executor().clone(),
                &scope,
                &ToolContext {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    turn_id: Some("turn".to_string()),
                    ..ToolContext::default()
                },
                &cell_id,
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok((service, receipt, store))
    }

    #[tokio::test]
    async fn boot_recovery_settles_only_the_preexisting_started_awaiter() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let result = awaiter_result("boot-orphan", "boot-cell");
        let acknowledgement = AwaiterResultAcknowledgement {
            execution_id: result.receipt.execution_id.clone(),
            attempt: result.receipt.attempt,
            watch_generation: result.receipt.watch_generation,
            cell_id: result.receipt.cell_id.clone(),
            acknowledged_turn_id: "orphan-turn".to_string(),
            outcome: AwaiterDeliveryOutcome::OutcomeUnknown,
        };
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
                    result: Box::new(result),
                },
            )
            .map_err(|error| error.to_string())?;
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                    acknowledgement: acknowledgement.clone(),
                },
            )
            .map_err(|error| error.to_string())?;

        service.ensure_awaiter_recovery().await?;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement: ack }
                if ack == &acknowledgement
        )));
        Ok(())
    }

    #[tokio::test]
    async fn publish_waits_for_boot_recovery_readiness() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        service.ensure_awaiter_recovery().await?;
        let (barrier, entered, release) = recovery_barrier();
        service.reset_awaiter_recovery_for_test(Some(barrier)).await;
        let publisher = service.clone();
        let publishing = tokio::spawn(async move {
            publisher
                .publish_awaiter_result(awaiter_result("blocked-publish", "blocked-cell"))
                .await
        });
        entered
            .await
            .map_err(|_| "Awaiter recovery did not reach its barrier".to_string())?;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.is_empty());
        release
            .send(())
            .map_err(|_| "Awaiter recovery release was dropped".to_string())?;
        publishing.await.map_err(|error| error.to_string())??;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultReady { .. }
        )));
        Ok(())
    }

    #[tokio::test]
    async fn per_turn_projection_never_acknowledges_a_live_started_delivery() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        service.ensure_awaiter_recovery().await?;
        let result = awaiter_result("live-started", "live-cell");
        let mut acknowledgement = AwaiterResultAcknowledgement {
            execution_id: result.receipt.execution_id.clone(),
            attempt: result.receipt.attempt,
            watch_generation: result.receipt.watch_generation,
            cell_id: result.receipt.cell_id.clone(),
            acknowledged_turn_id: "live-turn".to_string(),
            outcome: AwaiterDeliveryOutcome::OutcomeUnknown,
        };
        for event in [
            crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
                result: Box::new(result),
            },
            crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                acknowledgement: acknowledgement.clone(),
            },
        ] {
            service
                .chat_events
                .append("global", Some("conversation"), "root-message", event)
                .map_err(|error| error.to_string())?;
        }
        assert!(
            service
                .project_pending_awaiter_results("global", "conversation", "another-turn")
                .await?
                .is_none()
        );
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(!replay.events.iter().any(|event| matches!(
            event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { .. }
        )));

        acknowledgement.outcome = AwaiterDeliveryOutcome::Drained;
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
                    acknowledgement: acknowledgement.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.payload,
                    crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement: ack }
                        if ack == &acknowledgement
                ))
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_after_delivery_started_is_acknowledged_by_the_publish_owner()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        service.ensure_awaiter_recovery().await?;
        let turns = crate::foreground_turn::ForegroundTurnControl::default();
        let lease = turns
            .begin(
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                "conversation",
                "root-message",
            )
            .map_err(|error| error.to_string())?;
        service.bind_foreground_turns(turns);
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("awaiter-shutdown-test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        service.bind_agent("global", "conversation", &agent);
        let (barrier, entered, release) = started_barrier();
        *service
            .awaiter_started_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(barrier);
        let publisher = service.clone();
        let publishing = tokio::spawn(async move {
            publisher
                .publish_awaiter_result(awaiter_result("shutdown-started", "shutdown-cell"))
                .await
        });
        entered
            .await
            .map_err(|_| "publish owner did not persist DeliveryStarted".to_string())?;
        service.begin_shutdown()?;
        release
            .send(())
            .map_err(|_| "publish owner Started barrier was dropped".to_string())?;
        publishing.await.map_err(|error| error.to_string())??;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement }
                if acknowledgement.execution_id == "shutdown-started"
                    && acknowledgement.outcome == AwaiterDeliveryOutcome::OutcomeUnknown
        )));
        lease.settle(crate::chat_driver::TurnOutcome::Cancelled);
        service.shutdown().await
    }

    #[tokio::test]
    async fn accepted_awaiter_drains_framework_terminal_and_persists_ready_during_shutdown()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (service, receipt, _store) = accepted_awaiter_fixture(temp.path()).await?;
        service.begin_shutdown()?;
        service.shutdown().await?;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultReady { result }
                if result.receipt.execution_id == receipt.execution_id
                    && result.cell.phase.is_terminal()
                    && result.cell.terminal_cause.is_some()
        )));
        Ok(())
    }

    #[tokio::test]
    async fn accepted_awaiter_publication_failure_becomes_shutdown_debt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (service, receipt, _store) = accepted_awaiter_fixture(temp.path()).await?;
        service.reset_awaiter_recovery_for_test(None).await;
        service.begin_shutdown()?;
        let error = service
            .shutdown()
            .await
            .err()
            .ok_or_else(|| "Awaiter publication debt was silently accepted".to_string())?;
        assert!(error.contains("command-cell projection debt"));
        assert!(
            service
                .projection_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.cell_id == receipt.execution_id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_failure_retries_and_shutdown_joins_the_owned_recovery() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        service.ensure_awaiter_recovery().await?;
        service.reset_awaiter_recovery_for_test(None).await;
        service.fail_next_awaiter_recovery_for_test();
        assert!(service.ensure_awaiter_recovery().await.is_err());
        service.ensure_awaiter_recovery().await?;

        let (barrier, entered, release) = recovery_barrier();
        service.reset_awaiter_recovery_for_test(Some(barrier)).await;
        let recovering_service = service.clone();
        let recovering =
            tokio::spawn(async move { recovering_service.ensure_awaiter_recovery().await });
        entered
            .await
            .map_err(|_| "shutdown recovery did not reach its barrier".to_string())?;
        service.begin_shutdown()?;
        let shutdown_service = service.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release
            .send(())
            .map_err(|_| "shutdown recovery release was dropped".to_string())?;
        recovering.await.map_err(|error| error.to_string())??;
        shutdown.await.map_err(|error| error.to_string())?
    }

    #[tokio::test]
    async fn awaiter_ready_is_idempotent_and_acknowledgement_clears_pending() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let result = awaiter_result("awaiter-execution", "cell-ready");
        let event = || crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
            result: Box::new(result.clone()),
        };
        let first = service
            .chat_events
            .append("global", Some("conversation"), "root-message", event())
            .map_err(|error| error.to_string())?;
        let duplicate = service
            .chat_events
            .append("global", Some("conversation"), "root-message", event())
            .map_err(|error| error.to_string())?;
        assert_eq!(first.event_id, duplicate.event_id);
        let pending = service
            .chat_events
            .pending_awaiter_results("global", "conversation", "root-message")
            .map_err(|error| error.to_string())?;
        assert_eq!(pending, vec![result.clone()]);
        assert_eq!(
            pending.first().map(|result| result.cell.phase),
            Some(BackgroundCellPhase::Succeeded)
        );

        let acknowledgement = AwaiterResultAcknowledgement {
            execution_id: result.receipt.execution_id.clone(),
            attempt: result.receipt.attempt,
            watch_generation: result.receipt.watch_generation,
            cell_id: result.receipt.cell_id.clone(),
            acknowledged_turn_id: "next-turn".to_string(),
            outcome: AwaiterDeliveryOutcome::Drained,
        };
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                    acknowledgement: acknowledgement.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement },
            )
            .map_err(|error| error.to_string())?;
        let pending = service
            .chat_events
            .pending_awaiter_results("global", "conversation", "root-message")
            .map_err(|error| error.to_string())?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[test]
    fn dedicated_surface_projection_preserves_typed_runtime_truth() {
        let result = awaiter_result("awaiter-surface", "cell-surface");
        let ready = crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
            result: Box::new(result.clone()),
        };
        assert_eq!(
            project_awaiter_surface_event(&ready),
            Some(AwaiterSurfaceProjection::Ready {
                execution_id: "awaiter-surface".to_string(),
                cell_id: "cell-surface".to_string(),
                phase: BackgroundCellPhase::Succeeded,
                terminal_cause: Some(BackgroundCellTerminalCause::Exited),
                exit_code: Some(0),
                artifact_status: BackgroundCellArtifactStatus::BelowThreshold,
            })
        );
        let acknowledgement = crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
            acknowledgement: AwaiterResultAcknowledgement {
                execution_id: result.receipt.execution_id,
                attempt: 1,
                watch_generation: 1,
                cell_id: result.cell.cell_id,
                acknowledged_turn_id: "safe-turn".to_string(),
                outcome: AwaiterDeliveryOutcome::Drained,
            },
        };
        assert_eq!(
            project_awaiter_surface_event(&acknowledgement),
            Some(AwaiterSurfaceProjection::Acknowledged {
                execution_id: "awaiter-surface".to_string(),
                cell_id: "cell-surface".to_string(),
                acknowledged_turn_id: "safe-turn".to_string(),
                outcome: AwaiterDeliveryOutcome::Drained,
            })
        );
    }

    #[test]
    fn unacknowledged_awaiter_ready_pins_its_retained_segment() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service_with_retention(
            temp.path(),
            crate::chat_event_log::ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 2,
                max_replay_events: 128,
            },
        )?;
        let result = awaiter_result("awaiter-pinned", "cell-pinned");
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
                    result: Box::new(result.clone()),
                },
            )
            .map_err(|error| error.to_string())?;
        for index in 0..12 {
            service
                .chat_events
                .append(
                    "global",
                    Some("conversation"),
                    "root-message",
                    crate::chat_driver::ChatDriverEvent::ExecutionPath {
                        observed_path: format!("chat-{index}"),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        let pending = service
            .chat_events
            .pending_awaiter_results("global", "conversation", "root-message")
            .map_err(|error| error.to_string())?;
        assert_eq!(pending, vec![result]);
        Ok(())
    }

    #[tokio::test]
    async fn next_turn_projection_delivers_and_acknowledges_pending_results() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let result = awaiter_result("awaiter-next-turn", "cell-next-turn");
        service
            .chat_events
            .append(
                "global",
                Some("conversation"),
                "root-message",
                crate::chat_driver::ChatDriverEvent::AwaiterResultReady {
                    result: Box::new(result.clone()),
                },
            )
            .map_err(|error| error.to_string())?;

        let projection = service
            .project_pending_awaiter_results("global", "conversation", "next-turn")
            .await?
            .ok_or_else(|| "pending Awaiter result was not projected".to_string())?;
        assert!(projection.contains("awaiter-next-turn"));
        assert!(projection.contains("\"phase\":\"succeeded\""));
        let pending = service
            .chat_events
            .pending_awaiter_results_for_conversation("global", "conversation")
            .map_err(|error| error.to_string())?;
        assert!(pending.is_empty());
        assert!(
            service
                .project_pending_awaiter_results("global", "conversation", "later-turn")
                .await?
                .is_none()
        );
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted { .. }
        )));
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            crate::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement }
                if acknowledgement.outcome == AwaiterDeliveryOutcome::OutcomeUnknown
        )));
        Ok(())
    }

    #[tokio::test]
    async fn exact_awaiter_interrupt_does_not_stop_its_cell() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::global(temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 30".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        let key = AwaiterWatchKey {
            workspace_id: "global".to_string(),
            conversation_id: "conversation".to_string(),
            run_id: None,
            root_turn_id: "root-message".to_string(),
            cell_id: cell_id.clone(),
        };
        let receipt = AwaiterWatchReceipt {
            execution_id: "awaiter-interrupt".to_string(),
            control_task_id: format!("awaiter:{cell_id}:1"),
            attempt: 1,
            watch_generation: 1,
            cell_id: cell_id.clone(),
            workspace_id: "global".to_string(),
            conversation_id: "conversation".to_string(),
            run_id: None,
            root_turn_id: "root-message".to_string(),
            state: AwaiterWatchState::Started,
            started_at: chrono::Utc::now(),
            settled_at: None,
        };
        let cancel = echo_agent::agent::CancellationToken::new();
        let executor = Arc::new(echo_agent::agent::subagent::SubagentExecutor::new(
            Arc::new(echo_agent::agent::subagent::SubagentRegistry::new()),
            echo_agent::agent::subagent::SubagentExecutorConfig::default(),
        ));
        service
            .awaiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .insert(
                key,
                ActiveAwaiterWatch {
                    receipt: receipt.clone(),
                    executor,
                    handle: None,
                    cancel: cancel.clone(),
                },
            );

        assert!(
            service
                .interrupt_awaiter(&receipt.execution_id, 2)
                .await
                .is_err()
        );
        assert!(!cancel.is_cancelled());
        service.interrupt_awaiter(&receipt.execution_id, 1).await?;
        assert!(cancel.is_cancelled());
        let cell = registry
            .wait(&cell_id, 0, 0)
            .await
            .map_err(|error| error.to_string())?;
        assert!(!cell.snapshot.phase.is_terminal());
        registry.stop(&cell_id);
        Ok(())
    }

    #[tokio::test]
    async fn direct_awaiter_dispatch_returns_receipt_and_retains_join_until_ready()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let scope = crate::workspace::WorkspaceExecutionScope::global(temp.path());
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(scope.clone(), Some(store.clone()));
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 1; printf owned-awaiter-result".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        let mut parent = echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .build()
            .map_err(|error| error.to_string())?;
        let definition = echo_agent::agent::subagent::SubagentBuilder::new("awaiter")
            .description("test Awaiter")
            .background()
            .build();
        parent.register_subagent_with_definition(
            definition,
            Box::new(
                echo_agent::testing::MockAgent::new("awaiter")
                    .with_response("diagnostic summary only"),
            ),
        );
        let executor = parent.subagent_executor().clone();
        let context = ToolContext {
            conversation_id: Some("conversation".to_string()),
            message_id: Some("root-message".to_string()),
            turn_id: Some("turn".to_string()),
            ..ToolContext::default()
        };
        let started = std::time::Instant::now();
        let receipt = service
            .watch_cell(
                registry.clone(),
                executor,
                &scope,
                &context,
                &cell_id,
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(receipt.state, AwaiterWatchState::Started);
        let duplicate = service
            .watch_cell(
                registry.clone(),
                parent.subagent_executor().clone(),
                &scope,
                &context,
                &cell_id,
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(duplicate.execution_id, receipt.execution_id);
        assert_eq!(duplicate.watch_generation, receipt.watch_generation);

        let pending = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pending = service
                    .chat_events
                    .pending_awaiter_results("global", "conversation", "root-message")
                    .map_err(|error| error.to_string())?;
                if let Some(result) = pending.into_iter().next() {
                    return Ok::<AwaiterResult, String>(result);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "owned Awaiter did not publish Ready".to_string())??;
        assert_eq!(pending.receipt.execution_id, receipt.execution_id);
        assert_eq!(pending.cell.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(pending.awaiter_status, AwaiterSummaryStatus::Completed);
        assert!(
            pending
                .awaiter_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("diagnostic summary only"))
        );
        assert!(
            service
                .awaiters
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn awaiter_provider_failure_preserves_cell_truth() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let scope = crate::workspace::WorkspaceExecutionScope::global(temp.path());
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(scope.clone(), Some(store.clone()));
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 0.2; printf cell-still-succeeded".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        let mut parent = echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .build()
            .map_err(|error| error.to_string())?;
        parent.register_subagent_with_definition(
            echo_agent::agent::subagent::SubagentBuilder::new("awaiter")
                .description("failing test Awaiter")
                .background()
                .build(),
            Box::new(
                echo_agent::testing::FailingMockAgent::new("awaiter", "provider unavailable")
                    .with_failure(echo_agent::testing::MockAgentFailure::Subagent),
            ),
        );
        let context = ToolContext {
            conversation_id: Some("conversation".to_string()),
            message_id: Some("root-message".to_string()),
            turn_id: Some("turn".to_string()),
            ..ToolContext::default()
        };
        let receipt = service
            .watch_cell(
                registry,
                parent.subagent_executor().clone(),
                &scope,
                &context,
                &cell_id,
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pending = service
                    .chat_events
                    .pending_awaiter_results("global", "conversation", "root-message")
                    .map_err(|error| error.to_string())?;
                if let Some(result) = pending.into_iter().next() {
                    return Ok::<AwaiterResult, String>(result);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "failed Awaiter did not publish its result".to_string())??;
        assert_eq!(result.receipt.execution_id, receipt.execution_id);
        assert_eq!(result.awaiter_status, AwaiterSummaryStatus::Failed);
        assert_eq!(result.cell.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(
            result.cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Exited)
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_runtime_store_fallback_records_one_start_and_one_finish() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cell-run",
                "workspace",
                "conversation",
                "message",
                DomainProfile::AiCoding,
                "run a background check",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cell-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 0.2; echo projected-cell-result".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("cell-run".to_string()),
                    turn_id: Some("turn-1".to_string()),
                    message_id: Some("message".to_string()),
                    execution_id: Some("execution-1".to_string()),
                    call_id: Some("call-1".to_string()),
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        if store.active_operation_count() == 0 {
            return Err(
                "command-cell observer did not retain TaskRuntime operation ownership".to_string(),
            );
        }
        store.begin_operation_shutdown()?;
        let rejected = super::super::executor::TaskRuntimeBlockingAdapter::new(store.clone())
            .run_owned("late command-cell operation", || Ok(()))
            .await;
        if !rejected.is_err_and(|error| error.to_string().contains("admission is closed")) {
            return Err("command-cell phase-one fixture accepted a late operation".to_string());
        }

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let cells = store.list_background_cells("cell-run").unwrap_or_default();
                if cells
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "cell projection did not reach terminal state".to_string())?;

        let events = store
            .list_events("cell-run", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellStarted)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellFinished)
                .count(),
            1
        );
        let capsule = build_runtime_recovery_capsule(&store, "cell-run")
            .ok_or_else(|| "cell result missing from recovery capsule".to_string())?;
        assert!(capsule.contains("projected-cell-result"));
        assert!(capsule.contains(&cell_id));
        store.shutdown_operations().await?;
        Ok(())
    }

    #[tokio::test]
    async fn explicit_run_cancel_stops_owned_cells_without_turn_token_coupling()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cancel-owned-cells",
                "workspace",
                "conversation",
                "message",
                DomainProfile::AiCoding,
                "cancel a background cell",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cancel-owned-cells", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 30".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("cancel-owned-cells".to_string()),
                    message_id: Some("message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        assert_eq!(service.stop_run("workspace", "cancel-owned-cells"), 1);
        let terminal = service
            .inner
            .wait(&cell_id, 0, 5_000)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            terminal.snapshot.phase,
            echo_agent::tools::cell::CommandCellPhase::Cancelled
        );
        Ok(())
    }

    #[tokio::test]
    async fn ordinary_chat_cell_uses_exact_conversation_and_root_message_journal()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::global(temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "printf ordinary-chat-result".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation-a".to_string()),
                    turn_id: Some("turn-a".to_string()),
                    message_id: Some("root-message-a".to_string()),
                    call_id: Some("call-a".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        let settled = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let replay = service
                    .chat_events
                    .replay("global", Some("conversation-a"), "root-message-a", 0)
                    .map_err(|error| error.to_string())?;
                if let Some(cell) = find_chat_cell_fact(&replay.events, &cell_id, true) {
                    return Ok::<BackgroundCellState, String>(cell.clone());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "ordinary Chat cell did not settle in its journal".to_string())??;
        assert_eq!(settled.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(
            settled.terminal_cause,
            Some(BackgroundCellTerminalCause::Exited)
        );
        assert_eq!(
            settled.artifact_status,
            BackgroundCellArtifactStatus::NotRequested
        );
        assert!(
            settled
                .output_excerpt
                .as_deref()
                .is_some_and(|output| output.contains("ordinary-chat-result"))
        );
        let wrong_conversation = service
            .chat_events
            .replay("global", Some("conversation-b"), "root-message-a", 0)
            .map_err(|error| error.to_string())?;
        assert!(wrong_conversation.events.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn phase_one_cancels_long_cell_before_operation_join() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::global(temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 30".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        store.begin_operation_shutdown()?;
        service.begin_shutdown()?;
        tokio::time::timeout(Duration::from_secs(5), store.shutdown_operations())
            .await
            .map_err(|_| "operation join waited for the uncancelled long command".to_string())??;
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .map_err(|_| "command-cell shutdown exceeded its total deadline".to_string())??;
        let replay = service
            .chat_events
            .replay("global", Some("conversation"), "root-message", 0)
            .map_err(|error| error.to_string())?;
        let terminal = find_chat_cell_fact(&replay.events, &cell_id, true).ok_or_else(|| {
            "phase-one cancellation did not persist terminal cell truth".to_string()
        })?;
        assert!(!terminal.is_active());
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_shutdown_callers_share_one_stable_framework_settlement()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let first = service.clone();
        let second = service.clone();
        let (first_begin, second_begin) = tokio::join!(
            tokio::spawn(async move { first.begin_shutdown() }),
            tokio::spawn(async move { second.begin_shutdown() }),
        );
        first_begin
            .map_err(|error| format!("first shutdown broadcast failed to join: {error}"))??;
        second_begin
            .map_err(|error| format!("second shutdown broadcast failed to join: {error}"))??;

        let first = service.clone();
        let second = service.clone();
        let (first_settlement, second_settlement) =
            tokio::join!(first.shutdown(), second.shutdown());
        assert_eq!(first_settlement, second_settlement);
        assert!(first_settlement.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_run_ids_in_two_workspaces_cannot_cross_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let store_a = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks-a"), "workspace-a")
                .map_err(|error| error.to_string())?,
        );
        let store_b = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks-b"), "workspace-b")
                .map_err(|error| error.to_string())?,
        );
        for (store, workspace, conversation) in [
            (&store_a, "workspace-a", "conversation-a"),
            (&store_b, "workspace-b", "conversation-b"),
        ] {
            store
                .create_run(
                    "duplicate-run",
                    workspace,
                    conversation,
                    "root-message",
                    DomainProfile::AiCoding,
                    "verify exact scope",
                    "task",
                    AttendedMode::Attended,
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run("duplicate-run", TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
        }
        let workspace_a = crate::workspace::WorkspaceId::from_name("workspace-a");
        let registry_a = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_a, temp.path()),
            Some(store_a.clone()),
        );
        let workspace_b = crate::workspace::WorkspaceId::from_name("workspace-b");
        let _registry_b = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_b, temp.path()),
            Some(store_b.clone()),
        );
        let cell_id = registry_a
            .launch(CommandCellRequest {
                command: "printf workspace-a-only".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation-a".to_string()),
                    run_id: Some("duplicate-run".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store_a
                    .list_background_cells("duplicate-run")
                    .unwrap_or_default()
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "workspace A cell did not settle".to_string())?;
        assert!(
            store_b
                .list_background_cells("duplicate-run")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn started_append_failure_executes_no_process_and_leaves_no_active_cell()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "start-failure",
                "workspace",
                "conversation",
                "root-message",
                DomainProfile::AiCoding,
                "prove no side effect before Started",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("start-failure", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_next_cell_started_for_test();
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let side_effect = temp.path().join("must-not-exist");
        let result = registry
            .launch(CommandCellRequest {
                command: format!("touch {}", side_effect.display()),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("start-failure".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await;
        assert!(matches!(result, Err(CommandCellError::Runtime { .. })));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!side_effect.exists());
        assert!(
            store
                .list_background_cells("start-failure")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn committed_start_with_degraded_projection_aborts_and_repairs_terminal()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "projection-failure",
                "workspace",
                "conversation",
                "root-message",
                DomainProfile::AiCoding,
                "repair committed start projection",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("projection-failure", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_next_cell_started_projection_for_test();
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let side_effect = temp.path().join("must-not-exist-after-degraded-projection");
        let result = registry
            .launch(CommandCellRequest {
                command: format!("touch {}", side_effect.display()),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("projection-failure".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await;
        assert!(matches!(result, Err(CommandCellError::Runtime { .. })));

        let cell = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let cells = store
                    .list_background_cells("projection-failure")
                    .map_err(|error| error.to_string())?;
                if let Some(cell) = cells.into_iter().find(|cell| !cell.is_active()) {
                    return Ok::<BackgroundCellState, String>(cell);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "committed start did not repair to terminal".to_string())??;
        assert!(!side_effect.exists());
        assert_eq!(cell.phase, BackgroundCellPhase::LaunchFailed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::LaunchFailed)
        );
        let events = store
            .list_events("projection-failure", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellStarted)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellFinished)
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_persistence_failure_retains_owner_until_retry_succeeds() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "terminal-repair",
                "workspace",
                "conversation",
                "root-message",
                DomainProfile::AiCoding,
                "repair terminal persistence",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("terminal-repair", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_cell_terminal_writes_for_test(5);
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "printf repaired".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("terminal-repair".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if service
                    .projection_diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.cell_id == cell_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "projection degradation was not exposed".to_string())?;
        assert_eq!(
            service
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .filter(|cells| cells.contains(&cell_id))
                .count(),
            1
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store
                    .list_background_cells("terminal-repair")
                    .unwrap_or_default()
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                    && service
                        .run_cells
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .values()
                        .all(|cells| !cells.contains(&cell_id))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "terminal repair did not recover after the old retry window".to_string())?;
        assert!(service.projection_diagnostics().is_empty());
        assert!(
            service
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .all(|cells| !cells.contains(&cell_id))
        );
        Ok(())
    }
}
