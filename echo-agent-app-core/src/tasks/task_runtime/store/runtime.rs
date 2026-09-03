/// Goals longer than this spill the full text to `objective.md` next to the
/// journal; in-context projections stay bounded and point at the artifact.
const OBJECTIVE_ARTIFACT_SPILL_CHARS: usize = 8_000;

impl TaskRuntimeStore {
    /// Phase-one process shutdown: close driver admission and broadcast every
    /// accepted driver cancellation without awaiting settlement.
    pub fn begin_run_driver_shutdown(&self) -> Result<(), String> {
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| "TaskRun driver supervisor lock is poisoned".to_string())?;
        supervisor.accepting = false;
        for cancel in supervisor.driver_cancels.values() {
            cancel.cancel();
        }
        super::continuation::shutdown(self);
        Ok(())
    }

    /// Whether the process runtime still accepts new finite TaskRun drivers.
    /// Long-horizon coordinators use this to stop cleanly during application
    /// shutdown and leave durable recovery to the next process.
    pub fn is_run_driver_admission_open(&self) -> bool {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| supervisor.accepting)
            .unwrap_or(false)
    }

    /// Create the store at the default location.
    ///
    /// task/plan data lives under the file shadow root (`~/.eko/tasks/`);
    /// No database is opened. Root authority, durable directory creation, and
    /// cross-process lease failures are propagated to bootstrap.
    pub fn new() -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(
            super::file_shadow::FileTaskShadow::default_root(),
        )?);
        Ok(Self::with_shadow(shadow, "global"))
    }

    /// Open one workspace-owned runtime store at its immutable task root.
    ///
    /// Unlike [`Self::rebind_shadow_root`], this constructor never changes an
    /// existing runtime generation. Independent workspace hosts therefore keep
    /// distinct cancellation, continuation, hook, and file-authority owners.
    pub fn open_for_workspace(
        shadow_root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(shadow_root)?);
        Ok(Self::with_shadow(shadow, workspace_id.into()))
    }

    fn with_shadow(
        shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
        workspace_id: impl Into<String>,
    ) -> Self {
        Self {
            task_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_subagent_controls: std::sync::Mutex::new(std::collections::HashMap::new()),
            run_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            next_run_cancel_registration: std::sync::atomic::AtomicU64::new(0),
            run_driver_supervisor: std::sync::Mutex::new(RunDriverSupervisor::default()),
            run_driver_admission_idle: tokio::sync::Notify::new(),
            run_driver_idle: tokio::sync::Notify::new(),
            continuation_runtime: std::sync::OnceLock::new(),
            boot_reconciler: std::sync::OnceLock::new(),
            execution_target_resolver: std::sync::RwLock::new(None),
            command_cell_runtime: std::sync::RwLock::new(None),
            #[cfg(test)]
            run_driver_shutdown_started: tokio::sync::Notify::new(),
            #[cfg(test)]
            abort_next_run_driver_shutdown_reporter: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            run_driver_admission_test_barrier: std::sync::Mutex::new(None),
            #[cfg(test)]
            run_driver_registration_test_barrier: std::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            fail_next_run_driver_registration: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_recovery_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_recovery_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_cell_started: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_cell_started_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_runtime_mutation_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_cell_terminal_remaining: std::sync::atomic::AtomicUsize::new(0),
            shadow,
            shadow_generation: std::sync::Mutex::new(ShadowGeneration {
                active_operations: 0,
                workspace_id: workspace_id.into(),
                transitioning: false,
            }),
            hook_event_dispatcher: std::sync::Mutex::new(None),
            plan_locks: dashmap::DashMap::new(),
            operation_supervisor: super::executor::TaskRuntimeOperationSupervisor::new(),
        }
    }

    pub(crate) fn operation_supervisor(
        &self,
    ) -> std::sync::Arc<super::executor::TaskRuntimeOperationSupervisor> {
        std::sync::Arc::clone(&self.operation_supervisor)
    }

    pub fn active_operation_count(&self) -> usize {
        self.operation_supervisor.active_count()
    }

    pub fn begin_operation_shutdown(&self) -> Result<(), String> {
        self.operation_supervisor.begin_shutdown()
    }

    pub async fn shutdown_operations(&self) -> Result<(), String> {
        self.begin_operation_shutdown()?;
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.operation_supervisor.join(),
        )
        .await
        .map_err(|_| "TaskRuntime operation shutdown timed out after 30 seconds".to_string())?
    }

    /// In-memory store for tests / fallback. The file shadow is backed by a
    /// per-process temp dir so every test exercises the file-authority path.
    pub fn new_in_memory() -> anyhow::Result<Self> {
        let shadow_root = std::env::temp_dir().join(format!(
            "echo-agent-task-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Self::new_in_memory_with_shadow_root(shadow_root)
    }

    /// In-memory store whose file shadow is rooted at `shadow_root`. Tests use
    /// this (with a `tempfile::tempdir()` root) so they can read the written
    /// `events.jsonl` / projection files back directly and so runs are isolated
    /// under a known directory. Replaces the old `attach_shadow` test hook.
    pub fn new_in_memory_with_shadow_root(shadow_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(shadow_root)?);
        Ok(Self::with_shadow(shadow, "test"))
    }

    /// Attach the application-layer HookEventDispatcher so every event written
    /// via `append_event_line` is translated into framework HookEvents.
    ///
    /// Idempotent (first call wins). Intended to be called once during
    /// bootstrap, after the agent + bridges exist (the store is built earlier).
    /// Until attached, task/subagent events are not dispatched to hooks.
    pub fn attach_hook_event_dispatcher(
        &self,
        dispatcher: super::hook_event_dispatcher::HookEventDispatcher,
    ) -> Result<bool, StoreError> {
        let Ok(mut owned_dispatcher) = self.hook_event_dispatcher.lock() else {
            tracing::warn!("HookEventDispatcher ownership lock is poisoned");
            return Err(StoreError::LockPoisoned);
        };
        if owned_dispatcher.is_some() {
            return Ok(false);
        }
        let event_dispatcher = dispatcher.clone();
        let hook: std::sync::Arc<dyn Fn(&super::types::RuntimeTaskEvent) + Send + Sync> =
            std::sync::Arc::new(move |event| {
                if let Err(error) = event_dispatcher.dispatch(event) {
                    tracing::warn!(%error, "Failed to enqueue task hook event");
                }
            });
        let _operation = self.shadow_operation()?;
        if !self.shadow.try_attach_event_hook(hook) {
            return Ok(false);
        }
        *owned_dispatcher = Some(dispatcher);
        Ok(true)
    }

    pub fn attach_execution_target_resolver(
        &self,
        resolver: std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>,
    ) {
        *self
            .execution_target_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resolver);
    }

    pub(crate) fn execution_target_resolver(
        &self,
    ) -> Option<std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>> {
        self.execution_target_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Wait for every persisted task/subagent hook event to finish firing.
    pub async fn flush_hook_events(&self) -> Result<(), String> {
        let dispatcher = self
            .hook_event_dispatcher
            .lock()
            .map_err(|_| "HookEventDispatcher ownership lock is poisoned".to_string())?
            .clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.flush().await
        } else {
            Ok(())
        }
    }

    /// Drain and stop the hook consumer. Repeated calls are harmless.
    pub async fn shutdown_hook_events(&self) -> Result<(), String> {
        let dispatcher = self
            .hook_event_dispatcher
            .lock()
            .map_err(|_| "HookEventDispatcher ownership lock is poisoned".to_string())?
            .clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.shutdown().await
        } else {
            Ok(())
        }
    }

    /// Stop accepting TaskRun drivers, cancel every accepted driver, and await
    /// their owned settlement before the store's hook consumer is torn down.
    pub async fn shutdown_run_drivers(
        self: &std::sync::Arc<Self>,
    ) -> Result<(), TaskRunDriverShutdownError> {
        self.begin_run_driver_shutdown()
            .map_err(|error| TaskRunDriverShutdownError {
                driver_errors: vec![error],
                abandoned_settlements: Vec::new(),
            })?;
        let (mut shutdown_result, shutdown_sender, shutdown_reporter) = {
            let mut supervisor =
                self.run_driver_supervisor
                    .lock()
                    .map_err(|_| TaskRunDriverShutdownError {
                        driver_errors: vec![
                            "TaskRun driver supervisor lock is poisoned".to_string(),
                        ],
                        abandoned_settlements: Vec::new(),
                    })?;
            if let (Some(sender), Some(result), Some(reporter)) = (
                supervisor.shutdown_result_sender.as_ref(),
                supervisor.shutdown_result.as_ref(),
                supervisor.shutdown_reporter.as_ref(),
            ) {
                (result.clone(), sender.clone(), reporter.clone())
            } else {
                supervisor.accepting = false;
                #[cfg(test)]
                self.run_driver_shutdown_started.notify_one();
                for cancel in supervisor.driver_cancels.values() {
                    cancel.cancel();
                }
                let (result_sender, result_receiver) = tokio::sync::watch::channel(None);
                supervisor.shutdown_result_sender = Some(result_sender.clone());
                supervisor.shutdown_result = Some(result_receiver.clone());
                let settlement_store = std::sync::Arc::clone(self);
                let owner = std::sync::Arc::new(tokio::sync::Mutex::new(
                    RunDriverShutdownOwner::Running(tokio::spawn(async move {
                        settlement_store.settle_run_driver_shutdown().await
                    })),
                ));
                supervisor.shutdown_owner = Some(owner.clone());
                let reporter = std::sync::Arc::new(tokio::sync::Mutex::new(
                    RunDriverShutdownReporter::Running(
                        self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
                    ),
                ));
                supervisor.shutdown_reporter = Some(reporter.clone());
                (result_receiver, result_sender, reporter)
            }
        };
        super::continuation::shutdown(self);

        loop {
            let observed_result = shutdown_result.borrow().clone();
            if let Some(result) = observed_result {
                return result;
            }
            tokio::select! {
                changed = shutdown_result.changed() => {
                    if changed.is_err() {
                        self.restart_run_driver_shutdown_reporter(
                            &shutdown_reporter,
                            &shutdown_sender,
                            "TaskRun driver shutdown result channel closed before publication"
                                .to_string(),
                        )
                        .await;
                    }
                }
                () = self.observe_run_driver_shutdown_reporter(
                    &shutdown_reporter,
                    &shutdown_sender,
                ) => {}
            }
        }
    }

    fn spawn_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        owner: std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownOwner>>,
        result_sender: tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
    ) -> tokio::task::JoinHandle<()> {
        let reporter_store = std::sync::Arc::clone(self);
        #[cfg(test)]
        let abort_reporter = self
            .abort_next_run_driver_shutdown_reporter
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let abort_reporter = false;
        let reporter = tokio::spawn(async move {
            if abort_reporter {
                futures::future::pending::<()>().await;
            }
            let mut result = {
                let mut owner_state = owner.lock().await;
                match &mut *owner_state {
                    RunDriverShutdownOwner::Completed(result) => result.clone(),
                    RunDriverShutdownOwner::Running(owner_handle) => {
                        let result = match owner_handle.await {
                            Ok(result) => result,
                            Err(error) => Err(TaskRunDriverShutdownError {
                                driver_errors: vec![format!(
                                    "TaskRun driver shutdown settlement owner failed: {error}"
                                )],
                                abandoned_settlements: Vec::new(),
                            }),
                        };
                        *owner_state = RunDriverShutdownOwner::Completed(result.clone());
                        result
                    }
                }
            };
            let reporter_errors = {
                let mut supervisor = reporter_store
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                std::mem::take(&mut supervisor.shutdown_reporter_errors)
            };
            for error in reporter_errors {
                add_shutdown_driver_error(&mut result, error);
            }
            result_sender.send_replace(Some(result));
        });
        if abort_reporter {
            reporter.abort();
        }
        reporter
    }

    async fn observe_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        reporter: &std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>,
        result_sender: &tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
    ) {
        let mut reporter_state = reporter.lock().await;
        let RunDriverShutdownReporter::Running(reporter_handle) = &mut *reporter_state else {
            return;
        };
        match reporter_handle.await {
            Ok(()) => {
                *reporter_state = RunDriverShutdownReporter::Completed;
            }
            Err(error) => {
                let reporter_error = format!("TaskRun driver shutdown reporter failed: {error}");
                self.run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .shutdown_reporter_errors
                    .push(reporter_error);
                let owner = self
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .shutdown_owner
                    .clone();
                let Some(owner) = owner else {
                    return;
                };
                *reporter_state = RunDriverShutdownReporter::Running(
                    self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
                );
            }
        }
    }

    async fn restart_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        reporter: &std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>,
        result_sender: &tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
        error: String,
    ) {
        let mut reporter_state = reporter.lock().await;
        self.run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown_reporter_errors
            .push(error);
        let owner = self
            .run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown_owner
            .clone();
        let Some(owner) = owner else {
            return;
        };
        *reporter_state = RunDriverShutdownReporter::Running(
            self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
        );
    }

    async fn settle_run_driver_shutdown(&self) -> Result<(), TaskRunDriverShutdownError> {
        let mut driver_settlements = loop {
            let admission_released = self.run_driver_admission_idle.notified();
            let settlements = {
                let mut supervisor = self
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for cancel in supervisor.driver_cancels.values() {
                    cancel.cancel();
                }
                if supervisor.pending_admissions == 0 {
                    Some(std::mem::take(&mut supervisor.driver_settlements))
                } else {
                    None
                }
            };
            if let Some(settlements) = settlements {
                break settlements;
            }
            admission_released.await;
        };
        let mut driver_errors = Vec::new();
        while let Some(driver) = driver_settlements.join_next().await {
            match driver {
                Ok((_, Ok(()))) => {}
                Ok((_, Err(error))) => driver_errors.push(error),
                Err(error) => driver_errors.push(error.to_string()),
            }
        }
        let retry_error = self.retry_run_settlement_debts().await.err();
        let abandoned_settlements = if retry_error.is_some() {
            self.abandon_run_settlement_debts().await
        } else {
            Vec::new()
        };
        if let Some(error) = retry_error
            && abandoned_settlements.is_empty()
        {
            driver_errors.push(error.to_string());
        }
        let remaining_receipts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            supervisor.driver_cancels.clear();
            supervisor
                .execution_receipts
                .keys()
                .copied()
                .collect::<Vec<_>>()
        };
        for driver_token in remaining_receipts {
            self.release_run_driver_receipts(driver_token).await;
        }
        if driver_errors.is_empty() && abandoned_settlements.is_empty() {
            Ok(())
        } else {
            Err(TaskRunDriverShutdownError {
                driver_errors,
                abandoned_settlements,
            })
        }
    }

    pub(crate) fn active_run_driver_count(&self) -> Result<usize, String> {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| {
                supervisor
                    .driver_cancels
                    .len()
                    .saturating_add(supervisor.pending_admissions)
                    .saturating_add(supervisor.settlement_debts.len())
            })
            .map_err(|_| "TaskRuntime run driver supervisor is unavailable".to_string())
    }

    #[cfg(test)]
    pub(crate) async fn wait_run_driver_shutdown_started(&self) {
        self.run_driver_shutdown_started.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn abort_next_run_driver_shutdown_reporter_for_test(&self) {
        self.abort_next_run_driver_shutdown_reporter
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn park_next_run_driver_admission_for_test(
        &self,
    ) -> Result<
        (
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::SyncSender<()>,
        ),
        String,
    > {
        let (reserved_tx, reserved_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut barrier = self
            .run_driver_admission_test_barrier
            .lock()
            .map_err(|_| "TaskRuntime admission test barrier lock is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("TaskRuntime admission test barrier is already installed".to_string());
        }
        *barrier = Some(RunDriverAdmissionTestBarrier {
            reserved: reserved_tx,
            release: release_rx,
        });
        Ok((reserved_rx, release_tx))
    }

    #[cfg(test)]
    pub(crate) fn park_next_run_driver_registration_for_test(
        &self,
    ) -> Result<
        (
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::SyncSender<()>,
        ),
        String,
    > {
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut barrier = self
            .run_driver_registration_test_barrier
            .lock()
            .map_err(|_| "TaskRuntime registration test barrier lock is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("TaskRuntime registration test barrier is already installed".to_string());
        }
        *barrier = Some(RunDriverRegistrationTestBarrier {
            registered: registered_tx,
            release: release_rx,
        });
        Ok((registered_rx, release_tx))
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-utils"))]
    pub fn fail_next_run_driver_registration_for_test(&self) {
        self.fail_next_run_driver_registration
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_recovery_commit_for_test(&self) {
        self.fail_next_recovery_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_recovery_projection_for_test(&self) {
        self.fail_next_recovery_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_cell_started_for_test(&self) {
        self.fail_next_cell_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_cell_started_projection_for_test(&self) {
        self.fail_next_cell_started_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_runtime_mutation_projection_for_test(&self) {
        self.fail_next_runtime_mutation_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_cell_terminal_writes_for_test(&self, count: usize) {
        self.fail_cell_terminal_remaining
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn active_run_driver_receipt_count(&self) -> Result<usize, String> {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| supervisor.execution_receipts.values().map(Vec::len).sum())
            .map_err(|_| "TaskRuntime run driver supervisor is unavailable".to_string())
    }

    /// Transfer a resource acquired inside a framework-spawned tool task to
    /// the exact canonical driver. Unknown, stale, or mismatched context is
    /// rejected by returning ownership to the caller.
    pub(crate) fn retain_run_driver_receipt_from_context<Receipt>(
        &self,
        run_id: &str,
        execution_context_id: &str,
        receipt: Receipt,
    ) -> Result<(), Receipt>
    where
        Receipt: RunDriverExecutionReceipt + 'static,
    {
        if !execution_context_id.starts_with(RunDriverReceiptOwner::EXECUTION_CONTEXT_PREFIX) {
            return Err(receipt);
        }
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = supervisor.driver_contexts.get(execution_context_id) else {
            return Err(receipt);
        };
        let token = context.driver_token;
        if context.run_id != run_id {
            return Err(receipt);
        }
        if !supervisor.driver_cancels.contains_key(&token) {
            return Err(receipt);
        }
        supervisor
            .execution_receipts
            .entry(token)
            .or_default()
            .push(Box::new(receipt));
        Ok(())
    }

    /// Retry durable terminal writes that previously failed while retaining
    /// their generation lease. A workspace transition remains Busy until the
    /// debt is settled or the application reports shutdown degradation.
    pub(crate) async fn retry_run_settlement_debts(&self) -> Result<(), StoreError> {
        let debts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut supervisor.settlement_debts)
        };
        let mut remaining = Vec::new();
        for mut debt in debts {
            match self.finalize_run(&debt.run_id, debt.target, debt.note.as_deref()) {
                Ok(_) => {
                    if let Some(driver_token) = debt.driver_token {
                        self.release_run_driver_receipts(driver_token).await;
                    }
                    drop(debt.generation_lease);
                }
                Err(error) => {
                    debt.last_error = error.to_string();
                    remaining.push(debt);
                }
            }
        }
        if remaining.is_empty() {
            return Ok(());
        }
        let details = remaining
            .iter()
            .map(|debt| format!("{}: {}", debt.run_id, debt.last_error))
            .collect::<Vec<_>>()
            .join("; ");
        self.run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .settlement_debts
            .extend(remaining);
        Err(StoreError::InvalidPlan(format!(
            "unsettled TaskRun terminal writes: {details}"
        )))
    }

    /// Final shutdown settlement for debts that remained after the last
    /// durable retry. Preserve typed diagnostics, release each exact driver's
    /// receipts in LIFO order, then release its workspace generation lease.
    async fn abandon_run_settlement_debts(&self) -> Vec<AbandonedRunSettlement> {
        let debts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut supervisor.settlement_debts)
        };
        let mut abandoned = Vec::with_capacity(debts.len());
        for debt in debts {
            abandoned.push(AbandonedRunSettlement {
                run_id: debt.run_id.clone(),
                driver_token: debt.driver_token,
                root: debt.root.clone(),
                target: debt.target,
                error: debt.last_error.clone(),
            });
            if let Some(driver_token) = debt.driver_token {
                self.release_run_driver_receipts(driver_token).await;
            }
            drop(debt.generation_lease);
        }
        abandoned
    }

    /// Finalize a run or quarantine the supplied generation receipt for a
    /// later retry. The receipt is never dropped on an unverified write.
    pub(crate) fn finalize_run_with_lease(
        &self,
        generation_lease: &mut Option<WorkspaceGenerationLease>,
        driver_token: Option<u64>,
        run_id: &str,
        target: TaskRunStatus,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        match self.finalize_run(run_id, target, note) {
            Ok(run) => Ok(run),
            Err(error) => {
                if let Some(generation_lease) = generation_lease.take() {
                    self.run_driver_supervisor
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .settlement_debts
                        .push(RunSettlementDebt {
                            generation_lease,
                            driver_token,
                            run_id: run_id.to_string(),
                            root: self.shadow.root(),
                            target,
                            note: note.map(str::to_string),
                            last_error: error.to_string(),
                        });
                }
                Err(error)
            }
        }
    }

    /// Reserve canonical driver admission before any run mutation or secondary
    /// workspace-bound resource is acquired. Shutdown waits for every accepted
    /// reservation to register an exact driver or be dropped.
    pub(crate) fn reserve_run_driver_admission(
        self: &std::sync::Arc<Self>,
        run_id: String,
        cancel: echo_agent::agent::CancellationToken,
    ) -> Result<RunDriverAdmissionReservation, StoreError> {
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        if !supervisor.accepting {
            return Err(StoreError::InvalidPlan(
                "task runtime is shutting down".to_string(),
            ));
        }
        supervisor.pending_admissions =
            supervisor
                .pending_admissions
                .checked_add(1)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(
                        "TaskRun driver admission reservation capacity exhausted".to_string(),
                    )
                })?;
        drop(supervisor);
        let reservation = RunDriverAdmissionReservation {
            store: std::sync::Arc::clone(self),
            run_id,
            cancel,
            active: true,
        };
        #[cfg(test)]
        if let Some(barrier) = self
            .run_driver_admission_test_barrier
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .take()
        {
            barrier.reserved.send(()).map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRuntime admission test observer stopped before reservation".to_string(),
                )
            })?;
            barrier
                .release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| {
                    StoreError::InvalidPlan(format!(
                        "TaskRuntime admission test barrier was not released: {error}"
                    ))
                })?;
        }
        Ok(reservation)
    }

    /// Register the exact owned driver before its caller performs any
    /// workspace-bound preparation or TaskRuntime mutation.
    pub(crate) fn register_run_driver<T>(
        self: &std::sync::Arc<Self>,
        admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
    ) -> Result<RegisteredRunDriver<T>, StoreError>
    where
        T: Send + 'static,
    {
        self.register_run_driver_with_requirement(admission, generation_lease)
    }

    fn register_run_driver_with_requirement<T>(
        self: &std::sync::Arc<Self>,
        mut admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
    ) -> Result<RegisteredRunDriver<T>, StoreError>
    where
        T: Send + 'static,
    {
        #[cfg(any(test, feature = "test-utils"))]
        if self
            .fail_next_run_driver_registration
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(StoreError::InvalidPlan(
                "injected TaskRun driver registration failure".to_string(),
            ));
        }
        if !std::sync::Arc::ptr_eq(self, &admission.store) {
            return Err(StoreError::InvalidPlan(
                "TaskRun driver admission belongs to another runtime store".to_string(),
            ));
        }
        let runtime_handle = tokio::runtime::Handle::try_current().map_err(|error| {
            StoreError::InvalidPlan(format!(
                "TaskRun driver registration requires an active Tokio runtime: {error}"
            ))
        })?;
        let run_id = admission.run_id.clone();
        let cancel = admission.cancel.clone();
        let cancellation_registration =
            self.register_run_cancellation_internal(&run_id, cancel.clone())?;
        let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        let driver_token = supervisor.next_driver_token.checked_add(1).ok_or_else(|| {
            StoreError::InvalidPlan("TaskRun driver token capacity exhausted".to_string())
        })?;
        supervisor.next_driver_token = driver_token;
        while let Some(result) = supervisor.driver_settlements.try_join_next() {
            match result {
                Ok((completed_token, Ok(()))) => {
                    supervisor.driver_cancels.remove(&completed_token);
                }
                Ok((completed_token, Err(error))) => {
                    supervisor.driver_cancels.remove(&completed_token);
                    tracing::warn!(%error, "completed TaskRun driver owner reported an error");
                }
                Err(error) => {
                    tracing::warn!(%error, "completed TaskRun driver owner failed");
                }
            }
        }
        let settlement_store = std::sync::Arc::clone(self);
        let driver_cancel = cancel.clone();
        let execution_context_id = loop {
            let candidate = format!(
                "{}{}",
                RunDriverReceiptOwner::EXECUTION_CONTEXT_PREFIX,
                uuid::Uuid::new_v4()
            );
            if !supervisor.driver_contexts.contains_key(&candidate) {
                break candidate;
            }
        };
        let receipt_owner = RunDriverReceiptOwner {
            store: std::sync::Arc::clone(self),
            driver_token,
            execution_context_id: execution_context_id.clone(),
        };
        let operation =
            super::executor::TaskRuntimeOperation::new(std::sync::Arc::clone(self));
        let operation_reservation =
            operation.reserve_settlement("drive registered TaskRun")?;
        admission.active = false;
        supervisor.pending_admissions = supervisor.pending_admissions.saturating_sub(1);
        let reservations_idle = supervisor.pending_admissions == 0;
        if !supervisor.accepting {
            cancel.cancel();
        }
        supervisor
            .driver_cancels
            .insert(driver_token, cancel.clone());
        supervisor.driver_contexts.insert(
            execution_context_id.clone(),
            RunDriverExecutionContext {
                driver_token,
                run_id: run_id.clone(),
            },
        );
        let operation_driver_token = driver_token;
        let driver_operation = operation.spawn_reserved_settlement(
            "drive registered TaskRun",
            operation_reservation,
            async move {
            let mut generation_lease = Some(generation_lease);
            let _cancellation_registration = cancellation_registration;
            let start = start_receiver.await;
            let (mut result, should_settle) = match start {
                Ok(RunDriverStart::Execute(future)) => {
                    let result = match tokio::spawn(future).await {
                        Ok(result) => result,
                        Err(error) => {
                            let message = format!("TaskRun driver task failed: {error}");
                            Err(message)
                        }
                    };
                    (result, true)
                }
                Ok(RunDriverStart::PreparationFailed(error)) => {
                    (Err(error), true)
                }
                Ok(RunDriverStart::Reject(error)) => (Err(error), false),
                Err(error) => (
                    Err(format!(
                        "TaskRun driver preparation channel closed before start: {error}"
                    )),
                    false,
                ),
            };
            let operation_store = settlement_store.clone();
            let operation_run_id = run_id.clone();
            let (settled_result, release_receipts) =
                super::executor::TaskRuntimeOperation::new(settlement_store.clone())
                    .run_owned("settle registered TaskRun", move || {
                let mut release_receipts = !should_settle;
                if should_settle {
                    let settlement = match &result {
                        Ok(_) => operation_store
                            .confirm_run_settled(&operation_run_id),
                        Err(error) => match operation_store.get_run(&operation_run_id) {
                            // A missing run shares the terminal-settlement
                            // path: finalize pushes a durable settlement debt
                            // (with the shutdown-aware target) instead of
                            // silently passing.
                            Ok(Some(run)) if run.status == TaskRunStatus::Paused => Ok(()),
                            Err(read_error) => Err(read_error),
                            _ => {
                                let target = if driver_cancel.is_cancelled() {
                                    TaskRunStatus::Cancelled
                                } else {
                                    TaskRunStatus::Failed
                                };
                                operation_store
                                    .finalize_run_with_lease(
                                        &mut generation_lease,
                                        Some(driver_token),
                                        &operation_run_id,
                                        target,
                                        Some(error),
                                    )
                                    .map(|_| ())
                            }
                        },
                    };
                    if let Err(settlement_error) = settlement {
                        let original = result.as_ref().err().cloned().unwrap_or_else(|| {
                            "TaskRun driver returned before durable settlement".to_string()
                        });
                        if generation_lease.is_some() {
                            match operation_store.finalize_run_with_lease(
                                &mut generation_lease,
                                Some(driver_token),
                                &operation_run_id,
                                TaskRunStatus::Failed,
                                Some(&original),
                            ) {
                                Ok(_) => {
                                    release_receipts = true;
                                    result = Err(format!(
                                        "{original}; recovered non-terminal driver result after: {settlement_error}"
                                    ));
                                }
                                Err(recovery_error) => {
                                    let combined = format!(
                                        "{original}; terminal settlement failed: {settlement_error}; fallback terminal settlement failed: {recovery_error}"
                                    );
                                    result = Err(combined);
                                }
                            }
                        } else {
                            let combined =
                                format!("{original}; terminal settlement failed: {settlement_error}");
                            result = Err(combined);
                        }
                    } else {
                        release_receipts = true;
                    }
                }
                Ok((result, release_receipts))
            })
            .await?;
            result = settled_result;
            if release_receipts {
                settlement_store
                    .release_run_driver_receipts(driver_token)
                    .await;
            }
            match result {
                Ok(value) => {
                    let _ = result_sender.send(Ok(value));
                }
                Err(error) => {
                    let _ = result_sender.send(Err(error.clone()));
                }
            }
            settlement_store
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .driver_cancels
                .remove(&driver_token);
            // A terminal write failure is owned by settlement_debts together
            // with the exact generation and execution receipts. Shutdown and
            // workspace transition retry that canonical debt and report only
            // if it remains unsettled.
            Ok((driver_token, Ok(())))
            },
        );
        supervisor.driver_settlements.spawn_on(
            async move {
                match driver_operation.await {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => (operation_driver_token, Err(error.to_string())),
                    Err(error) => (
                        operation_driver_token,
                        Err(format!("TaskRun operation receipt was lost: {error}")),
                    ),
                }
            },
            &runtime_handle,
        );
        drop(supervisor);
        if reservations_idle {
            self.run_driver_admission_idle.notify_one();
        }
        #[cfg(test)]
        if let Some(barrier) = self
            .run_driver_registration_test_barrier
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .take()
        {
            barrier.registered.send(()).map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRuntime registration test observer stopped before registration"
                        .to_string(),
                )
            })?;
            barrier
                .release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| {
                    StoreError::InvalidPlan(format!(
                        "TaskRuntime registration test barrier was not released: {error}"
                    ))
                })?;
        }
        Ok(RegisteredRunDriver {
            start_sender: Some(start_sender),
            result_receiver: Some(result_receiver),
            receipt_owner: Some(receipt_owner),
            preparation_started: false,
            active: true,
        })
    }

    /// Accept an owned TaskRun driver. The caller receives only a result
    /// waiter; cancellation of that waiter does not cancel the retained task.
    #[cfg(test)]
    pub(crate) fn spawn_run_driver<T, F, Factory>(
        self: &std::sync::Arc<Self>,
        admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
        factory: Factory,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<T, String>>, StoreError>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
    {
        let registration = self.register_run_driver(admission, generation_lease)?;
        Ok(registration.start(factory))
    }

    async fn release_run_driver_receipts(&self, driver_token: u64) {
        let receipts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(context_id) =
                supervisor
                    .driver_contexts
                    .iter()
                    .find_map(|(context_id, context)| {
                        (context.driver_token == driver_token).then(|| context_id.clone())
                    })
            {
                supervisor.driver_contexts.remove(&context_id);
            }
            supervisor
                .execution_receipts
                .remove(&driver_token)
                .unwrap_or_default()
        };
        // Receipts are acquired TaskRuntime -> memory -> pool. Release in the
        // inverse order so asynchronous pool settlement completes before the
        // workspace-bound memory generation can be rebound.
        for receipt in receipts.into_iter().rev() {
            receipt.release().await;
        }
    }

    /// Atomically admit a binary/UI TaskRun driver, run its synchronous
    /// preparation while the current workspace generation is pinned, and
    /// transfer that pin to the canonical owned driver supervisor.
    pub fn spawn_supervised_run_driver<T, Prepared, Context, F, Factory, Preflight, Prepare>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        prepare: Prepare,
    ) -> Result<(Prepared, tokio::sync::oneshot::Receiver<Result<T, String>>), StoreError>
    where
        T: Send + 'static,
        Context: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
        Prepare: FnOnce(Context) -> Result<(Prepared, Factory), StoreError>,
    {
        let admission = self.reserve_run_driver_admission(run_id, cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let mut registration = self.register_run_driver(admission, generation_lease)?;
        let context = match preflight() {
            Ok(context) => context,
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        registration.mark_preparation_started();
        let (prepared, factory) = match prepare(context) {
            Ok(prepared) => prepared,
            Err(error) => {
                registration.fail_preparation(error.to_string());
                return Err(error);
            }
        };
        let waiter = registration.start(factory);
        Ok((prepared, waiter))
    }

    /// Choose between acceptance retry and durable process-recovery retry while
    /// the caller's exact driver registration pins one TaskRuntime generation.
    fn prepare_task_retry(
        &self,
        run_id: &str,
        task_id: &str,
        has_recovery_blocker: bool,
    ) -> Result<TaskRetryPreparation, StoreError> {
        if has_recovery_blocker {
            self.resolve_recovery_task(run_id, task_id, RecoveryDecision::Retry)?;
            Ok(TaskRetryPreparation::Recovery)
        } else {
            self.retry_blocked_task(run_id, task_id)
                .map(|next_attempt| TaskRetryPreparation::Acceptance { next_attempt })
        }
    }

    /// TUI/CLI retry facade. Exact supervisor registration and generation
    /// admission complete before resource preflight and before the canonical
    /// recovery-vs-acceptance mutation is selected.
    pub fn spawn_supervised_task_retry<Context, F, Factory, Preflight>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        task_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        factory: Factory,
    ) -> Result<
        (
            TaskRetryPreparation,
            tokio::sync::oneshot::Receiver<Result<(), String>>,
        ),
        StoreError,
    >
    where
        Context: Send + 'static,
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
        Factory: FnOnce(Context, RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
    {
        let admission = self.reserve_run_driver_admission(run_id.clone(), cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let mut registration = self.register_run_driver::<()>(admission, generation_lease)?;
        let context = match preflight() {
            Ok(context) => context,
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        let has_recovery_blocker = match self.list_recovery_blockers(&run_id) {
            Ok(blockers) => blockers.iter().any(|blocker| blocker.task_id == task_id),
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        registration.mark_preparation_started();
        let preparation = match self.prepare_task_retry(&run_id, &task_id, has_recovery_blocker) {
            Ok(preparation) => preparation,
            Err(error) => {
                registration.fail_preparation(error.to_string());
                return Err(error);
            }
        };
        let waiter = match preparation {
            TaskRetryPreparation::Acceptance { .. } => {
                registration.start(move |owner| factory(context, owner))
            }
            TaskRetryPreparation::Recovery => registration.start(|_| async { Ok(()) }),
        };
        Ok((preparation, waiter))
    }

    /// Async-surface variant of [`Self::spawn_supervised_task_retry`]. Driver
    /// admission remains on the Tokio runtime, while the journal-backed retry
    /// preparation runs through the process-wide bounded blocking operation.
    ///
    /// The registration is owned by the blocking closure once file I/O starts.
    /// Dropping the caller therefore cannot release the workspace generation
    /// while the accepted non-interruptible journal mutation is still running.
    pub async fn spawn_supervised_task_retry_async<Context, F, Factory, Preflight>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        task_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        factory: Factory,
    ) -> Result<
        (
            TaskRetryPreparation,
            tokio::sync::oneshot::Receiver<Result<(), String>>,
        ),
        StoreError,
    >
    where
        Context: Send + 'static,
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
        Factory: FnOnce(Context, RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
    {
        let admission = self.reserve_run_driver_admission(run_id.clone(), cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let registration = self.register_run_driver::<()>(admission, generation_lease)?;
        let operation_store = std::sync::Arc::clone(self);
        let (registration, preparation, context) =
            super::executor::TaskRuntimeOperation::new(std::sync::Arc::clone(self))
                .run_owned("prepare supervised task retry", move || {
                    let mut registration = registration;
                    let context = match preflight() {
                        Ok(context) => context,
                        Err(error) => {
                            registration.reject(error.to_string());
                            return Err(error);
                        }
                    };
                    registration.mark_preparation_started();
                    let has_recovery_blocker = match operation_store.list_recovery_blockers(&run_id)
                    {
                        Ok(blockers) => blockers.iter().any(|blocker| blocker.task_id == task_id),
                        Err(error) => {
                            registration.fail_preparation(error.to_string());
                            return Err(error);
                        }
                    };
                    match operation_store.prepare_task_retry(
                        &run_id,
                        &task_id,
                        has_recovery_blocker,
                    ) {
                        Ok(preparation) => Ok((registration, preparation, context)),
                        Err(error) => {
                            registration.fail_preparation(error.to_string());
                            Err(error)
                        }
                    }
                })
                .await?;
        let waiter = match preparation {
            TaskRetryPreparation::Acceptance { .. } => {
                registration.start(move |owner| factory(context, owner))
            }
            TaskRetryPreparation::Recovery => registration.start(|_| async { Ok(()) }),
        };
        Ok((preparation, waiter))
    }

    fn confirm_run_settled(&self, run_id: &str) -> Result<(), StoreError> {
        let Some(run) = self.get_run(run_id)? else {
            return Err(StoreError::RunNotFound(run_id.to_string()));
        };
        if matches!(
            run.status,
            TaskRunStatus::Completed
                | TaskRunStatus::Failed
                | TaskRunStatus::Cancelled
                | TaskRunStatus::Paused
        ) {
            Ok(())
        } else if run.status == TaskRunStatus::Running
            && self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.enabled && state.active_turn.is_none())
        {
            // A long-horizon Goal may intentionally be Running between finite
            // RunTurns (for deferral or queued continuation). The event-folded
            // active-turn claim, not a driver future, is the authority here.
            Ok(())
        } else {
            Err(StoreError::InvalidPlan(format!(
                "TaskRun driver returned with non-terminal status {} for {run_id}",
                run.status.as_str()
            )))
        }
    }

    /// 在持有某 run 的 plan/state 写锁期间执行闭包 (F2-1 / F3-3 / F3-4)。
    ///
    /// 用 closure 模式而非返回 Guard: std::sync::MutexGuard 借自 &Mutex, 而
    /// Mutex 在 Arc 内, Arc 作为局部变量时 Guard 跨函数返回即悬垂 (自引用
    /// struct 在 Rust 里无法直接表达)。closure 把锁的获取与释放封装在内部,
    /// 闭包体内是临界区。revision compare-and-commit / transition_run 用它包裹
    /// "读事件 → 校验 → 追加 → 重建投影"全程。
    pub(super) fn with_run_lock<R>(
        &self,
        run_id: &str,
        f: impl FnOnce() -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let _operation = self.shadow_operation()?;
        let arc = self
            .plan_locks
            .entry(run_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone();
        // 持锁调用闭包; poison 时恢复 (与 working_dir 同款 into_inner, 不 panic)。
        let _guard = arc.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    fn shadow_operation(&self) -> Result<ShadowOperation<'_>, StoreError> {
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.active_operations = generation.active_operations.saturating_add(1);
        Ok(ShadowOperation { store: self })
    }

    /// Atomically close generation admission when no operation is active.
    /// Workspace IPC gets an observable Busy error instead of blocking a Tokio
    /// runtime thread.
    pub(crate) async fn begin_workspace_transition(
        &self,
    ) -> Result<TaskRuntimeWorkspaceTransition<'_>, StoreError> {
        self.retry_run_settlement_debts().await?;
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.transitioning = true;
        let active_operations = generation.active_operations;
        drop(generation);
        let transition = TaskRuntimeWorkspaceTransition {
            store: self,
            active: true,
        };
        if active_operations != 0 {
            return Err(StoreError::WorkspaceTransitionBusy { active_operations });
        }
        Ok(transition)
    }

    /// Pin a multi-step application operation to one workspace generation.
    /// Individual store calls already take short leases; cron and other
    /// long-running adapters use this outer lease so a rebind cannot occur
    /// between their run creation, execution, and settlement writes.
    pub(crate) fn lease_active_workspace_generation(
        self: &std::sync::Arc<Self>,
    ) -> Result<WorkspaceGenerationLease, StoreError> {
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.active_operations = generation.active_operations.saturating_add(1);
        drop(generation);
        Ok(WorkspaceGenerationLease {
            store: std::sync::Arc::clone(self),
        })
    }

    /// Pin the active TaskRuntime generation for an application foreground
    /// driver. Surfaces acquire this after foreground admission and before the
    /// memory-generation and agent-pool receipts.
    pub fn lease_foreground_generation(
        self: &std::sync::Arc<Self>,
    ) -> Result<TaskRuntimeGenerationReceipt, StoreError> {
        self.lease_active_workspace_generation()
            .map(|lease| TaskRuntimeGenerationReceipt { _lease: lease })
    }

    /// Atomically switch the file authority after all operations using the
    /// previous root have completed. The store Arc and event hook stay intact.
    pub async fn rebind_shadow_root(
        &self,
        root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        let transition = self.begin_workspace_transition().await?;
        transition.rebind_shadow_root(root, workspace_id)
    }

    pub fn active_workspace_id(&self) -> String {
        self.shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .workspace_id
            .clone()
    }

    pub(crate) fn bind_command_cell_runtime(
        &self,
        runtime: std::sync::Weak<super::command_cells::CommandCellRuntimeService>,
    ) {
        *self
            .command_cell_runtime
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(runtime);
    }

    pub(crate) fn stop_owned_command_cells(&self, run_id: &str) -> Result<usize, StoreError> {
        let runtime = self
            .command_cell_runtime
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        Ok(runtime
            .map(|runtime| runtime.stop_run(&self.active_workspace_id(), run_id))
            .unwrap_or(0))
    }

    #[cfg(test)]
    pub(crate) fn active_shadow_root(&self) -> PathBuf {
        self.shadow.root()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_run_for_active_workspace(
        &self,
        run_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        self.create_run_for_active_workspace_with_profile(
            run_id,
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
            TaskRunExecutionProfile::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_run_for_active_workspace_with_profile(
        &self,
        run_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
        execution_profile: TaskRunExecutionProfile,
    ) -> Result<TaskRun, StoreError> {
        let _operation = self.shadow_operation()?;
        let workspace_id = self.active_workspace_id();
        self.create_run_with_profile(
            run_id,
            &workspace_id,
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
            execution_profile,
        )
    }

    /// Construct a pending run bound to the active workspace without making it
    /// visible. The caller must publish it with a framework-validated revision
    /// through `compare_and_publish_initial_revisioned_task_graph`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_run_for_active_workspace(
        &self,
        run_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        let _operation = self.shadow_operation()?;
        Ok(new_pending_run(
            run_id,
            &self.active_workspace_id(),
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
        ))
    }

    /// Build a `FileTaskStore` over the shadow, for read delegation.
    fn file_store(&self) -> Result<ShadowFileStore<'_>, StoreError> {
        let operation = self.shadow_operation()?;
        Ok(ShadowFileStore {
            _operation: operation,
            store: super::file_store::FileTaskStore::new((*self.shadow).clone()),
        })
    }

    pub(crate) fn completion_gate_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<super::event_rebuild::CompletionGateProjection>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow
            .read_completion_gate_projection(run_id)
            .map_err(StoreError::Shadow)
    }

    // ── Runs ────────────────────────────────────────────────────────────

    /// Create a new run in `Pending` and emit `RunCreated`. Returns the
    /// existing run when `run_id` is already present.
    #[allow(clippy::too_many_arguments)] // run identity + routing fields all thread through
    pub fn create_run(
        &self,
        run_id: &str,
        workspace_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        self.create_run_with_profile(
            run_id,
            workspace_id,
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
            TaskRunExecutionProfile::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_run_with_profile(
        &self,
        run_id: &str,
        workspace_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
        execution_profile: TaskRunExecutionProfile,
    ) -> Result<TaskRun, StoreError> {
        self.with_run_lock(run_id, || {
            if let Some(existing) = self.get_run(run_id)? {
                let existing_profile = self
                    .get_run_state(run_id)?
                    .map(|snapshot| snapshot.execution_profile)
                    .unwrap_or_default();
                if existing.workspace_id != workspace_id
                    || existing.conversation_id != conversation_id
                    || existing.root_message_id != root_message_id
                    || existing.domain_profile != domain_profile
                    || existing.route != route
                    || existing.attended_mode != attended_mode
                    || existing_profile != execution_profile
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "TaskRun '{run_id}' already exists with a different immutable identity"
                    )));
                }
                return Ok(existing);
            }

            let run = new_pending_run(
                run_id,
                workspace_id,
                conversation_id,
                root_message_id,
                domain_profile,
                goal,
                route,
                attended_mode,
            );

            // U1c phase-0/0bc step-2: file is the write authority. Append the
            // RunCreated event to events.jsonl and rebuild plan.json — no SQL
            // write.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": goal,
                    "goal_revision": run.goal_revision,
                    "goal_sha256": run.goal_sha256,
                    "domain_profile": domain_profile.as_str(),
                    "workspace_id": run.workspace_id,
                    "conversation_id": run.conversation_id,
                    "root_message_id": run.root_message_id,
                    "route": run.route,
                    "attended_mode": attended_mode.as_str(),
                    "created_at": echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
                    "execution_profile": execution_profile,
                }),
            ))?;
            self.spill_objective_artifact_if_bounded(
                run_id,
                run.goal_revision,
                &run.goal_sha256,
                goal,
            );
            Ok(run)
        })
    }

    /// Write the full objective next to the journal when a Goal exceeds the
    /// in-context contract bound. Best-effort derived view; the journal stays
    /// authoritative.
    fn spill_objective_artifact_if_bounded(
        &self,
        run_id: &str,
        goal_revision: u64,
        goal_sha256: &str,
        goal: &str,
    ) {
        if goal.chars().count() <= OBJECTIVE_ARTIFACT_SPILL_CHARS {
            return;
        }
        if let Err(error) =
            self.shadow
                .write_objective_artifact(run_id, goal_revision, goal_sha256, goal)
        {
            tracing::warn!(run_id, %error, "objective artifact spill failed");
        }
    }

    /// Path of the spilled objective artifact for a run, when one exists.
    pub fn objective_artifact_path(&self, run_id: &str) -> Option<std::path::PathBuf> {
        let run = self.get_run(run_id).ok().flatten()?;
        self.shadow
            .objective_artifact_path(run_id, run.goal_revision, &run.goal_sha256)
    }

    /// Anchor an incremental user constraint (steer) in the run journal so it
    /// survives context compression alongside the Goal. Best-effort by design:
    /// callers skip unknown runs instead of failing the steer delivery.
    pub fn record_run_steer(
        &self,
        run_id: &str,
        turn_id: &str,
        text: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let bounded: String = text.chars().take(2_000).collect();
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                Some(turn_id),
                RuntimeEventKind::RunSteerRecorded,
                serde_json::json!({ "turn_id": turn_id, "text": bounded }),
            ))?;
            Ok(())
        })
    }

    /// Replace the sole authoritative Goal after an explicit local-user action.
    ///
    /// The event append is the transaction: its fold updates the Goal and keeps
    /// continuation deferred until a new Plan revision binds the new Goal.
    pub fn update_run_goal(
        &self,
        run_id: &str,
        expected_goal_revision: u64,
        new_goal: &str,
        reason: &str,
        actor_source: RunGoalActorSource,
    ) -> Result<TaskRun, StoreError> {
        let actor_user_id = crate::infra::load_or_create_cache_user_id();
        self.with_run_lock(run_id, || {
            if new_goal.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "new goal must not be empty".to_string(),
                });
            }
            if reason.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "update reason must not be empty".to_string(),
                });
            }
            if actor_user_id.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "local user identity is unavailable".to_string(),
                });
            }

            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.goal_revision != expected_goal_revision {
                return Err(StoreError::GoalConflict {
                    run_id: run_id.to_string(),
                    expected: expected_goal_revision,
                    current: run.goal_revision,
                });
            }
            if run.status != TaskRunStatus::Paused {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: format!(
                        "run must be paused, current status is {}",
                        run.status.as_str()
                    ),
                });
            }
            if self.is_run_active(run_id) {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active driver".to_string(),
                });
            }

            let new_goal_sha256 = task_goal_sha256(new_goal);
            if new_goal_sha256 == run.goal_sha256 {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "new goal is unchanged".to_string(),
                });
            }
            let new_goal_revision =
                run.goal_revision
                    .checked_add(1)
                    .ok_or_else(|| StoreError::GoalUpdateRejected {
                        run_id: run_id.to_string(),
                        reason: "goal revision overflow".to_string(),
                    })?;

            if self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|state| state.active_turn)
                .is_some()
            {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active RunTurn".to_string(),
                });
            }
            if !self.active_subagent_boundaries(run_id)?.is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active Subagent attempt".to_string(),
                });
            }
            if self
                .list_background_cells(run_id)?
                .iter()
                .any(BackgroundCellState::is_active)
            {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active command cell".to_string(),
                });
            }

            let updated_at = Utc::now();
            let old_requirements = self
                .get_plan(run_id)?
                .as_ref()
                .map(super::completion_gate::requirements_for_plan)
                .unwrap_or_default();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunGoalUpdated,
                serde_json::json!({
                    "old_goal_revision": run.goal_revision,
                    "new_goal_revision": new_goal_revision,
                    "old_goal_sha256": run.goal_sha256,
                    "new_goal_sha256": new_goal_sha256,
                    "new_goal": new_goal,
                    "reason": reason,
                    "actor_source": actor_source.as_str(),
                    "actor_user_id": actor_user_id,
                    "updated_at": echo_agent::utils::time::to_local(updated_at).to_rfc3339(),
                    "continuation_deferred_reason": "goal_revision_unbound",
                }),
            )];
            for requirement in old_requirements {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(requirement.task_id.as_str()),
                    None,
                    RuntimeEventKind::RequirementEvidenceInvalidated,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "old_goal_revision": run.goal_revision,
                        "new_goal_revision": new_goal_revision,
                        "old_plan_revision": requirement.plan_revision,
                        "reason": reason,
                    }),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            self.spill_objective_artifact_if_bounded(
                run_id,
                new_goal_revision,
                &new_goal_sha256,
                new_goal,
            );
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))
        })
    }

    /// Record an explicit local-user decision to skip one exact requirement.
    /// The task must already be Skipped through the canonical revisioned graph.
    pub fn skip_goal_requirement(
        &self,
        run_id: &str,
        expected_goal_revision: u64,
        requirement_id: &str,
        reason: &str,
        actor_source: RunGoalActorSource,
    ) -> Result<CompletionGateReport, StoreError> {
        let actor_user_id = crate::infra::load_or_create_cache_user_id();
        self.with_run_lock(run_id, || {
            if reason.trim().is_empty() || requirement_id.trim().is_empty() {
                return Err(StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: "requirement id and Skip reason must not be empty".to_string(),
                });
            }
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.goal_revision != expected_goal_revision {
                return Err(StoreError::GoalConflict {
                    run_id: run_id.to_string(),
                    expected: expected_goal_revision,
                    current: run.goal_revision,
                });
            }
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            validate_plan_goal_binding(&run, &plan)?;
            let requirement = super::completion_gate::requirements_for_plan(&plan)
                .into_iter()
                .find(|item| item.requirement_id == requirement_id)
                .ok_or_else(|| StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: format!("unknown requirement '{requirement_id}'"),
                })?;
            let task = plan
                .tasks
                .iter()
                .find(|item| item.id == requirement.task_id)
                .ok_or_else(|| StoreError::TaskNotFound(requirement.task_id.clone()))?;
            if task.status != echo_agent::tasks::TaskStatus::Skipped {
                return Err(StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: format!(
                        "task '{}' must first be skipped through task_update(base_revision)",
                        task.id
                    ),
                });
            }
            // Audit allowlist: requirement acceptance is evidence history, not
            // operational hot state; exact Goal/hash correlation needs the
            // complete append-only evidence stream.
            let duplicate = self.list_events(run_id, 0)?.into_iter().any(|event| {
                event.event_type == RuntimeEventKind::RequirementSkipped
                    && event
                        .payload
                        .get("requirement_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(requirement.requirement_id.as_str())
                    && event
                        .payload
                        .get("requirement_sha256")
                        .and_then(serde_json::Value::as_str)
                        == Some(requirement.requirement_sha256.as_str())
                    && event
                        .payload
                        .get("goal_revision")
                        .and_then(serde_json::Value::as_u64)
                        == Some(run.goal_revision)
            });
            if !duplicate {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(task.id.as_str()),
                    None,
                    RuntimeEventKind::RequirementSkipped,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "goal_revision": run.goal_revision,
                        "plan_revision": plan.revision,
                        "reason": reason,
                        "actor_source": actor_source.as_str(),
                        "actor_user_id": actor_user_id,
                    }),
                ))?;
            }
            self.completion_gate_report(run_id)
        })
    }

    /// Bind user-uploaded attachments to a run so plan-level subagents see the
    /// same images/files as the main agent.
    ///
    /// Follows the event-sourcing pattern: append a `RunAttachmentsUpdated`
    /// event then rewrite plan.json so subsequent `get_run` reads reflect it.
    pub fn set_run_attachments(
        &self,
        run_id: &str,
        attachments: &[crate::attachments::AttachmentRef],
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            // Validate the run exists (mirrors set_task_status / transition_run).
            self.get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunAttachmentsUpdated,
                serde_json::json!({ "attachments": attachments }),
            ))?;
            Ok(())
        })
    }

    /// Serialize a run transition and durably append `RunStatusChanged`.
    /// Projection refresh is self-healing, not I/O failure-atomic with append.
    /// Rejects illegal transitions (see [`TaskRunStatus::can_transition_to`]).
    pub fn transition_run(&self, run_id: &str, next: TaskRunStatus) -> Result<TaskRun, StoreError> {
        // F3-3/F3-4: 串行化"读→验证→写", 防并发 transition 丢更新 + 崩溃中态。
        // 用 closure 包裹临界区 (见 with_run_lock 说明)。
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file is the read/write authority. Read the
            // current run from the file to validate the transition, then append the
            // status-changed event + rewrite plan.json. No SQL write.
            let run = self
                .get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            let current = run.status;
            if !current.can_transition_to(next) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: current.as_str().to_string(),
                    to: next.as_str().to_string(),
                });
            }
            let now = Utc::now();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({ "from": current.as_str(), "to": next.as_str() }),
            )];
            if next == TaskRunStatus::Cancelled {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            let mut run = run;
            run.status = next;
            run.updated_at = now;
            Ok(run)
        })
    }

    /// Persist and verify a terminal TaskRun status before execution receipts
    /// may be released. Existing completed/cancelled truth wins over a late
    /// driver failure.
    pub(crate) fn finalize_run(
        &self,
        run_id: &str,
        target: TaskRunStatus,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        self.finalize_run_with_note_task(run_id, target, None, note)
    }

    pub(crate) fn finalize_run_with_note_task(
        &self,
        run_id: &str,
        target: TaskRunStatus,
        note_task_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        if !matches!(
            target,
            TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
        ) {
            return Err(StoreError::InvalidPlan(format!(
                "finalize_run requires a terminal status, got {}",
                target.as_str()
            )));
        }
        self.with_run_lock(run_id, || {
            let current = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let mut events = Vec::new();
            if let Some(note) = note {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    note_task_id,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({ "message": note }),
                ));
            }
            if matches!(
                current.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) || current.status == target
            {
                if !events.is_empty() {
                    self.commit_runtime_events(run_id, events)?;
                }
                return Ok(current);
            }
            let mut from = current.status;
            if target != TaskRunStatus::Cancelled && from != TaskRunStatus::Running {
                if !from.can_transition_to(TaskRunStatus::Running) {
                    return Err(StoreError::IllegalTransition {
                        run_id: run_id.to_string(),
                        from: from.as_str().to_string(),
                        to: TaskRunStatus::Running.as_str().to_string(),
                    });
                }
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": from.as_str(),
                        "to": TaskRunStatus::Running.as_str(),
                    }),
                ));
                from = TaskRunStatus::Running;
            }
            if !from.can_transition_to(target) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: from.as_str().to_string(),
                    to: target.as_str().to_string(),
                });
            }
            events.push(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({ "from": from.as_str(), "to": target.as_str() }),
            ));
            if target == TaskRunStatus::Cancelled {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            self.commit_runtime_events(run_id, events)?;

            let settled = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if settled.status != target {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} terminal write was not durable: expected {}, read back {}",
                    target.as_str(),
                    settled.status.as_str()
                )));
            }
            Ok(settled)
        })
    }

    pub(crate) fn finalize_cancelled_tasks_and_run(&self, run_id: &str) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status == TaskRunStatus::Completed {
                return Ok(());
            }
            let mut events = if let Some(graph) = self.load_revisioned_task_graph(run_id)? {
                let before = graph.snapshot;
                let mut after = before.clone();
                let settlement = echo_agent::tasks::settle_runtime_interruption(
                    &mut after,
                    before.revision,
                    echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled,
                )?;
                if settlement
                    == echo_agent::tasks::RuntimeInterruptionSettlementOutcome::ReloadSnapshot
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "run {run_id} changed while holding its cancellation lock"
                    )));
                }
                runtime_execution_change_events(
                    run_id,
                    &before,
                    &after,
                    Some("cancelled with parent run"),
                )?
            } else {
                Vec::new()
            };
            if run.status != TaskRunStatus::Cancelled {
                if !run.status.can_transition_to(TaskRunStatus::Cancelled) {
                    return Err(StoreError::IllegalTransition {
                        run_id: run_id.to_string(),
                        from: run.status.as_str().to_string(),
                        to: TaskRunStatus::Cancelled.as_str().to_string(),
                    });
                }
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": run.status.as_str(),
                        "to": TaskRunStatus::Cancelled.as_str(),
                    }),
                ));
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            if !events.is_empty() {
                self.commit_runtime_events(run_id, events)?;
            }
            Ok(())
        })
    }

    /// Resume a paused run: `Paused → Running`.
    ///
    /// Test-only convenience for state-machine fixtures. Production callers
    /// must carry a journal-bound [`TaskRunResumeIdentity`].
    #[cfg(test)]
    pub fn resume_task_run(&self, run_id: &str) -> Result<TaskRun, StoreError> {
        self.with_run_lock(run_id, || {
            let mut run = self.validate_resume_locked(run_id, None)?;
            self.append_resume_events(run_id, &run, true)?;
            run.status = TaskRunStatus::Running;
            run.updated_at = Utc::now();
            Ok(run)
        })
    }

    /// Resume one exact paused TaskRun epoch without claiming a continuation
    /// turn. Ordinary planned-run executors use this path; long-horizon chat
    /// uses [`Self::resume_and_claim_run_turn_expected`].
    pub fn resume_task_run_expected(
        &self,
        expected: &TaskRunResumeIdentity,
    ) -> Result<TaskRun, StoreError> {
        let run_id = expected.run_id.as_str();
        self.with_run_lock(run_id, || {
            let mut run = self.validate_resume_locked(run_id, Some(expected))?;
            if let Err(error) = self.append_resume_events(run_id, &run, true) {
                let reconciled = self.get_run_state(run_id).map_err(|reconcile_error| {
                    StoreError::ResumeOutcomeUnknown {
                        run_id: run_id.to_string(),
                        details: format!(
                            "resume failed and journal reconciliation also failed: {error}; {reconcile_error}"
                        ),
                    }
                })?;
                match reconciled {
                    Some(snapshot)
                        if snapshot.run.status == TaskRunStatus::Running
                            && snapshot.journal_sequence > expected.journal_sequence =>
                    {
                        return Ok(snapshot.run);
                    }
                    Some(snapshot) if expected.validate_resumable(&snapshot).is_ok() => {
                        return Err(error);
                    }
                    Some(snapshot) => {
                        return Err(StoreError::ResumeOutcomeUnknown {
                            run_id: run_id.to_string(),
                            details: format!(
                                "resume could not be reconciled safely after {error}; current status is {} at journal sequence {}",
                                snapshot.run.status.as_str(),
                                snapshot.journal_sequence
                            ),
                        });
                    }
                    None => {
                        return Err(StoreError::ResumeOutcomeUnknown {
                            run_id: run_id.to_string(),
                            details: format!(
                                "resume could not be reconciled after {error}; TaskRun disappeared"
                            ),
                        });
                    }
                }
            }
            run.status = TaskRunStatus::Running;
            run.updated_at = Utc::now();
            Ok(run)
        })
    }

    fn validate_resume_locked(
        &self,
        run_id: &str,
        expected: Option<&TaskRunResumeIdentity>,
    ) -> Result<TaskRun, StoreError> {
        let blockers = self.list_recovery_blockers(run_id)?;
        if !blockers.is_empty() {
            let details = blockers
                .iter()
                .map(|blocker| format!("{}: {}", blocker.task_id, blocker.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(StoreError::RecoveryBlocked {
                run_id: run_id.to_string(),
                details,
            });
        }
        let snapshot = self
            .get_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let run = snapshot.run.clone();
        if let Some(expected) = expected
            && let Err(error) = expected.validate_resumable(&snapshot)
        {
            // Audit allowlist: exact resume inspects only its post-capture
            // suffix. Execution-path Notes are diagnostic and can be
            // published just after driver idle; every other fact remains
            // identity-changing and is rejected.
            let after_sequence = i64::try_from(expected.journal_sequence).map_err(|_| {
                StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
            })?;
            let suffix = self.list_events(run_id, after_sequence)?;
            let diagnostic_only = !suffix.is_empty()
                && suffix.iter().all(|event| {
                    event.event_type == RuntimeEventKind::Note
                        && event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            == Some("execution_path")
                });
            if diagnostic_only {
                let mut semantic_snapshot = snapshot.clone();
                semantic_snapshot.journal_sequence = expected.journal_sequence;
                expected
                    .validate_resumable(&semantic_snapshot)
                    .map_err(StoreError::InvalidPlan)?;
            } else {
                let latest = suffix
                    .last()
                    .map(|event| format!("{} at sequence {}", event.event_type.as_str(), event.seq))
                    .unwrap_or_else(|| "unavailable".to_string());
                return Err(StoreError::InvalidPlan(format!(
                    "{error}; latest event after queued identity: {latest}"
                )));
            }
        }
        match self.get_plan(run_id)? {
            Some(plan) => validate_plan_goal_binding(&run, &plan)?,
            None if snapshot.execution_profile.plan_policy == RunPlanPolicy::AllowDirect => {}
            None => return Err(StoreError::PlanNotFound(run_id.to_string())),
        }
        if let Some(continuation) = snapshot.continuation {
            if continuation.active_turn.is_some() {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} still has an active RunTurn; wait for exact driver settlement before resume"
                )));
            }
            if continuation
                .token_budget
                .is_some_and(|budget| continuation.tokens_used >= budget)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} exhausted its continuation token budget"
                )));
            }
            if continuation
                .time_budget_seconds
                .is_some_and(|budget| continuation.time_used_seconds >= budget)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} exhausted its continuation time budget"
                )));
            }
        }
        if !run.status.can_transition_to(TaskRunStatus::Running) {
            return Err(StoreError::IllegalTransition {
                run_id: run_id.to_string(),
                from: run.status.as_str().to_string(),
                to: TaskRunStatus::Running.as_str().to_string(),
            });
        }
        Ok(run)
    }

    /// Evaluate cold-start admission without changing state. The actual
    /// transition re-runs this policy under the per-run lock.
    pub fn boot_auto_resume_decision(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
    ) -> Result<BootAutoResumeDecision, StoreError> {
        self.boot_auto_resume_decision_at(
            run_id,
            launcher_ready,
            interactive_owner_ready,
            Utc::now(),
        )
    }

    /// Atomically re-check and resume a boot-recovered run. This preserves a
    /// persisted provider retry schedule; explicit user resume uses
    /// `resume_task_run` and resets it instead.
    pub fn resume_task_run_after_boot(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
    ) -> Result<BootAutoResumeOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let now = Utc::now();
            match self.boot_auto_resume_decision_at(
                run_id,
                launcher_ready,
                interactive_owner_ready,
                now,
            )? {
                BootAutoResumeDecision::Blocked(blockers) => {
                    Ok(BootAutoResumeOutcome::Blocked(blockers))
                }
                BootAutoResumeDecision::Ready {
                    retry_not_before: Some(deadline),
                } if deadline > now => Ok(BootAutoResumeOutcome::WaitingUntil(deadline)),
                BootAutoResumeDecision::Ready { .. } => {
                    let mut run = self
                        .get_run(run_id)?
                        .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                    self.append_resume_events(run_id, &run, false)?;
                    run.status = TaskRunStatus::Running;
                    run.updated_at = now;
                    Ok(BootAutoResumeOutcome::Resumed(Box::new(run)))
                }
            }
        })
    }

    fn append_resume_events(
        &self,
        run_id: &str,
        run: &TaskRun,
        reset_provider_retry: bool,
    ) -> Result<(), StoreError> {
        let events = self.prepare_resume_events(run_id, run, reset_provider_retry)?;
        self.commit_runtime_events(run_id, events)
    }

    fn prepare_resume_events(
        &self,
        run_id: &str,
        run: &TaskRun,
        reset_provider_retry: bool,
    ) -> Result<Vec<RuntimeJournalEvent>, StoreError> {
        let mut events = if let Some(graph) = self.load_revisioned_task_graph(run_id)? {
            let before = graph.snapshot;
            let mut after = before.clone();
            let paused_tasks = before
                .tasks
                .iter()
                .filter(|task| {
                    matches!(
                        task.execution.status,
                        echo_agent::tasks::TaskStatus::Paused(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            for paused in paused_tasks {
                match echo_agent::tasks::resume_runtime_task(&mut after, &paused, before.revision)?
                {
                    echo_agent::tasks::RuntimeTaskResumeOutcome::Resumed => {}
                    echo_agent::tasks::RuntimeTaskResumeOutcome::Superseded => {
                        return Err(StoreError::InvalidPlan(format!(
                            "paused task '{}' changed while holding its resume lock",
                            paused.spec.id
                        )));
                    }
                }
            }
            runtime_execution_change_events(
                run_id,
                &before,
                &after,
                Some("resumed without consuming retry budget"),
            )?
        } else {
            Vec::new()
        };
        events.extend([
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": run.status.as_str(),
                    "to": TaskRunStatus::Running.as_str(),
                }),
            ),
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunPauseReasonChanged,
                serde_json::json!({ "reason": serde_json::Value::Null }),
            ),
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationResumed,
                serde_json::json!({
                    "deferred": false,
                    "reset_blocker_audit": true,
                    "reset_provider_retry": reset_provider_retry,
                }),
            ),
        ]);
        Ok(events)
    }

    fn boot_auto_resume_decision_at(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
        now: DateTime<Utc>,
    ) -> Result<BootAutoResumeDecision, StoreError> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let snapshot = self
            .get_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let execution_profile = snapshot.execution_profile;
        let state = snapshot.continuation;
        let plan = self.get_plan(run_id)?;
        let mut blockers = Vec::new();
        if run.status != TaskRunStatus::Paused {
            blockers.push(BootAutoResumeBlocker::RunNotPaused);
        }
        if state
            .as_ref()
            .and_then(|state| state.pause.as_ref())
            .map(|pause| pause.reason)
            != Some(RunPauseReason::BootRecovery)
        {
            blockers.push(BootAutoResumeBlocker::NotBootRecovery);
        }
        if !state.as_ref().is_some_and(|state| state.enabled) {
            blockers.push(BootAutoResumeBlocker::ContinuationDisabled);
        }
        if !state
            .as_ref()
            .is_some_and(|state| state.auto_resume_after_restart)
        {
            blockers.push(BootAutoResumeBlocker::AutoResumeDisabled);
        }
        if !launcher_ready {
            blockers.push(BootAutoResumeBlocker::LauncherUnavailable);
        }
        if run.attended_mode == AttendedMode::Attended && !interactive_owner_ready {
            blockers.push(BootAutoResumeBlocker::InteractiveOwnerUnavailable);
        }
        if run.workspace_id != self.active_workspace_id() {
            blockers.push(BootAutoResumeBlocker::WorkspaceMismatch);
        }
        match plan.as_ref() {
            None if execution_profile.plan_policy == RunPlanPolicy::AllowDirect => {}
            None => blockers.push(BootAutoResumeBlocker::PlanUnavailable),
            Some(plan)
                if plan.goal_revision != run.goal_revision
                    || plan.goal_sha256 != run.goal_sha256 =>
            {
                blockers.push(BootAutoResumeBlocker::GoalPlanMismatch);
            }
            Some(_) => {}
        }
        if let Some(state) = state.as_ref() {
            if state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget)
            {
                blockers.push(BootAutoResumeBlocker::TokenBudgetExhausted);
            }
            if state
                .time_budget_seconds
                .is_some_and(|budget| state.time_used_seconds >= budget)
            {
                blockers.push(BootAutoResumeBlocker::TimeBudgetExhausted);
            }
            if state.active_turn.is_some() {
                blockers.push(BootAutoResumeBlocker::ActiveRunTurn);
            }
        }
        if !self.active_subagent_boundaries(run_id)?.is_empty() {
            blockers.push(BootAutoResumeBlocker::ActiveSubagent);
        }
        if self
            .list_background_cells(run_id)?
            .iter()
            .any(BackgroundCellState::is_active)
        {
            blockers.push(BootAutoResumeBlocker::ActiveCommandCell);
        }
        if !self.list_recovery_blockers(run_id)?.is_empty() {
            blockers.push(BootAutoResumeBlocker::RecoveryBlocker);
        }
        if !blockers.is_empty() {
            return Ok(BootAutoResumeDecision::Blocked(blockers));
        }
        let retry_not_before = state
            .and_then(|state| state.provider_retry)
            .filter(|retry| !retry.exhausted && retry.next_retry_at > now)
            .map(|retry| retry.next_retry_at);
        Ok(BootAutoResumeDecision::Ready { retry_not_before })
    }

    /// Atomically mark a running run completed only when the latest committed
    /// revision is quiescent. A concurrent plan patch wins the same run lock
    /// and makes this return `false`, causing the executor to drain again.
    pub fn complete_run_if_quiescent(&self, run_id: &str) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status == TaskRunStatus::Completed {
                return Ok(true);
            }
            if run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            let report = self.completion_gate_report(run_id)?;
            if !report.ready {
                return Ok(false);
            }
            let active_turn = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|continuation| continuation.active_turn);
            if active_turn.is_some() {
                // The owning RunTurn commits Goal completion in the same
                // journal batch as RunTurnFinished. Returning true tells the
                // task executor the graph is quiescent without opening a
                // Completed + active-turn crash window.
                return Ok(true);
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": TaskRunStatus::Running.as_str(),
                    "to": TaskRunStatus::Completed.as_str(),
                    "plan_revision": report.plan_revision,
                    "goal_revision": report.goal_revision,
                    "requirement_count": report.requirements.len(),
                }),
            ))?;
            Ok(true)
        })
    }

    // ── Task-level cancellation ────────────────────────────────────────────
    // These in-memory tokens let runtime control actions stop one Subagent
    // promptly without changing the immutable task specification.

    /// Register a cancellation token for a task that is about to start running.
    /// Called by the executor before dispatching the subagent. The token is a
    /// child of the run-level cancel, so run cancel still propagates.
    pub fn register_task_cancel_token(
        &self,
        run_id: &str,
        task_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.insert(key, token);
        }
    }

    /// Remove a task's cancellation token after it completes (success/fail).
    /// Called by the executor when execute_task returns.
    pub fn unregister_task_cancel_token(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.remove(&key);
        }
    }

    /// Cancel a specific task's Subagent if one is currently running.
    pub fn cancel_task(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            #[allow(clippy::collapsible_if)]
            // nested let-Ok/let-Some reads clearer than a let-chain
            if let Some(token) = map.remove(&key) {
                token.cancel();
            }
        }
    }

    /// Register the active driver token and automatically restore/remove it
    /// when the returned guard is dropped.
    pub fn register_run_cancellation(
        self: &std::sync::Arc<Self>,
        run_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) -> Result<RunCancellationRegistration, StoreError> {
        self.register_run_cancellation_internal(run_id, token)
    }

    fn register_run_cancellation_internal(
        self: &std::sync::Arc<Self>,
        run_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) -> Result<RunCancellationRegistration, StoreError> {
        let registration_id = self
            .next_run_cancel_registration
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |current| current.checked_add(1),
            )
            .map(|previous| previous.saturating_add(1))
            .map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRun cancellation registration capacity exhausted".to_string(),
                )
            })?;
        self.run_cancel_tokens
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .entry(run_id.to_string())
            .or_default()
            .push((registration_id, token));
        Ok(RunCancellationRegistration {
            store: self.clone(),
            run_id: run_id.to_string(),
            registration_id,
        })
    }

    /// Whether this process currently owns a live driver for `run_id`.
    /// Persisted `Running` alone is insufficient because a killed/restarted
    /// process can leave that status behind; cleanup uses this in-memory fact
    /// to avoid touching a worktree that an active run still owns.
    pub fn is_run_active(&self, run_id: &str) -> bool {
        self.run_cancel_tokens
            .lock()
            .map(|map| map.contains_key(run_id))
            .unwrap_or(false)
    }

    /// Wait until no live driver owns this run. The notification is armed
    /// before the state check so a release between the two cannot be missed.
    pub async fn wait_for_run_driver_idle(&self, run_id: &str) {
        loop {
            let released = self.run_driver_idle.notified();
            if !self.is_run_active(run_id) {
                return;
            }
            released.await;
        }
    }

    fn active_run_cancel_tokens(&self, run_id: &str) -> Vec<echo_agent::agent::CancellationToken> {
        self.run_cancel_tokens
            .lock()
            .ok()
            .and_then(|map| map.get(run_id).cloned())
            .map(|entries| entries.into_iter().map(|(_, token)| token).collect())
            .unwrap_or_default()
    }

    /// Request cancellation through the single TaskRuntime control path.
    /// Active runs are stopped through their driver token so the executor owns
    /// the terminal transition. Runs without a driver may only be cancelled
    /// directly when they are not executing.
    pub fn request_cancel(&self, run_id: &str) -> Result<bool, StoreError> {
        let _operation = self.shadow_operation()?;
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        if !tokens.is_empty() {
            // Durable Cancelled is committed before signalling the driver. It
            // therefore wins a concurrent Paused intent at the controller cut.
            self.finalize_cancelled_tasks_and_run(run_id)?;
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
            self.stop_owned_command_cells(run_id)?;
            return Ok(true);
        }
        let Some(run) = self.get_run(run_id)? else {
            return Ok(false);
        };
        match run.status {
            TaskRunStatus::Pending
            | TaskRunStatus::Running
            | TaskRunStatus::Paused
            | TaskRunStatus::Failed => {
                self.finalize_cancelled_tasks_and_run(run_id)?;
                super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
                self.stop_owned_command_cells(run_id)?;
                Ok(true)
            }
            TaskRunStatus::Cancelled | TaskRunStatus::Completed => Ok(false),
        }
    }

    /// Pause an actively driven run. The status changes first, then the same
    /// run-scoped token used for cancellation stops in-flight Subagents. The
    /// executor observes the durable Paused status and leaves the run resumable.
    /// Resolve the durable control identity of the currently active
    /// execution under `execution_id`, if any. Used by the Subagent uplink to
    /// address sibling attempts without knowing their task/attempt fields.
    pub fn active_control_identity(
        &self,
        execution_id: &str,
    ) -> Option<crate::tasks::task_runtime::types::SubagentControlIdentity> {
        self.active_subagent_controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(execution_id)
            .map(|target| target.control_identity())
    }

    pub fn request_pause(&self, run_id: &str) -> Result<bool, StoreError> {
        self.request_pause_with_reason(run_id, RunPauseReason::User, None)
    }

    /// Pause an active driver while persisting the structured reason in the
    /// same durable transition event. Background command cells intentionally keep
    /// running; explicit cancellation is the only path that stops them.
    pub fn request_pause_with_reason(
        &self,
        run_id: &str,
        reason: RunPauseReason,
        detail: Option<&str>,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let transition = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            self.commit_runtime_events(
                run_id,
                vec![
                    RuntimeJournalEvent::for_append(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::RunPauseReasonChanged,
                        serde_json::json!({
                            "reason": reason.as_str(),
                            "detail": detail.map(|text| text.chars().take(600).collect::<String>()),
                        }),
                    ),
                    RuntimeJournalEvent::for_append(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::RunStatusChanged,
                        serde_json::json!({
                            "from": TaskRunStatus::Running.as_str(),
                            "to": TaskRunStatus::Paused.as_str(),
                        }),
                    ),
                ],
            )?;
            Ok(true)
        });
        let transitioned = transition?;
        if !transitioned {
            return Ok(false);
        }
        for token in tokens {
            token.cancel();
        }
        super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        Ok(true)
    }

    /// Unit-test fixture helper for committing a prepared initial plan.
    #[cfg(test)]
    pub(crate) fn attach_plan_for_test(&self, plan: &TaskPlan) -> Result<(), StoreError> {
        self.with_run_lock(&plan.run_id, || {
            let run = self
                .get_run(&plan.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(plan.run_id.clone()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot create a plan for terminal run {} ({:?})",
                    plan.run_id, run.status
                )));
            }
            if self.get_plan(&plan.run_id)?.is_some() {
                return Err(StoreError::InvalidPlan(
                    "plan already exists; submit a revisioned task_update".to_string(),
                ));
            }
            if plan.tasks.iter().any(|task| {
                task.status != echo_agent::tasks::TaskStatus::Pending
                    || task.retry_count != 0
                    || task.failure_fingerprint.is_some()
            }) {
                return Err(StoreError::InvalidPlan(
                    "initial plan tasks must have pending execution state".to_string(),
                ));
            }
            validate_runtime_plan(&plan.tasks)?;
            let mut committed = plan.clone();
            committed.revision = 1;
            committed.goal_revision = run.goal_revision;
            committed.goal_sha256 = run.goal_sha256;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                plan.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": 0,
                    "reason": "initial complete plan",
                    "created_task_ids": committed.tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                    "plan": committed.specification(),
                }),
            ))?;
            Ok(())
        })
    }

    /// Load the product-neutral framework graph without projecting rich task
    /// execution states through EKO's smaller UI status enum.
    pub(crate) fn load_revisioned_task_graph(
        &self,
        run_id: &str,
    ) -> Result<Option<echo_agent::tasks::RevisionedTaskGraph>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow.ensure_projections_current(run_id)?;
        let Some(plan) = self.shadow.read_plan(run_id)? else {
            return Ok(None);
        };
        let state = self
            .shadow
            .read_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let run = state.run.clone();
        let mut executions = state
            .tasks
            .into_iter()
            .map(|execution| (execution.task_id.clone(), execution))
            .collect::<std::collections::HashMap<_, _>>();
        let mut tasks = Vec::with_capacity(plan.tasks.len());
        for spec in plan.tasks {
            let execution = executions
                .remove(&spec.id)
                .unwrap_or_else(|| echo_agent::tasks::TaskExecution::pending(spec.id.clone()));
            let framework_spec: echo_agent::tasks::TaskSpec = spec
                .try_into()
                .map_err(StoreError::InvalidPlan)?;
            tasks.push(echo_agent::tasks::Task {
                spec: framework_spec,
                execution: echo_agent::tasks::TaskExecution {
                    task_id: execution.task_id,
                    status: execution.status,
                    retry_count: execution.retry_count,
                    failure_fingerprint: execution.failure_fingerprint,
                    claim: execution.claim,
                },
            });
        }
        let context_metadata = serde_json::to_value(EkoPlanMetadata {
            plan_id: plan.plan_id,
            domain_profile: plan.domain_profile,
            goal_revision: plan.goal_revision,
            goal_sha256: plan.goal_sha256,
        })?;
        Ok(Some(echo_agent::tasks::RevisionedTaskGraph {
            snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                revision: plan.revision,
                tasks,
            },
            context: echo_agent::tasks::TaskGraphContext {
                goal: run.goal,
                assumptions: plan.assumptions,
                risks: plan.risks,
                execution_mode: match plan.execution_mode {
                    ExecutionMode::Sequential => {
                        echo_agent::tasks::TaskGraphExecutionMode::Sequential
                    }
                    ExecutionMode::Parallel => echo_agent::tasks::TaskGraphExecutionMode::Parallel,
                },
                metadata: context_metadata,
            },
        }))
    }

    /// Persist one framework-computed graph candidate with optimistic
    /// concurrency. Patch semantics and DAG validation have already run in
    /// `TaskRevisionService`; this adapter only validates the EKO extension and
    /// serializes the optimistic commit and all evidence revalidation as one
    /// atomic journal batch.
    pub(crate) fn compare_and_commit_revisioned_task_graph(
        &self,
        run_id: &str,
        commit: echo_agent::tasks::TaskGraphCommit,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot modify terminal run {} ({:?})",
                    run_id, run.status
                )));
            }
            let previous_plan = self.get_plan(run_id)?;
            let previous_requirements = previous_plan
                .as_ref()
                .map(super::completion_gate::requirements_for_plan)
                .unwrap_or_default();
            let current = self.load_revisioned_task_graph(run_id)?;
            let prepared = prepare_revisioned_graph_commit(run_id, &run, current.as_ref(), commit)?;
            let revalidated_requirements = previous_plan
                .as_ref()
                .filter(|previous| previous.goal_revision != run.goal_revision)
                .map(|_previous| {
                    super::completion_gate::requirements_for_revision(&prepared.plan)
                        .into_iter()
                        .filter_map(|requirement| {
                            previous_requirements
                                .iter()
                                .find(|old| {
                                    old.requirement_id == requirement.requirement_id
                                        && old.requirement_sha256 == requirement.requirement_sha256
                                })
                                .map(|old| (old.clone(), requirement))
                        })
                        .map(|(old, requirement)| {
                            (old.goal_revision, old.plan_revision, requirement)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                prepared.payload,
            )];
            for (old_goal_revision, old_plan_revision, requirement) in revalidated_requirements {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(requirement.task_id.as_str()),
                    None,
                    RuntimeEventKind::RequirementEvidenceRevalidated,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "old_goal_revision": old_goal_revision,
                        "new_goal_revision": run.goal_revision,
                        "old_plan_revision": old_plan_revision,
                        "new_plan_revision": requirement.plan_revision,
                    }),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            Ok(prepared.next)
        })
    }

    pub(crate) fn compare_and_commit_direct_completion(
        &self,
        run_id: &str,
        commit: echo_agent::tasks::TaskGraphCommit,
        summary: &TaskExecutionSummary,
        task_summary: &str,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status != TaskRunStatus::Running {
                return Err(StoreError::InvalidPlan(format!(
                    "direct completion requires a Running TaskRun, got {}",
                    run.status.as_str()
                )));
            }
            let active_turn = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|continuation| continuation.active_turn);
            if active_turn.is_none() {
                return Err(StoreError::InvalidPlan(
                    "direct completion requires an active canonical RunTurn".to_string(),
                ));
            }
            let current = self.load_revisioned_task_graph(run_id)?;
            let prepared = prepare_revisioned_graph_commit(run_id, &run, current.as_ref(), commit)?;
            let task = prepared.plan.tasks.first().ok_or_else(|| {
                StoreError::InvalidPlan("direct completion contains no task".to_string())
            })?;
            if prepared.plan.tasks.len() != 1
                || task.id != summary.task_id
                || summary.run_id != run_id
                || task.kind != PlanTaskKind::Summary
                || !task.required_artifacts.is_empty()
                || !task.execution_checks.is_empty()
                || !task.acceptance_criteria.is_empty()
                || !task.allowed_tools.is_empty()
                || task.execution_target.is_some()
                || summary.outcome.status != SubagentStatus::Completed
                || summary.outcome.summary.trim().is_empty()
                || !summary.outcome.remaining_work.is_empty()
                || summary.outcome.evidence.iter().any(|evidence| {
                    evidence.kind == "file_write"
                        && evidence.outcome.as_deref() == Some("succeeded")
                })
                || !summary.outcome.touched_files.written.is_empty()
                || task_summary.trim().is_empty()
            {
                return Err(StoreError::InvalidPlan(
                    "direct completion does not satisfy the fixed single-summary evidence contract"
                        .to_string(),
                ));
            }
            if !self.active_subagent_boundaries(run_id)?.is_empty()
                || self
                    .list_background_cells(run_id)?
                    .iter()
                    .any(BackgroundCellState::is_active)
                || !self.list_recovery_blockers(run_id)?.is_empty()
            {
                return Err(StoreError::InvalidPlan(
                    "direct completion is blocked by active or unresolved runtime work".to_string(),
                ));
            }
            let events = vec![
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::PlanRevisionCommitted,
                    prepared.payload,
                ),
                task_status_runtime_event(TaskStatusEvent {
                    run_id,
                    task_id: &task.id,
                    task_subject: &task.title,
                    status: echo_agent::tasks::TaskStatus::Running,
                    owner_agent: Some(&task.agent_role),
                    summary: None,
                    claim: None,
                }),
                RuntimeJournalEvent::for_append(
                    run_id,
                    Some(&task.id),
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "kind": "summary_persisted",
                        "summary": summary,
                    }),
                ),
                task_status_runtime_event(TaskStatusEvent {
                    run_id,
                    task_id: &task.id,
                    task_subject: &task.title,
                    status: echo_agent::tasks::TaskStatus::Completed,
                    owner_agent: Some(&task.agent_role),
                    summary: Some(task_summary),
                    claim: None,
                }),
            ];
            self.commit_runtime_events(run_id, events)?;
            Ok(prepared.next)
        })
    }

    /// Publish a pending run and revision 1 as one visible file generation.
    /// A process failure before the final rename leaves only a hidden staging
    /// directory, which startup removes without exposing a partial TaskRun.
    pub(crate) fn compare_and_publish_initial_revisioned_task_graph(
        &self,
        run: &TaskRun,
        trigger: &InitialRunTriggerMetadata,
        continuation: Option<(bool, bool, Option<u64>, Option<u64>)>,
        commit: echo_agent::tasks::TaskGraphCommit,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(&run.run_id, || {
            if self.get_run(&run.run_id)?.is_some() {
                return Err(StoreError::PlanConflict {
                    run_id: run.run_id.clone(),
                    expected: 0,
                    current: self
                        .load_revisioned_task_graph(&run.run_id)?
                        .map(|graph| graph.snapshot.revision)
                        .unwrap_or_default(),
                });
            }
            if run.status != TaskRunStatus::Pending || run.plan_id.is_some() {
                return Err(StoreError::InvalidPlan(
                    "initial task publication requires an uncommitted pending run".to_string(),
                ));
            }
            let prepared = prepare_revisioned_graph_commit(&run.run_id, run, None, commit)?;
            let timestamp = Utc::now();
            let mut events = vec![
                super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({
                        "goal": run.goal,
                        "goal_revision": run.goal_revision,
                        "goal_sha256": run.goal_sha256,
                        "domain_profile": run.domain_profile.as_str(),
                        "workspace_id": run.workspace_id,
                        "conversation_id": run.conversation_id,
                        "root_message_id": run.root_message_id,
                        "route": run.route,
                        "attended_mode": run.attended_mode.as_str(),
                        "attachments": run.attachments,
                        "created_at": echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
                        "execution_profile": TaskRunExecutionProfile::default(),
                    }),
                    timestamp,
                ),
                super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::PlanRevisionCommitted,
                    prepared.payload,
                    timestamp,
                ),
            ];
            if let Some((enabled, auto_resume, token_budget, time_budget_seconds)) = continuation {
                events.push(super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({
                        "enabled": enabled,
                        "auto_resume_after_restart": auto_resume,
                        "token_budget": token_budget,
                        "time_budget_seconds": time_budget_seconds,
                    }),
                    timestamp,
                ));
            }
            events.push(super::run_authority::RuntimeJournalEvent::new(
                run.run_id.clone(),
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "trigger_metadata",
                    "source": trigger.source,
                    "task_kind": trigger.kind,
                    "prompt": trigger.prompt,
                    "priority": trigger.priority.min(10),
                }),
                timestamp,
            ));
            self.shadow
                .publish_initial_event_batch(&run.run_id, events)?;
            Ok(prepared.next)
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_initial_publish_before_rename(&self) {
        self.shadow.fail_next_initial_publish_before_rename();
    }

    /// Unit-test convenience for exercising the canonical framework patch
    /// engine through EKO's file commit operation.
    #[cfg(test)]
    pub(crate) fn apply_task_patch_for_test(
        &self,
        run_id: &str,
        request: &TaskUpdateRequest,
    ) -> Result<TaskPlan, StoreError> {
        self.get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let current = self
            .load_revisioned_task_graph(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        if current.snapshot.revision != request.base_revision {
            return Err(StoreError::PlanConflict {
                run_id: run_id.to_string(),
                expected: request.base_revision,
                current: current.snapshot.revision,
            });
        }
        if request.reason.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "task_update requires a non-empty reason".to_string(),
            ));
        }
    let patch: echo_agent::tasks::TaskPlanPatch = request
        .try_into()
            .map_err(StoreError::InvalidPlan)?;
        let application = echo_agent::tasks::TaskPatchEngine::apply_operations(
            &current.snapshot.tasks,
            patch.operations,
            false,
        )
        .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        echo_agent::tasks::PlanValidator::default()
            .validate_task_snapshot(&application.tasks)
            .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))?;
        let next_revision = current
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPlan("plan revision overflow".to_string()))?;
        self.compare_and_commit_revisioned_task_graph(
            run_id,
            echo_agent::tasks::TaskGraphCommit {
                expected_revision: Some(current.snapshot.revision),
                next: echo_agent::tasks::RevisionedTaskGraph {
                    snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                        revision: next_revision,
                        tasks: application.tasks,
                    },
                    context: current.context,
                },
                reason: patch.reason,
                effects: application.effects,
            },
        )?;
        self.get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
    }

    // ── Task / todo mutations ───────────────────────────────────────────

    /// Test-only fixture helper. Production task transitions are exclusively
    /// owned by the framework runtime and revision services above.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn set_task_status(
        &self,
        run_id: &str,
        task_id: &str,
        status: echo_agent::tasks::TaskStatus,
        owner_agent: Option<&str>,
        summary: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let status = match status {
                echo_agent::tasks::TaskStatus::Failed(detail) if detail.is_empty() => {
                    echo_agent::tasks::TaskStatus::Failed(
                        summary.unwrap_or("task failed").to_string(),
                    )
                }
                echo_agent::tasks::TaskStatus::Blocked(detail) if detail.is_empty() => {
                    echo_agent::tasks::TaskStatus::Blocked(
                        summary.unwrap_or("task blocked").to_string(),
                    )
                }
                echo_agent::tasks::TaskStatus::TimedOut { error } if error.is_empty() => {
                    echo_agent::tasks::TaskStatus::TimedOut {
                        error: summary.unwrap_or("task timed out").to_string(),
                    }
                }
                other => other,
            };
            // U1c phase-0/0bc step-2: file authority. Validate the task exists
            // (read plan from file), then append the canonical task status event with
            // explicit started_at/completed_at and rewrite plan.json. No SQL write.
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            let task = plan
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            self.append_task_status_event(TaskStatusEvent {
                run_id,
                task_id,
                task_subject: &task.title,
                status,
                owner_agent,
                summary,
                claim: None,
            })
        })
    }

    /// Load one coherent framework snapshot under the run mutation lock.
    pub(crate) fn load_runtime_plan_snapshot(
        &self,
        run_id: &str,
    ) -> Result<echo_agent::tasks::RuntimePlanSnapshot, StoreError> {
        self.with_run_lock(run_id, || {
            self.load_revisioned_task_graph(run_id)?
                .map(|graph| graph.snapshot)
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
        })
    }

    fn persist_runtime_execution_changes(
        &self,
        run_id: &str,
        before: &echo_agent::tasks::RuntimePlanSnapshot,
        after: &echo_agent::tasks::RuntimePlanSnapshot,
        summary: Option<&str>,
        mut product_events: Vec<RuntimeJournalEvent>,
    ) -> Result<(), StoreError> {
        let mut events = runtime_execution_change_events(run_id, before, after, summary)?;
        events.append(&mut product_events);
        if events.is_empty() {
            return Ok(());
        }
        self.commit_runtime_events(run_id, events)
    }

    fn commit_runtime_event(&self, event: RuntimeJournalEvent) -> Result<(), StoreError> {
        let run_id = event.run_id().to_string();
        self.commit_runtime_events(&run_id, vec![event])
    }

    pub(super) fn commit_runtime_events(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<(), StoreError> {
        let receipt = self.commit_runtime_events_with_receipt(run_id, events)?;
        self.observe_projection_receipt(run_id, &receipt);
        Ok(())
    }

    fn commit_runtime_events_with_receipt(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<ProjectionCommitReceipt, StoreError> {
        let committed = self.shadow.append_event_batch(run_id, events)?;
        let sequence = i64::try_from(committed.apply.last_sequence).map_err(|_| {
            StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
        })?;
        let projection = self.refresh_committed_projection(run_id, sequence);
        Ok(Self::classify_committed_projection(
            sequence,
            committed.apply.journal,
            committed.apply.checkpoint,
            committed.history,
            projection,
        ))
    }

    fn observe_projection_receipt(&self, run_id: &str, receipt: &ProjectionCommitReceipt) {
        if let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail } = receipt {
            tracing::warn!(
                run_id,
                seq,
                %detail,
                "TaskRuntime event committed; derived projection will self-heal on read"
            );
        }
    }

    fn refresh_committed_projection(
        &self,
        run_id: &str,
        last_committed_seq: i64,
    ) -> ProjectionCommitReceipt {
        #[cfg(test)]
        if self
            .fail_next_runtime_mutation_projection
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: last_committed_seq,
                detail: "injected runtime mutation projection degradation".to_string(),
            };
        }
        match self.shadow.rewrite_plan(run_id) {
            Ok(()) => ProjectionCommitReceipt::Durable {
                seq: last_committed_seq,
            },
            Err(super::file_shadow::ShadowError::CommittedProjectionDegraded { seq, detail }) => {
                ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail }
            }
            Err(error) => ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: last_committed_seq,
                detail: error.to_string(),
            },
        }
    }

    fn classify_committed_projection(
        sequence: i64,
        journal: JournalDurabilityStatus,
        checkpoint: CheckpointApplyStatus,
        history: HistoryProjectionApplyStatus,
        projection: ProjectionCommitReceipt,
    ) -> ProjectionCommitReceipt {
        let mut degraded = Vec::new();
        match journal {
            JournalDurabilityStatus::Confirmed => {}
            JournalDurabilityStatus::Unconfirmed => {
                degraded.push("journal durability unconfirmed".to_string());
            }
            JournalDurabilityStatus::Degraded { error } => {
                degraded.push(format!("journal durability degraded: {error}"));
            }
        }
        if let CheckpointApplyStatus::Degraded { error } = checkpoint {
            degraded.push(format!("checkpoint durability degraded: {error}"));
        }
        if let HistoryProjectionApplyStatus::Degraded { error } = history {
            degraded.push(format!("history projection degraded: {error}"));
        }
        if let ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. } = projection {
            degraded.push(format!("projection refresh degraded: {detail}"));
        }
        if degraded.is_empty() {
            ProjectionCommitReceipt::Durable { seq: sequence }
        } else {
            ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: sequence,
                detail: degraded.join("; "),
            }
        }
    }

    /// Atomically claim a Pending task through the framework's canonical
    /// compare-and-set transformation.
    pub fn claim_runtime_task(
        &self,
        run_id: &str,
        expected_task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> Result<echo_agent::tasks::RuntimeTaskClaimOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::claim_runtime_task(
                &mut graph.snapshot,
                expected_task,
                expected_revision,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                None,
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Verify one physical claim against the exact persisted framework graph.
    pub fn runtime_task_claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            echo_agent::tasks::runtime_claim_is_current(&graph.snapshot, task_id, claim)
                .map_err(StoreError::from)
        })
    }

    /// Atomically commit one framework-owned dispatch resolution.
    pub(crate) fn settle_runtime_task_resolution(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        request: echo_agent::tasks::RuntimeTaskResolutionRequest,
        product: RuntimeTaskProductSettlement,
    ) -> Result<echo_agent::tasks::RuntimeTaskResolution, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::settle_runtime_resolution(
                &mut graph.snapshot,
                task_id,
                claim,
                request,
            )?;
            let RuntimeTaskProductSettlement {
                summary,
                execution_summary,
                review,
                diagnostic_note,
                typed_terminal: _,
            } = product;
            let mut product_events = Vec::new();
            if outcome != echo_agent::tasks::RuntimeTaskResolution::Superseded {
                if let Some(summary) = execution_summary {
                    product_events.push(RuntimeJournalEvent::for_append(
                        run_id,
                        Some(task_id),
                        None,
                        RuntimeEventKind::Note,
                        serde_json::json!({
                            "kind": "summary_persisted",
                            "summary": summary,
                        }),
                    ));
                }
                if let Some(review) = review {
                    product_events.push(review_runtime_event(&review, Some(claim)));
                }
                if let Some(message) = diagnostic_note {
                    product_events.push(RuntimeJournalEvent::for_append(
                        run_id,
                        Some(task_id),
                        None,
                        RuntimeEventKind::Note,
                        serde_json::json!({
                            "message": message,
                            "claim_id": claim.claim_id,
                            "plan_revision": claim.revision,
                            "attempt": claim.attempt,
                            "spec_hash": claim.spec_hash,
                        }),
                    ));
                }
            }
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                product_events,
            )?;
            Ok(outcome)
        })
    }

    /// Settle one exact claim to a terminal or product-blocked state.
    pub fn settle_runtime_task_claim(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        status: echo_agent::tasks::TaskStatus,
        summary: Option<String>,
    ) -> Result<echo_agent::tasks::RuntimeTaskSettlementOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::settle_runtime_claim(
                &mut graph.snapshot,
                task_id,
                claim,
                status,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Settle all unfinished tasks for one exact run-level interruption.
    pub fn settle_runtime_task_interruption(
        &self,
        run_id: &str,
        expected_revision: u64,
        disposition: echo_agent::tasks::RuntimeInterruptionDisposition,
    ) -> Result<echo_agent::tasks::RuntimeInterruptionSettlementOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let summary = match &disposition {
                echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled => {
                    "run cancelled".to_string()
                }
                echo_agent::tasks::RuntimeInterruptionDisposition::Paused { reason } => {
                    reason.clone()
                }
            };
            let outcome = echo_agent::tasks::settle_runtime_interruption(
                &mut graph.snapshot,
                expected_revision,
                disposition,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                Some(&summary),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Explicitly retry one exact unclaimed task. Run restart policy remains
    /// outside this framework mutation.
    pub fn retry_runtime_task(
        &self,
        run_id: &str,
        expected_task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> Result<echo_agent::tasks::RuntimeTaskRetryOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::retry_runtime_task(
                &mut graph.snapshot,
                expected_task,
                expected_revision,
            )?;
            let summary = match outcome {
                echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count } => {
                    Some(format!("explicit retry (retry_count {retry_count})"))
                }
                echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted { .. }
                | echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => None,
            };
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn append_task_status_event(&self, event: TaskStatusEvent<'_>) -> Result<(), StoreError> {
        let run_id = event.run_id;
        self.commit_runtime_events(run_id, vec![task_status_runtime_event(event)])
    }

    /// Atomically retry a Blocked/Failed task in a Paused/Failed run.
    ///
    /// Framework retry state and the EKO run restart event are appended in one
    /// journal batch. Dependency blockers remain a framework DAG projection;
    /// this product transaction never discovers descendants from summary text.
    pub fn retry_blocked_task(&self, run_id: &str, task_id: &str) -> Result<u32, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if !matches!(run.status, TaskRunStatus::Paused | TaskRunStatus::Failed) {
                return Err(StoreError::InvalidPlan(format!(
                    "run {} is {:?}; retry requires Paused or Failed",
                    run_id, run.status
                )));
            }
            if !run.status.can_transition_to(TaskRunStatus::Running) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: run.status.as_str().to_string(),
                    to: TaskRunStatus::Running.as_str().to_string(),
                });
            }
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let expected_task = graph
                .snapshot
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .cloned()
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::retry_runtime_task(
                &mut graph.snapshot,
                &expected_task,
                before.revision,
            )?;
            let next = match outcome {
                echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count } => retry_count,
                echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted {
                    retry_count,
                    max_retries,
                } => {
                    return Err(StoreError::InvalidPlan(format!(
                        "task {task_id} retry budget exhausted ({retry_count}/{max_retries})"
                    )));
                }
                echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => {
                    return Err(StoreError::InvalidPlan(format!(
                        "task {task_id} changed before retry"
                    )));
                }
            };
            let product_events = vec![
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "message": format!("user retried blocked task {task_id} (attempt {next})"),
                    }),
                ),
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": run.status.as_str(),
                        "to": TaskRunStatus::Running.as_str(),
                    }),
                ),
            ];
            let summary = format!("user-initiated retry (attempt {next})");
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                Some(&summary),
                product_events,
            )?;
            Ok(next)
        })
    }

    #[cfg(test)]
    pub(crate) fn add_review(&self, r: &ReviewResult) -> Result<(), StoreError> {
        self.with_run_lock(&r.run_id, || {
            self.commit_runtime_events(&r.run_id, vec![review_runtime_event(r, None)])
        })
    }

    pub fn add_artifact(&self, a: &Artifact) -> Result<(), StoreError> {
        self.with_run_lock(&a.run_id, || {
            // U1c phase-0/0bc step-2: file authority. ArtifactProduced carries the
            // full artifact so FileTaskStore.list_artifacts can derive it. No SQL.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                a.run_id.as_str(),
                a.task_id.as_deref(),
                None,
                RuntimeEventKind::ArtifactProduced,
                serde_json::json!({
                    "artifact_id": a.id,
                    "kind": a.kind.as_str(),
                    "title": a.title,
                    "task_id": a.task_id,
                    "path": a.path,
                    "metadata": a.metadata,
                }),
            ))
        })
    }

    /// Persist or overwrite the per-task execution summary. Primary key is
    /// `(run_id, task_id)` so a re-execution replaces the prior summary. The
    /// write is transactional and appends a `Note` event so the GUI and the
    /// recovery path can tell when a summary was updated (consistent with the
    /// "every state-relevant change writes a TaskEvent" invariant).
    pub fn put_summary(&self, s: &TaskExecutionSummary) -> Result<(), StoreError> {
        self.with_run_lock(&s.run_id, || {
            // U1c phase-0/0bc step-2: file authority. Note{summary_persisted}
            // carries the full summary so FileTaskStore.get_summary can derive it.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                s.run_id.as_str(),
                Some(s.task_id.as_str()),
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "summary_persisted",
                    // Full summary so events.jsonl can rebuild plan.json task summaries.
                    "summary": s,
                }),
            ))
        })
    }

    // ── Read paths (used by Tauri query commands + recovery) ────────────

    pub fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, StoreError> {
        // U1c phase-0/0bc step-2: read delegates to the file store (file authority).
        self.file_store()?
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Read just the `route` column for a given run. Returns `None` when the
    /// run does not exist.
    pub fn get_run_route(&self, run_id: &str) -> Result<Option<String>, StoreError> {
        // U1c phase-0/0bc step-2: delegate to file store, project the route field.
        self.file_store()?
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
            .map(|r| r.map(|r| r.route))
    }

    /// Latest run for a conversation (used by GUI to bind a chat to its run).
    pub fn latest_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()?
            .latest_run_for_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Latest run that belongs in the task UI. Eager conversation runs remain
    /// journaled but stay out of the complex-task projection until they commit
    /// a real plan; orchestrated direct runs remain visible without a plan.
    pub fn latest_task_ui_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        let runs = self
            .file_store()?
            .list_runs()
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?;
        for run in runs
            .into_iter()
            .filter(|run| run.conversation_id == conversation_id)
        {
            let profile = self
                .get_run_state(&run.run_id)?
                .map(|snapshot| snapshot.execution_profile)
                .unwrap_or_default();
            if !profile.is_conversation_turn() || self.get_plan(&run.run_id)?.is_some() {
                return Ok(Some(run));
            }
        }
        Ok(None)
    }

    /// Find an in-progress (Running or Paused) run for a conversation, if any.
    /// Used by the interrupt-detection logic: if a user sends a new message
    /// while a run is still executing, the system should prompt them rather
    /// than silently starting a second run.
    pub fn find_in_progress_run_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()?
            .find_in_progress_run_by_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_runs_in(&self, statuses: &[TaskRunStatus]) -> Result<Vec<TaskRun>, StoreError> {
        self.file_store()?
            .list_runs_in(statuses)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Rebuild all Subagent execution instances for a run from lifecycle and
    /// usage events. `SubagentReleased.usage` is the terminal aggregate when
    /// available; usage events provide the live projection while it is running.
    pub fn list_subagent_runs(&self, run_id: &str) -> Result<Vec<SubagentRun>, StoreError> {
        self.list_subagent_run_snapshots(run_id, usize::MAX)
            .map(|snapshots| snapshots.into_iter().map(|snapshot| snapshot.run).collect())
    }

    /// Rebuild at most `limit` Subagent attempts while scanning the journal in
    /// fixed-size pages. The retained map is bounded even when the TaskRun has
    /// a long history; ordering matches [`Self::list_subagent_runs`].
    pub fn list_subagent_run_snapshots(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<SubagentRunSnapshot>, StoreError> {
        self.project_subagent_runs(run_id, limit, None)
            .map(|runs| runs.into_values().collect())
    }

    /// Rebuild one exact Subagent attempt without first constructing the full
    /// run history vector.
    pub fn get_subagent_run_snapshot(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<Option<SubagentRunSnapshot>, StoreError> {
        self.project_subagent_runs(run_id, 1, Some(execution_id))
            .map(|mut runs| runs.remove(execution_id))
    }

    fn project_subagent_runs(
        &self,
        run_id: &str,
        limit: usize,
        exact_execution_id: Option<&str>,
    ) -> Result<std::collections::BTreeMap<String, SubagentRunSnapshot>, StoreError> {
        const SCAN_PAGE_SIZE: usize = 256;

        if limit == 0 {
            return Ok(std::collections::BTreeMap::new());
        }
        let mut runs = std::collections::BTreeMap::new();
        let mut after_sequence = 0_i64;
        loop {
            let events = self.query_events_bounded(
                run_id,
                RuntimeEventQuery::new(after_sequence, SCAN_PAGE_SIZE),
            )?;
            let event_count = events.len();
            if event_count == 0 {
                break;
            }
            for event in events {
                after_sequence = event.seq;
                apply_subagent_projection_event(
                    &mut runs,
                    run_id,
                    limit,
                    exact_execution_id,
                    event,
                );
            }
            if event_count < SCAN_PAGE_SIZE {
                break;
            }
        }
        Ok(runs)
    }

    pub fn list_runs_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TaskRun>, StoreError> {
        self.file_store()?
            .list_runs()
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
            .map(|runs| {
                runs.into_iter()
                    .filter(|run| run.conversation_id == conversation_id)
                    .collect()
            })
    }

    /// Remove every TaskRun owned by one conversation after its drivers have
    /// settled. The outer conversation deletion transaction owns retries; this
    /// primitive owns only TaskRuntime files and process-local projections.
    pub fn remove_conversation(&self, conversation_id: &str) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let runs = super::file_store::FileTaskStore::new((*self.shadow).clone())
            .list_runs()
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?
            .into_iter()
            .filter(|run| run.conversation_id == conversation_id)
            .collect::<Vec<_>>();
        let active_run_ids = runs
            .iter()
            .filter(|run| self.is_run_active(&run.run_id))
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        if !active_run_ids.is_empty() {
            return Err(StoreError::ConversationHasActiveRuns {
                conversation_id: conversation_id.to_string(),
                run_ids: active_run_ids,
            });
        }

        let run_ids = runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>();
        for run_id in &run_ids {
            self.stop_owned_command_cells(run_id)?;
        }
        let removal = self.shadow.remove_runs(&run_ids);
        let committed_degraded = matches!(
            &removal,
            Err(super::file_shadow::ShadowError::CommittedDeletionDegraded { .. })
        );
        if removal.is_err() && !committed_degraded {
            return removal.map_err(Into::into);
        }
        for run_id in &run_ids {
            super::continuation::clear_launcher(self, run_id);
            self.plan_locks.remove(run_id);
        }
        if let Ok(mut tokens) = self.run_cancel_tokens.lock() {
            for run_id in &run_ids {
                tokens.remove(run_id);
            }
        }
        if let Ok(mut tokens) = self.task_cancel_tokens.lock() {
            tokens.retain(|key, _| {
                !run_ids
                    .iter()
                    .any(|run_id| key.starts_with(&format!("{run_id}::")))
            });
        }
        if let Ok(mut controls) = self.active_subagent_controls.lock() {
            controls
                .retain(|_, target| !run_ids.iter().any(|run_id| target.belongs_to_run(run_id)));
        }
        removal.map_err(Into::into)
    }

    pub(crate) fn active_subagent_boundaries(
        &self,
        run_id: &str,
    ) -> Result<Vec<ActiveSubagentBoundary>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.event_index.active_subagents)
            .unwrap_or_default())
    }

    fn active_tool_boundaries(&self, run_id: &str) -> Result<Vec<ActiveToolBoundary>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.event_index.active_tools)
            .unwrap_or_default())
    }

    #[cfg(test)]
    fn record_recovery_blocker(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: Option<&str>,
        call_id: Option<&str>,
        tool_name: Option<&str>,
        reason: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            execution_id,
            RuntimeEventKind::RecoveryBlocked,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "reason": reason,
            }),
        ))
    }

    /// Recover every run whose process-scoped driver disappeared at restart.
    ///
    /// One `RunStatusChanged` event contains the complete recovery generation.
    /// Failure before that append leaves `Running` as the retry marker. Failure
    /// after it can only leave derived files stale; the next canonical read
    /// repairs those files from the event tail without appending a duplicate.
    pub fn recover_incomplete(&self) -> Result<usize, StoreError> {
        let _operation = self.shadow_operation()?;
        const INTERRUPTED: &[TaskRunStatus] = &[TaskRunStatus::Running, TaskRunStatus::Paused];
        let zombies = self.list_runs_in(INTERRUPTED)?;
        let mut recovered = 0_usize;
        for run in &zombies {
            let changed = match run.status {
                TaskRunStatus::Running => self.recover_interrupted_run(run)?,
                TaskRunStatus::Paused => self.recover_paused_orphan_cells(run)?,
                _ => false,
            };
            if changed {
                recovered = recovered.saturating_add(1);
            }
        }
        super::subagent_control::reconcile_subagent_guidance_at_boot(self)?;
        Ok(recovered)
    }

    fn recover_paused_orphan_cells(&self, run: &TaskRun) -> Result<bool, StoreError> {
        let active = self
            .list_background_cells(&run.run_id)?
            .into_iter()
            .filter(BackgroundCellState::is_active)
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(false);
        }
        for cell in active {
            let artifact_interrupted =
                cell.artifact_status == BackgroundCellArtifactStatus::Writing;
            self.record_background_cell_finished(
                &run.run_id,
                &cell.cell_id,
                &cell.name,
                BackgroundCellPhase::Failed,
                Some(BackgroundCellTerminalCause::Interrupted),
                Some("command cell was interrupted by process restart"),
                None,
                if artifact_interrupted {
                    BackgroundCellArtifactStatus::Failed
                } else {
                    cell.artifact_status
                },
                if artifact_interrupted {
                    Some("artifact finalization was interrupted by process restart")
                } else {
                    cell.artifact_message.as_deref()
                },
                cell.total_output_bytes,
                cell.output_truncated,
                cell.output_excerpt.as_deref(),
                cell.artifact_path.as_deref(),
                cell.artifact_sha256.as_deref(),
                cell.call_id.as_deref(),
            )?;
        }
        Ok(true)
    }

    fn recover_interrupted_run(&self, run: &TaskRun) -> Result<bool, StoreError> {
        self.with_run_lock(&run.run_id, || {
            let state = self
                .get_run_state(&run.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run.run_id.clone()))?;
            if state.run.status != TaskRunStatus::Running {
                let was_boot_recovered = state
                    .continuation
                    .as_ref()
                    .and_then(|continuation| continuation.pause.as_ref())
                    .is_some_and(|pause| pause.reason == RunPauseReason::BootRecovery);
                self.shadow.rewrite_plan(&run.run_id)?;
                return Ok(was_boot_recovered);
            }

            let active_turn = state
                .continuation
                .as_ref()
                .and_then(|continuation| continuation.active_turn.as_ref())
                .map(|turn| serde_json::json!({ "turn_id": turn.turn_id }));
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 1_200,
                ..Default::default()
            };
            let orphan_cells = state
                .background_cells
                .iter()
                .filter(|cell| cell.is_active())
                .map(|cell| {
                    serde_json::json!({
                        "cell_id": cell.cell_id,
                        "name": cell.name,
                        "call_id": cell.call_id,
                        "total_output_bytes": cell.total_output_bytes,
                        "output_truncated": cell.output_truncated,
                        "output_excerpt": retention.sanitize_text(
                            "cell process ended with the previous application process"
                        ),
                        "artifact_path": cell.artifact_path,
                        "artifact_sha256": cell.artifact_sha256,
                    })
                })
                .collect::<Vec<_>>();
            let plan = self.get_plan(&run.run_id)?;
            let active_subagents = self.active_subagent_boundaries(&run.run_id)?;
            let active_tools = self.active_tool_boundaries(&run.run_id)?;
            let conversational_without_plan = plan.is_none()
                && state.execution_profile.provenance == TaskRunProvenance::ConversationTurn;
            let completed_turn_waiting_for_settlement = conversational_without_plan
                && active_turn.is_none()
                && orphan_cells.is_empty()
                && active_subagents.is_empty()
                && active_tools.is_empty()
                && state
                    .continuation
                    .as_ref()
                    .and_then(|continuation| continuation.last_turn.as_ref())
                    .is_some_and(|turn| turn.status == RunTurnStatus::Ended);
            let recovery_target = if completed_turn_waiting_for_settlement {
                TaskRunStatus::Completed
            } else if conversational_without_plan {
                TaskRunStatus::Cancelled
            } else {
                TaskRunStatus::Paused
            };
            let running_task_ids = state
                .tasks
                .iter()
                .filter(|task| task.status.is_running())
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>();
            let mut recovered_tasks = Vec::with_capacity(running_task_ids.len());
            for task_id in running_task_ids {
                let task = plan
                    .as_ref()
                    .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id));
                let execution_id = task.and_then(|task| {
                    task.claim
                        .as_ref()
                        .map(|claim| claim.execution_id(&run.run_id, &task.id))
                });
                let completed_subagent = match task.and_then(|task| task.claim.as_ref()) {
                    Some(claim) => self.recoverable_subagent_outcome_for_attempt(
                        &run.run_id,
                        &task_id,
                        &claim.execution_id(&run.run_id, &task_id),
                        claim.revision,
                        claim.attempt,
                    )?,
                    None => None,
                };
                let active_tool = active_tools
                    .iter()
                    .find(|boundary| boundary.task_id == task_id && !boundary.replay_safe)
                    .cloned();
                let active_subagent = active_subagents
                    .iter()
                    .find(|boundary| boundary.task_id == task_id && !boundary.replay_safe)
                    .cloned();
                let (next_status, summary) = if completed_subagent.is_some() {
                    (
                        echo_agent::tasks::TaskStatus::Pending,
                        "Subagent completed before interruption; pending review",
                    )
                } else if active_tool.is_some() || active_subagent.is_some() {
                    (
                        echo_agent::tasks::TaskStatus::Blocked(
                            "mutating side effect is indeterminate after restart".to_string(),
                        ),
                        "mutating side effect is indeterminate after restart",
                    )
                } else {
                    (
                        echo_agent::tasks::TaskStatus::Pending,
                        "interrupted; pending resume",
                    )
                };
                let blocker = if matches!(&next_status, echo_agent::tasks::TaskStatus::Blocked(_)) {
                    let (boundary_execution_id, call_id, tool_name) =
                        if let Some(tool) = active_tool {
                            (tool.execution_id, Some(tool.call_id), Some(tool.tool_name))
                        } else if let Some(subagent) = active_subagent {
                            (Some(subagent.execution_id), None, None)
                        } else {
                            (execution_id, None, None)
                        };
                    Some(serde_json::json!({
                        "execution_id": boundary_execution_id,
                        "call_id": call_id,
                        "tool_name": tool_name,
                        "reason": summary,
                    }))
                } else {
                    None
                };
                let (status_name, status_detail) = task_status_wire(&next_status);
                recovered_tasks.push(serde_json::json!({
                    "task_id": task_id,
                    "status": status_name,
                    "status_detail": status_detail,
                    "summary": summary,
                    "blocker": blocker,
                }));
            }
            let recovered_subagents = active_subagents
                .iter()
                .map(|boundary| {
                    serde_json::json!({
                        "task_id": boundary.task_id,
                        "execution_id": boundary.execution_id,
                        "status": SubagentStatus::Failed.as_str(),
                        "terminal_cause": "process_interrupted",
                    })
                })
                .collect::<Vec<_>>();
            let recovered_tools = active_tools
                .iter()
                .map(|boundary| {
                    serde_json::json!({
                        "task_id": boundary.task_id,
                        "execution_id": boundary.execution_id,
                        "call_id": boundary.call_id,
                        "tool_name": boundary.tool_name,
                    })
                })
                .collect::<Vec<_>>();

            #[cfg(test)]
            if self
                .fail_next_recovery_commit
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::InvalidPlan(
                    "injected recovery commit failure".to_string(),
                ));
            }
            let recovery_event = RuntimeJournalEvent::for_append(
                &run.run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": TaskRunStatus::Running.as_str(),
                    "to": recovery_target.as_str(),
                    "recovery": {
                        "kind": "boot_recovery",
                        "message": if completed_turn_waiting_for_settlement {
                            "completed conversational turn settlement recovered after process restart"
                        } else {
                            "recovered from running (interrupted by process restart)"
                        },
                        "active_turn": active_turn,
                        "pause": (recovery_target == TaskRunStatus::Paused).then(|| serde_json::json!({
                            "reason": RunPauseReason::BootRecovery.as_str(),
                            "detail": "the application process ended while this run was active",
                        })),
                        "cells": orphan_cells,
                        "tasks": recovered_tasks,
                        "subagents": recovered_subagents,
                        "tools": recovered_tools,
                    },
                }),
            );
            let mut recovery_events = vec![recovery_event];
            if recovery_target == TaskRunStatus::Cancelled {
                recovery_events.push(RuntimeJournalEvent::for_append(
                    &run.run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "reason": "process_interrupted" }),
                ));
            }
            #[cfg(test)]
            let inject_projection_degradation = self
                .fail_next_recovery_projection
                .swap(false, std::sync::atomic::Ordering::SeqCst);
            #[cfg(not(test))]
            let inject_projection_degradation = false;
            let receipt = if inject_projection_degradation {
                let committed = self
                    .shadow
                    .append_event_batch(&run.run_id, recovery_events)?;
                let sequence = i64::try_from(committed.apply.last_sequence).map_err(|_| {
                    StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
                })?;
                Self::classify_committed_projection(
                    sequence,
                    committed.apply.journal,
                    committed.apply.checkpoint,
                    committed.history,
                    ProjectionCommitReceipt::CommittedProjectionDegraded {
                        seq: sequence,
                        detail: "injected recovery projection degradation".to_string(),
                    },
                )
            } else {
                self.commit_runtime_events_with_receipt(&run.run_id, recovery_events)?
            };
            self.observe_projection_receipt(&run.run_id, &receipt);
            tracing::info!(
                run_id = %run.run_id,
                from = %run.status.as_str(),
                to = %recovery_target.as_str(),
                "recovered interrupted run at boot"
            );
            Ok(true)
        })
    }

    pub fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, StoreError> {
        self.file_store()?
            .get_plan(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Return the immutable revision artifact without joining execution state.
    /// Surface callers combine this with the read-only Todo projection.
    pub fn get_plan_revision(&self, run_id: &str) -> Result<Option<PlanRevision>, StoreError> {
        self.file_store()?
            .get_plan_revision(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    pub fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, StoreError> {
        self.file_store()?
            .list_todos(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        self.file_store()?
            .list_events(run_id, since_seq)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Execute a bounded event query at the journal boundary. Filtering is
    /// applied while scanning fixed-size pages, so unrelated events cannot
    /// consume the caller's result limit and the complete suffix is never
    /// materialized as an intermediate vector.
    pub fn query_events_bounded(
        &self,
        run_id: &str,
        query: RuntimeEventQuery,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        const SCAN_PAGE_SIZE: usize = 256;

        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.file_store()?;
        let mut after_sequence = query.after_sequence;
        let mut matched = Vec::with_capacity(query.limit.min(SCAN_PAGE_SIZE));
        loop {
            let events = store
                .list_events_bounded(run_id, after_sequence, SCAN_PAGE_SIZE)
                .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?;
            let event_count = events.len();
            if event_count == 0 {
                break;
            }
            for event in events {
                after_sequence = event.seq;
                let execution_matches = query
                    .execution_id
                    .as_deref()
                    .is_none_or(|execution_id| event.step_id.as_deref() == Some(execution_id));
                let type_matches =
                    query.event_types.is_empty() || query.event_types.contains(&event.event_type);
                if execution_matches && type_matches {
                    matched.push(event);
                    if matched.len() >= query.limit {
                        return Ok(matched);
                    }
                }
            }
            if event_count < SCAN_PAGE_SIZE {
                break;
            }
        }
        Ok(matched)
    }

    /// Read the deterministic checkpoint-backed run-state projection.
    pub fn get_run_state(&self, run_id: &str) -> Result<Option<RunStateSnapshot>, StoreError> {
        self.file_store()?
            .get_run_state(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    /// Rebuild one diagnostic projection from the complete journal with an
    /// empty in-memory checkpoint. The production authority remains unchanged.
    pub fn diagnose_full_journal_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow
            .diagnostic_full_replay(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("journal diagnostic: {error}")))
    }

    /// Configure long-horizon execution without introducing a second Goal store.
    pub fn configure_run_continuation(
        &self,
        run_id: &str,
        enabled: bool,
        auto_resume_after_restart: bool,
        token_budget: Option<u64>,
        time_budget_seconds: Option<u64>,
    ) -> Result<RunContinuationState, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation);
            let unchanged = current.as_ref().is_some_and(|state| {
                state.enabled == enabled
                    && state.auto_resume_after_restart == auto_resume_after_restart
                    && state.token_budget == token_budget
                    && state.time_budget_seconds == time_budget_seconds
            });
            if !unchanged {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({
                        "enabled": enabled,
                        "auto_resume_after_restart": auto_resume_after_restart,
                        "token_budget": token_budget,
                        "time_budget_seconds": time_budget_seconds,
                    }),
                ))?;
            }
            self.get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after configuration for {run_id}"
                    ))
                })
        })
    }

    /// Persist a deterministic cross-RunTurn retry schedule for one typed
    /// transient provider failure. Provider display text is deliberately not
    /// stored here; callers pass a stable, non-sensitive fingerprint.
    pub fn schedule_provider_retry(
        &self,
        run_id: &str,
        error_fingerprint: &str,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        self.schedule_provider_retry_at(run_id, error_fingerprint, Utc::now())
    }

    #[cfg(test)]
    pub(crate) fn schedule_provider_retry_at_for_test(
        &self,
        run_id: &str,
        error_fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        self.schedule_provider_retry_at(run_id, error_fingerprint, now)
    }

    fn schedule_provider_retry_at(
        &self,
        run_id: &str,
        error_fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        if error_fingerprint.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "provider retry fingerprint must not be empty".to_string(),
            ));
        }
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let disposition = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let continuation = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|state| state.enabled)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "run {run_id} is not configured for long-horizon continuation"
                    ))
                })?;
            let budget_pause = continuation.pause.as_ref().is_some_and(|pause| {
                matches!(
                    pause.reason,
                    RunPauseReason::TokenBudget | RunPauseReason::TimeBudget
                )
            });
            if run.status != TaskRunStatus::Running
                && !(run.status == TaskRunStatus::Paused && budget_pause)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "provider retry requires a Running or budget-paused run, current status is {}",
                    run.status.as_str()
                )));
            }
            if continuation.active_turn.is_some() {
                return Err(StoreError::InvalidPlan(format!(
                    "provider retry cannot be scheduled while run {run_id} has an active RunTurn"
                )));
            }
            let previous_retry = continuation.provider_retry.as_ref();
            let attempt_count = previous_retry
                .map(|retry| retry.attempt_count.saturating_add(1))
                .unwrap_or(1);
            let first_failure_at = previous_retry
                .map(|retry| retry.first_failure_at)
                .unwrap_or(now);
            let delay_millis = stable_provider_retry_delay_millis(
                run_id,
                error_fingerprint,
                attempt_count,
            );
            let delay_i64 = i64::try_from(delay_millis).unwrap_or(i64::MAX);
            let next_retry_at = now
                .checked_add_signed(chrono::Duration::milliseconds(delay_i64))
                .ok_or_else(|| {
                    StoreError::InvalidPlan("provider retry deadline overflow".to_string())
                })?;
            let attempts_exhausted = attempt_count >= MAX_PROVIDER_RETRY_ATTEMPTS;
            let token_budget_exhausted = continuation
                .token_budget
                .is_some_and(|budget| continuation.tokens_used >= budget);
            let time_budget_exhausted = continuation
                .time_budget_seconds
                .is_some_and(|budget| continuation.time_used_seconds >= budget);
            let exhausted =
                attempts_exhausted || token_budget_exhausted || time_budget_exhausted;
            let pause_detail = exhausted.then(|| {
                if attempts_exhausted {
                    format!(
                        "provider remained unavailable after {attempt_count} durable attempts"
                    )
                } else if token_budget_exhausted {
                    "provider retry stopped because the TaskRun token budget is exhausted"
                        .to_string()
                } else {
                    "provider retry stopped because the TaskRun time budget is exhausted"
                        .to_string()
                }
            });
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunProviderRetryScheduled,
                serde_json::json!({
                    "error_fingerprint": error_fingerprint,
                    "attempt_count": attempt_count,
                    "delay_millis": delay_millis,
                    "next_retry_at": echo_agent::utils::time::to_local(next_retry_at).to_rfc3339(),
                    "first_failure_at": echo_agent::utils::time::to_local(first_failure_at).to_rfc3339(),
                    "exhausted": exhausted,
                    "pause_reason": exhausted.then(|| RunPauseReason::ProviderUnavailable.as_str()),
                    "pause_detail": pause_detail,
                }),
            ))?;
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.provider_retry)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "provider retry projection missing after schedule for {run_id}"
                    ))
                })?;
            Ok(if exhausted {
                ProviderRetryDisposition::Exhausted(state)
            } else {
                ProviderRetryDisposition::Scheduled(state)
            })
        })?;
        if matches!(disposition, ProviderRetryDisposition::Exhausted(_)) {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(disposition)
    }

    /// Update only the budgets of an already-enabled continuation. Product
    /// surfaces use this instead of the bootstrap configuration API so a typo
    /// cannot silently turn an ordinary one-shot run into a long-horizon run.
    pub fn update_run_continuation_budgets(
        &self,
        run_id: &str,
        token_budget: Option<u64>,
        time_budget_seconds: Option<u64>,
    ) -> Result<RunContinuationState, StoreError> {
        if token_budget == Some(0) || time_budget_seconds == Some(0) {
            return Err(StoreError::InvalidPlan(
                "continuation budgets must be positive or omitted".to_string(),
            ));
        }
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let (updated, paused) = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|continuation| continuation.enabled)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "run {run_id} is not configured for long-horizon continuation"
                    ))
                })?;
            let pause_reason = if run.status == TaskRunStatus::Running
                && token_budget.is_some_and(|budget| current.tokens_used >= budget)
            {
                Some(RunPauseReason::TokenBudget)
            } else if run.status == TaskRunStatus::Running
                && time_budget_seconds.is_some_and(|budget| current.time_used_seconds >= budget)
            {
                Some(RunPauseReason::TimeBudget)
            } else {
                None
            };
            let pause_detail = pause_reason.map(|reason| match reason {
                RunPauseReason::TokenBudget => {
                    "the lowered continuation token budget is already exhausted"
                }
                RunPauseReason::TimeBudget => {
                    "the lowered continuation time budget is already exhausted"
                }
                _ => "the lowered continuation budget is already exhausted",
            });
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationConfigured,
                serde_json::json!({
                    "enabled": true,
                    "auto_resume_after_restart": current.auto_resume_after_restart,
                    "token_budget": token_budget,
                    "time_budget_seconds": time_budget_seconds,
                    "pause_reason": pause_reason.map(RunPauseReason::as_str),
                    "pause_detail": pause_detail,
                }),
            ))?;
            let updated = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after budget update for {run_id}"
                    ))
                })?;
            Ok((updated, pause_reason.is_some()))
        })?;
        if paused {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(updated)
    }

    /// Atomically claim the next RunTurn ordinal when this run is eligible.
    pub fn claim_run_turn(
        &self,
        run_id: &str,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let event = match self.prepare_run_turn_start(
                run_id,
                turn_id,
                origin,
                transcript_visibility,
                run.status,
                &snapshot,
            )? {
                RunTurnClaimPreparation::Start(event) => event,
                RunTurnClaimPreparation::NotSubmitted(reason) => {
                    return Ok(RunTurnClaimOutcome::NotSubmitted(reason));
                }
            };
            self.commit_runtime_events(run_id, vec![event])?;
            self.read_claimed_run_turn(run_id, turn_id)
        })
    }

    /// Atomically validate one queued resume identity, resume the exact paused
    /// run, and claim its first resumed RunTurn in one journal batch.
    pub fn resume_and_claim_run_turn_expected(
        &self,
        expected: &TaskRunResumeIdentity,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        let run_id = expected.run_id.as_str();
        self.with_run_lock(run_id, || {
            if origin != RunTurnOrigin::Resume {
                return Err(StoreError::InvalidPlan(
                    "expected TaskRun resume identity requires RunTurn origin resume".to_string(),
                ));
            }
            let run = self.validate_resume_locked(run_id, Some(expected))?;
            let mut snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let continuation = snapshot.continuation.get_or_insert_with(Default::default);
            continuation.deferred = false;
            continuation.deferred_reason = None;
            continuation.provider_retry = None;
            continuation.blocker_audit = None;
            let start = match self.prepare_run_turn_start(
                run_id,
                turn_id,
                origin,
                transcript_visibility,
                TaskRunStatus::Running,
                &snapshot,
            )? {
                RunTurnClaimPreparation::Start(event) => event,
                RunTurnClaimPreparation::NotSubmitted(reason) => {
                    return Ok(RunTurnClaimOutcome::NotSubmitted(reason));
                }
            };
            let mut events = self.prepare_resume_events(run_id, &run, true)?;
            events.push(start);
            self.commit_runtime_events(run_id, events)?;
            self.read_claimed_run_turn(run_id, turn_id)
        })
    }

    fn prepare_run_turn_start(
        &self,
        run_id: &str,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
        run_status: TaskRunStatus,
        snapshot: &RunStateSnapshot,
    ) -> Result<RunTurnClaimPreparation, StoreError> {
        if turn_id.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "RunTurn id must not be empty".to_string(),
            ));
        }
        if run_status != TaskRunStatus::Running {
            return Ok(RunTurnClaimPreparation::NotSubmitted(
                ContinuationNotSubmittedReason::RunNotRunning,
            ));
        }
        let state = snapshot.continuation.clone().unwrap_or_default();
        let rejected = if !state.enabled {
            Some(ContinuationNotSubmittedReason::Disabled)
        } else if state.deferred {
            Some(ContinuationNotSubmittedReason::Deferred)
        } else if state
            .provider_retry
            .as_ref()
            .is_some_and(|retry| retry.exhausted || retry.next_retry_at > Utc::now())
        {
            Some(ContinuationNotSubmittedReason::ProviderRetryBackoff)
        } else if state.active_turn.is_some() {
            Some(ContinuationNotSubmittedReason::AlreadyRunning)
        } else if state
            .token_budget
            .is_some_and(|budget| state.tokens_used >= budget)
        {
            Some(ContinuationNotSubmittedReason::TokenBudgetExhausted)
        } else if state
            .time_budget_seconds
            .is_some_and(|budget| state.time_used_seconds >= budget)
        {
            Some(ContinuationNotSubmittedReason::TimeBudgetExhausted)
        } else {
            None
        };
        if let Some(reason) = rejected {
            return Ok(RunTurnClaimPreparation::NotSubmitted(reason));
        }
        if snapshot.event_index.started_turns.contains(turn_id) {
            return Err(StoreError::InvalidPlan(format!(
                "RunTurn id {turn_id} was already used by {run_id}"
            )));
        }
        Ok(RunTurnClaimPreparation::Start(
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunTurnStarted,
                serde_json::json!({
                    "event_id": format!("{run_id}:{turn_id}:started"),
                    "turn_id": turn_id,
                    "ordinal": state.next_turn_ordinal.max(1),
                    "origin": origin.as_str(),
                    "transcript_visibility": transcript_visibility.as_str(),
                }),
            ),
        ))
    }

    fn read_claimed_run_turn(
        &self,
        run_id: &str,
        turn_id: &str,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        self.get_run_state(run_id)?
            .and_then(|snapshot| snapshot.continuation)
            .and_then(|state| state.active_turn)
            .map(RunTurnClaimOutcome::Started)
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "active RunTurn missing after claim for {run_id}:{turn_id}"
                ))
            })
    }

    /// Account a provider usage envelope exactly once. Returns true once the
    /// optional user token budget is exhausted.
    pub fn account_run_turn_usage(
        &self,
        run_id: &str,
        turn_id: &str,
        provider_event_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let exhausted = self.with_run_lock(run_id, || {
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "usage event targets inactive RunTurn {turn_id} in {run_id}"
                )));
            }
            let event_id = format!("{run_id}:{turn_id}:usage:{provider_event_id}");
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            let already_recorded = self
                .get_run_state(run_id)?
                .is_some_and(|state| state.event_index.accounted_usage.contains(&event_id));
            let added_tokens = input_tokens.saturating_add(output_tokens);
            let will_exhaust = !already_recorded
                && current.token_budget.is_some_and(|budget| {
                    current.tokens_used.saturating_add(added_tokens) >= budget
                });
            if !already_recorded {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": event_id,
                        "turn_id": turn_id,
                        "provider_event_id": provider_event_id,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "source_scope": "primary_turn",
                        "pause_reason": will_exhaust.then_some(RunPauseReason::TokenBudget.as_str()),
                        "pause_detail": will_exhaust.then_some("the configured token budget was reached at a provider usage boundary"),
                    }),
                ))?;
            }
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            Ok(state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget))
        })?;
        if exhausted {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(exhausted)
    }

    /// Fold one PlanTask Subagent usage source into the owning Goal budget.
    /// Duration is charged only without an active parent RunTurn; otherwise
    /// that RunTurn's wall clock already includes the Subagent execution.
    #[allow(clippy::too_many_arguments)]
    pub fn account_subagent_usage(
        &self,
        run_id: &str,
        execution_id: &str,
        source_event_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let exhausted = self.with_run_lock(run_id, || {
            let Some(current) = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|continuation| continuation.enabled)
            else {
                return Ok(false);
            };
            let state = self.get_run_state(run_id)?;
            let assigned = state
                .as_ref()
                .is_some_and(|state| state.event_index.assigned_subagents.contains(execution_id));
            if !assigned {
                return Err(StoreError::InvalidPlan(format!(
                    "usage event targets unknown Subagent execution {execution_id} in {run_id}"
                )));
            }
            let event_id =
                format!("{run_id}:subagent:{execution_id}:usage:{source_event_id}");
            let already_recorded = state
                .as_ref()
                .is_some_and(|state| state.event_index.accounted_usage.contains(&event_id));
            let active_turn_id = current
                .active_turn
                .as_ref()
                .map(|turn| turn.turn_id.clone());
            let elapsed_seconds = if active_turn_id.is_some() || duration_ms == 0 {
                0
            } else {
                duration_ms.saturating_add(999) / 1_000
            };
            let added_tokens = input_tokens.saturating_add(output_tokens);
            let token_exhausted = !already_recorded
                && current.token_budget.is_some_and(|budget| {
                    current.tokens_used.saturating_add(added_tokens) >= budget
                });
            let time_exhausted = !already_recorded
                && !token_exhausted
                && current.time_budget_seconds.is_some_and(|budget| {
                    current.time_used_seconds.saturating_add(elapsed_seconds) >= budget
                });
            let pause_reason = if token_exhausted {
                Some(RunPauseReason::TokenBudget)
            } else if time_exhausted {
                Some(RunPauseReason::TimeBudget)
            } else {
                None
            };
            if !already_recorded {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    Some(execution_id),
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": event_id,
                        "turn_id": active_turn_id,
                        "source_scope": "subagent",
                        "source_event_id": source_event_id,
                        "execution_id": execution_id,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "duration_ms": duration_ms,
                        "elapsed_seconds": elapsed_seconds,
                        "pause_reason": pause_reason.map(RunPauseReason::as_str),
                        "pause_detail": pause_reason.map(|reason| match reason {
                            RunPauseReason::TokenBudget => "a PlanTask Subagent reached the configured token budget",
                            RunPauseReason::TimeBudget => "a PlanTask Subagent reached the configured time budget",
                            _ => "a PlanTask Subagent reached a configured budget",
                        }),
                    }),
                ))?;
            }
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            Ok(state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget)
                || state
                    .time_budget_seconds
                    .is_some_and(|budget| state.time_used_seconds >= budget))
        })?;
        if exhausted {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(exhausted)
    }

    pub fn record_run_turn_compaction(
        &self,
        run_id: &str,
        turn_id: &str,
        provider_event_id: &str,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "compaction event targets inactive RunTurn {turn_id} in {run_id}"
                )));
            }
            let event_id = format!("{run_id}:{turn_id}:compact:{provider_event_id}");
            let already_recorded = self
                .get_run_state(run_id)?
                .is_some_and(|state| state.event_index.accounted_compactions.contains(&event_id));
            if already_recorded {
                return Ok(());
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({
                    "event_id": event_id,
                    "turn_id": turn_id,
                }),
            ))?;
            Ok(())
        })
    }

    /// Finish the active RunTurn exactly once and return the rebuilt state.
    pub fn finish_run_turn(
        &self,
        run_id: &str,
        completion: RunTurnCompletion<'_>,
    ) -> Result<RunContinuationState, StoreError> {
        self.finish_run_turn_with_agent_failure(run_id, completion, None)
    }

    pub(crate) fn finish_run_turn_with_agent_failure(
        &self,
        run_id: &str,
        completion: RunTurnCompletion<'_>,
        agent_failure: Option<&echo_agent::error::AgentFailure>,
    ) -> Result<RunContinuationState, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            // Audit allowlist: the completion progress fingerprint summarizes
            // task/result history beyond the operational RunStateSnapshot.
            let events = self.list_events(run_id, 0)?;
            let already_recorded = self.get_run_state(run_id)?.is_some_and(|state| {
                state
                    .event_index
                    .finished_turns
                    .contains(completion.turn_id)
            });
            if already_recorded {
                return self
                    .get_run_state(run_id)?
                    .and_then(|snapshot| snapshot.continuation)
                    .ok_or_else(|| {
                        StoreError::InvalidPlan(format!(
                            "continuation projection missing after finishing {}",
                            completion.turn_id
                        ))
                    });
            }
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(completion.turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "finish targets inactive RunTurn {} in {run_id}",
                    completion.turn_id
                )));
            }
            {
                let progress_fingerprint = run_progress_fingerprint(&events);
                let made_progress = run_turn_made_progress(&events, completion.turn_id);
                let blocker_fingerprint = (!made_progress).then(|| {
                    blocker_fingerprint(completion.error_fingerprint, &progress_fingerprint)
                });
                let mut terminal_events = vec![RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "event_id": format!("{run_id}:{}:finished", completion.turn_id),
                        "turn_id": completion.turn_id,
                        "status": completion.status.as_str(),
                        "elapsed_seconds": completion.elapsed_seconds,
                        "final_message_id": completion.final_message_id,
                        "error_fingerprint": completion.error_fingerprint,
                        "progress_fingerprint": progress_fingerprint,
                        "made_progress": made_progress,
                        "blocker_fingerprint": blocker_fingerprint,
                        "agent_failure": agent_failure,
                    }),
                )];
                let run = self
                    .get_run(run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                if completion.status == RunTurnStatus::Ended && run.status == TaskRunStatus::Running
                {
                    let report = self.completion_gate_report(run_id)?;
                    if report.ready {
                        terminal_events.push(RuntimeJournalEvent::for_append(
                            run_id,
                            None,
                            None,
                            RuntimeEventKind::RunStatusChanged,
                            serde_json::json!({
                                "from": TaskRunStatus::Running.as_str(),
                                "to": TaskRunStatus::Completed.as_str(),
                                "plan_revision": report.plan_revision,
                                "goal_revision": report.goal_revision,
                                "requirement_count": report.requirements.len(),
                                "completed_with_run_turn": completion.turn_id,
                            }),
                        ));
                    }
                }
                self.commit_runtime_events(run_id, terminal_events)?;
            }
            self.get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after finishing {}",
                        completion.turn_id
                    ))
                })
        })
    }

    pub fn set_continuation_deferred(
        &self,
        run_id: &str,
        deferred: bool,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.deferred);
            if current == deferred {
                return Ok(());
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                if deferred {
                    RuntimeEventKind::RunContinuationDeferred
                } else {
                    RuntimeEventKind::RunContinuationResumed
                },
                serde_json::json!({ "deferred": deferred }),
            ))?;
            Ok(())
        })
    }

    /// Atomically clear a deferred continuation only when no task or command
    /// cell is active. Every producer of those facts uses the same run lock, so
    /// terminal settlement cannot observe a mixed runtime generation.
    pub(crate) fn resume_deferred_continuation_if_quiet(
        &self,
        run_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let runtime_active = snapshot
                .background_cells
                .iter()
                .any(BackgroundCellState::is_active)
                || snapshot.tasks.iter().any(|task| task.status.is_running());
            let resumable = snapshot.run.status == TaskRunStatus::Running
                && !runtime_active
                && snapshot
                    .continuation
                    .is_some_and(|continuation| continuation.enabled && continuation.deferred);
            if !resumable {
                return Ok(false);
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationResumed,
                serde_json::json!({ "deferred": false, "reason": "runtime_quiet" }),
            ))?;
            Ok(true)
        })
    }

    /// Atomically preserve deferral when execution activity appears before a
    /// queued continuation claims its next RunTurn.
    pub(crate) fn defer_continuation_if_runtime_active(
        &self,
        run_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let runtime_active = snapshot
                .background_cells
                .iter()
                .any(BackgroundCellState::is_active)
                || snapshot.tasks.iter().any(|task| task.status.is_running());
            if !runtime_active || snapshot.run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            let continuation = snapshot.continuation.unwrap_or_default();
            if !continuation.enabled {
                return Ok(false);
            }
            if !continuation.deferred {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunContinuationDeferred,
                    serde_json::json!({
                        "deferred": true,
                        "reason": "runtime_active",
                    }),
                ))?;
            }
            Ok(true)
        })
    }

    /// Atomically observe active cells and defer continuation under the same
    /// run lock used by terminal cell persistence.
    pub fn defer_continuation_for_active_cells(&self, run_id: &str) -> Result<usize, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let active_cells = self
                .list_background_cells(run_id)?
                .into_iter()
                .filter(BackgroundCellState::is_active)
                .count();
            if active_cells == 0 {
                return Ok(0);
            }
            let deferred = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.deferred);
            if !deferred {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunContinuationDeferred,
                    serde_json::json!({
                        "deferred": true,
                        "reason": "background_cells_active",
                    }),
                ))?;
            }
            Ok(active_cells)
        })
    }

    pub fn record_run_pause_reason(
        &self,
        run_id: &str,
        reason: RunPauseReason,
        detail: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunPauseReasonChanged,
                serde_json::json!({
                    "reason": reason.as_str(),
                    "detail": detail.map(|text| text.chars().take(600).collect::<String>()),
                }),
            ))?;
            Ok(())
        })
    }

    /// Fold the append-only cell lifecycle events for one run.
    pub fn list_background_cells(
        &self,
        run_id: &str,
    ) -> Result<Vec<BackgroundCellState>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.background_cells)
            .unwrap_or_default())
    }

    /// Persist one cell launch exactly once. The framework registry remains
    /// the execution authority; this event is the EKO recovery/UI projection.
    #[allow(clippy::too_many_arguments)]
    pub fn record_background_cell_started(
        &self,
        run_id: &str,
        cell_id: &str,
        name: &str,
        command_hash: &str,
        turn_id: Option<&str>,
        execution_id: Option<&str>,
        call_id: Option<&str>,
    ) -> Result<ProjectionCommitReceipt, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            #[cfg(test)]
            if self
                .fail_next_cell_started
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::InvalidPlan(
                    "injected BackgroundCellStarted append failure".to_string(),
                ));
            }
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 240,
                ..Default::default()
            };
            let payload = serde_json::json!({
                "cell_id": cell_id,
                "name": retention.sanitize_text(name),
                "command_hash": command_hash,
                "turn_id": turn_id,
                "execution_id": execution_id,
                "call_id": call_id,
                "phase": BackgroundCellPhase::Prepared,
                "artifact_status": BackgroundCellArtifactStatus::NotRequested,
            });
            let current = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let existing = current
                .background_cells
                .into_iter()
                .find(|cell| cell.cell_id == cell_id);
            let current_seq = i64::try_from(current.journal_sequence).map_err(|_| {
                StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
            })?;
            let append_receipt;
            if let Some(existing) = existing {
                if existing.name != retention.sanitize_text(name)
                    || existing.command_hash != command_hash
                    || existing.turn_id.as_deref() != turn_id
                    || existing.execution_id.as_deref() != execution_id
                    || existing.call_id.as_deref() != call_id
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "conflicting BackgroundCellStarted fact for cell {cell_id}"
                    )));
                }
                append_receipt = None;
            } else {
                append_receipt = Some(self.shadow.append_event_line_with_receipt(
                    run_id,
                    None,
                    call_id,
                    RuntimeEventKind::BackgroundCellStarted,
                    payload,
                )?);
            }
            let committed_seq = append_receipt
                .as_ref()
                .map_or(current_seq, |(event, _, _)| event.seq);
            #[cfg(test)]
            let inject_projection_degradation = self
                .fail_next_cell_started_projection
                .swap(false, std::sync::atomic::Ordering::SeqCst);
            #[cfg(not(test))]
            let inject_projection_degradation = false;
            let projection = if inject_projection_degradation {
                ProjectionCommitReceipt::CommittedProjectionDegraded {
                    seq: committed_seq,
                    detail: "injected BackgroundCellStarted projection failure".to_string(),
                }
            } else {
                self.refresh_committed_projection(run_id, committed_seq)
            };
            let Some((_, apply, history)) = append_receipt else {
                let (journal, history) = self.shadow.settle_event_state(run_id)?;
                return Ok(Self::classify_committed_projection(
                    committed_seq,
                    journal,
                    CheckpointApplyStatus::NotDue,
                    history,
                    projection,
                ));
            };
            Ok(Self::classify_committed_projection(
                committed_seq,
                apply.journal,
                apply.checkpoint,
                history,
                projection,
            ))
        })
    }

    /// Persist one terminal cell result exactly once. Durable excerpts are
    /// redacted and bounded before they enter events.jsonl.
    #[allow(clippy::too_many_arguments)]
    pub fn record_background_cell_finished(
        &self,
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
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            #[cfg(test)]
            if self
                .fail_cell_terminal_remaining
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                return Err(StoreError::InvalidPlan(
                    "injected BackgroundCellFinished append failure".to_string(),
                ));
            }
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 1_200,
                ..Default::default()
            };
            let payload = serde_json::json!({
                "cell_id": cell_id,
                "name": retention.sanitize_text(name),
                "phase": phase,
                "terminal_cause": terminal_cause,
                "terminal_message": terminal_message.map(|text| retention.sanitize_text(text)),
                "exit_code": exit_code,
                "artifact_status": artifact_status,
                "artifact_message": artifact_message.map(|text| retention.sanitize_text(text)),
                "total_output_bytes": total_output_bytes,
                "output_truncated": output_truncated,
                "output_excerpt": output_excerpt.map(|text| retention.sanitize_text(text)),
                "artifact_path": artifact_path,
                "artifact_sha256": artifact_sha256,
                "call_id": call_id,
            });
            let existing = self
                .list_background_cells(run_id)?
                .into_iter()
                .find(|cell| cell.cell_id == cell_id && !cell.is_active());
            if let Some(existing) = existing {
                if existing.phase != phase
                    || existing.terminal_cause != terminal_cause
                    || existing.terminal_message.as_deref()
                        != terminal_message
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.exit_code != exit_code
                    || existing.artifact_status != artifact_status
                    || existing.artifact_message.as_deref()
                        != artifact_message
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.total_output_bytes != total_output_bytes
                    || existing.output_truncated != output_truncated
                    || existing.output_excerpt.as_deref()
                        != output_excerpt
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.artifact_path.as_deref() != artifact_path
                    || existing.artifact_sha256.as_deref() != artifact_sha256
                    || existing.call_id.as_deref() != call_id
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "conflicting BackgroundCellFinished fact for cell {cell_id}"
                    )));
                }
            } else {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    call_id,
                    RuntimeEventKind::BackgroundCellFinished,
                    payload,
                ))?;
            }
            Ok(())
        })
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, StoreError> {
        self.file_store()?
            .list_artifacts(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_reviews(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewResult>, StoreError> {
        self.file_store()?
            .list_reviews(run_id, task_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, StoreError> {
        self.file_store()?
            .get_summary(run_id, task_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Append a free-form `Note` event for diagnostics / trace breadcrumbs.
    pub fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            task_id,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({ "message": message }),
        ))
    }

    /// Persist trigger/scheduling metadata without expanding the TaskRun state
    /// model. Consumers may rebuild this projection from the append-only event.
    pub fn record_trigger_metadata(
        &self,
        run_id: &str,
        source: &str,
        kind: &str,
        prompt: &str,
        priority: u8,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "trigger_metadata",
                "source": source,
                "task_kind": kind,
                "prompt": prompt,
                "priority": priority.min(10),
            }),
        ))
    }

    pub fn record_execution_path(
        &self,
        run_id: &str,
        observed_path: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "execution_path",
                "observed_path": observed_path,
            }),
        ))
    }

    /// Persist the boundary immediately before a task Subagent starts model/tool
    /// execution. A matching [`record_subagent_released`](Self::record_subagent_released)
    /// makes the Subagent outcome recoverable without dispatching it again.
    #[allow(clippy::too_many_arguments)]
    pub fn record_subagent_assigned(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        agent_name: &str,
        task_subject: &str,
        plan_revision: u64,
        attempt: u32,
        replay_safe: bool,
        dispatch_hook: bool,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(execution_id),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": execution_id,
                "agent_name": agent_name,
                "title": task_subject,
                "plan_revision": plan_revision,
                "attempt": attempt,
                "replay_safe": replay_safe,
                "dispatch_hook": dispatch_hook,
            }),
        ))
    }

    /// Persist a Subagent terminal fact with the structured outcome needed for resume.
    pub(crate) fn record_subagent_released(
        &self,
        record: SubagentReleaseRecord<'_>,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let SubagentReleaseRecord {
            run_id,
            task_id,
            execution_id,
            agent_name,
            task_subject,
            plan_revision,
            attempt,
            status,
            outcome,
            full_output,
            usage,
            dispatch_hook,
        } = record;
        self.with_run_lock(run_id, || {
            let summary = outcome.map(|value| bounded_event_text(&value.summary, 2_000));
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                Some(task_id),
                Some(execution_id),
                RuntimeEventKind::SubagentReleased,
                serde_json::json!({
                    "execution_id": execution_id,
                    "agent_name": agent_name,
                    "title": task_subject,
                    "plan_revision": plan_revision,
                    "attempt": attempt,
                    "status": status,
                    "summary": summary,
                    "outcome": outcome,
                    "full_output": full_output,
                    "usage": usage,
                    "dispatch_hook": dispatch_hook,
                }),
            )];
            events.extend(
                super::subagent_control::control_settlements_for_subagent_release(
                    self,
                    run_id,
                    task_id,
                    execution_id,
                    plan_revision,
                    attempt,
                    status,
                )?,
            );
            self.commit_runtime_events(run_id, events).map(|_| ())
        })
    }

    /// Persist a tool dispatch before execution. Raw arguments are deliberately
    /// excluded from the durable event to avoid leaking secrets or inflating
    /// the run file; `call_id` is the idempotency/correlation key.
    pub fn record_tool_started(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        replay_safe: bool,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(call_id),
            RuntimeEventKind::ToolStarted,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "replay_safe": replay_safe,
            }),
        ))
    }

    /// Persist a tool terminal fact. The result preview is diagnostic only;
    /// canonical tool output remains in the agent checkpoint/transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn record_tool_finished(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        success: bool,
        result: &str,
        failure: Option<&echo_agent::tools::ToolFailure>,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let event_type = if success {
            RuntimeEventKind::ToolCompleted
        } else {
            RuntimeEventKind::ToolFailed
        };
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(call_id),
            event_type,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "success": success,
                "result_preview": bounded_event_text(result, 500),
                "result_chars": result.chars().count(),
                "failure": failure,
            }),
        ))
    }

    /// Return a completed Subagent outcome for a stable logical attempt.
    ///
    /// A physical claim gets a fresh execution id when an interrupted task is
    /// reclaimed. Revision and attempt remain stable across that reclaim, so
    /// they form the durable idempotency key. A later assignment for the same
    /// logical attempt clears the terminal fact, while a retry or edited task
    /// has a different attempt or revision and cannot reuse stale output.
    pub(crate) fn recoverable_subagent_outcome_for_attempt(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        plan_revision: u64,
        attempt: u32,
    ) -> Result<Option<RecoverableSubagentOutcome>, StoreError> {
        let events = self.list_events(run_id, 0)?;
        let current_claim_index = events.iter().position(|event| {
            event.task_id.as_deref() == Some(task_id)
                && event.event_type == RuntimeEventKind::TaskStarted
                && json_string(&event.payload, "execution_id").as_deref() == Some(execution_id)
        });
        let mut current_result = None;
        let mut prior_result = None;
        // Audit allowlist: exact physical-attempt recovery needs the complete
        // assignment/release sequence to enforce its TaskStarted cut.
        // The same physical execution may recover its own completed release.
        // A reclaimed execution may reuse only a release committed before its
        // TaskStarted claim event; a late old release cannot cross that cut.
        for (index, event) in events.into_iter().enumerate() {
            let matches_attempt = event.task_id.as_deref() == Some(task_id)
                && event
                    .payload
                    .get("plan_revision")
                    .and_then(serde_json::Value::as_u64)
                    == Some(plan_revision)
                && event
                    .payload
                    .get("attempt")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(attempt));
            if !matches_attempt {
                continue;
            }
            let event_execution_id = json_string(&event.payload, "execution_id");
            let targets_current = event_execution_id.as_deref() == Some(execution_id);
            let targets_prior = current_claim_index.is_some_and(|claim_index| index < claim_index);
            match event.event_type {
                RuntimeEventKind::SubagentAssigned if targets_current => current_result = None,
                RuntimeEventKind::SubagentAssigned if targets_prior => prior_result = None,
                RuntimeEventKind::SubagentReleased => {
                    let candidate =
                        if json_string(&event.payload, "status").as_deref() == Some("completed") {
                            event
                                .payload
                                .get("outcome")
                                .cloned()
                                .and_then(|value| {
                                    serde_json::from_value::<SubagentOutcome>(value).ok()
                                })
                                .map(|outcome| RecoverableSubagentOutcome {
                                    full_output: json_string(&event.payload, "full_output")
                                        .filter(|output| !output.trim().is_empty())
                                        .unwrap_or_else(|| outcome.summary.clone()),
                                    outcome,
                                })
                        } else {
                            None
                        };
                    if targets_current {
                        current_result = candidate;
                    } else if targets_prior {
                        prior_result = candidate;
                    }
                }
                _ => {}
            }
        }
        Ok(current_result.or(prior_result))
    }

    /// Current unresolved recovery barriers, folded from append-only events.
    pub fn list_recovery_blockers(&self, run_id: &str) -> Result<Vec<RecoveryBlocker>, StoreError> {
        let _operation = self.shadow_operation()?;
        let mut blockers = self
            .get_run_state(run_id)?
            .map(|state| {
                state
                    .event_index
                    .recovery_blockers
                    .into_iter()
                    .map(|blocker| (blocker.task_id.clone(), blocker))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        // If the dedicated RecoveryBlocked append was interrupted after the
        // canonical TaskStatus landed, synthesize the barrier so resume fails closed.
        let tasks = self
            .get_plan(run_id)?
            .map(|plan| plan.tasks)
            .unwrap_or_default();
        for task in tasks.into_iter().filter(|task| {
            matches!(
                &task.status,
                echo_agent::tasks::TaskStatus::Blocked(detail)
                    if detail == "mutating side effect is indeterminate after restart"
            )
        }) {
            blockers
                .entry(task.id.clone())
                .or_insert_with(|| RecoveryBlocker {
                    run_id: run_id.to_string(),
                    task_id: task.id,
                    execution_id: None,
                    call_id: None,
                    tool_name: None,
                    reason: "mutating side effect is indeterminate after restart".to_string(),
                });
        }
        Ok(blockers.into_values().collect())
    }

    /// Resolve one recovery barrier after the user inspects the workspace.
    pub fn resolve_recovery_task(
        &self,
        run_id: &str,
        task_id: &str,
        decision: RecoveryDecision,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.with_run_lock(run_id, || {
            let blocker = self
                .list_recovery_blockers(run_id)?
                .into_iter()
                .find(|blocker| blocker.task_id == task_id)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "task {task_id} has no unresolved recovery barrier"
                    ))
                })?;
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let expected_task = current
                .snapshot
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .cloned()
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            let recovery_event = RuntimeJournalEvent::for_append(
                run_id,
                Some(task_id),
                blocker.execution_id.as_deref(),
                RuntimeEventKind::RecoveryResolved,
                serde_json::json!({
                    "decision": decision.as_str(),
                    "previous_reason": blocker.reason,
                }),
            );
            let events = match decision {
                RecoveryDecision::Retry => {
                    let before = current.snapshot;
                    let mut after = before.clone();
                    match echo_agent::tasks::retry_runtime_task(
                        &mut after,
                        &expected_task,
                        before.revision,
                    )? {
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { .. } => {}
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted {
                            retry_count,
                            max_retries,
                        } => {
                            return Err(StoreError::InvalidPlan(format!(
                                "task {task_id} recovery retry budget exhausted ({retry_count}/{max_retries})"
                            )));
                        }
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => {
                            return Err(StoreError::InvalidPlan(format!(
                                "task {task_id} changed before recovery retry"
                            )));
                        }
                    }
                    let mut events = vec![recovery_event];
                    events.extend(runtime_execution_change_events(
                        run_id,
                        &before,
                        &after,
                        Some("recovery retry confirmed by user"),
                    )?);
                    events
                }
                RecoveryDecision::Skip => {
                    let application = echo_agent::tasks::TaskPatchEngine::apply_operations(
                        &current.snapshot.tasks,
                        vec![echo_agent::tasks::TaskPlanPatchOp::Skip {
                            task_id: task_id.to_string(),
                        }],
                        false,
                    )
                    .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
                    echo_agent::tasks::PlanValidator::default()
                        .validate_task_snapshot(&application.tasks)
                        .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))?;
                    let next_revision = current
                        .snapshot
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| {
                            StoreError::InvalidPlan("plan revision overflow".to_string())
                        })?;
                    let prepared = prepare_revisioned_graph_commit(
                        run_id,
                        &run,
                        Some(&current),
                        echo_agent::tasks::TaskGraphCommit {
                            expected_revision: Some(current.snapshot.revision),
                            next: echo_agent::tasks::RevisionedTaskGraph {
                                snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                                    revision: next_revision,
                                    tasks: application.tasks,
                                },
                                context: current.context.clone(),
                            },
                            reason: format!(
                                "resolve recovery barrier for task {task_id} by skipping it"
                            ),
                            effects: application.effects,
                        },
                    )?;
                    let mut events = vec![
                        RuntimeJournalEvent::for_append(
                            run_id,
                            None,
                            None,
                            RuntimeEventKind::PlanRevisionCommitted,
                            prepared.payload,
                        ),
                        recovery_event,
                    ];
                    events.extend(runtime_execution_change_events(
                        run_id,
                        &current.snapshot,
                        &prepared.next.snapshot,
                        Some("recovery skip confirmed by user"),
                    )?);
                    events
                }
            };
            self.commit_runtime_events(run_id, events)?;
            Ok(())
        })
    }
}
