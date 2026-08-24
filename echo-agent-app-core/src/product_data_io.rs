//! Bounded async boundary for EKO's synchronous product-data I/O.
//!
//! File, research, and analysis stores intentionally remain simple
//! filesystem-backed domain modules. Async surfaces must enter them through
//! this adapter so blocking filesystem work cannot occupy Tokio workers.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::state::ScopedWorkspaceControl;

const PROCESS_PRODUCT_DATA_IO_LIMIT: usize = 8;
static PROCESS_PRODUCT_DATA_IO: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(PROCESS_PRODUCT_DATA_IO_LIMIT)));

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
}

/// Run one synchronous product-data operation on the bounded blocking pool.
///
/// The closure may itself return a domain `Result`; keeping that result nested
/// preserves typed errors for the surface adapter instead of erasing them.
pub async fn run<T, F>(operation: &'static str, function: F) -> Result<T, ProductDataIoError>
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
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        function()
    })
    .await
    .map_err(|error| ProductDataIoError::Join {
        operation,
        error: error.to_string(),
    })
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
}

impl ScopedProductData {
    pub(crate) fn new(
        control: ScopedWorkspaceControl,
        analysis_runs: Arc<AnalysisRunSupervisor>,
    ) -> Self {
        Self {
            control,
            analysis_runs,
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
        run(operation, move || function(control.data_root())).await
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
        run(operation, move || {
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
            .start_owned(self.clone(), analysis_id, cancel, async move {
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
        let runner_product_data = product_data.clone();
        let runner_analysis_id = analysis_id.to_string();
        let cancel = Arc::new(CancellationToken::new());
        let runner_cancel = Arc::clone(&cancel);
        self.start_owned(product_data, analysis_id, cancel, async move {
            crate::analysis::run_analysis_with_product_data(
                &runner_product_data,
                &runner_analysis_id,
                Some(runner_cancel),
            )
            .await
            .map_err(|error| error.to_string())
        })
    }

    fn start_owned<F>(
        self: &Arc<Self>,
        product_data: ScopedProductData,
        analysis_id: &str,
        cancel: Arc<CancellationToken>,
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
            return Err(AnalysisRunControlError::SupervisorClosed);
        }
        match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(existing) => {
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

        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            remove_analysis_owner(&self.entries, &key, &entry);
            AnalysisRunControlError::Execution(format!(
                "analysis supervisor requires a Tokio runtime: {error}"
            ))
        })?;
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
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let operation = tokio::spawn(super::run("heartbeat fixture", move || {
            release_rx
                .recv()
                .map_err(|error| format!("blocking fixture release failed: {error}"))
        }));

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
