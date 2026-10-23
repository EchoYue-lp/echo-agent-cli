/// File-backed TaskRuntime store. One instance per process; cheap to clone
/// behind `Arc`. The event stream is authoritative; plan and execution files
/// are deterministic read projections.
pub struct TaskRuntimeStore {
    /// Per-task cancellation tokens (in-memory runtime state, not persisted).
    /// Key = `"{run_id}::{task_id}"`. `execute_task` registers a token when a
    /// task starts and removes it on completion; runtime control actions use
    /// the token to stop that Subagent promptly.
    task_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// Exact execution-to-framework routing only. Durable commands and their
    /// outcomes remain in events.jsonl; no message is stored in this map.
    pub(super) active_subagent_controls: std::sync::Mutex<
        std::collections::HashMap<String, super::subagent_control::ActiveSubagentControlTarget>,
    >,
    /// Active TaskRun driver tokens. Every entry point registers here so pause
    /// and cancel target the real executor instead of a surface-local map.
    run_cancel_tokens: std::sync::Mutex<
        std::collections::HashMap<String, Vec<(u64, echo_agent::agent::CancellationToken)>>,
    >,
    next_run_cancel_registration: std::sync::atomic::AtomicU64,
    /// Accepted TaskRun driver tasks. The store is the existing runtime owner,
    /// so dropping an individual surface waiter never drops the actual driver.
    run_driver_supervisor: std::sync::Mutex<RunDriverSupervisor>,
    lifecycle_retention:
        std::sync::RwLock<std::sync::Weak<crate::runtime::ApplicationLifecycleRetention>>,
    /// Wakes the store-owned shutdown settlement after the last pre-shutdown
    /// driver admission reservation either registers a driver or is released.
    run_driver_admission_idle: tokio::sync::Notify,
    /// Wakes the continuation coordinator after the exact current driver has
    /// released its run-scoped cancellation registration.
    run_driver_idle: tokio::sync::Notify,
    /// EKO-owned control plane for finite primary-Agent RunTurns. The runtime
    /// keeps only a weak store reference, so this does not create an Arc cycle.
    pub(super) continuation_runtime:
        std::sync::OnceLock<std::sync::Arc<super::continuation::TaskContinuationRuntime>>,
    pub(super) boot_reconciler:
        std::sync::OnceLock<std::sync::Arc<super::boot_reconciler::TaskRunBootReconciler>>,
    /// Process routing adapter for optional cross-workspace PlanTask targets.
    /// The adapter owns no task state and is intentionally absent in tests or
    /// embedding applications that only execute local tasks.
    execution_target_resolver: std::sync::RwLock<
        Option<std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>>,
    >,
    command_cell_runtime:
        std::sync::RwLock<Option<std::sync::Weak<super::command_cells::CommandCellRuntimeService>>>,
    #[cfg(test)]
    run_driver_shutdown_started: tokio::sync::Notify,
    #[cfg(test)]
    abort_next_run_driver_shutdown_reporter: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    run_driver_admission_test_barrier: std::sync::Mutex<Option<RunDriverAdmissionTestBarrier>>,
    #[cfg(test)]
    run_driver_registration_test_barrier:
        std::sync::Mutex<Option<RunDriverRegistrationTestBarrier>>,
    #[cfg(any(test, feature = "test-utils"))]
    fail_next_run_driver_registration: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_recovery_commit: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_recovery_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_cell_started: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_cell_started_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_runtime_mutation_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_cell_terminal_remaining: std::sync::atomic::AtomicUsize,
    /// File-backed event authority and deterministic projections.
    pub(super) shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
    shadow_generation: std::sync::Mutex<ShadowGeneration>,
    /// Owns the bounded task/subagent hook consumer so shutdown can drain it.
    hook_event_dispatcher:
        std::sync::Mutex<Option<super::hook_event_dispatcher::HookEventDispatcher>>,
    /// Per-run plan/state 写互斥锁 (F2-1 / F3-3 / F3-4)。
    ///
    /// revision compare-and-commit / transition_run 都是
    /// "读事件 → 校验 → 追加 → 重建投影"事务, 必须按 run 串行化。
    /// Different runs keep independent locks.
    plan_locks: dashmap::DashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
    operation_supervisor: std::sync::Arc<super::executor::TaskRuntimeOperationSupervisor>,
}

struct ShadowGeneration {
    active_operations: usize,
    workspace_id: String,
    transitioning: bool,
}

struct ShadowOperation<'a> {
    store: &'a TaskRuntimeStore,
}

/// Keeps one product operation bound to the current workspace generation.
/// Rebinding returns Busy until every lease from the previous generation drops.
#[must_use]
pub(crate) struct WorkspaceGenerationLease {
    // The application/real producer owns Store; its own registry must not
    // retain Store again through a generation counter's backedge.
    store: std::sync::Weak<TaskRuntimeStore>,
}

/// Opaque application receipt used by foreground surfaces to establish the
/// canonical lock order before memory and pool admission. The TaskRuntime
/// store remains the only generation authority; this type only retains its
/// existing lease until the outer foreground driver settles.
#[must_use]
pub struct TaskRuntimeGenerationReceipt {
    _lease: WorkspaceGenerationLease,
}

struct RunDriverSupervisor {
    accepting: bool,
    pending_admissions: usize,
    driver_cancels: std::collections::HashMap<u64, echo_agent::agent::CancellationToken>,
    /// Opaque capability and exact run identity for every live driver token.
    /// Framework-spawned tool calls must match both before transferring a
    /// receipt here; sequential internal tokens are never exposed as authority.
    driver_contexts: std::collections::HashMap<String, RunDriverExecutionContext>,
    driver_settlements: tokio::task::JoinSet<(u64, Result<(), String>)>,
    settlement_debts: Vec<RunSettlementDebt>,
    uncertain_drivers: std::collections::BTreeMap<u64, RetainedUncertainDriver>,
    tool_receipts: std::collections::BTreeMap<String, RetainedToolReceipt>,
    next_driver_token: u64,
    execution_receipts: std::collections::HashMap<u64, std::sync::Weak<RunDriverResources>>,
    shutdown_result_sender:
        Option<tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>>,
    shutdown_result:
        Option<tokio::sync::watch::Receiver<Option<Result<(), TaskRunDriverShutdownError>>>>,
    shutdown_owner: Option<std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownOwner>>>,
    /// Canonical store-owned reporter. Polling its JoinHandle through this
    /// shared mutex is cancellation-safe: a dropped waiter never takes it.
    shutdown_reporter: Option<std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>>,
    shutdown_reporter_errors: Vec<String>,
}

struct RunDriverExecutionContext {
    driver_token: u64,
    run_id: String,
}

enum RunDriverShutdownReporter {
    Running(tokio::task::JoinHandle<()>),
    Completed,
}

enum RunDriverShutdownOwner {
    Running(tokio::task::JoinHandle<Result<(), TaskRunDriverShutdownError>>),
    Completed(Result<(), TaskRunDriverShutdownError>),
}

#[cfg(test)]
struct RunDriverAdmissionTestBarrier {
    reserved: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
struct RunDriverRegistrationTestBarrier {
    registered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// Durable TaskRun terminal state that could not be written during the final
/// shutdown retry. The on-disk run remains authoritative; this diagnostic
/// records the uncommitted target and error. Despite its historical name,
/// admitted execution resources remain retained until original-owner settlement;
/// only resources proven never started may be released on shutdown failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedRunSettlement {
    pub run_id: String,
    pub driver_token: Option<u64>,
    pub root: PathBuf,
    pub target: TaskRunStatus,
    pub error: String,
}

impl std::fmt::Display for AbandonedRunSettlement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let driver_token = self
            .driver_token
            .map(|token| token.to_string())
            .unwrap_or_else(|| "none".to_string());
        write!(
            formatter,
            "run={} driver_token={} root={} target={} error={}",
            self.run_id,
            driver_token,
            self.root.display(),
            self.target.as_str(),
            self.error
        )
    }
}

/// Aggregated shutdown degradation. Accepted drivers are fully drained and
/// all exact execution receipts are released before this error is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunDriverShutdownError {
    pub driver_errors: Vec<String>,
    pub abandoned_settlements: Vec<AbandonedRunSettlement>,
}

impl std::fmt::Display for TaskRunDriverShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut details = self.driver_errors.clone();
        details.extend(self.abandoned_settlements.iter().map(ToString::to_string));
        write!(
            formatter,
            "TaskRun driver shutdown degraded: {}",
            details.join("; ")
        )
    }
}

impl std::error::Error for TaskRunDriverShutdownError {}

fn add_shutdown_driver_error(
    result: &mut Result<(), TaskRunDriverShutdownError>,
    driver_error: String,
) {
    match result {
        Ok(()) => {
            *result = Err(TaskRunDriverShutdownError {
                driver_errors: vec![driver_error],
                abandoned_settlements: Vec::new(),
            });
        }
        Err(error) => error.driver_errors.push(driver_error),
    }
}

/// One execution resource retained by the canonical TaskRun driver until its
/// durable terminal state (or settlement debt) has completed.
pub trait RunDriverExecutionReceipt: Send {
    /// Project uncertainty into the same receipt's existing admission. This
    /// does not settle, drop or transfer its underlying physical ownership.
    fn observe_unsettled(&self, _run_id: &str, _execution_id: Option<&str>) {}
    /// Release the resource after later-acquired receipts have settled.
    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()>;
}

type RunDriverResource =
    std::sync::Arc<std::sync::Mutex<Option<Box<dyn RunDriverExecutionReceipt>>>>;

#[derive(Default)]
struct RunDriverResourceState {
    receipts: Vec<RunDriverResource>,
    observation: Option<(String, Option<String>)>,
    released: bool,
}

/// Resource projection of the original supervisor. The application root and
/// real producer own this Arc; Store keeps only a Weak index into the same
/// bundle, breaking receipt -> Pool/Agent/Tool -> Store -> receipt cycles.
#[derive(Default)]
pub(crate) struct RunDriverResources {
    state: std::sync::Mutex<RunDriverResourceState>,
}

impl RunDriverResources {
    fn retain<Receipt: RunDriverExecutionReceipt + 'static>(
        &self,
        receipt: Receipt,
    ) -> Result<(), Receipt> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.released {
            return Err(receipt);
        }
        let receipt: RunDriverResource =
            std::sync::Arc::new(std::sync::Mutex::new(Some(Box::new(receipt))));
        state.receipts.push(receipt.clone());
        let observation = state.observation.clone();
        drop(state);
        if let Some((run, execution)) = observation {
            Self::observe_receipt(&receipt, &run, execution.as_deref());
        }
        Ok(())
    }

    fn observe_receipt(receipt: &RunDriverResource, run: &str, execution: Option<&str>) {
        let receipt = receipt.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(receipt) = receipt.as_ref() {
            receipt.observe_unsettled(run, execution);
        }
    }

    pub(crate) fn observe_unsettled(&self, run: &str, execution: Option<&str>) {
        let receipts = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.observation = Some((run.to_string(), execution.map(str::to_string)));
            state.receipts.clone()
        };
        // No supervisor or bundle-state monitor is held during receipt code.
        for receipt in receipts {
            Self::observe_receipt(&receipt, run, execution);
        }
    }

    fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .receipts
            .len()
    }

    pub(crate) fn is_released(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .released
    }

    async fn release(&self) {
        let receipts = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released = true;
            std::mem::take(&mut state.receipts)
        };
        for receipt in receipts.into_iter().rev() {
            let receipt = receipt
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(receipt) = receipt {
                receipt.release().await;
            }
        }
    }
}

/// Capability handed only to an accepted TaskRun driver. Pool-backed adapters
/// transfer their execution receipt here immediately after acquisition so it
/// survives inner future errors and panics until durable run settlement.
pub struct RunDriverReceiptOwner {
    resources: std::sync::Arc<RunDriverResources>,
    execution_context_id: String,
    run_id: String,
}

/// Internal observer error; normal callers still use their original String
/// result. The typed branch must reach the sole supervisor before projection.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RunDriverFailure {
    #[error("{0}")]
    Error(String),
    #[error("execution for run {run_id} is not physically settled")]
    Observation {
        run_id: String,
        execution_id: Option<String>,
    },
}

impl From<&str> for RunDriverFailure {
    fn from(value: &str) -> Self {
        Self::Error(value.to_string())
    }
}
impl From<String> for RunDriverFailure {
    fn from(value: String) -> Self {
        Self::Error(value)
    }
}
impl From<StoreError> for RunDriverFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::RecoveryBlocked { run_id, .. } => Self::Observation {
                run_id,
                execution_id: None,
            },
            other => Self::Error(other.to_string()),
        }
    }
}
impl From<echo_agent::error::ReactError> for RunDriverFailure {
    fn from(error: echo_agent::error::ReactError) -> Self {
        match error {
            echo_agent::error::ReactError::ExecutionNotSettled {
                run_id,
                execution_id,
            } => Self::Observation {
                run_id,
                execution_id,
            },
            other => Self::Error(other.to_string()),
        }
    }
}
impl RunDriverFailure {
    pub fn is_observation(&self) -> bool {
        matches!(self, Self::Observation { .. })
    }
    /// Preserve the nonterminal identity when a surface returns to framework
    /// streaming APIs; string conversion is only for final presentation.
    pub fn into_react_error(self) -> echo_agent::error::ReactError {
        match self {
            Self::Observation {
                run_id,
                execution_id,
            } => echo_agent::error::ReactError::ExecutionNotSettled {
                run_id,
                execution_id,
            },
            Self::Error(message) => echo_agent::error::ReactError::Other(message),
        }
    }
}

pub(crate) trait RunDriverFailureKind:
    Clone + From<String> + std::fmt::Display + Send + 'static
{
    fn is_observation(&self) -> bool;
    fn owner_unsettled(run_id: String, execution_id: Option<String>, detail: String) -> Self;
}
impl RunDriverFailureKind for String {
    fn owner_unsettled(run_id: String, execution_id: Option<String>, detail: String) -> Self {
        format!("execution for run {run_id} ({execution_id:?}) is not physically settled: {detail}")
    }
    fn is_observation(&self) -> bool {
        false
    }
}
impl RunDriverFailureKind for RunDriverFailure {
    fn owner_unsettled(run_id: String, execution_id: Option<String>, _detail: String) -> Self {
        Self::Observation {
            run_id,
            execution_id,
        }
    }
    fn is_observation(&self) -> bool {
        matches!(self, Self::Observation { .. })
    }
}

type BoxRunDriverFuture<T, E> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'static>>;

enum RunDriverStart<T, E> {
    Execute(BoxRunDriverFuture<T, E>),
    PreparationFailed(String),
    Reject(String),
}

/// Exact driver registration completed before callers mutate TaskRuntime.
/// Dropping an unstarted registration wakes the canonical owner as a rejected
/// preparation, so shutdown never waits forever for an accepted slot.
#[must_use]
pub(crate) struct RegisteredRunDriver<T: Send + 'static, E: RunDriverFailureKind = String> {
    start_sender: Option<tokio::sync::oneshot::Sender<RunDriverStart<T, E>>>,
    result_receiver: Option<tokio::sync::oneshot::Receiver<Result<T, E>>>,
    receipt_owner: Option<RunDriverReceiptOwner>,
    preparation_started: bool,
    active: bool,
}

/// Exact pre-execution admission owned by the canonical TaskRuntime
/// supervisor. It is acquired before any run mutation or workspace-bound
/// memory/pool admission and consumed only when the driver is registered.
#[must_use]
pub(crate) struct RunDriverAdmissionReservation {
    store: std::sync::Arc<TaskRuntimeStore>,
    run_id: String,
    cancel: echo_agent::agent::CancellationToken,
    active: bool,
}

impl RunDriverReceiptOwner {
    const EXECUTION_CONTEXT_PREFIX: &'static str = "eko-task-driver:";

    /// Retain one driver resource. Factories passed to `spawn_run_driver` must
    /// call this from the returned future, not while constructing that future,
    /// because driver admission is serialized by the supervisor lock.
    pub fn retain<Receipt>(&mut self, receipt: Receipt) -> Result<(), RunDriverFailure>
    where
        Receipt: RunDriverExecutionReceipt + 'static,
    {
        // This unique capability lives only inside the original producer;
        // physical release cannot precede that producer's returned result.
        self.resources
            .retain(receipt)
            .map_err(|_| RunDriverFailure::Observation {
                run_id: self.run_id.clone(),
                execution_id: None,
            })
    }

    /// Opaque value-carried identity for framework-spawned tool execution.
    /// The canonical store validates it against this exact live driver.
    pub(crate) fn execution_context_id(&self) -> String {
        self.execution_context_id.clone()
    }
}

impl<T: Send + 'static, E: RunDriverFailureKind> RegisteredRunDriver<T, E> {
    pub(crate) fn mark_preparation_started(&mut self) {
        self.preparation_started = true;
    }

    pub(crate) fn start<F, Factory>(
        mut self,
        factory: Factory,
    ) -> tokio::sync::oneshot::Receiver<Result<T, E>>
    where
        F: std::future::Future<Output = Result<T, E>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
    {
        let receiver = self.result_receiver.take().unwrap_or_else(|| {
            let (_sender, receiver) = tokio::sync::oneshot::channel();
            receiver
        });
        let start = self
            .receipt_owner
            .take()
            .map(|owner| RunDriverStart::Execute(Box::pin(factory(owner))));
        if let (Some(sender), Some(start)) = (self.start_sender.take(), start) {
            let _start_delivered = sender.send(start);
        }
        self.active = false;
        receiver
    }

    pub(crate) fn reject(mut self, error: impl Into<String>) {
        if let Some(sender) = self.start_sender.take() {
            let _start_delivered = sender.send(RunDriverStart::Reject(error.into()));
        }
        self.active = false;
    }

    pub(crate) fn fail_preparation(mut self, error: impl Into<String>) {
        if let Some(sender) = self.start_sender.take() {
            let _start_delivered = sender.send(RunDriverStart::PreparationFailed(error.into()));
        }
        self.active = false;
    }
}

impl<T: Send + 'static, E: RunDriverFailureKind> Drop for RegisteredRunDriver<T, E> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(sender) = self.start_sender.take() {
            let message =
                "TaskRun driver registration dropped before preparation completed".to_string();
            let start = if self.preparation_started {
                RunDriverStart::PreparationFailed(message)
            } else {
                RunDriverStart::Reject(message)
            };
            let _start_delivered = sender.send(start);
        }
    }
}

impl Drop for RunDriverAdmissionReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let became_idle = {
            let mut supervisor = self
                .store
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            supervisor.pending_admissions = supervisor.pending_admissions.saturating_sub(1);
            supervisor.pending_admissions == 0
        };
        if became_idle {
            self.store.run_driver_admission_idle.notify_one();
        }
    }
}

struct RetainedToolReceipt {
    intent: PhysicalOwnerIntent,
    _generation_lease: WorkspaceGenerationLease,
    resources: std::sync::Weak<RunDriverResources>,
    physical_terminal_observed: bool,
}

struct RetainedUncertainDriver {
    _generation_lease: WorkspaceGenerationLease,
    _cancellation_registration: RunCancellationRegistration,
    intent: PhysicalOwnerIntent,
    observation: String,
    physical_terminal_observed: bool,
    pending_terminal: Option<TaskRunStatus>,
}

struct RunSettlementDebt {
    // The original admitted driver identity, not a filesystem absence guess.
    physical_owner_intent: Option<PhysicalOwnerIntent>,
    generation_lease: WorkspaceGenerationLease,
    driver_token: Option<u64>,
    run_id: String,
    root: PathBuf,
    target: TaskRunStatus,
    note: Option<String>,
    last_error: String,
}

impl Default for RunDriverSupervisor {
    fn default() -> Self {
        Self {
            accepting: true,
            pending_admissions: 0,
            driver_cancels: std::collections::HashMap::new(),
            driver_contexts: std::collections::HashMap::new(),
            driver_settlements: tokio::task::JoinSet::new(),
            settlement_debts: Vec::new(),
            uncertain_drivers: std::collections::BTreeMap::new(),
            tool_receipts: std::collections::BTreeMap::new(),
            next_driver_token: 0,
            execution_receipts: std::collections::HashMap::new(),
            shutdown_result_sender: None,
            shutdown_result: None,
            shutdown_owner: None,
            shutdown_reporter: None,
            shutdown_reporter_errors: Vec::new(),
        }
    }
}

/// Exclusive workspace-generation transition. New operations receive a typed
/// busy error until this guard is dropped.
#[must_use]
pub(crate) struct TaskRuntimeWorkspaceTransition<'a> {
    store: &'a TaskRuntimeStore,
    active: bool,
}

impl TaskRuntimeWorkspaceTransition<'_> {
    #[cfg(test)]
    pub(crate) fn list_runs_in(
        &self,
        statuses: &[TaskRunStatus],
    ) -> Result<Vec<TaskRun>, StoreError> {
        super::file_store::FileTaskStore::from_root(self.store.shadow.root())
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?
            .list_runs_in(statuses)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    pub(crate) fn rebind_shadow_root(
        &self,
        root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        let root = root.into();
        echo_agent::utils::fs::create_dir_all_durable(&root)
            .map_err(|error| super::file_shadow::ShadowError::Io(error.to_string()))?;
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !generation.transitioning || generation.active_operations != 0 {
            return Err(StoreError::InvalidPlan(
                "task runtime workspace transition lost exclusive admission".to_string(),
            ));
        }
        self.store.shadow.rebind_root(root)?;
        let previous_workspace_id = generation.workspace_id.clone();
        let workspace_id = workspace_id.into();
        generation.workspace_id = workspace_id.clone();
        drop(generation);
        if let Some(runtime) = self
            .store
            .command_cell_runtime
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            runtime.rebind_store_workspace(&previous_workspace_id, &workspace_id);
        }
        Ok(())
    }
}

impl Drop for TaskRuntimeWorkspaceTransition<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.transitioning = false;
        self.active = false;
    }
}

struct ShadowFileStore<'a> {
    _operation: ShadowOperation<'a>,
    store: super::file_store::FileTaskStore,
}

impl std::ops::Deref for ShadowFileStore<'_> {
    type Target = super::file_store::FileTaskStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl Drop for ShadowOperation<'_> {
    fn drop(&mut self) {
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.active_operations = generation.active_operations.saturating_sub(1);
    }
}

impl Drop for WorkspaceGenerationLease {
    fn drop(&mut self) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let mut generation = store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.active_operations = generation.active_operations.saturating_sub(1);
    }
}

/// RAII registration for one active TaskRun driver. Each guard removes only
/// its own token, so overlapping drivers can finish in either order.
pub struct RunCancellationRegistration {
    store: std::sync::Weak<TaskRuntimeStore>,
    run_id: String,
    registration_id: u64,
}

#[cfg(test)]
fn validate_runtime_plan(tasks: &[PlanTask]) -> Result<(), StoreError> {
    let runtime_tasks = tasks
        .iter()
        .map(echo_agent::tasks::Task::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::InvalidPlan)?;
    echo_agent::tasks::PlanValidator::default()
        .validate_task_snapshot(&runtime_tasks)
        .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoverableSubagentOutcome {
    pub(crate) outcome: SubagentOutcome,
    pub(crate) full_output: String,
}

struct TaskStatusEvent<'a> {
    run_id: &'a str,
    task_id: &'a str,
    task_subject: &'a str,
    status: echo_agent::tasks::TaskStatus,
    owner_agent: Option<&'a str>,
    summary: Option<&'a str>,
    claim: Option<&'a echo_agent::tasks::TaskClaim>,
}

fn review_runtime_event(
    review: &ReviewResult,
    claim: Option<&echo_agent::tasks::TaskClaim>,
) -> RuntimeJournalEvent {
    let kind = match review.outcome {
        ReviewOutcome::Pass => RuntimeEventKind::ReviewPassed,
        ReviewOutcome::NeedsFix => RuntimeEventKind::ReviewNeedsFix,
        ReviewOutcome::Blocked => RuntimeEventKind::ReviewBlocked,
    };
    RuntimeJournalEvent::for_append(
        review.run_id.as_str(),
        Some(review.task_id.as_str()),
        None,
        kind,
        serde_json::json!({
            "review_id": review.id,
            "reviewer": review.reviewer_agent,
            "outcome": review.outcome.as_str(),
            "issues": review.issues,
            "failure_fingerprint": review.failure_fingerprint,
            "created_fix_task_id": review.created_fix_task_id,
            "created_at": echo_agent::utils::time::to_local(review.created_at).to_rfc3339(),
            "claim_id": claim.map(|claim| claim.claim_id.as_str()),
            "plan_revision": claim.map(|claim| claim.revision),
            "attempt": claim.map(|claim| claim.attempt),
            "spec_hash": claim.map(|claim| claim.spec_hash.as_str()),
        }),
    )
}

fn task_status_wire(status: &echo_agent::tasks::TaskStatus) -> (&'static str, Option<&str>) {
    match status {
        echo_agent::tasks::TaskStatus::Pending => ("pending", None),
        echo_agent::tasks::TaskStatus::Running => ("running", None),
        echo_agent::tasks::TaskStatus::Blocked(detail) => ("blocked", Some(detail.as_str())),
        echo_agent::tasks::TaskStatus::Completed => ("completed", None),
        echo_agent::tasks::TaskStatus::Failed(detail) => ("failed", Some(detail.as_str())),
        echo_agent::tasks::TaskStatus::Skipped => ("skipped", None),
        echo_agent::tasks::TaskStatus::Cancelled => ("cancelled", None),
        echo_agent::tasks::TaskStatus::TimedOut { error } => ("timed_out", Some(error.as_str())),
        echo_agent::tasks::TaskStatus::Retrying { last_error, .. } => {
            ("retrying", Some(last_error.as_str()))
        }
        echo_agent::tasks::TaskStatus::Paused(detail) => ("paused", Some(detail.as_str())),
    }
}

fn runtime_task_event_kind(status: &echo_agent::tasks::TaskStatus) -> RuntimeEventKind {
    match status {
        echo_agent::tasks::TaskStatus::Running => RuntimeEventKind::TaskStarted,
        echo_agent::tasks::TaskStatus::Completed => RuntimeEventKind::TaskCompleted,
        echo_agent::tasks::TaskStatus::Failed(_) => RuntimeEventKind::TaskFailed,
        echo_agent::tasks::TaskStatus::Cancelled => RuntimeEventKind::TaskCancelled,
        echo_agent::tasks::TaskStatus::TimedOut { .. } => RuntimeEventKind::TaskTimedOut,
        echo_agent::tasks::TaskStatus::Skipped => RuntimeEventKind::TaskSkipped,
        echo_agent::tasks::TaskStatus::Blocked(_) => RuntimeEventKind::TaskBlocked,
        echo_agent::tasks::TaskStatus::Pending
        | echo_agent::tasks::TaskStatus::Retrying { .. }
        | echo_agent::tasks::TaskStatus::Paused(_) => RuntimeEventKind::TaskStatusChanged,
    }
}

fn runtime_execution_change_event(
    run_id: &str,
    before: &echo_agent::tasks::Task,
    after: &echo_agent::tasks::Task,
    summary: Option<&str>,
) -> Result<Option<RuntimeJournalEvent>, StoreError> {
    if before.execution == after.execution {
        return Ok(None);
    }
    if before.spec != after.spec {
        return Err(StoreError::InvalidPlan(format!(
            "runtime mutation changed task specification '{}'",
            after.spec.id
        )));
    }
    let extension: EkoTaskSpec = after
        .spec
        .clone()
        .try_into()
        .map_err(StoreError::InvalidPlan)?;
    let (status, status_detail) = task_status_wire(&after.execution.status);
    let now = echo_agent::utils::time::now_local().to_rfc3339();
    let started = matches!(
        after.execution.status,
        echo_agent::tasks::TaskStatus::Running | echo_agent::tasks::TaskStatus::Retrying { .. }
    );
    let finished = after.execution.status.is_terminal();
    Ok(Some(RuntimeJournalEvent::for_append(
        run_id,
        Some(&after.spec.id),
        None,
        runtime_task_event_kind(&after.execution.status),
        serde_json::json!({
            "status": status,
            "status_detail": status_detail,
            "claim": after.execution.claim,
            // Store the canonical execution as one lossless internal fact.
            "execution": &after.execution,
            "execution_id": after
                .execution
                .claim
                .as_ref()
                .map(|claim| claim.execution_id(run_id, &after.spec.id)),
            "retry_count": after.execution.retry_count,
            "failure_fingerprint": after.execution.failure_fingerprint,
            "owner_agent": extension.agent_role,
            "title": after.spec.title,
            "summary": summary,
            "started_at": if started { Some(now.as_str()) } else { None },
            "completed_at": if finished { Some(now.as_str()) } else { None },
        }),
    )))
}

fn runtime_execution_change_events(
    run_id: &str,
    before: &echo_agent::tasks::RuntimePlanSnapshot,
    after: &echo_agent::tasks::RuntimePlanSnapshot,
    summary: Option<&str>,
) -> Result<Vec<RuntimeJournalEvent>, StoreError> {
    if before.tasks.len() != after.tasks.len() {
        return Err(StoreError::InvalidPlan(
            "execution diff changed the revisioned task graph shape".to_string(),
        ));
    }
    let mut events = Vec::new();
    for after_task in &after.tasks {
        let before_task = before
            .tasks
            .iter()
            .find(|task| task.spec.id == after_task.spec.id)
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "runtime mutation introduced task '{}'",
                    after_task.spec.id
                ))
            })?;
        if let Some(event) =
            runtime_execution_change_event(run_id, before_task, after_task, summary)?
        {
            events.push(event);
        }
    }
    Ok(events)
}

pub(crate) struct SubagentReleaseRecord<'a> {
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub execution_id: &'a str,
    pub agent_name: &'a str,
    pub task_subject: &'a str,
    pub plan_revision: u64,
    pub attempt: u32,
    pub status: &'a str,
    pub outcome: Option<&'a SubagentOutcome>,
    pub full_output: Option<&'a str>,
    pub usage: Option<&'a ExecutionUsage>,
    pub dispatch_hook: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeTaskProductSettlement {
    pub summary: Option<String>,
    pub execution_summary: Option<TaskExecutionSummary>,
    pub review: Option<ReviewResult>,
    pub diagnostic_note: Option<String>,
    pub typed_terminal: Option<echo_agent::error::AgentFailure>,
}

/// Durable result of one dispatch resolution.
///
/// The live event stream projects this receipt instead of the staged attempt
/// payload, so rejected success evidence can never surface as a completed task
/// while the journal records an interruption.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeTaskResolutionReceipt {
    /// Framework resolution committed by the canonical graph.
    pub resolution: echo_agent::tasks::RuntimeTaskResolution,
    /// Interruption-first classification of the attempted evidence.
    pub settlement: SuccessEvidenceSettlement,
    /// Final acceptance of the attempt's execution-success evidence.
    ///
    /// This is the last word, decided after the canonical settlement ran:
    /// `settlement` can still report `Committed` when a durable claim
    /// interruption kept the run `Running`, or when a non-success attempt
    /// (review `NeedsFix`/`Blocked`) reached settlement after its run/claim was
    /// interrupted. Consumers project the live payload from this fact.
    pub acceptance: SuccessEvidenceAcceptance,
    /// Summary text actually committed to the task projection, if any.
    pub committed_summary: Option<String>,
    /// Task status read back from the committed graph.
    pub committed_status: Option<echo_agent::tasks::TaskStatus>,
}


enum RunTurnClaimPreparation {
    Start(RuntimeJournalEvent),
    NotSubmitted(ContinuationNotSubmittedReason),
}

impl Drop for RunCancellationRegistration {
    fn drop(&mut self) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        if let Ok(mut map) = store.run_cancel_tokens.lock() {
            let remove_run = if let Some(tokens) = map.get_mut(&self.run_id) {
                tokens.retain(|(registration_id, _)| registration_id != &self.registration_id);
                tokens.is_empty()
            } else {
                false
            };
            if remove_run {
                map.remove(&self.run_id);
            }
        }
        store.run_driver_idle.notify_waiters();
    }
}
