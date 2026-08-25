//! Bounded async boundary for EKO's synchronous product-data I/O.
//!
//! File, research, and analysis stores intentionally remain simple
//! filesystem-backed domain modules. Async surfaces must enter them through
//! this adapter so blocking filesystem work cannot occupy Tokio workers.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::state::ScopedWorkspaceControl;

const PROCESS_PRODUCT_DATA_IO_LIMIT: usize = 8;
const MAX_PRODUCT_DATA_FAILURES: usize = 64;
static PROCESS_PRODUCT_DATA_IO: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(PROCESS_PRODUCT_DATA_IO_LIMIT)));

struct ProductDataOperationState {
    admission_open: bool,
    active: usize,
    failures: Vec<String>,
    failure_overflow: usize,
}

impl ProductDataOperationState {
    fn record_failure(&mut self, failure: String) {
        if self.failures.len() < MAX_PRODUCT_DATA_FAILURES {
            self.failures.push(failure);
        } else {
            self.failure_overflow = self.failure_overflow.saturating_add(1);
        }
    }

    fn failure_receipt(&self) -> Vec<String> {
        let mut failures = self.failures.clone();
        if self.failure_overflow > 0 {
            failures.push(format!(
                "{} additional product-data failures were omitted",
                self.failure_overflow
            ));
        }
        failures
    }
}

struct ProductDataOperationSupervisor {
    state: Mutex<ProductDataOperationState>,
    settled: tokio::sync::watch::Sender<u64>,
    #[cfg(test)]
    barrier: Mutex<Option<ProductDataIoTestBarrier>>,
}

#[cfg(test)]
struct ProductDataIoTestBarrier {
    operation: &'static str,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

impl ProductDataOperationSupervisor {
    fn new() -> Self {
        let (settled, _initial_receiver) = tokio::sync::watch::channel(0);
        Self {
            state: Mutex::new(ProductDataOperationState {
                admission_open: true,
                active: 0,
                failures: Vec::new(),
                failure_overflow: 0,
            }),
            settled,
            #[cfg(test)]
            barrier: Mutex::new(None),
        }
    }

    fn admit(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<ProductDataOperation, ProductDataIoError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| ProductDataIoError::Admission {
                operation,
                error: format!("product-data operation registry is poisoned: {error}"),
            })?;
        if !state.admission_open {
            return Err(ProductDataIoError::Admission {
                operation,
                error: "application shutdown has closed product-data admission".to_string(),
            });
        }
        state.active =
            state
                .active
                .checked_add(1)
                .ok_or_else(|| ProductDataIoError::Admission {
                    operation,
                    error: "product-data active operation capacity is exhausted".to_string(),
                })?;
        Ok(ProductDataOperation {
            supervisor: Arc::clone(self),
            operation,
            settled: false,
        })
    }

    fn admit_nested(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<ProductDataOperation, ProductDataIoError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| ProductDataIoError::Admission {
                operation,
                error: format!("product-data operation registry is poisoned: {error}"),
            })?;
        state.active =
            state
                .active
                .checked_add(1)
                .ok_or_else(|| ProductDataIoError::Admission {
                    operation,
                    error: "product-data active operation capacity is exhausted".to_string(),
                })?;
        Ok(ProductDataOperation {
            supervisor: Arc::clone(self),
            operation,
            settled: false,
        })
    }

    fn begin_shutdown(&self) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        state.admission_open = false;
        Ok(())
    }

    async fn join_shutdown(&self) -> Result<(), String> {
        let mut settled = self.settled.subscribe();
        loop {
            let outcome = {
                let state = self.state.lock().map_err(|error| error.to_string())?;
                (state.active == 0).then(|| state.failure_receipt())
            };
            if let Some(failures) = outcome {
                return if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                };
            }
            settled.changed().await.map_err(|_| {
                "product-data settlement signal closed before active work completed".to_string()
            })?;
        }
    }

    #[cfg(test)]
    fn take_barrier(&self, operation: &'static str) -> Option<ProductDataIoTestBarrier> {
        let mut barrier = self
            .barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let matches = barrier
            .as_ref()
            .is_some_and(|candidate| candidate.operation == operation);
        if matches { barrier.take() } else { None }
    }
}

#[derive(Clone)]
pub struct ProductDataIoService {
    supervisor: Arc<ProductDataOperationSupervisor>,
}

impl ProductDataIoService {
    pub fn new() -> Self {
        Self {
            supervisor: Arc::new(ProductDataOperationSupervisor::new()),
        }
    }

    pub async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        run_with(Arc::clone(&self.supervisor), operation, function).await
    }

    pub fn begin_shutdown(&self) -> Result<(), String> {
        self.supervisor.begin_shutdown()
    }

    pub async fn join_shutdown(&self) -> Result<(), String> {
        self.begin_shutdown()?;
        self.supervisor.join_shutdown().await
    }

    pub fn begin_owned_flow(
        &self,
        operation: &'static str,
    ) -> Result<ProductDataIoFlow, ProductDataIoError> {
        let owner = self.supervisor.admit(operation)?;
        Ok(ProductDataIoFlow {
            inner: Arc::new(ProductDataIoFlowInner {
                supervisor: Arc::clone(&self.supervisor),
                owner: Mutex::new(Some(owner)),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn install_test_barrier(
        &self,
        operation: &'static str,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self
            .supervisor
            .barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ProductDataIoTestBarrier {
            operation,
            entered,
            release,
        });
    }
}

#[must_use = "owned product-data flows must be settled so shutdown can report their outcome"]
#[derive(Clone)]
pub struct ProductDataIoFlow {
    inner: Arc<ProductDataIoFlowInner>,
}

struct ProductDataIoFlowInner {
    supervisor: Arc<ProductDataOperationSupervisor>,
    owner: Mutex<Option<ProductDataOperation>>,
}

impl ProductDataIoFlow {
    pub(crate) fn service(&self) -> ProductDataIoService {
        ProductDataIoService {
            supervisor: Arc::clone(&self.inner.supervisor),
        }
    }

    pub async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let nested_owner = {
            let owner = self
                .inner
                .owner
                .lock()
                .map_err(|error| ProductDataIoError::Admission {
                    operation,
                    error: format!("product-data flow owner is poisoned: {error}"),
                })?;
            if owner.is_none() {
                return Err(ProductDataIoError::Admission {
                    operation,
                    error: "product-data flow has already settled".to_string(),
                });
            }
            self.inner.supervisor.admit_nested(operation)?
        };
        run_nested_with(
            Arc::clone(&self.inner.supervisor),
            nested_owner,
            operation,
            function,
        )
        .await
    }

    pub fn settle(&self, failure: Option<String>) {
        let owner = self
            .inner
            .owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(owner) = owner {
            owner.settle(failure);
        }
    }
}

impl Default for ProductDataIoService {
    fn default() -> Self {
        Self::new()
    }
}

struct ProductDataOperation {
    supervisor: Arc<ProductDataOperationSupervisor>,
    operation: &'static str,
    settled: bool,
}

impl ProductDataOperation {
    fn settle(mut self, failure: Option<String>) {
        if let Ok(mut state) = self.supervisor.state.lock() {
            state.active = state.active.saturating_sub(1);
            if let Some(failure) = failure {
                state.record_failure(format!("{}: {failure}", self.operation));
            }
        }
        self.settled = true;
        self.supervisor
            .settled
            .send_modify(|version| *version = version.saturating_add(1));
    }
}

impl Drop for ProductDataOperation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Ok(mut state) = self.supervisor.state.lock() {
            state.active = state.active.saturating_sub(1);
            state.record_failure(format!("{}: operation owner dropped", self.operation));
        }
        self.supervisor
            .settled
            .send_modify(|version| *version = version.saturating_add(1));
    }
}

#[derive(Debug, Error)]
pub enum ProductDataIoError {
    #[error("product-data I/O admission closed during {operation}: {error}")]
    Admission {
        operation: &'static str,
        error: String,
    },
    #[error("product-data I/O task failed during {operation}: {error}")]
    Join {
        operation: &'static str,
        error: String,
    },
    #[error("product-data I/O owner closed during {operation}")]
    OwnerClosed { operation: &'static str },
}

async fn run_with<T, F>(
    supervisor: Arc<ProductDataOperationSupervisor>,
    operation: &'static str,
    function: F,
) -> Result<T, ProductDataIoError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = PROCESS_PRODUCT_DATA_IO
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ProductDataIoError::Admission {
            operation,
            error: error.to_string(),
        })?;
    let owner = supervisor.admit(operation)?;
    #[cfg(test)]
    let barrier = supervisor.take_barrier(operation);
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        #[cfg(test)]
        if let Some(barrier) = barrier {
            let _entered = barrier.entered.send(());
            let _released = barrier.release.await;
        }
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            function()
        })
        .await
        .map_err(|error| ProductDataIoError::Join {
            operation,
            error: error.to_string(),
        });
        let failure = result.as_ref().err().map(ToString::to_string);
        owner.settle(failure);
        let _delivered = result_tx.send(result);
    });
    result_rx
        .await
        .map_err(|_| ProductDataIoError::OwnerClosed { operation })?
}

async fn run_nested_with<T, F>(
    _supervisor: Arc<ProductDataOperationSupervisor>,
    owner: ProductDataOperation,
    operation: &'static str,
    function: F,
) -> Result<T, ProductDataIoError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = PROCESS_PRODUCT_DATA_IO
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ProductDataIoError::Admission {
            operation,
            error: error.to_string(),
        })?;
    #[cfg(test)]
    let barrier = _supervisor.take_barrier(operation);
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        #[cfg(test)]
        if let Some(barrier) = barrier {
            let _entered = barrier.entered.send(());
            let _released = barrier.release.await;
        }
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            function()
        })
        .await
        .map_err(|error| ProductDataIoError::Join {
            operation,
            error: error.to_string(),
        });
        let failure = result.as_ref().err().map(ToString::to_string);
        owner.settle(failure);
        let _delivered = result_tx.send(result);
    });
    result_rx
        .await
        .map_err(|_| ProductDataIoError::OwnerClosed { operation })?
}

/// Exact workspace authority shared by GUI, TUI, CLI, and channel product
/// data commands.
///
/// Every blocking closure captures a clone of this value. Dropping the async
/// waiter therefore cannot release the workspace control lease before a
/// non-abortable `spawn_blocking` operation settles.
#[derive(Clone)]
pub struct ScopedProductData {
    control: ScopedWorkspaceControl,
    analysis_runs: Arc<AnalysisRunSupervisor>,
    io: ProductDataIoService,
}

#[must_use = "scoped product-data flows must settle before releasing their workspace owner"]
pub(crate) struct ScopedProductDataFlow {
    control: ScopedWorkspaceControl,
    io: ProductDataIoFlow,
}

impl ScopedProductDataFlow {
    pub(crate) async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let control = self.control.clone();
        self.io
            .run(operation, move || {
                let _control = control;
                function()
            })
            .await
    }

    pub(crate) fn settle(self, failure: Option<String>) {
        self.io.settle(failure);
    }
}

impl ScopedProductData {
    pub(crate) fn new(
        control: ScopedWorkspaceControl,
        analysis_runs: Arc<AnalysisRunSupervisor>,
        io: ProductDataIoService,
    ) -> Self {
        Self {
            control,
            analysis_runs,
            io,
        }
    }

    pub fn workspace_id(&self) -> &str {
        self.control.workspace_id()
    }

    pub fn generation(&self) -> String {
        self.control.generation()
    }

    pub fn data_root(&self) -> &std::path::Path {
        self.control.data_root()
    }

    pub fn project_root(&self) -> std::path::PathBuf {
        self.control.project_root()
    }

    pub fn runtime(&self) -> &crate::state::ScopedChatRuntime {
        self.control.runtime()
    }

    pub(crate) fn begin_owned_flow(
        &self,
        operation: &'static str,
    ) -> Result<ScopedProductDataFlow, ProductDataIoError> {
        Ok(ScopedProductDataFlow {
            control: self.control.clone(),
            io: self.io.begin_owned_flow(operation)?,
        })
    }

    fn begin_runtime_flow(
        &self,
        operation: &'static str,
    ) -> Result<ProductDataIoFlow, ProductDataIoError> {
        self.io.begin_owned_flow(operation)
    }

    pub async fn data<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce(&std::path::Path) -> T + Send + 'static,
    {
        let control = self.control.clone();
        self.io
            .run(operation, move || function(control.data_root()))
            .await
    }

    pub async fn project<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce(&std::path::Path) -> T + Send + 'static,
    {
        let control = self.control.clone();
        self.io
            .run(operation, move || {
                let root = control.project_root();
                function(&root)
            })
            .await
    }

    /// Cloneable receipt for domain async flows that start blocking phases
    /// after an awaited provider or tool call.
    pub fn settlement_receipt(&self) -> ScopedWorkspaceControl {
        self.control.clone()
    }

    pub async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, ProductDataIoError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let control = self.control.clone();
        self.io
            .run(operation, move || {
                let _control = control;
                function()
            })
            .await
    }

    pub fn start_analysis(
        &self,
        analysis_id: &str,
    ) -> Result<AnalysisRunReceipt, AnalysisRunControlError> {
        self.analysis_runs.start(self.clone(), analysis_id)
    }

    pub async fn wait_analysis(
        &self,
        receipt: &AnalysisRunReceipt,
    ) -> Result<crate::analysis::AnalysisDocument, AnalysisRunControlError> {
        self.analysis_runs.wait(self, receipt).await
    }

    pub fn poll_analysis(
        &self,
        receipt: &AnalysisRunReceipt,
    ) -> Result<AnalysisWaitReceipt, AnalysisRunControlError> {
        self.analysis_runs.poll(self, receipt)
    }

    pub async fn cancel_analysis(
        &self,
        analysis_id: &str,
    ) -> Result<AnalysisCancelReceipt, AnalysisRunControlError> {
        self.analysis_runs.cancel_and_join(self, analysis_id).await
    }

    pub async fn delete_analysis(&self, analysis_id: &str) -> Result<(), AnalysisRunControlError> {
        let deletion =
            self.analysis_runs
                .claim_exclusive(self, analysis_id, PHASE_DELETING, "delete")?;
        let analysis_id = analysis_id.to_string();
        self.data("delete analysis", move |root| {
            let _deletion = deletion;
            crate::analysis::delete_analysis(root, &analysis_id)
        })
        .await
        .map_err(|error| AnalysisRunControlError::Execution(error.to_string()))?
        .map_err(|error| AnalysisRunControlError::Execution(error.to_string()))
    }

    pub async fn save_analysis(
        &self,
        analysis_id: &str,
        request: crate::analysis::SaveAnalysisRequest,
    ) -> Result<crate::analysis::AnalysisDocument, AnalysisRunControlError> {
        let mutation =
            self.analysis_runs
                .claim_exclusive(self, analysis_id, PHASE_EDITING, "edit")?;
        let analysis_id = analysis_id.to_string();
        self.data("save analysis", move |root| {
            let _mutation = mutation;
            crate::analysis::save_analysis(root, &analysis_id, request)
        })
        .await
        .map_err(|error| AnalysisRunControlError::Execution(error.to_string()))?
        .map_err(|error| AnalysisRunControlError::Execution(error.to_string()))
    }

    #[cfg(test)]
    pub(crate) fn start_analysis_fixture(
        &self,
        analysis_id: &str,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<AnalysisRunReceipt, AnalysisRunControlError> {
        let cancel = Arc::new(CancellationToken::new());
        self.analysis_runs
            .start_owned(self.clone(), analysis_id, cancel, None, async move {
                let _ = entered.send(());
                release
                    .await
                    .map_err(|_| "analysis fixture release was dropped".to_string())?;
                Err("analysis fixture settled".to_string())
            })
    }
}

fn analysis_cancel_key(workspace_id: &str, analysis_id: &str) -> String {
    serde_json::json!(["analysis", workspace_id, analysis_id]).to_string()
}

const ANALYSIS_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const PHASE_STARTED: u8 = 0;
const PHASE_DRAINING: u8 = 1;
const PHASE_JOINED: u8 = 2;
const PHASE_CLEANUP_FAILED: u8 = 3;
const PHASE_DELETING: u8 = 4;
const PHASE_EDITING: u8 = 5;
const COMPLETED_ANALYSIS_LIMIT: usize = 64;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AnalysisRunReceipt {
    pub workspace_id: String,
    pub workspace_generation: String,
    pub analysis_id: String,
    pub owner_id: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnalysisCancelReceipt {
    Joined {
        receipt: AnalysisRunReceipt,
        execution_error: Option<String>,
    },
    CleanupTimedOut {
        receipt: AnalysisRunReceipt,
        timeout_seconds: u64,
    },
    CleanupFailed {
        receipt: AnalysisRunReceipt,
        error: String,
    },
}

impl AnalysisCancelReceipt {
    pub fn cleanup_joined(&self) -> bool {
        matches!(self, Self::Joined { .. })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnalysisWaitReceipt {
    Started {
        receipt: AnalysisRunReceipt,
        draining: bool,
    },
    Joined {
        receipt: AnalysisRunReceipt,
        document: Option<Box<crate::analysis::AnalysisDocument>>,
        execution_error: Option<String>,
    },
    CleanupFailed {
        receipt: AnalysisRunReceipt,
        error: String,
    },
}

#[derive(Debug, Error)]
pub enum AnalysisRunControlError {
    #[error("analysis supervisor is shutting down")]
    SupervisorClosed,
    #[error("analysis '{analysis_id}' is already owned by run {owner_id}")]
    AlreadyRunning {
        analysis_id: String,
        owner_id: String,
    },
    #[error("analysis '{analysis_id}' has no active run in workspace '{workspace_id}'")]
    NotFound {
        workspace_id: String,
        analysis_id: String,
    },
    #[error("analysis run receipt no longer owns '{analysis_id}'")]
    ReceiptMismatch { analysis_id: String },
    #[error("analysis '{analysis_id}' is busy in phase '{phase}'")]
    Busy { analysis_id: String, phase: String },
    #[error("analysis '{analysis_id}' cleanup task failed: {error}")]
    CleanupFailed { analysis_id: String, error: String },
    #[error("analysis operation failed: {0}")]
    Execution(String),
}

#[derive(Default)]
pub struct AnalysisRunSupervisor {
    entries: dashmap::DashMap<String, Arc<AnalysisRunEntry>>,
    completed: dashmap::DashMap<String, CompletedAnalysisRun>,
    completion_sequence: std::sync::atomic::AtomicU64,
    admission: std::sync::Mutex<()>,
    closed: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
struct CompletedAnalysisRun {
    sequence: u64,
    receipt: AnalysisRunReceipt,
    result: Result<crate::analysis::AnalysisDocument, String>,
}

struct AnalysisRunEntry {
    receipt: AnalysisRunReceipt,
    _control: Option<ScopedWorkspaceControl>,
    cancel: Arc<CancellationToken>,
    phase: std::sync::atomic::AtomicU8,
    exclusive: bool,
    terminal: std::sync::Mutex<Option<AnalysisTaskTerminal>>,
    _monitor: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    settled: tokio::sync::Notify,
}

struct AnalysisDeleteReceipt {
    supervisor: Arc<AnalysisRunSupervisor>,
    key: String,
    entry: Arc<AnalysisRunEntry>,
}

impl Drop for AnalysisDeleteReceipt {
    fn drop(&mut self) {
        self.entry
            .phase
            .store(PHASE_JOINED, std::sync::atomic::Ordering::Release);
        self.entry.settled.notify_waiters();
        remove_analysis_owner(&self.supervisor.entries, &self.key, &self.entry);
    }
}

#[derive(Clone)]
enum AnalysisTaskTerminal {
    Joined(Box<Result<crate::analysis::AnalysisDocument, String>>),
    CleanupFailed(String),
}

impl AnalysisRunSupervisor {
    fn publish_completed(
        &self,
        active_key: &str,
        entry: &Arc<AnalysisRunEntry>,
        result: Result<crate::analysis::AnalysisDocument, String>,
    ) {
        let sequence = self
            .completion_sequence
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| current.checked_add(1),
            )
            .unwrap_or_else(|current| current);
        self.completed.insert(
            entry.receipt.owner_id.clone(),
            CompletedAnalysisRun {
                sequence,
                receipt: entry.receipt.clone(),
                result,
            },
        );
        remove_analysis_owner(&self.entries, active_key, entry);
        while self.completed.len() > COMPLETED_ANALYSIS_LIMIT {
            let oldest = self
                .completed
                .iter()
                .min_by_key(|completed| completed.sequence)
                .map(|completed| completed.key().clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.completed.remove(&oldest);
        }
    }

    pub fn begin_shutdown(&self) {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        drop(admission);
        for entry in self.entries.iter() {
            if entry.exclusive {
                continue;
            }
            let _ = entry.phase.compare_exchange(
                PHASE_STARTED,
                PHASE_DRAINING,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            );
            entry.cancel.cancel();
        }
    }

    pub async fn join_shutdown(&self) -> Vec<AnalysisCancelReceipt> {
        let entries = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), Arc::clone(entry.value())))
            .collect::<Vec<_>>();
        let mut receipts = Vec::with_capacity(entries.len());
        for (key, entry) in entries {
            let receipt = entry.receipt.clone();
            let outcome = if entry.exclusive {
                match tokio::time::timeout(ANALYSIS_DRAIN_TIMEOUT, wait_exclusive_release(&entry))
                    .await
                {
                    Ok(()) => AnalysisCancelReceipt::Joined {
                        receipt,
                        execution_error: None,
                    },
                    Err(_) => AnalysisCancelReceipt::CleanupTimedOut {
                        receipt,
                        timeout_seconds: ANALYSIS_DRAIN_TIMEOUT.as_secs(),
                    },
                }
            } else {
                match tokio::time::timeout(ANALYSIS_DRAIN_TIMEOUT, wait_terminal(&entry)).await {
                    Err(_) => AnalysisCancelReceipt::CleanupTimedOut {
                        receipt,
                        timeout_seconds: ANALYSIS_DRAIN_TIMEOUT.as_secs(),
                    },
                    Ok(AnalysisTaskTerminal::CleanupFailed(error)) => {
                        AnalysisCancelReceipt::CleanupFailed { receipt, error }
                    }
                    Ok(AnalysisTaskTerminal::Joined(result)) => {
                        remove_analysis_owner(&self.entries, &key, &entry);
                        self.ack_completed(&receipt);
                        AnalysisCancelReceipt::Joined {
                            receipt,
                            execution_error: (*result).err(),
                        }
                    }
                }
            };
            receipts.push(outcome);
        }
        receipts
    }

    pub async fn shutdown(&self) -> Vec<AnalysisCancelReceipt> {
        self.begin_shutdown();
        self.join_shutdown().await
    }

    fn start(
        self: &Arc<Self>,
        product_data: ScopedProductData,
        analysis_id: &str,
    ) -> Result<AnalysisRunReceipt, AnalysisRunControlError> {
        let product_data_flow = product_data
            .begin_runtime_flow("run file-backed analysis")
            .map_err(|error| AnalysisRunControlError::Execution(error.to_string()))?;
        let runner_flow = product_data_flow.clone();
        let runner_product_data = product_data.clone();
        let runner_analysis_id = analysis_id.to_string();
        let cancel = Arc::new(CancellationToken::new());
        let runner_cancel = Arc::clone(&cancel);
        let future = async move {
            crate::analysis::run_analysis_with_product_data(
                &runner_product_data,
                &runner_flow,
                &runner_analysis_id,
                Some(runner_cancel),
            )
            .await
            .map_err(|error| error.to_string())
            .inspect(|_| runner_flow.settle(None))
            .inspect_err(|error| runner_flow.settle(Some(error.clone())))
        };
        self.start_owned(
            product_data,
            analysis_id,
            cancel,
            Some(product_data_flow),
            future,
        )
    }

    fn start_owned<F>(
        self: &Arc<Self>,
        product_data: ScopedProductData,
        analysis_id: &str,
        cancel: Arc<CancellationToken>,
        product_data_flow: Option<ProductDataIoFlow>,
        future: F,
    ) -> Result<AnalysisRunReceipt, AnalysisRunControlError>
    where
        F: std::future::Future<Output = Result<crate::analysis::AnalysisDocument, String>>
            + Send
            + 'static,
    {
        let key = analysis_cancel_key(product_data.workspace_id(), analysis_id);
        let receipt = AnalysisRunReceipt {
            workspace_id: product_data.workspace_id().to_string(),
            workspace_generation: product_data.generation(),
            analysis_id: analysis_id.to_string(),
            owner_id: uuid::Uuid::new_v4().to_string(),
        };
        let entry = Arc::new(AnalysisRunEntry {
            receipt: receipt.clone(),
            _control: Some(product_data.settlement_receipt()),
            cancel,
            phase: std::sync::atomic::AtomicU8::new(PHASE_STARTED),
            exclusive: false,
            terminal: std::sync::Mutex::new(None),
            _monitor: std::sync::Mutex::new(None),
            settled: tokio::sync::Notify::new(),
        });
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(flow) = product_data_flow.as_ref() {
                flow.settle(None);
            }
            return Err(AnalysisRunControlError::SupervisorClosed);
        }
        match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(existing) => {
                if let Some(flow) = product_data_flow.as_ref() {
                    flow.settle(None);
                }
                return Err(AnalysisRunControlError::AlreadyRunning {
                    analysis_id: analysis_id.to_string(),
                    owner_id: existing.get().receipt.owner_id.clone(),
                });
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(Arc::clone(&entry));
            }
        }
        drop(admission);

        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                remove_analysis_owner(&self.entries, &key, &entry);
                if let Some(flow) = product_data_flow.as_ref() {
                    flow.settle(None);
                }
                return Err(AnalysisRunControlError::Execution(format!(
                    "analysis supervisor requires a Tokio runtime: {error}"
                )));
            }
        };
        let runner = runtime.spawn(future);
        let monitored_entry = Arc::clone(&entry);
        let monitor_supervisor = Arc::clone(self);
        let monitor_key = key.clone();
        let monitor = runtime.spawn(async move {
            let terminal = match runner.await {
                Ok(result) => AnalysisTaskTerminal::Joined(Box::new(result)),
                Err(error) => AnalysisTaskTerminal::CleanupFailed(error.to_string()),
            };
            monitored_entry.phase.store(
                match &terminal {
                    AnalysisTaskTerminal::Joined(_) => PHASE_JOINED,
                    AnalysisTaskTerminal::CleanupFailed(_) => PHASE_CLEANUP_FAILED,
                },
                std::sync::atomic::Ordering::Release,
            );
            if let AnalysisTaskTerminal::Joined(result) = &terminal {
                monitor_supervisor.publish_completed(
                    &monitor_key,
                    &monitored_entry,
                    result.as_ref().clone(),
                );
            }
            let mut stored = monitored_entry
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *stored = Some(terminal);
            drop(stored);
            monitored_entry.settled.notify_waiters();
        });
        let mut monitor_owner = entry
            ._monitor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *monitor_owner = Some(monitor);
        drop(monitor_owner);
        Ok(receipt)
    }

    fn receipt(
        &self,
        product_data: &ScopedProductData,
        analysis_id: &str,
    ) -> Result<AnalysisRunReceipt, AnalysisRunControlError> {
        let key = analysis_cancel_key(product_data.workspace_id(), analysis_id);
        self.entries
            .get(&key)
            .map(|entry| entry.receipt.clone())
            .ok_or_else(|| AnalysisRunControlError::NotFound {
                workspace_id: product_data.workspace_id().to_string(),
                analysis_id: analysis_id.to_string(),
            })
    }

    async fn wait(
        &self,
        product_data: &ScopedProductData,
        receipt: &AnalysisRunReceipt,
    ) -> Result<crate::analysis::AnalysisDocument, AnalysisRunControlError> {
        if let Some(result) = self.take_completed(product_data, receipt)? {
            return result.map_err(AnalysisRunControlError::Execution);
        }
        let (key, entry) = self.entry_for_receipt(product_data, receipt)?;
        let terminal = wait_terminal(&entry).await;
        match terminal {
            AnalysisTaskTerminal::Joined(result) => {
                remove_analysis_owner(&self.entries, &key, &entry);
                self.ack_completed(receipt);
                (*result).map_err(AnalysisRunControlError::Execution)
            }
            AnalysisTaskTerminal::CleanupFailed(error) => {
                Err(AnalysisRunControlError::CleanupFailed {
                    analysis_id: receipt.analysis_id.clone(),
                    error,
                })
            }
        }
    }

    fn poll(
        &self,
        product_data: &ScopedProductData,
        receipt: &AnalysisRunReceipt,
    ) -> Result<AnalysisWaitReceipt, AnalysisRunControlError> {
        if let Some(result) = self.take_completed(product_data, receipt)? {
            return Ok(match result {
                Ok(document) => AnalysisWaitReceipt::Joined {
                    receipt: receipt.clone(),
                    document: Some(Box::new(document)),
                    execution_error: None,
                },
                Err(error) => AnalysisWaitReceipt::Joined {
                    receipt: receipt.clone(),
                    document: None,
                    execution_error: Some(error),
                },
            });
        }
        let (key, entry) = self.entry_for_receipt(product_data, receipt)?;
        let terminal = entry
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        match terminal {
            None => Ok(AnalysisWaitReceipt::Started {
                receipt: receipt.clone(),
                draining: entry.phase.load(std::sync::atomic::Ordering::Acquire) == PHASE_DRAINING,
            }),
            Some(AnalysisTaskTerminal::Joined(result)) => {
                remove_analysis_owner(&self.entries, &key, &entry);
                self.ack_completed(receipt);
                match *result {
                    Ok(document) => Ok(AnalysisWaitReceipt::Joined {
                        receipt: receipt.clone(),
                        document: Some(Box::new(document)),
                        execution_error: None,
                    }),
                    Err(error) => Ok(AnalysisWaitReceipt::Joined {
                        receipt: receipt.clone(),
                        document: None,
                        execution_error: Some(error),
                    }),
                }
            }
            Some(AnalysisTaskTerminal::CleanupFailed(error)) => {
                Ok(AnalysisWaitReceipt::CleanupFailed {
                    receipt: receipt.clone(),
                    error,
                })
            }
        }
    }

    async fn cancel_and_join(
        &self,
        product_data: &ScopedProductData,
        analysis_id: &str,
    ) -> Result<AnalysisCancelReceipt, AnalysisRunControlError> {
        let receipt = self.receipt(product_data, analysis_id)?;
        let (key, entry) = self.entry_for_receipt(product_data, &receipt)?;
        let _ = entry.phase.compare_exchange(
            PHASE_STARTED,
            PHASE_DRAINING,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
        entry.cancel.cancel();
        let terminal =
            match tokio::time::timeout(ANALYSIS_DRAIN_TIMEOUT, wait_terminal(&entry)).await {
                Ok(terminal) => terminal,
                Err(_) => {
                    return Ok(AnalysisCancelReceipt::CleanupTimedOut {
                        receipt,
                        timeout_seconds: ANALYSIS_DRAIN_TIMEOUT.as_secs(),
                    });
                }
            };
        match terminal {
            AnalysisTaskTerminal::Joined(result) => {
                remove_analysis_owner(&self.entries, &key, &entry);
                self.ack_completed(&receipt);
                Ok(AnalysisCancelReceipt::Joined {
                    receipt,
                    execution_error: (*result).err(),
                })
            }
            AnalysisTaskTerminal::CleanupFailed(error) => {
                Ok(AnalysisCancelReceipt::CleanupFailed { receipt, error })
            }
        }
    }

    fn claim_exclusive(
        self: &Arc<Self>,
        product_data: &ScopedProductData,
        analysis_id: &str,
        phase: u8,
        owner_kind: &str,
    ) -> Result<AnalysisDeleteReceipt, AnalysisRunControlError> {
        let key = analysis_cancel_key(product_data.workspace_id(), analysis_id);
        let entry = Arc::new(AnalysisRunEntry {
            receipt: AnalysisRunReceipt {
                workspace_id: product_data.workspace_id().to_string(),
                workspace_generation: product_data.generation(),
                analysis_id: analysis_id.to_string(),
                owner_id: format!("{owner_kind}:{}", uuid::Uuid::new_v4()),
            },
            _control: Some(product_data.settlement_receipt()),
            cancel: Arc::new(CancellationToken::new()),
            phase: std::sync::atomic::AtomicU8::new(phase),
            exclusive: true,
            terminal: std::sync::Mutex::new(None),
            _monitor: std::sync::Mutex::new(None),
            settled: tokio::sync::Notify::new(),
        });
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AnalysisRunControlError::SupervisorClosed);
        }
        match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(active) => Err(AnalysisRunControlError::Busy {
                analysis_id: analysis_id.to_string(),
                phase: analysis_phase(
                    active
                        .get()
                        .phase
                        .load(std::sync::atomic::Ordering::Acquire),
                )
                .to_string(),
            }),
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(Arc::clone(&entry));
                let receipt = AnalysisDeleteReceipt {
                    supervisor: Arc::clone(self),
                    key,
                    entry,
                };
                drop(admission);
                Ok(receipt)
            }
        }
    }

    #[cfg(test)]
    fn ensure_key_idle(
        &self,
        workspace_id: &str,
        analysis_id: &str,
    ) -> Result<(), AnalysisRunControlError> {
        let key = analysis_cancel_key(workspace_id, analysis_id);
        match self.entries.get(&key) {
            Some(entry) => Err(AnalysisRunControlError::Busy {
                analysis_id: analysis_id.to_string(),
                phase: analysis_phase(entry.phase.load(std::sync::atomic::Ordering::Acquire))
                    .to_string(),
            }),
            None => Ok(()),
        }
    }

    fn entry_for_receipt(
        &self,
        product_data: &ScopedProductData,
        receipt: &AnalysisRunReceipt,
    ) -> Result<(String, Arc<AnalysisRunEntry>), AnalysisRunControlError> {
        if receipt.workspace_id != product_data.workspace_id()
            || receipt.workspace_generation != product_data.generation()
        {
            return Err(AnalysisRunControlError::ReceiptMismatch {
                analysis_id: receipt.analysis_id.clone(),
            });
        }
        let key = analysis_cancel_key(&receipt.workspace_id, &receipt.analysis_id);
        let entry = self
            .entries
            .get(&key)
            .map(|entry| Arc::clone(entry.value()));
        match entry {
            Some(entry) if entry.receipt.owner_id == receipt.owner_id => Ok((key, entry)),
            _ => Err(AnalysisRunControlError::ReceiptMismatch {
                analysis_id: receipt.analysis_id.clone(),
            }),
        }
    }

    fn take_completed(
        &self,
        product_data: &ScopedProductData,
        receipt: &AnalysisRunReceipt,
    ) -> Result<Option<Result<crate::analysis::AnalysisDocument, String>>, AnalysisRunControlError>
    {
        let completed = self
            .completed
            .get(&receipt.owner_id)
            .map(|entry| entry.clone());
        match completed {
            None => Ok(None),
            Some(completed)
                if completed.receipt.workspace_id == product_data.workspace_id()
                    && completed.receipt.workspace_id == receipt.workspace_id
                    && completed.receipt.workspace_generation == product_data.generation()
                    && completed.receipt.workspace_generation == receipt.workspace_generation
                    && completed.receipt.analysis_id == receipt.analysis_id =>
            {
                self.ack_completed(receipt);
                Ok(Some(completed.result))
            }
            Some(_) => Err(AnalysisRunControlError::ReceiptMismatch {
                analysis_id: receipt.analysis_id.clone(),
            }),
        }
    }

    fn ack_completed(&self, receipt: &AnalysisRunReceipt) {
        self.completed.remove_if(&receipt.owner_id, |_, current| {
            current.receipt.workspace_id == receipt.workspace_id
                && current.receipt.workspace_generation == receipt.workspace_generation
                && current.receipt.analysis_id == receipt.analysis_id
                && current.receipt.owner_id == receipt.owner_id
        });
    }
}

async fn wait_terminal(entry: &AnalysisRunEntry) -> AnalysisTaskTerminal {
    loop {
        let notified = entry.settled.notified();
        let terminal = {
            entry
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        };
        if let Some(terminal) = terminal {
            return terminal;
        }
        notified.await;
    }
}

async fn wait_exclusive_release(entry: &AnalysisRunEntry) {
    loop {
        let notified = entry.settled.notified();
        if entry.phase.load(std::sync::atomic::Ordering::Acquire) == PHASE_JOINED {
            return;
        }
        notified.await;
    }
}

fn remove_analysis_owner(
    entries: &dashmap::DashMap<String, Arc<AnalysisRunEntry>>,
    key: &str,
    expected: &Arc<AnalysisRunEntry>,
) {
    entries.remove_if(key, |_, current| Arc::ptr_eq(current, expected));
}

fn analysis_phase(phase: u8) -> &'static str {
    match phase {
        PHASE_STARTED => "started",
        PHASE_DRAINING => "draining",
        PHASE_JOINED => "joined_pending_receipt",
        PHASE_CLEANUP_FAILED => "cleanup_failed",
        PHASE_DELETING => "deleting",
        PHASE_EDITING => "editing",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn blocking_product_data_io_keeps_async_heartbeat_responsive() -> Result<(), String> {
        let service = super::ProductDataIoService::new();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let operation_service = service.clone();
        let operation = tokio::spawn(async move {
            operation_service
                .run("heartbeat fixture", move || {
                    release_rx
                        .recv()
                        .map_err(|error| format!("blocking fixture release failed: {error}"))
                })
                .await
        });

        tokio::time::timeout(Duration::from_millis(250), async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "Tokio heartbeat stalled behind product-data I/O".to_string())?;
        release_tx
            .send(())
            .map_err(|error| format!("blocking fixture release send failed: {error}"))?;
        operation
            .await
            .map_err(|error| format!("product-data I/O test task failed: {error}"))?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        service.join_shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_detach_owned_product_data_operation() -> Result<(), String> {
        let service = super::ProductDataIoService::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let operation_service = service.clone();
        let caller = tokio::spawn(async move {
            operation_service
                .run("caller-drop fixture", move || {
                    let _entered = entered_tx.send(());
                    release_rx.recv().map_err(|error| error.to_string())
                })
                .await
        });
        entered_rx.await.map_err(|error| error.to_string())?;
        caller.abort();
        let _cancelled = caller.await;
        service.begin_shutdown()?;
        let join = tokio::spawn(async move { service.join_shutdown().await });
        tokio::task::yield_now().await;
        if join.is_finished() {
            return Err("product-data shutdown ignored the detached operation".to_string());
        }
        release_tx.send(()).map_err(|error| error.to_string())?;
        join.await.map_err(|error| error.to_string())??;
        Ok(())
    }

    #[tokio::test]
    async fn application_services_do_not_close_each_other_or_poison_restart() -> Result<(), String>
    {
        let first = super::ProductDataIoService::new();
        let second = super::ProductDataIoService::new();
        first.begin_shutdown()?;
        first.join_shutdown().await?;
        second
            .run("second application", || 7_u8)
            .await
            .map_err(|error| error.to_string())?;
        second.begin_shutdown()?;
        second.join_shutdown().await?;

        let restarted = super::ProductDataIoService::new();
        let value = restarted
            .run("restarted application", || 11_u8)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(value, 11);
        restarted.begin_shutdown()?;
        restarted.join_shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn application_shutdown_joins_only_its_own_active_operations() -> Result<(), String> {
        let first = super::ProductDataIoService::new();
        let second = super::ProductDataIoService::new();
        let (first_entered_tx, first_entered_rx) = tokio::sync::oneshot::channel();
        let (first_release_tx, first_release_rx) = std::sync::mpsc::sync_channel(0);
        let first_operation_service = first.clone();
        let first_operation = tokio::spawn(async move {
            first_operation_service
                .run("first application operation", move || {
                    let _entered = first_entered_tx.send(());
                    first_release_rx.recv().map_err(|error| error.to_string())
                })
                .await
        });
        let (second_entered_tx, second_entered_rx) = tokio::sync::oneshot::channel();
        let (second_release_tx, second_release_rx) = std::sync::mpsc::sync_channel(0);
        let second_operation_service = second.clone();
        let second_operation = tokio::spawn(async move {
            second_operation_service
                .run("second application operation", move || {
                    let _entered = second_entered_tx.send(());
                    second_release_rx.recv().map_err(|error| error.to_string())
                })
                .await
        });
        first_entered_rx.await.map_err(|error| error.to_string())?;
        second_entered_rx.await.map_err(|error| error.to_string())?;

        first.begin_shutdown()?;
        let first_join_service = first.clone();
        let first_join = tokio::spawn(async move { first_join_service.join_shutdown().await });
        tokio::task::yield_now().await;
        if first_join.is_finished() {
            return Err("first application ignored its accepted operation".to_string());
        }
        first_release_tx
            .send(())
            .map_err(|error| error.to_string())?;
        first_join.await.map_err(|error| error.to_string())??;
        first_operation
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

        let second_probe = second
            .run("second application remains open", || 17_u8)
            .await
            .map_err(|error| error.to_string())?;
        if second_probe != 17 {
            return Err("second application returned an unexpected probe".to_string());
        }
        second_release_tx
            .send(())
            .map_err(|error| error.to_string())?;
        second_operation
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        second.join_shutdown().await?;

        let restarted = super::ProductDataIoService::new();
        restarted
            .run("later application generation", || ())
            .await
            .map_err(|error| error.to_string())?;
        restarted.join_shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn sealed_service_allows_owned_flow_nested_io_and_reports_debt() -> Result<(), String> {
        let service = super::ProductDataIoService::new();
        let flow = service
            .begin_owned_flow("owned deletion flow")
            .map_err(|error| error.to_string())?;
        service.begin_shutdown()?;
        let value = flow
            .run("nested deletion I/O", || 23_u8)
            .await
            .map_err(|error| error.to_string())?;
        if value != 23 {
            return Err("owned flow nested I/O returned an unexpected value".to_string());
        }
        if service.run("new standalone operation", || ()).await.is_ok() {
            return Err("sealed service admitted a new standalone operation".to_string());
        }
        flow.settle(Some("injected typed deletion debt".to_string()));
        let debt = service
            .join_shutdown()
            .await
            .err()
            .ok_or_else(|| "owned flow debt was not reported at shutdown".to_string())?;
        if !debt.contains("injected typed deletion debt") {
            return Err(format!("shutdown reported the wrong debt: {debt}"));
        }
        Ok(())
    }

    #[test]
    fn stale_analysis_cleanup_does_not_remove_replacement_owner() {
        let entries = dashmap::DashMap::new();
        let key = super::analysis_cancel_key("workspace-a", "shared");
        let stale = analysis_entry("stale");
        entries.insert(key.clone(), Arc::clone(&stale));
        entries.remove(&key);
        let replacement = analysis_entry("replacement");
        entries.insert(key.clone(), Arc::clone(&replacement));

        super::remove_analysis_owner(&entries, &key, &stale);

        assert!(
            entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current.value(), &replacement))
        );
    }

    #[test]
    fn analysis_cancellation_key_is_workspace_scoped_and_collision_safe() {
        assert_ne!(
            super::analysis_cancel_key("workspace-a", "shared"),
            super::analysis_cancel_key("workspace-b", "shared")
        );
        assert_ne!(
            super::analysis_cancel_key("workspace:a", "b"),
            super::analysis_cancel_key("workspace", "a:b")
        );
    }

    #[tokio::test]
    async fn two_phase_shutdown_closes_admission_before_join() {
        let supervisor = super::AnalysisRunSupervisor::default();
        supervisor.begin_shutdown();
        assert!(supervisor.closed.load(std::sync::atomic::Ordering::Acquire));
        assert!(supervisor.join_shutdown().await.is_empty());
    }

    #[test]
    fn delete_is_busy_until_the_exact_analysis_owner_is_joined() {
        let supervisor = super::AnalysisRunSupervisor::default();
        let key = super::analysis_cancel_key("workspace-a", "shared");
        let entry = analysis_entry("active");
        entry.phase.store(
            super::PHASE_CLEANUP_FAILED,
            std::sync::atomic::Ordering::Release,
        );
        let mut terminal = entry
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *terminal = Some(super::AnalysisTaskTerminal::CleanupFailed(
            "backend cleanup failed".to_string(),
        ));
        drop(terminal);
        supervisor.entries.insert(key, entry);

        assert!(matches!(
            supervisor.ensure_key_idle("workspace-a", "shared"),
            Err(super::AnalysisRunControlError::Busy { .. })
        ));
        assert!(supervisor.ensure_key_idle("workspace-b", "shared").is_ok());
    }

    #[test]
    fn joined_analysis_releases_active_owner_without_polling() {
        let supervisor = super::AnalysisRunSupervisor::default();
        let key = super::analysis_cancel_key("workspace-a", "shared");
        let entry = analysis_entry("joined-owner");
        let receipt = entry.receipt.clone();
        supervisor.entries.insert(key.clone(), Arc::clone(&entry));

        supervisor.publish_completed(&key, &entry, Err("fixture result".to_string()));

        assert!(supervisor.ensure_key_idle("workspace-a", "shared").is_ok());
        assert!(supervisor.completed.contains_key(&receipt.owner_id));
        let rerun = analysis_entry("rerun-owner");
        let rerun_admitted = match supervisor.entries.entry(key) {
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(Arc::clone(&rerun));
                true
            }
            dashmap::mapref::entry::Entry::Occupied(_) => false,
        };
        assert!(rerun_admitted);
        supervisor.ack_completed(&receipt);
        assert!(!supervisor.completed.contains_key(&receipt.owner_id));
    }

    fn analysis_entry(owner_id: &str) -> Arc<super::AnalysisRunEntry> {
        Arc::new(super::AnalysisRunEntry {
            receipt: super::AnalysisRunReceipt {
                workspace_id: "workspace-a".to_string(),
                workspace_generation: "generation-a".to_string(),
                analysis_id: "shared".to_string(),
                owner_id: owner_id.to_string(),
            },
            _control: None,
            cancel: Arc::new(CancellationToken::new()),
            phase: std::sync::atomic::AtomicU8::new(super::PHASE_STARTED),
            exclusive: false,
            terminal: std::sync::Mutex::new(None),
            _monitor: std::sync::Mutex::new(None),
            settled: tokio::sync::Notify::new(),
        })
    }
}
