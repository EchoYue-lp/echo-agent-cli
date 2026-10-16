//! Agent runtime bootstrap and application-service composition shared by every
//! EKO interaction surface.
//!
//! This module consolidates the common agent initialization logic that was
//! previously duplicated in `main.rs` (TUI/CLI/channel) and `desktop.rs` (GUI).
//!
//! # Usage
//!
//! ```rust,ignore
//! use echo_agent_app_core::runtime::AgentRuntime;
//!
//! let mcp_config_path = echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(
//!     None,
//!     &app_config,
//! );
//! let runtime = AgentRuntime::bootstrap(&app_config, params, mcp_config_path).await?;
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;

use crate::agent_handle::AgentHandle;
use crate::config::EkoConfig;
use crate::evolution::ReviewIntegration;
use crate::hitl::HitlDispatcher;
use crate::infra::{self, AgentCreateParams};
use crate::state::AppState;
use echo_agent::agent::Agent;
use echo_agent::evolution::ReviewConfig;
use echo_agent::intent::{
    KeywordClassifier, LlmIntentClassifier, SkillDescription, TriggerSupervisor,
};

/// Why the application lifecycle owner was asked to settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationLifecycleReason {
    Shutdown,
    BootstrapRollback,
}

impl std::fmt::Display for ApplicationLifecycleReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => formatter.write_str("shutdown"),
            Self::BootstrapRollback => formatter.write_str("bootstrap rollback"),
        }
    }
}

/// One failed owner in an otherwise best-effort lifecycle drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationLifecycleFailure {
    pub owner: String,
    pub error: String,
}

/// Typed aggregate returned by every EKO root (GUI, TUI, CLI/JSONL, channel).
/// A primary surface/bootstrap error is kept separate from teardown failures so
/// launchers never mistake a failed application for a successful cleanup.
#[derive(Clone)]
pub struct ApplicationLifecycleReceipt {
    pub reason: ApplicationLifecycleReason,
    pub primary_error: Option<String>,
    pub failures: Vec<ApplicationLifecycleFailure>,
    // Rust ownership only; public diagnostic fields and wire shapes are unchanged.
    continuation: Option<Arc<ApplicationLifecycleRetention>>,
}

impl std::fmt::Debug for ApplicationLifecycleReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationLifecycleReceipt")
            .field("reason", &self.reason)
            .field("primary_error", &self.primary_error)
            .field("failures", &self.failures)
            .finish()
    }
}
impl PartialEq for ApplicationLifecycleReceipt {
    fn eq(&self, other: &Self) -> bool {
        self.reason == other.reason
            && self.primary_error == other.primary_error
            && self.failures == other.failures
    }
}
impl Eq for ApplicationLifecycleReceipt {}

impl ApplicationLifecycleReceipt {
    fn new(reason: ApplicationLifecycleReason, primary_error: Option<anyhow::Error>) -> Self {
        Self {
            reason,
            primary_error: primary_error.map(|error| error.to_string()),
            failures: Vec::new(),
            continuation: None,
        }
    }

    fn record(&mut self, owner: impl Into<String>, error: impl std::fmt::Display) {
        self.failures.push(ApplicationLifecycleFailure {
            owner: owner.into(),
            error: error.to_string(),
        });
    }

    pub fn is_clean(&self) -> bool {
        self.continuation.is_none() && self.primary_error.is_none() && self.failures.is_empty()
    }

    fn record_retained(&mut self, owner: impl Into<String>, error: impl std::fmt::Display) {
        let failure = ApplicationLifecycleFailure {
            owner: owner.into(),
            error: error.to_string(),
        };
        // Re-observing the same retained owner does not append the same
        // diagnostic forever. Original normal-phase failure ordering is kept.
        if !self.failures.contains(&failure) {
            self.failures.push(failure);
        }
    }

    pub fn into_result(self) -> Result<(), ApplicationLifecycleError> {
        if self.is_clean() {
            Ok(())
        } else {
            Err(ApplicationLifecycleError { receipt: self })
        }
    }

    pub fn into_error(self) -> ApplicationLifecycleError {
        ApplicationLifecycleError { receipt: self }
    }
}

fn record_analysis_cleanup_outcomes(
    receipt: &mut ApplicationLifecycleReceipt,
    outcomes: Vec<crate::product_data_io::AnalysisCancelReceipt>,
) {
    for outcome in outcomes {
        match outcome {
            crate::product_data_io::AnalysisCancelReceipt::Joined { .. } => {}
            crate::product_data_io::AnalysisCancelReceipt::CleanupTimedOut {
                receipt: run,
                timeout_seconds,
            } => receipt.record(
                format!("analysis run {}", run.owner_id),
                format!("cleanup timed out after {timeout_seconds} seconds"),
            ),
            crate::product_data_io::AnalysisCancelReceipt::CleanupFailed {
                receipt: run,
                error,
            } => receipt.record(format!("analysis run {}", run.owner_id), error),
        }
    }
}

impl std::fmt::Display for ApplicationLifecycleReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "application {}", self.reason)?;
        if let Some(error) = self.primary_error.as_deref() {
            write!(formatter, " failed: {error}")?;
        }
        for failure in &self.failures {
            write!(formatter, "; {}: {}", failure.owner, failure.error)?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{receipt}")]
pub struct ApplicationLifecycleError {
    pub receipt: ApplicationLifecycleReceipt,
}

struct ApplicationBackgroundTask {
    name: String,
    handle: tokio::task::JoinHandle<()>,
}

type ApplicationLifecycleBegin = Box<dyn FnOnce() -> Result<(), String> + Send>;
type ApplicationLifecycleJoin =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'static>>;

struct ApplicationExternalOwner {
    name: String,
    begin: Option<ApplicationLifecycleBegin>,
    join: ApplicationLifecycleJoin,
    join_panicked: bool,
}

#[derive(Clone)]
struct LifecyclePublication {
    receipt: ApplicationLifecycleReceipt,
    final_receipt: bool,
}

#[derive(Default)]
struct LifecycleContinuationState {
    owner: Option<(ApplicationLifecycleOwner, ApplicationLifecycleReceipt)>,
    polling: bool,
    revision: u64,
}

/// Private resource projection of the original application-generation owner.
/// Child registries hold Weak links only; no cached publication owns this root.
pub(crate) struct ApplicationLifecycleRetention {
    state: std::sync::Mutex<LifecycleContinuationState>,
    publications: tokio::sync::watch::Sender<Option<LifecyclePublication>>,
    driver_resources:
        std::sync::Mutex<Vec<Arc<crate::tasks::task_runtime::store::RunDriverResources>>>,
    claimed: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    before_no_runtime_publication: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ApplicationLifecycleRetention {
    #[cfg(test)]
    pub(crate) fn resource_scope_for_test() -> Arc<Self> {
        Self::new()
    }

    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(LifecycleContinuationState::default()),
            publications: tokio::sync::watch::channel(None).0,
            driver_resources: std::sync::Mutex::new(Vec::new()),
            claimed: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            before_no_runtime_publication: std::sync::Mutex::new(None),
        })
    }

    pub(crate) fn retain_driver(
        &self,
        resources: Arc<crate::tasks::task_runtime::store::RunDriverResources>,
    ) {
        let mut owned = self
            .driver_resources
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        owned.retain(|current| !current.is_released());
        owned.push(resources);
    }

    pub(crate) fn release_driver(
        self: &Arc<Self>,
        resources: &Arc<crate::tasks::task_runtime::store::RunDriverResources>,
    ) {
        self.driver_resources
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|current| !Arc::ptr_eq(current, resources));
        self.note_progress();
    }

    pub(crate) fn note_progress(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.revision = state.revision.saturating_add(1);
        }
        self.kick();
    }

    fn kick(self: &Arc<Self>) {
        let (owner, receipt, revision) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.polling {
                return;
            }
            let Some((owner, receipt)) = state.owner.take() else {
                return;
            };
            state.polling = true;
            (owner, receipt, state.revision)
        };
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                let mut observation = receipt.clone();
                observation.record(
                    "application lifecycle",
                    format!("settlement requires a Tokio runtime: {error}"),
                );
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                state.owner = Some((owner, receipt));
                drop(state);
                #[cfg(test)]
                {
                    let hook = self
                        .before_no_runtime_publication
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                self.publications.send_replace(Some(LifecyclePublication {
                    receipt: observation,
                    final_receipt: false,
                }));
                // Publication is inside the same single-flight claim even
                // when this caller has no Tokio runtime.
                let retry = {
                    let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                    state.polling = false;
                    state.revision != revision
                };
                if retry {
                    self.kick();
                }
                return;
            }
        };
        let root = self.clone();
        let poll: futures::future::BoxFuture<'static, ()> = Box::pin(async move {
            // Borrow the original Owner across cleanup. Unwinding a foreign
            // future must not consume/drop all the other retained resources.
            let mut owner = owner;
            let mut fallback = receipt.clone();
            let (retained, mut receipt) =
                match std::panic::AssertUnwindSafe(owner.join_owned(receipt))
                    .catch_unwind()
                    .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        owner.cleanup_panicked = true;
                        fallback.record_retained(
                            "application lifecycle",
                            "original cleanup panicked; ownership remains unknown and retained",
                        );
                        (true, fallback)
                    }
                };
            receipt.continuation = None;
            let final_receipt = !retained;
            {
                let mut state = root.state.lock().unwrap_or_else(|error| error.into_inner());
                if retained {
                    state.owner = Some((owner, receipt.clone()));
                }
                // Keep the single-flight claim until publication. Otherwise a
                // new progress callback could publish final, then be overwritten
                // by this older retained observation.
            }
            root.publications.send_replace(Some(LifecyclePublication {
                receipt,
                final_receipt,
            }));
            let retry = {
                let mut state = root.state.lock().unwrap_or_else(|error| error.into_inner());
                state.polling = false;
                !final_receipt && state.revision != revision
            };
            // One AgentRuntime is one application generation. Keep its owner
            // admission sealed; repeated observers use this same publication,
            // never a new generation on a channel still held by old observers.
            if retry {
                root.kick();
            }
        });
        runtime.spawn(poll);
    }
}

#[derive(Clone)]
pub struct ApplicationLifecycleSettlement {
    result: tokio::sync::watch::Receiver<Option<LifecyclePublication>>,
    fallback: ApplicationLifecycleReceipt,
    retention: Arc<ApplicationLifecycleRetention>,
}

impl ApplicationLifecycleSettlement {
    pub async fn wait(mut self) -> ApplicationLifecycleReceipt {
        if self.result.borrow().is_some() {
            self.retention.note_progress();
        } else {
            self.retention.kick();
        }
        loop {
            if let Some(publication) = self.result.borrow().clone() {
                let mut receipt = publication.receipt;
                if !publication.final_receipt {
                    receipt.continuation = Some(self.retention.clone());
                }
                return receipt;
            }
            if self.result.changed().await.is_err() {
                let mut fallback = self.fallback;
                fallback.record(
                    "application lifecycle",
                    "settlement owner ended before publishing its receipt",
                );
                fallback.continuation = Some(self.retention);
                return fallback;
            }
        }
    }
}

/// One-shot EKO process lifecycle owner.
///
/// The owner is deliberately application-side: it composes existing subsystem
/// shutdown APIs but does not replace their internal authority. Shutdown has a
/// strict two-phase boundary: first close admission/broadcast cancellation,
/// then await every accepted owner and aggregate all failures.
pub struct ApplicationLifecycleOwner {
    retention: std::sync::Weak<ApplicationLifecycleRetention>,
    // Present only before start_join transfers the strong root to the real
    // producer/Settlement; always None while this Owner is stored in root.
    initial_retention: Option<Arc<ApplicationLifecycleRetention>>,
    normal_drained: bool,
    begin_receipt: Option<ApplicationLifecycleReceipt>,
    primary_closed: bool,
    cleanup_panicked: bool,
    root_cancel: tokio_util::sync::CancellationToken,
    app_state: Option<Arc<AppState>>,
    primary_agent: Option<AgentHandle>,
    pool: Option<Arc<crate::agent_pool::AgentPool>>,
    task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    command_cell_runtime:
        Option<Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>>,
    review_integration: Option<Arc<ReviewIntegration>>,
    plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    config_watcher: Option<Arc<crate::config_watcher::ConfigWatcherHandle>>,
    mcp_config_runtime: Option<Arc<crate::mcp_config_runtime::McpConfigRuntime>>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    product_data_io: Option<crate::product_data_io::ProductDataIoService>,
    subagent_projection:
        Option<Arc<crate::subagent_event_projection::SubagentEnvelopeProjectionService>>,
    background_tasks: Vec<ApplicationBackgroundTask>,
    external_owners: Vec<ApplicationExternalOwner>,
    #[cfg(test)]
    specialist_teardown_started: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown_begun: bool,
    armed: bool,
}

impl ApplicationLifecycleOwner {
    pub fn new(root_cancel: tokio_util::sync::CancellationToken) -> Self {
        let retention = ApplicationLifecycleRetention::new();
        retention
            .claimed
            .store(true, std::sync::atomic::Ordering::Release);
        Self::with_retention(root_cancel, retention)
    }

    fn with_retention(
        root_cancel: tokio_util::sync::CancellationToken,
        retention: Arc<ApplicationLifecycleRetention>,
    ) -> Self {
        Self {
            retention: Arc::downgrade(&retention),
            initial_retention: Some(retention),
            normal_drained: false,
            begin_receipt: None,
            primary_closed: false,
            cleanup_panicked: false,
            root_cancel,
            app_state: None,
            primary_agent: None,
            pool: None,
            task_runtime_store: None,
            command_cell_runtime: None,
            review_integration: None,
            plugin_runtime: None,
            config_watcher: None,
            mcp_config_runtime: None,
            browser_runtime: None,
            product_data_io: None,
            subagent_projection: None,
            background_tasks: Vec::new(),
            external_owners: Vec::new(),
            #[cfg(test)]
            specialist_teardown_started: None,
            shutdown_begun: false,
            armed: true,
        }
    }

    pub fn bind_app_state(&mut self, state: Arc<AppState>) {
        if let Some(root) = self.retention.upgrade() {
            state.workspace.runtimes.bind_lifecycle_retention(&root);
            state
                .session
                .foreground_turns
                .bind_lifecycle_retention(&root);
            state.session.analysis_runs.bind_lifecycle_retention(&root);
        }
        self.app_state = Some(state);
    }

    pub fn bind_primary_agent(&mut self, agent: AgentHandle) {
        self.primary_agent = Some(agent);
    }

    pub fn bind_pool(&mut self, pool: Arc<crate::agent_pool::AgentPool>) {
        if let Some(root) = self.retention.upgrade() {
            pool.bind_lifecycle_retention(&root);
        }
        self.pool = Some(pool);
    }

    pub fn bind_task_runtime(&mut self, store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>) {
        if let Some(root) = self.retention.upgrade() {
            store.bind_lifecycle_retention(&root);
        }
        self.task_runtime_store = Some(store);
    }

    pub fn bind_command_cell_runtime(
        &mut self,
        runtime: Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
    ) {
        self.command_cell_runtime = Some(runtime);
    }

    pub fn bind_review_integration(&mut self, integration: Arc<ReviewIntegration>) {
        self.review_integration = Some(integration);
    }

    pub fn bind_plugin_runtime(
        &mut self,
        runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
    ) {
        self.plugin_runtime = Some(runtime);
    }

    pub fn bind_config_watcher(
        &mut self,
        watcher: Arc<crate::config_watcher::ConfigWatcherHandle>,
    ) {
        self.config_watcher = Some(watcher);
    }

    pub fn bind_mcp_config_runtime(
        &mut self,
        runtime: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    ) {
        self.mcp_config_runtime = Some(runtime);
    }

    pub fn bind_browser_runtime(&mut self, runtime: Arc<crate::browser::BrowserRuntime>) {
        self.browser_runtime = Some(runtime);
    }

    pub fn bind_product_data_io(
        &mut self,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) {
        if let Some(root) = self.retention.upgrade() {
            product_data_io.bind_lifecycle_retention(&root);
        }
        self.product_data_io = Some(product_data_io);
    }

    pub fn bind_subagent_projection(
        &mut self,
        service: Arc<crate::subagent_event_projection::SubagentEnvelopeProjectionService>,
    ) {
        self.subagent_projection = Some(service);
    }

    #[cfg(test)]
    fn install_specialist_teardown_probe(&mut self, started: tokio::sync::oneshot::Sender<()>) {
        self.specialist_teardown_started = Some(started);
    }

    pub fn track_background_task(
        &mut self,
        name: impl Into<String>,
        handle: tokio::task::JoinHandle<()>,
    ) {
        self.background_tasks.push(ApplicationBackgroundTask {
            name: name.into(),
            handle,
        });
    }

    /// Register a surface-specific owner without moving its policy into
    /// app-core. The synchronous callback participates in phase-one admission
    /// close; its join future participates in the typed phase-two receipt.
    pub fn track_external_owner<Begin, Join>(
        &mut self,
        name: impl Into<String>,
        begin: Begin,
        join: Join,
    ) where
        Begin: FnOnce() -> Result<(), String> + Send + 'static,
        Join: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        self.external_owners.push(ApplicationExternalOwner {
            name: name.into(),
            begin: Some(Box::new(begin)),
            join: Box::pin(join),
            join_panicked: false,
        });
    }

    /// Disarm a bootstrap rollback owner after all fallible construction has
    /// committed. Normal process shutdown is owned by a fresh root-bound owner.
    pub fn disarm(mut self) {
        self.armed = false;
    }

    /// Synchronously close every application-level admission path and broadcast
    /// cancellation. No task is awaited in this phase.
    pub fn begin_shutdown(
        &mut self,
        reason: ApplicationLifecycleReason,
        primary_error: Option<anyhow::Error>,
    ) -> ApplicationLifecycleReceipt {
        let mut receipt = ApplicationLifecycleReceipt::new(reason, primary_error);
        if self.shutdown_begun {
            receipt.record(
                "application lifecycle",
                "shutdown admission was already closed",
            );
            return receipt;
        }
        self.shutdown_begun = true;

        // Phase one: no joins. Every producer sees admission close or a shared
        // cancellation broadcast before teardown waits on a dependent owner.
        self.root_cancel.cancel();
        if let Some(product_data_io) = self.product_data_io.as_ref()
            && let Err(error) = product_data_io.begin_shutdown()
        {
            receipt.record("product-data I/O", error);
        }
        if let Some(state) = self.app_state.as_ref()
            && let Err(error) = state.broadcast_application_shutdown()
        {
            receipt.record("application admission", error);
        } else if self.app_state.is_none() {
            if let Some(store) = self.task_runtime_store.as_ref()
                && let Err(error) = store.begin_run_driver_shutdown()
            {
                receipt.record("TaskRun driver admission", error);
            }
            if let Some(store) = self.task_runtime_store.as_ref()
                && let Err(error) = store.begin_operation_shutdown()
            {
                receipt.record("TaskRuntime operation admission", error);
            }
            if let Some(pool) = self.pool.as_ref() {
                pool.begin_shutdown();
            }
            if let Some(integration) = self.review_integration.as_ref() {
                integration.begin_background_review_shutdown();
            }
            if let Some(runtime) = self.command_cell_runtime.as_ref()
                && let Err(error) = runtime.begin_shutdown()
            {
                receipt.record("command cells", error);
            }
        }
        for owner in &mut self.external_owners {
            if let Some(begin) = owner.begin.take()
                && let Err(error) = begin()
            {
                receipt.record(owner.name.clone(), error);
            }
        }
        // Metadata only: never cache a root-bearing public observation here.
        let mut begun = receipt.clone();
        begun.continuation = None;
        self.begin_receipt = Some(begun);
        receipt
    }

    /// Start the state-owned settlement task. The task owns every resource and
    /// publishes through a shared receiver, so dropping one caller's wait future
    /// cannot abandon shutdown or its typed receipt.
    pub fn start_join(
        mut self,
        mut receipt: ApplicationLifecycleReceipt,
    ) -> ApplicationLifecycleSettlement {
        self.armed = false;
        let retention = self
            .initial_retention
            .take()
            .or_else(|| self.retention.upgrade())
            .unwrap_or_else(ApplicationLifecycleRetention::new);
        self.retention = Arc::downgrade(&retention);
        receipt.continuation = None;
        let fallback = receipt.clone();
        let result = retention.publications.subscribe();
        retention.publications.send_replace(None);
        {
            let mut state = retention
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.owner = Some((self, receipt));
        }
        retention.kick();
        ApplicationLifecycleSettlement {
            result,
            fallback,
            retention,
        }
    }

    /// Await all work accepted before [`Self::begin_shutdown`]. Earlier failures
    /// never suppress later joins; all of them are appended to one typed receipt.
    pub async fn join(self, receipt: ApplicationLifecycleReceipt) -> ApplicationLifecycleReceipt {
        self.start_join(receipt).wait().await
    }

    async fn join_owned(
        &mut self,
        mut receipt: ApplicationLifecycleReceipt,
    ) -> (bool, ApplicationLifecycleReceipt) {
        if self.cleanup_panicked {
            receipt.record_retained(
                "application lifecycle",
                "original cleanup panicked; no physical completion evidence",
            );
            return (true, receipt);
        }
        if !self.shutdown_begun {
            receipt.record(
                "application lifecycle",
                "join requested before shutdown admission closed",
            );
            let begin_receipt = self.begin_shutdown(receipt.reason, None);
            receipt.failures.extend(begin_receipt.failures);
        }

        if !self.normal_drained {
            // Phase two: settle accepted work. Continue after every failure so one
            // broken owner cannot suppress cleanup of later process resources.
            if let Some(state) = self.app_state.as_ref() {
                if let Err(error) = state.shutdown_agent_deliveries().await {
                    receipt.record("Agent deliveries", error);
                }
                if let Err(error) = state.session.foreground_turns.shutdown().await {
                    receipt.record("foreground turns", error);
                }
                record_analysis_cleanup_outcomes(
                    &mut receipt,
                    state.join_analysis_run_shutdown().await,
                );
                if let Err(error) = state.shutdown_model_mutations().await {
                    receipt.record("model mutations", error);
                }
            }

            for task in std::mem::take(&mut self.background_tasks) {
                if let Err(error) = task.handle.await {
                    receipt.record(task.name, error);
                }
            }
            for owner in &mut self.external_owners {
                // Do not additionally drop the poisoned future or the original
                // outer Owner/bundles. Foreign unwind may already have destroyed
                // its own internal locals; that is not recoverable proof.
                // Never repoll this future.
                match std::panic::AssertUnwindSafe(owner.join.as_mut())
                    .catch_unwind()
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => receipt.record(owner.name.clone(), error),
                    Err(_) => {
                        owner.join_panicked = true;
                        receipt.record_retained(owner.name.clone(), "original external owner join panicked; resources retained without a physical receipt");
                    }
                }
            }
            self.external_owners.retain(|owner| owner.join_panicked);
            if let Some(integration) = self.review_integration.as_ref()
                && let Err(error) = integration.shutdown_background_reviews().await
            {
                receipt.record("background reviews", error);
            }
            if let Some(state) = self.app_state.as_ref() {
                if let Err(error) = state.join_workspace_transition().await {
                    receipt.record("workspace transition", error);
                }
            }
        }
        // Re-observe the original slot on every real continuation poll. A
        // previous observation does not settle this dependency of Agent close.
        let scheduler_uncertain = if let Some(state) = self.app_state.as_ref() {
            match state.shutdown_scheduler().await {
                Ok(()) => false,
                Err(error) => {
                    receipt.record_retained("scheduler", error);
                    true
                }
            }
        } else {
            false
        };
        if !self.normal_drained {
            if let Some(store) = self.task_runtime_store.as_ref() {
                if let Err(error) = store.shutdown_run_drivers().await {
                    receipt.record("TaskRun drivers", error);
                }
                if let Err(error) = store.shutdown_operations().await {
                    receipt.record("TaskRuntime operations", error);
                }
                if let Err(error) = store.shutdown_hook_events().await {
                    receipt.record("task hook dispatcher", error);
                }
            }
            // Bootstrap rollback has no AppState yet. The command service owns a
            // ProductData flow, so both paths must settle it before the aggregate join.
            if let Some(state) = self.app_state.as_ref() {
                if let Err(error) = state.shutdown_command_cells().await {
                    receipt.record("command cells", error);
                }
            } else if let Some(runtime) = self.command_cell_runtime.as_ref()
                && let Err(error) = runtime.shutdown().await
            {
                receipt.record("command cells", error);
            }
            self.normal_drained = true;
        }
        // A resumed owner retries only original known cleanup debt, never Agent work.
        if let Some(store) = self.task_runtime_store.as_ref() {
            if let Err(error) = store.retry_run_settlement_debts().await {
                receipt.record_retained("TaskRun cleanup debt", error);
            }
        }
        if let Some(state) = self.app_state.as_ref() {
            for error in state.session.analysis_runs.retry_retained_cleanup() {
                receipt.record_retained("analysis original cleanup debt", error);
            }
        }
        // Product-data admission was sealed in phase one. Extension mutations
        // hold an owned flow while their durable commit and specialist fanout
        // settle, so this join must complete while the pool and specialist
        // runtimes they captured are still alive.
        if let Some(product_data_io) = self.product_data_io.as_ref()
            && let Err(error) = product_data_io.join_shutdown().await
        {
            receipt.record_retained("product-data I/O", error);
        }
        // Stop the producer that can mutate Agent configuration before asking
        // the same Agent's close authority to drain. Tool/plugin/MCP/browser
        // dependencies themselves stay alive until that close actually succeeds.
        if let Some(watcher) = self.config_watcher.take() {
            if let Err(error) = watcher.shutdown().await {
                receipt.record_retained("config watcher", error);
            }
        }
        let mut uncertain =
            scheduler_uncertain || self.external_owners.iter().any(|owner| owner.join_panicked);
        if let Some(state) = self.app_state.as_ref() {
            if let Err(error) = state.workspace.runtimes.join_known_shutdown().await {
                receipt.record_retained("workspace execution owners", error);
            }
            uncertain |= state.session.foreground_turns.has_retained_observation();
            uncertain |= state.session.analysis_runs.has_retained_owners();
            uncertain |= state.workspace.runtimes.has_unsettled_owners().await;
        }
        if let Some(store) = self.task_runtime_store.as_ref() {
            uncertain |= store.has_unsettled_driver_resources();
        }
        if let Some(pool) = self.pool.as_ref() {
            if let Err(error) = pool.join_known_executions().await {
                receipt.record_retained("AgentPool original owners", error);
            }
            uncertain |= pool.has_unsettled_executions();
        }
        if let Some(io) = self.product_data_io.as_ref() {
            uncertain |= io.has_unsettled_flows();
        }
        // A shared Agent/ToolManager is a dependency of an uncertain original
        // execution. Do not close it merely to manufacture a drained receipt.
        // Independent C flows/leases/hosts above still received their real joins.
        if !uncertain && !self.primary_closed {
            if let Some(agent) = self.primary_agent.as_ref() {
                match agent
                    .read_async(|agent| Box::pin(async move { agent.close().await }))
                    .await
                {
                    Ok(()) => self.primary_closed = true,
                    Err(error) => {
                        uncertain = true;
                        receipt.record_retained("primary Agent original close", error);
                    }
                }
            } else {
                self.primary_closed = true;
            }
        }
        if uncertain {
            receipt.record_retained(
                "application lifecycle",
                "original execution ownership is not physically settled; resources retained",
            );
            // Keep the original Owner, AppState and dependency resources; the
            // publication is observation only, never a final drained receipt.
            return (true, receipt);
        }
        #[cfg(test)]
        if let Some(started) = self.specialist_teardown_started.take() {
            let _ = started.send(());
        }
        if let Some(state) = self.app_state.as_ref()
            && let Err(error) = state.shutdown_workspace_runtimes().await
        {
            receipt.record("workspace runtimes", error);
        }
        if let Some(pool) = self.pool.as_ref()
            && let Err(error) = pool.shutdown().await
        {
            receipt.record("agent pool", error);
        }
        if let Some(state) = self.app_state.as_ref()
            && let Err(error) = state.terminal.close_all().await
        {
            receipt.record("terminal sessions", error);
        }

        if let Some(runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = runtime.shutdown().await
        {
            receipt.record("plugin runtime", error);
        }
        if let Some(runtime) = self.mcp_config_runtime.as_ref() {
            runtime.shutdown().await;
        }
        if let Some(runtime) = self.browser_runtime.as_ref() {
            runtime.shutdown().await;
        }
        if let Some(service) = self.subagent_projection.as_ref()
            && let Err(error) = service.shutdown_and_join().await
        {
            receipt.record("Subagent event projection", error);
        }
        (false, receipt)
    }

    pub async fn settle(
        mut self,
        reason: ApplicationLifecycleReason,
        primary_error: Option<anyhow::Error>,
    ) -> ApplicationLifecycleReceipt {
        let receipt = self.begin_shutdown(reason, primary_error);
        self.join(receipt).await
    }
}

impl Drop for ApplicationLifecycleOwner {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Armed owners still hold their initial root. A cancelled bootstrap or
        // owning surface future must hand the ORIGINAL value into real cleanup,
        // not abort tasks and then mistake Drop for physical completion.
        let root = self
            .initial_retention
            .clone()
            .or_else(|| self.retention.upgrade());
        let Some(root) = root else {
            self.root_cancel.cancel();
            return;
        };
        let mut empty = Self::with_retention(self.root_cancel.clone(), root);
        empty.armed = false;
        let mut original = std::mem::replace(self, empty);
        original.armed = false;
        let receipt = if original.shutdown_begun {
            original.begin_receipt.take().unwrap_or_else(|| {
                ApplicationLifecycleReceipt::new(ApplicationLifecycleReason::Shutdown, None)
            })
        } else {
            original.begin_shutdown(ApplicationLifecycleReason::Shutdown, None)
        };
        let _settlement = original.start_join(receipt);
    }
}

/// Shared agent runtime context.
///
/// Created once at application startup and lives for the entire process lifetime.
pub struct AgentRuntime {
    lifecycle_retention: Arc<ApplicationLifecycleRetention>,
    pub agent_handle: AgentHandle,
    pub model_consumers: crate::infra::AgentModelConsumers,
    pub hitl_dispatcher: Arc<HitlDispatcher>,
    pub app_config: EkoConfig,
    /// Exact model generation selected for this process. This may differ from
    /// the durable default when startup used `--model`.
    pub active_runtime_model: Option<crate::model_config::ModelRuntimeConfig>,
    /// Non-persistent session view used when creating future pooled agents.
    /// A CLI/TUI `--model` selector updates this view without changing the
    /// durable application default consumed by configuration mutations.
    pub session_app_config: EkoConfig,
    pub keyword_classifier: KeywordClassifier,
    /// Shared `RuntimeStateStore` produced during bootstrap. Surfaced on the
    /// runtime so `init_pool` (and any future product paths) can inject the
    /// same instance into pooled agents — bypasses the previous `extract_from`
    /// path which only saw a `None` value because the primary agent never had
    /// a state store wired in.
    pub state_store: Option<Arc<dyn echo_agent::state::RuntimeStateStore>>,
    /// Memory review integration for staleness scoring, conflict detection,
    /// and garbage collection. Created in bootstrap when a `Store` is available.
    /// Used by `/memory-review` command and session-end review hooks.
    pub review_integration: Option<Arc<ReviewIntegration>>,
    /// Application-owned Playwright MCP runtime shared by every agent surface.
    pub browser_runtime: Arc<crate::browser::BrowserRuntime>,
    /// Static EKO prompt-module budget report captured at agent build time.
    pub prompt_assembly: crate::project::prompt::PromptAssembly,
    /// Process-level shared plugin runtime used by every interaction surface.
    pub plugin_runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
    /// Canonical durable user MCP configuration shared with application state.
    pub mcp_config_runtime: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    pub extension_control: Arc<crate::extension_control::ExtensionControlService>,
    pub command_cell_runtime:
        Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
    /// Single application-generation owner for blocking product-data work.
    pub product_data_io: crate::product_data_io::ProductDataIoService,
    /// Current linked project root used for repository `AGENTS.md` and
    /// project-local `.eko/skills/` discovery. This is separate from EKO's
    /// workspace data root.
    pub project_root: Option<std::path::PathBuf>,
}

/// Canonical EKO application composition shared by every interaction surface.
///
/// This is deliberately application-side: it wires framework/runtime primitives
/// to EKO product policy, while each surface retains only its input/output and
/// bridge lifetime. The lifecycle owner is kept private so a surface cannot
/// accidentally bypass the shared shutdown order.
pub struct ApplicationServices {
    pub app_state: Arc<AppState>,
    pub pool: Arc<crate::agent_pool::AgentPool>,
    pub subagent_projection:
        Arc<crate::subagent_event_projection::SubagentEnvelopeProjectionService>,
    lifecycle: Option<ApplicationLifecycleOwner>,
    lifecycle_retention: Arc<ApplicationLifecycleRetention>,
}

impl Drop for ApplicationServices {
    fn drop(&mut self) {
        // Owner::drop transfers its original fields/receipt into real cleanup.
        // This also covers an owning join/settle future dropped before poll.
        if let Some(owner) = self.lifecycle.take() {
            drop(owner);
        }
    }
}

impl ApplicationServices {
    /// Compose one complete EKO application generation around an Agent runtime.
    ///
    /// `explicit_config` is the same user-selected config path used for loading.
    /// Resolving both the watcher source and immutable save target here prevents
    /// workspace changes from redirecting later configuration mutations.
    pub async fn compose(
        runtime: &AgentRuntime,
        explicit_config: Option<&str>,
        conversation_store: Option<Arc<dyn echo_agent::memory::ConversationStore>>,
        initial_pool_permission: echo_agent::tools::permission::PermissionMode,
    ) -> anyhow::Result<Self> {
        let root_cancel = tokio_util::sync::CancellationToken::new();
        let mut lifecycle = runtime.lifecycle_owner(root_cancel.clone())?;
        infra::inject_conversation_store(&runtime.agent_handle, &conversation_store);

        let webhook_emitter = Arc::new(crate::webhook::WebhookEmitter::from_config(
            &runtime.app_config,
        ));
        let config_path = crate::config_watcher::resolve_config_path(explicit_config);
        let config_save_path = crate::config_watcher::resolve_config_save_path(explicit_config);
        let config_workspace_root = runtime
            .agent_handle
            .read(|agent| agent.working_dir())
            .await
            .unwrap_or_else(|| PathBuf::from("."));
        let config_watcher = Arc::new(crate::config_watcher::spawn_config_watcher(
            config_path,
            runtime.agent_handle.clone(),
            config_workspace_root,
            Some(runtime.plugin_runtime.clone()),
            runtime.extension_control.clone(),
            Some(webhook_emitter.clone()),
            root_cancel.clone(),
        ));
        lifecycle.bind_config_watcher(config_watcher.clone());

        let mut state = match AppState::from_shared(
            runtime.agent_handle.clone(),
            Some(runtime.model_consumers.clone()),
            runtime.hitl_dispatcher.clone(),
            conversation_store,
            runtime.state_store.clone(),
            runtime.app_config.clone(),
            runtime.mcp_config_runtime.clone(),
            runtime.product_data_io.clone(),
        ) {
            Ok(state) => state,
            Err(error) => return Err(rollback_composition(lifecycle, error).await),
        };
        if let Some(active_model) = runtime.active_runtime_model.as_ref() {
            state = state.with_active_model_id(active_model.id.clone());
        }
        state = state
            .with_config_path(config_save_path)
            .with_review_integration(runtime.review_integration.clone())
            .with_prompt_assembly(runtime.prompt_assembly.clone())
            .with_plugin_runtime(Some(runtime.plugin_runtime.clone()))
            .with_extension_control(runtime.extension_control.clone())
            .with_browser_runtime(Some(runtime.browser_runtime.clone()))
            .with_config_watcher(Some(config_watcher))
            .with_command_cell_runtime(runtime.command_cell_runtime.clone())
            .with_workspace_delete_hook(runtime.browser_runtime.clone());
        state.webhook.emitter = webhook_emitter;

        match state.recover_committed_conversation_deletions().await {
            Ok(receipts) if !receipts.is_empty() => tracing::info!(
                count = receipts.len(),
                "Recovered committed conversation deletion finalizers"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                %error,
                "Some committed conversation deletion finalizers remain pending"
            ),
        }

        let task_runtime = match state.tasks.runtime.clone() {
            Some(store) => store,
            None => {
                return Err(rollback_composition(
                    lifecycle,
                    anyhow::anyhow!("application TaskRuntime store is unavailable"),
                )
                .await);
            }
        };
        lifecycle.bind_task_runtime(task_runtime.clone());
        crate::tasks::task_runtime::register_task_tools_on_agent(
            &runtime.agent_handle,
            task_runtime.clone(),
            runtime.model_consumers.subagent_catalog(),
        )
        .await;

        let pool = match runtime
            .init_pool(
                crate::agent_pool::PoolConfig::default(),
                Some(task_runtime.clone()),
            )
            .await
        {
            Ok(pool) => pool,
            Err(error) => {
                lifecycle.bind_app_state(Arc::new(state));
                return Err(rollback_composition(lifecycle, error).await);
            }
        };
        lifecycle.bind_pool(pool.clone());
        // Apply the surface's initial pool policy before boot recovery can
        // resume a TaskRun. LH6 deliberately bypasses prompts; product surfaces
        // start in Default and may publish later user changes.
        pool.apply_permission_mode(initial_pool_permission).await;
        crate::tasks::task_runtime::bind_task_execute_to_pool(
            &runtime.agent_handle,
            task_runtime.clone(),
            &pool,
        )
        .await;
        // This setter also installs the workspace-aware execution-target
        // resolver. Direct field assignment leaves headless TaskRuns unable to
        // resolve cold or switched workspace targets.
        state.set_pool(pool.clone());

        let subagent_projector = Arc::new(
            crate::subagent_event_projection::SubagentEnvelopeProjector::new(
                runtime.model_consumers.subagent_event_bus(),
                Some(task_runtime.clone()),
                state.workspace.runtimes.clone(),
                state.session.foreground_turns.clone(),
                state.storage.chat_events.clone(),
                state.storage.tool_executions.clone(),
            ),
        );
        let subagent_projection =
            crate::subagent_event_projection::SubagentEnvelopeProjectionService::start(
                subagent_projector,
            );
        lifecycle.bind_subagent_projection(subagent_projection.clone());

        let scheduler_store: Arc<dyn echo_agent::memory::Store> = {
            let file_path = crate::data_root::user_data_path("scheduler_store");
            match echo_agent::memory::FileStore::new(&file_path) {
                Ok(store) => Arc::new(store),
                Err(error) => {
                    tracing::warn!(%error, "failed to create scheduler store; using in-memory");
                    Arc::new(echo_agent::memory::InMemoryStore::new())
                }
            }
        };
        if let Err(error) = state
            .start_scheduler_and_task_service(Some(scheduler_store))
            .await
        {
            lifecycle.bind_app_state(Arc::new(state));
            return Err(rollback_composition(lifecycle, anyhow::Error::new(error)).await);
        }
        if let Some(scheduler) = state.scheduler.runner.as_ref()
            && let Err(error) = runtime
                .plugin_runtime
                .bind_scheduler(scheduler.clone())
                .await
        {
            tracing::warn!(%error, "failed to bind plugin monitors to application scheduler");
        }

        let app_state = Arc::new(state);
        app_state.register_agent_control_tools().await;
        lifecycle.bind_app_state(app_state.clone());
        if let Err(error) = app_state.recover_pending_conversation_handoffs().await {
            return Err(rollback_composition(lifecycle, anyhow::anyhow!(error)).await);
        }
        match app_state
            .extension_control
            .reconcile_enabled_skills_on_load(&app_state)
            .await
        {
            Ok(receipt)
                if receipt.status == crate::extension_control::SkillSettlementStatus::Settled =>
            {
                tracing::info!("Extension Skill runtimes settled during application startup");
            }
            Ok(receipt) => tracing::warn!(
                status = ?receipt.status,
                "Extension Skill runtime reconciliation is degraded after application startup"
            ),
            Err(error) => tracing::warn!(
                %error,
                "Extension Skill policy reconciliation remains pending"
            ),
        }
        if let Err(error) = app_state.recover_agent_deliveries().await {
            tracing::warn!(%error, "failed to resume durable Agent deliveries during startup");
        }

        let health_task = infra::spawn_mcp_health_check(app_state.clone(), root_cancel.clone());
        lifecycle.track_background_task("MCP health check", health_task);
        if let Some(review_integration) = app_state.review_integration.clone() {
            let dreaming_task = infra::spawn_dreaming_task(review_integration, root_cancel);
            lifecycle.track_background_task("Dreaming", dreaming_task);
        }

        Ok(Self {
            app_state,
            pool,
            subagent_projection,
            lifecycle: Some(lifecycle),
            lifecycle_retention: runtime.lifecycle_retention.clone(),
        })
    }

    /// Add a surface-only bridge to the canonical application settlement.
    pub fn track_external_owner<Begin, Join>(
        &mut self,
        name: impl Into<String>,
        begin: Begin,
        join: Join,
    ) -> anyhow::Result<()>
    where
        Begin: FnOnce() -> Result<(), String> + Send + 'static,
        Join: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let lifecycle = self
            .lifecycle
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("application lifecycle owner is unavailable"))?;
        lifecycle.track_external_owner(name, begin, join);
        Ok(())
    }

    pub fn begin_shutdown(
        &mut self,
        reason: ApplicationLifecycleReason,
        primary_error: Option<anyhow::Error>,
    ) -> anyhow::Result<ApplicationLifecycleReceipt> {
        let lifecycle = self
            .lifecycle
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("application lifecycle owner is unavailable"))?;
        Ok(lifecycle.begin_shutdown(reason, primary_error))
    }

    pub async fn join(
        mut self,
        mut receipt: ApplicationLifecycleReceipt,
    ) -> ApplicationLifecycleReceipt {
        let _original_root = self.lifecycle_retention.clone();
        match self.lifecycle.take() {
            Some(lifecycle) => lifecycle.join(receipt).await,
            None => {
                receipt.record("application lifecycle", "settlement owner is unavailable");
                receipt
            }
        }
    }

    pub async fn settle(
        mut self,
        reason: ApplicationLifecycleReason,
        primary_error: Option<anyhow::Error>,
    ) -> ApplicationLifecycleReceipt {
        let _original_root = self.lifecycle_retention.clone();
        match self.lifecycle.take() {
            Some(lifecycle) => lifecycle.settle(reason, primary_error).await,
            None => {
                let mut receipt = ApplicationLifecycleReceipt::new(reason, primary_error);
                receipt.record("application lifecycle", "settlement owner is unavailable");
                receipt
            }
        }
    }
}

async fn rollback_composition(
    lifecycle: ApplicationLifecycleOwner,
    error: anyhow::Error,
) -> anyhow::Error {
    let receipt = lifecycle
        .settle(ApplicationLifecycleReason::BootstrapRollback, Some(error))
        .await;
    anyhow::Error::new(receipt.into_error())
}

impl AgentRuntime {
    /// Establish the process root owner immediately after bootstrap commits.
    /// Later construction binds TaskRuntime, AgentPool, AppState and surface
    /// owners progressively; any intermediate failure can therefore rollback
    /// every resource already accepted by the process.
    pub fn lifecycle_owner(
        &self,
        root_cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ApplicationLifecycleOwner> {
        if self
            .lifecycle_retention
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            let mut receipt = self
                .lifecycle_retention
                .publications
                .borrow()
                .as_ref()
                .map(|publication| publication.receipt.clone())
                .unwrap_or_else(|| {
                    ApplicationLifecycleReceipt::new(ApplicationLifecycleReason::Shutdown, None)
                });
            receipt.record("application generation", "lifecycle owner was already established; observe its original settlement instead of admitting another owner");
            receipt.continuation = Some(self.lifecycle_retention.clone());
            return Err(anyhow::Error::new(receipt.into_error()));
        }
        let mut owner = ApplicationLifecycleOwner::with_retention(
            root_cancel,
            self.lifecycle_retention.clone(),
        );
        owner.bind_primary_agent(self.agent_handle.clone());
        owner.bind_plugin_runtime(self.plugin_runtime.clone());
        owner.bind_mcp_config_runtime(self.mcp_config_runtime.clone());
        owner.bind_browser_runtime(self.browser_runtime.clone());
        owner.bind_product_data_io(self.product_data_io.clone());
        owner.bind_command_cell_runtime(self.command_cell_runtime.clone());
        if let Some(integration) = self.review_integration.as_ref() {
            owner.bind_review_integration(integration.clone());
        }
        Ok(owner)
    }

    /// Bootstrap the agent runtime.
    ///
    /// This is the single source of truth for agent initialization. Both TUI and
    /// GUI entry points call this method instead of duplicating the setup logic.
    ///
    /// # Steps performed
    /// 1. Create `ReactAgent` via `infra::create_agent`
    /// 2. Load MCP configuration
    /// 3. Configure auto-compression
    /// 4. Wrap in `AgentHandle`
    /// 5. Wire HITL dispatcher
    /// 6. Load user hooks
    /// 7. Create hook bridges (task + subagent lifecycle)
    /// 8. Initialize unified memory
    /// 9. Load plugins (skills / hooks / MCP)
    /// 10. Register LSP tools
    /// 11. Fire startup hook
    pub async fn bootstrap(
        app_config: &EkoConfig,
        mut params: AgentCreateParams,
        mcp_config_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let project_root = crate::project::context::resolve_project_root(
            params.project.as_deref(),
            params.working_dir.as_deref(),
        );
        let product_data_io = params.product_data_io.clone().unwrap_or_default();
        params.product_data_io = Some(product_data_io.clone());
        // ── 0a. Runtime state store (must be ready before agent is built so that
        //       conversation_id + state_store land on the AgentConfig together;
        //       otherwise `save_runtime_checkpoint` silently no-ops). ──
        let state_store = infra::create_runtime_state_store();
        if params.state_store.is_none() {
            params.state_store = state_store.clone();
        }

        // Default conversation_id for the *primary* agent. Both
        // `save_runtime_checkpoint` and `save_transcript_projection` early-return
        // when `conversation_id` is None. Use a fresh id per primary session to
        // avoid merging independent TUI/CLI runs into a shared "primary" row.
        if params.conversation_id.is_none() {
            params.conversation_id = Some(infra::default_primary_conversation_id());
        }

        // ── 0b. Resolve and parse the canonical MCP source before starting
        // background resources. A malformed existing file aborts bootstrap so
        // it cannot later be overwritten by an empty in-memory snapshot.
        let mcp_config_snapshot = crate::mcp_config_runtime::load_mcp_config_snapshot(
            &mcp_config_path,
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "canonical MCP config {} cannot be loaded: {error}",
                mcp_config_path.display()
            )
        })?;
        let mcp_config_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            mcp_config_path.clone(),
            mcp_config_snapshot.clone(),
        ));
        let extension_control =
            Arc::new(crate::extension_control::ExtensionControlService::default());

        let browser_runtime_injected = params.browser_runtime.is_some();
        let command_cell_runtime_injected = params.command_cell_runtime.is_some();
        let browser_runtime = match params.browser_runtime.clone() {
            Some(runtime) => runtime,
            None => {
                crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig::from_env())
                    .await
            }
        };
        params.browser_runtime = Some(browser_runtime.clone());

        let mut bootstrap_lifecycle =
            ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        bootstrap_lifecycle.bind_product_data_io(product_data_io.clone());
        bootstrap_lifecycle.bind_mcp_config_runtime(mcp_config_runtime.clone());
        if !browser_runtime_injected {
            bootstrap_lifecycle.bind_browser_runtime(browser_runtime.clone());
        }

        // ── 1. Create Agent ──
        let created = match infra::create_agent_with_diagnostics(&params, app_config).await {
            Ok(created) => created,
            Err(error) => {
                let receipt = bootstrap_lifecycle
                    .settle(
                        ApplicationLifecycleReason::BootstrapRollback,
                        Some(anyhow::anyhow!(error.to_string())),
                    )
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
        };
        let mut agent = created.agent;
        let prompt_assembly = created.prompt_assembly;
        let model_consumers = created.model_consumers;
        let active_runtime_model = created.runtime_model;
        let command_cell_runtime = created.command_cell_runtime;
        let session_app_config = match active_runtime_model.as_ref() {
            Some(runtime) => {
                match crate::model_config::session_config_for_runtime(app_config, runtime) {
                    Ok(config) => config,
                    Err(error) => {
                        bootstrap_lifecycle.bind_primary_agent(AgentHandle::new(agent));
                        if !command_cell_runtime_injected {
                            bootstrap_lifecycle
                                .bind_command_cell_runtime(command_cell_runtime.clone());
                        }
                        let receipt = bootstrap_lifecycle
                            .settle(
                                ApplicationLifecycleReason::BootstrapRollback,
                                Some(anyhow::Error::msg(error)),
                            )
                            .await;
                        return Err(anyhow::Error::new(receipt.into_error()));
                    }
                }
            }
            None => app_config.clone(),
        };

        // ── 2. Connect the same snapshot exposed to application state. ──
        tracing::info!(path = %mcp_config_path.display(), "Canonical MCP config selected");
        match agent.load_mcp_config(mcp_config_snapshot).await {
            Ok(clients) => tracing::info!(count = clients.len(), "MCP user servers connected"),
            Err(error) => tracing::warn!(%error, "MCP user config connection failed"),
        }

        // ── 3. Auto-compression ──
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::info!("Auto context compression configured");
        }

        let agent_handle = AgentHandle::new(agent);
        bootstrap_lifecycle.bind_primary_agent(agent_handle.clone());
        if !command_cell_runtime_injected {
            bootstrap_lifecycle.bind_command_cell_runtime(command_cell_runtime.clone());
        }
        if let Some(conversation_id) = params.conversation_id.as_deref() {
            let execution_scope = params.execution_scope.clone().unwrap_or_else(|| {
                crate::workspace::WorkspaceExecutionScope::global(
                    params
                        .working_dir
                        .clone()
                        .unwrap_or_else(|| std::path::PathBuf::from(".")),
                )
            });
            command_cell_runtime.bind_agent(
                execution_scope.workspace_id(),
                conversation_id,
                &agent_handle,
            );
        }
        if let Err(error) =
            crate::skills_hub::load_project_skills(&agent_handle, project_root.as_deref()).await
        {
            tracing::warn!(%error, "Failed to load project-local skills");
        }

        // ── NOTE: ExecuteTaskTool + the task-management tools are NOT registered
        // here. The TaskRuntimeStore doesn't exist yet at primary-agent build
        // time (GUI: AppState creates it later; TUI: built in main.rs after
        // bootstrap), so BOTH entry points call `register_task_tools_on_agent`
        // (in app-core `tasks/task_runtime/register.rs`) post-hoc once the store
        // is ready. TUI/GUI functional parity (AGENTS.md).
        // Chat 可用 agent_tool 做单个临时子任务;Auto/Task 的委派统一进入正式 TaskRuntime。
        // ── 4. HITL dispatcher ──
        let hitl_dispatcher = {
            let dispatcher = Arc::new(HitlDispatcher::new());
            agent_handle
                .write_async(|a| {
                    let d = dispatcher.clone();
                    Box::pin(async move {
                        a.set_human_loop_provider(d);
                        a.build_permission_service();
                    })
                })
                .await;
            tracing::info!(
                "HITL dispatcher + PermissionService wired to agent; surfaces register transports"
            );
            dispatcher
        };
        browser_runtime
            .set_default_approval_provider(hitl_dispatcher.clone())
            .await;

        // ── 6. User hooks ──
        // Single merged load: eko.yaml inline + ~/.eko/hooks.yaml +
        // .eko/hooks.yaml are merged into one HooksDefinition by
        // HookConfigLoader (P0-1), then registered once. The previous code
        // loaded inline and file sources separately, each calling
        // clear_user_hooks(), so the second load wiped the first — a silent
        // bug where eko.yaml inline hooks disappeared whenever any
        // hooks.yaml file existed.
        let hook_project_root = agent_handle.read(|agent| agent.working_dir()).await;
        infra::load_user_hooks(&agent_handle, app_config, hook_project_root.as_deref()).await;

        // ── 8b. Review integration — create when Store is available so
        //       /memory-review and session-end hooks can access it. ──
        // The `echo_agent_dir` MUST be the same root the memory store was
        // built from (see infra::create_agent), so hot-layer `MEMORY.md` and
        // warm-layer `store.json` land in the same project directory and never
        // diverge. We resolve it from `params.working_dir` (workspace root) —
        // identical to the store path resolution done in `create_agent`.
        let (_, review_echo_agent_dir) =
            infra::resolve_memory_store_paths(params.working_dir.as_deref());
        // Only the manifest at the Store's exact data root may provide the
        // durable workspace identity. Ancestor discovery could assign a child
        // project's Store to another root, and malformed manifests must never
        // silently downgrade pending workspace debt to global ownership.
        let review_workspace = match bootstrap_review_workspace(&review_echo_agent_dir) {
            Ok(workspace) => workspace,
            Err(error) => return Err(rollback_composition(bootstrap_lifecycle, error).await),
        };
        let review_echo_agent_dir = review_workspace
            .as_ref()
            .map(|workspace| crate::workspace::layout::WorkspaceLayout::state_dir(&workspace.root))
            .unwrap_or(review_echo_agent_dir);
        let review_store = agent_handle.read(|a| a.store().cloned()).await;
        let review_integration = review_store.map(|store| {
            Arc::new(match review_workspace.as_ref() {
                Some(workspace) => ReviewIntegration::new_scoped_with_project_root(
                    ReviewConfig::default(),
                    review_echo_agent_dir.clone(),
                    store,
                    workspace.id.to_string(),
                    workspace.opaque_product_data_generation(),
                    workspace.project_root.clone(),
                ),
                None => ReviewIntegration::new_with_project_root(
                    ReviewConfig::default(),
                    review_echo_agent_dir.clone(),
                    store,
                    project_root.clone(),
                ),
            })
        });
        if review_integration.is_some() {
            tracing::info!("ReviewIntegration created for session");
        }
        if let Some(review_integration) = &review_integration {
            bootstrap_lifecycle.bind_review_integration(review_integration.clone());
            review_integration.bind_rule_projection_primary_for_project(
                agent_handle.clone(),
                project_root.clone(),
            );
            let execution_scope = params.execution_scope.clone().unwrap_or_else(|| {
                crate::workspace::WorkspaceExecutionScope::global(
                    params
                        .working_dir
                        .clone()
                        .unwrap_or_else(|| std::path::PathBuf::from(".")),
                )
            });
            let projector = crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            )
            .with_command_cell_watches(command_cell_runtime.clone(), execution_scope)
            .with_hot_memory_source(review_integration.hot_memory_projection_source());
            agent_handle
                .read(|agent| agent.set_pre_model_context_projector(Some(Arc::new(projector))))
                .await;
            if let Err(error) = review_integration.initialize_rule_promotions().await {
                let receipt = bootstrap_lifecycle
                    .settle(
                        ApplicationLifecycleReason::BootstrapRollback,
                        Some(error.into()),
                    )
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
            let evolution_observer = crate::evolution::evolution_hook_observer(&agent_handle).await;
            review_integration.set_evolution_observer(evolution_observer);
            let memory_generation = match review_integration.lease_generation() {
                Ok(generation) => generation,
                Err(error) => {
                    let receipt = bootstrap_lifecycle
                        .settle(
                            ApplicationLifecycleReason::BootstrapRollback,
                            Some(anyhow::anyhow!(
                                "Failed to reserve memory generation: {error}"
                            )),
                        )
                        .await;
                    return Err(anyhow::Error::new(receipt.into_error()));
                }
            };
            let layer_manager = match memory_generation.layer_manager() {
                Ok(layer_manager) => layer_manager,
                Err(error) => {
                    let receipt = bootstrap_lifecycle
                        .settle(
                            ApplicationLifecycleReason::BootstrapRollback,
                            Some(anyhow::anyhow!(
                                "Failed to initialize memory layer: {error}"
                            )),
                        )
                        .await;
                    return Err(anyhow::Error::new(receipt.into_error()));
                }
            };
            let recovered_reviews = match memory_generation.recover_background_reviews().await {
                Ok(receipts) => receipts,
                Err(error) => {
                    let receipt = bootstrap_lifecycle
                        .settle(
                            ApplicationLifecycleReason::BootstrapRollback,
                            Some(anyhow::anyhow!(
                                "Failed to recover Background Review: {error}"
                            )),
                        )
                        .await;
                    return Err(anyhow::Error::new(receipt.into_error()));
                }
            };
            for receipt in recovered_reviews {
                tracing::warn!(
                    operation_id = %receipt.operation_id,
                    run_id = %receipt.identity.run_id,
                    result = ?receipt.result,
                    "recovered unfinished Background Review"
                );
            }
            let trigger_sink = review_integration.clone();
            let skill_policy = review_integration.clone();
            let skill_authority = match memory_generation.skill_mutation_authority() {
                Ok(authority) => authority,
                Err(error) => {
                    let receipt = bootstrap_lifecycle
                        .settle(
                            ApplicationLifecycleReason::BootstrapRollback,
                            Some(anyhow::anyhow!(
                                "Failed to initialize Skill authority: {error}"
                            )),
                        )
                        .await;
                    return Err(anyhow::Error::new(receipt.into_error()));
                }
            };
            agent_handle
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_layer_manager(layer_manager)?;
                        a.set_memory_trigger_sink(Some(trigger_sink));
                        a.set_skill_load_policy(Some(skill_policy));
                        a.set_skill_mutation_authority(Some(skill_authority));
                        let _ = a.reconcile_skill_load_policy().await;
                        Ok::<(), echo_agent::error::ReactError>(())
                    })
                })
                .await
                .map_err(|error| anyhow::anyhow!("Failed to install memory layer: {error}"))?;
            let projection = memory_generation.settle_hot_memory_projection().await;
            if projection.status == crate::evolution::MemoryProjectionSettlementStatus::Degraded {
                let error = projection
                    .error
                    .unwrap_or_else(|| "initial hot-memory projection did not settle".to_string());
                let receipt = bootstrap_lifecycle
                    .settle(
                        ApplicationLifecycleReason::BootstrapRollback,
                        Some(anyhow::anyhow!(error)),
                    )
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
            tracing::info!("Layered memory, evidence sink, and skill policy installed");
        }

        // ── 9. LSP runtime ──
        // Plugins and built-in project discovery share this single manager;
        // plugin reload atomically replaces its contents while every LSP tool
        // keeps the same Arc handle.
        let lsp_project_root = agent_handle
            .read(|agent| agent.working_dir())
            .await
            .unwrap_or_else(crate::data_root::user_data_dir);
        let lsp_runtime = register_lsp_tools(&agent_handle, &lsp_project_root).await;

        // ── 10. Plugins ──
        // Discovery, initial wiring, and later live mutations all go through
        // one runtime owner. This avoids bootstrap/reload double registration.
        let plugin_runtime = match crate::plugin_runtime::PluginRuntimeService::new(
            agent_handle.clone(),
            lsp_runtime,
            mcp_config_runtime.ownership(),
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let receipt = bootstrap_lifecycle
                    .settle(ApplicationLifecycleReason::BootstrapRollback, Some(error))
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
        };
        bootstrap_lifecycle.bind_plugin_runtime(plugin_runtime.clone());

        // ── 11. File-backed research library ──
        let auto_ingest_identity = crate::workspace::WorkspaceIoIdentity::global(
            params
                .working_dir
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        );
        let research_workspace_identity = auto_ingest_identity.clone();
        let research_product_data_io = product_data_io.clone();
        agent_handle
            .write(move |agent| {
                crate::research_connectors::install_auto_ingest_tools(
                    agent,
                    auto_ingest_identity,
                    research_product_data_io.clone(),
                );
                agent.add_tool(Box::new(crate::research_tool::ResearchLibraryTool::new(
                    research_product_data_io,
                    research_workspace_identity,
                )));
            })
            .await;

        // ── 12. Startup hook ──
        infra::fire_startup_hook(&agent_handle).await;

        // Intent routing is a projection of the live skill catalog. Dynamic
        // plugin publications reuse this exact builder for primary and pool.
        let keyword_classifier = agent_handle.write(configure_intent_router).await;

        let lifecycle_retention = bootstrap_lifecycle
            .retention
            .upgrade()
            .unwrap_or_else(ApplicationLifecycleRetention::new);
        lifecycle_retention
            .claimed
            .store(false, std::sync::atomic::Ordering::Release);
        bootstrap_lifecycle.disarm();

        Ok(Self {
            lifecycle_retention,
            agent_handle,
            model_consumers,
            hitl_dispatcher,
            app_config: app_config.clone(),
            active_runtime_model,
            session_app_config,
            keyword_classifier,
            state_store,
            review_integration,
            browser_runtime,
            prompt_assembly,
            plugin_runtime,
            mcp_config_runtime,
            extension_control,
            command_cell_runtime,
            product_data_io,
            project_root,
        })
    }

    /// Initialize an `AgentPool` from this runtime for multi-conversation
    /// parallel execution.
    ///
    /// Extracts shared resources from the primary agent and creates a pool
    /// that can spin up isolated agent instances on demand.
    pub async fn init_pool(
        &self,
        config: crate::agent_pool::PoolConfig,
        task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Arc<crate::agent_pool::AgentPool>> {
        crate::agent_pool::AgentPool::from_runtime(self, config, task_runtime_store).await
    }
}

fn bootstrap_review_workspace(
    echo_agent_dir: &std::path::Path,
) -> anyhow::Result<Option<crate::workspace::Workspace>> {
    let manifest = echo_agent_dir.join("workspace.json");
    let data = match std::fs::read_to_string(&manifest) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to read Background Review workspace identity at {}: {error}",
                manifest.display()
            ));
        }
    };
    let mut workspace: crate::workspace::Workspace =
        serde_json::from_str(&data).map_err(|error| {
            anyhow::anyhow!(
                "Invalid Background Review workspace identity at {}: {error}",
                manifest.display()
            )
        })?;
    let root = echo_agent_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Background Review data root has no workspace parent"))?
        .canonicalize()?;
    if workspace.root.canonicalize()? != root {
        anyhow::bail!(
            "Background Review workspace manifest {} belongs to a different root",
            manifest.display()
        );
    }
    workspace.root = root;
    workspace.refresh_product_data_generation();
    Ok(Some(workspace))
}

/// Rebuild EKO intent routing from one agent's committed skill catalog.
pub(crate) fn configure_intent_router(
    agent: &mut echo_agent::agent::ReactAgent,
) -> KeywordClassifier {
    let descriptors = agent.skill_descriptors();
    let mut keyword_classifier = KeywordClassifier::new();
    let mut skill_descriptions = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let triggers = descriptor
            .triggers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keyword_classifier.add_skill_keywords(&descriptor.name, &triggers);
        skill_descriptions.push(SkillDescription {
            name: descriptor.name.clone(),
            description: descriptor.description.clone(),
            example_triggers: descriptor.triggers.iter().take(3).cloned().collect(),
        });
    }
    let available_skill_names = skill_descriptions
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    let llm_classifier = agent
        .llm_client()
        .cloned()
        .map(|llm| LlmIntentClassifier::new(llm, skill_descriptions));
    let has_llm = llm_classifier.is_some();
    let supervisor = TriggerSupervisor::new(
        keyword_classifier.clone(),
        llm_classifier,
        agent.hook_activation_cache(),
    );
    let router = echo_agent::intent::IntentRouter::new(
        Box::new(supervisor),
        echo_agent::intent::IntentRouterConfig {
            confidence_threshold: 0.7,
            enable_direct_answer: true,
            enable_skill_routing: true,
            classification_timeout_ms: 5_000,
        },
    )
    .with_available_skills(available_skill_names);
    agent.set_intent_router(router);
    tracing::info!(
        has_llm,
        skill_count = descriptors.len(),
        "IntentRouter replaced from committed skill catalog"
    );
    keyword_classifier
}

/// Register the shared LSP specialist for one explicit workspace identity.
///
/// The caller owns workspace selection. Keeping cwd fallback out of this
/// function prevents a process-level directory from being mistaken for the
/// active GUI/headless workspace during generation changes.
pub(crate) async fn register_lsp_tools(
    agent_handle: &AgentHandle,
    project_root: &std::path::Path,
) -> crate::plugin_runtime::PluginLspRuntime {
    use echo_agent::lsp::LspManager;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let config = crate::plugin_runtime::PluginLspRuntime::config_for_workspace(project_root);
    if !config.servers.is_empty() {
        tracing::info!(
            root = %project_root.display(),
            languages = config.servers.len(),
            "LSP servers auto-discovered"
        );
    }

    let mut lsp_manager = LspManager::new();
    if let Err(error) = lsp_manager.load_config(&config) {
        tracing::warn!(%error, "LSP config failed to load for workspace");
    }
    lsp_manager.set_project_root(project_root);
    let languages: Vec<String> = lsp_manager
        .configured_languages()
        .into_iter()
        .map(str::to_string)
        .collect();
    for language in languages {
        if let Err(error) = lsp_manager.start_server(&language).await {
            tracing::warn!(%language, %error, "LSP server unavailable");
        }
    }

    let shared_lsp = Arc::new(RwLock::new(lsp_manager));
    agent_handle
        .write_async(|a| {
            let shared_lsp = shared_lsp.clone();
            Box::pin(async move {
                use echo_agent::tools::lsp::{
                    LspDiagnosticsTool, LspFindReferencesTool, LspGotoDefinitionTool, LspHoverTool,
                    LspStatusTool,
                };
                a.add_tool(Box::new(LspDiagnosticsTool::new(shared_lsp.clone())));
                a.add_tool(Box::new(LspGotoDefinitionTool::new(shared_lsp.clone())));
                a.add_tool(Box::new(LspFindReferencesTool::new(shared_lsp.clone())));
                a.add_tool(Box::new(LspHoverTool::new(shared_lsp.clone())));
                a.add_tool(Box::new(LspStatusTool::new(shared_lsp)));
            })
        })
        .await;
    tracing::info!("LSP tools registered");
    crate::plugin_runtime::PluginLspRuntime::new(shared_lsp, config, project_root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::intent::IntentClassifier;

    fn run_isolated_bootstrap_test(name: &str) -> anyhow::Result<bool> {
        if std::env::var_os("EKO_BOOTSTRAP_ISOLATED").is_some() {
            return Ok(false);
        }
        let temp = tempfile::tempdir()?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", name, "--nocapture"])
            .env("EKO_BOOTSTRAP_ISOLATED", "1")
            .env("EKO_DATA_DIR", temp.path().join("user-data"))
            .output()?;
        assert!(
            output.status.success(),
            "isolated bootstrap test failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        Ok(true)
    }

    async fn bootstrap_review_fixture(
        root: Option<PathBuf>,
        fixture_dir: &std::path::Path,
    ) -> anyhow::Result<AgentRuntime> {
        bootstrap_review_fixture_with_project(root, None, fixture_dir).await
    }

    async fn bootstrap_review_fixture_with_project(
        root: Option<PathBuf>,
        project: Option<String>,
        fixture_dir: &std::path::Path,
    ) -> anyhow::Result<AgentRuntime> {
        let browser = crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig {
            enabled: false,
            extension_enabled: false,
            session_dir: fixture_dir.join("browser-sessions"),
            ..Default::default()
        })
        .await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            AgentRuntime::bootstrap(
                &EkoConfig::default(),
                AgentCreateParams {
                    working_dir: root,
                    project,
                    browser_runtime: Some(browser.clone()),
                    state_store: Some(Arc::new(echo_agent::state::FileRuntimeStateStore::new(
                        fixture_dir.join("runtime-state"),
                    )?)),
                    ..Default::default()
                },
                fixture_dir.join("missing-mcp.json"),
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("bootstrap did not settle within 15 seconds"))?;
        if result.is_err() {
            browser.shutdown().await;
        }
        result
    }

    async fn projected_test_instructions(agent: &AgentHandle) -> String {
        let context = agent.read(|agent| agent.context().clone()).await;
        context
            .lock()
            .await
            .messages()
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn bootstrap_keeps_explicit_project_context_with_workspace_memory_owner()
    -> anyhow::Result<()> {
        use crate::workspace::{
            WorkspaceKind, layout::WorkspaceLayout, registry::WorkspaceRegistry,
        };

        if run_isolated_bootstrap_test(
            "runtime::tests::bootstrap_keeps_explicit_project_context_with_workspace_memory_owner",
        )? {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))?;
        let workspace = registry.create_at(
            "explicit-project",
            WorkspaceKind::General,
            temp.path().join("workspace"),
        )?;
        let linked_project = temp.path().join("linked-project");
        let explicit_project = temp.path().join("explicit-project");
        std::fs::create_dir_all(linked_project.join(".git"))?;
        std::fs::create_dir_all(explicit_project.join(".git"))?;
        std::fs::write(
            linked_project.join("AGENTS.md"),
            "LINKED_PROJECT_P_INSTRUCTION",
        )?;
        std::fs::write(
            explicit_project.join("AGENTS.md"),
            "EXPLICIT_PROJECT_Q_INSTRUCTION",
        )?;
        let workspace = registry.link_project(&workspace.id, linked_project)?;
        let explicit_project = explicit_project.canonicalize()?;
        let runtime = bootstrap_review_fixture_with_project(
            Some(workspace.root.clone()),
            Some(explicit_project.display().to_string()),
            temp.path(),
        )
        .await?;
        assert_eq!(runtime.project_root.as_ref(), Some(&explicit_project));
        let prompt = projected_test_instructions(&runtime.agent_handle).await;
        let review = runtime
            .review_integration
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bootstrap Review owner is missing"))?;
        let generation = review.lease_generation()?;
        assert!(
            generation
                .matches_workspace_binding(
                    &WorkspaceLayout::state_dir(&workspace.root.canonicalize()?),
                    workspace.id.as_str(),
                    &workspace.opaque_product_data_generation(),
                )
                .map_err(anyhow::Error::msg)?
        );
        let memory_store = generation.memory_store();
        drop(generation);
        let pool = runtime
            .init_pool(
                crate::agent_pool::PoolConfig {
                    enable_background_agent: false,
                    ..Default::default()
                },
                None,
            )
            .await?;
        pool.set_llm_client_override_for_test(Arc::new(echo_agent::testing::MockLlmClient::new()))
            .await;
        let existing_seed = pool.acquire("explicit-existing").await?;
        let existing_seed_agent = existing_seed.agent();
        drop(existing_seed);
        let hosts = crate::workspace::runtime::WorkspaceRuntimeRegistry::new();
        let host = hosts.get_or_open(workspace).await?;
        let execution = host.get_or_open_execution(&pool).await?;
        assert!(Arc::ptr_eq(review, &execution.review_integration()));
        let workspace_pool = execution.pool();
        let existing_workspace = workspace_pool.acquire("linked-existing").await?;
        let existing_workspace_agent = existing_workspace.agent();
        drop(existing_workspace);
        let rule = "EXPLICIT_AND_LINKED_COMMON_RULE";
        echo_agent::memory::TypedMemoryStore::new(memory_store)
            .put_typed(
                echo_agent::evolution::layer::WARM_NAMESPACE,
                "explicit-project-rule",
                rule,
                echo_agent::memory::MemoryMeta::new(
                    echo_agent::memory::MemoryType::ProjectFact,
                    echo_agent::memory::MemorySource::ExplicitSave,
                    "explicit-project-regression",
                )
                .with_confidence(0.99),
            )
            .await?;
        review
            .promote_rule(&crate::evolution::rule_promoter::RuleProposal {
                memory_key: "explicit-project-rule".to_string(),
                namespace: echo_agent::evolution::layer::WARM_NAMESPACE
                    .iter()
                    .map(|part| (*part).to_string())
                    .collect(),
                rule_text: format!("- **Project fact**: {rule}"),
                confidence: 0.99,
                memory_type: echo_agent::memory::MemoryType::ProjectFact,
                proposed_at: chrono::Utc::now(),
                reason: "one memory owner must retain each execution project's context".to_string(),
            })
            .await?;
        let future_seed = pool.acquire("explicit-future").await?;
        let future_seed_agent = future_seed.agent();
        drop(future_seed);
        let future_workspace = workspace_pool.acquire("linked-future").await?;
        let future_workspace_agent = future_workspace.agent();
        drop(future_workspace);
        let seed_prompts = [
            projected_test_instructions(&runtime.agent_handle).await,
            projected_test_instructions(&existing_seed_agent).await,
            projected_test_instructions(&future_seed_agent).await,
        ];
        let workspace_prompts = [
            projected_test_instructions(&execution.primary_agent()).await,
            projected_test_instructions(&existing_workspace_agent).await,
            projected_test_instructions(&future_workspace_agent).await,
        ];
        hosts.shutdown().await?;
        let mut lifecycle = runtime.lifecycle_owner(tokio_util::sync::CancellationToken::new())?;
        lifecycle.bind_pool(pool);
        lifecycle
            .settle(ApplicationLifecycleReason::Shutdown, None)
            .await
            .into_result()?;
        assert!(
            prompt.contains("EXPLICIT_PROJECT_Q_INSTRUCTION"),
            "bootstrap lost explicit project Q instructions; linked project P loaded: {}",
            prompt.contains("LINKED_PROJECT_P_INSTRUCTION")
        );
        assert!(
            !prompt.contains("LINKED_PROJECT_P_INSTRUCTION"),
            "bootstrap installed another target's linked-project instructions"
        );
        for text in seed_prompts {
            assert!(
                text.contains("EXPLICIT_PROJECT_Q_INSTRUCTION"),
                "seed lost explicit project context"
            );
            assert!(
                !text.contains("LINKED_PROJECT_P_INSTRUCTION"),
                "seed acquired Host project instructions"
            );
            assert!(text.contains(rule), "seed missed shared rule promotion");
        }
        for text in workspace_prompts {
            assert!(
                text.contains("LINKED_PROJECT_P_INSTRUCTION"),
                "Host lost linked project context"
            );
            assert!(
                !text.contains("EXPLICIT_PROJECT_Q_INSTRUCTION"),
                "Host acquired seed project instructions"
            );
            assert!(text.contains(rule), "Host missed shared rule promotion");
        }
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_recovers_workspace_review_before_publishing_runtime() -> anyhow::Result<()> {
        use crate::evolution::background_review_owner::{ReviewReceiptStore, ReviewTerminal};
        use crate::workspace::{
            WorkspaceKind, layout::WorkspaceLayout, registry::WorkspaceRegistry,
        };

        if run_isolated_bootstrap_test(
            "runtime::tests::bootstrap_recovers_workspace_review_before_publishing_runtime",
        )? {
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))?;
        let workspace = registry.create_at(
            "restart-workspace",
            WorkspaceKind::General,
            temp.path().join("workspace"),
        )?;
        let state_dir = WorkspaceLayout::state_dir(&workspace.root);
        let receipts = ReviewReceiptStore::open(&state_dir).map_err(anyhow::Error::msg)?;
        receipts
            .admit(
                "workspace-before-exit".to_string(),
                echo_agent::evolution::ReviewIdentity::for_run("workspace-review"),
                workspace.id.as_str(),
                &workspace.opaque_product_data_generation(),
                None,
            )
            .map_err(anyhow::Error::msg)?;
        drop(receipts);

        let runtime = bootstrap_review_fixture(Some(workspace.root.clone()), temp.path()).await?;
        let receipt = ReviewReceiptStore::open(&state_dir)
            .map_err(anyhow::Error::msg)?
            .receipt("workspace-before-exit")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("workspace recovery lost the admitted operation"))?;
        assert_eq!(receipt.authority_scope, workspace.id.as_str());
        assert!(matches!(
            receipt.terminal,
            Some(ReviewTerminal::Interrupted { .. })
        ));
        let services = ApplicationServices::compose(
            &runtime,
            None,
            None,
            echo_agent::tools::permission::PermissionMode::Default,
        )
        .await?;
        let host = services
            .app_state
            .workspace
            .runtimes
            .get_or_open(workspace.clone())
            .await?;
        let execution = host.get_or_open_execution(&services.pool).await?;
        let bootstrap_review = runtime
            .review_integration
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bootstrap Review owner is missing"))?;
        assert!(Arc::ptr_eq(
            bootstrap_review,
            &execution.review_integration()
        ));
        assert!(
            services
                .app_state
                .review_integration
                .as_ref()
                .is_some_and(|review| Arc::ptr_eq(bootstrap_review, review))
        );
        let primary_store = runtime
            .agent_handle
            .read(|agent| agent.store().cloned())
            .await
            .ok_or_else(|| anyhow::anyhow!("bootstrap memory store is missing"))?;
        let workspace_store = execution
            .primary_agent()
            .read(|agent| agent.store().cloned())
            .await
            .ok_or_else(|| anyhow::anyhow!("workspace memory store is missing"))?;
        assert!(Arc::ptr_eq(&primary_store, &workspace_store));
        services
            .settle(ApplicationLifecycleReason::Shutdown, None)
            .await
            .into_result()?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_recovers_project_review_with_global_identity() -> anyhow::Result<()> {
        use crate::evolution::background_review_owner::{ReviewReceiptStore, ReviewTerminal};
        use crate::workspace::layout::WorkspaceLayout;

        if run_isolated_bootstrap_test(
            "runtime::tests::bootstrap_recovers_project_review_with_global_identity",
        )? {
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let root = temp.path().join("plain-project");
        std::fs::create_dir_all(&root)?;
        let state_dir = WorkspaceLayout::state_dir(&root);
        let receipts = ReviewReceiptStore::open(&state_dir).map_err(anyhow::Error::msg)?;
        receipts
            .admit(
                "global-before-exit".to_string(),
                echo_agent::evolution::ReviewIdentity::for_run("global-review"),
                "global",
                "global",
                None,
            )
            .map_err(anyhow::Error::msg)?;
        drop(receipts);
        let runtime = bootstrap_review_fixture(Some(root), temp.path()).await?;
        let receipt = ReviewReceiptStore::open(&state_dir)
            .map_err(anyhow::Error::msg)?
            .receipt("global-before-exit")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("global recovery lost the admitted operation"))?;
        assert_eq!(receipt.authority_scope, "global");
        assert_eq!(receipt.workspace_generation, "global");
        assert!(matches!(
            receipt.terminal,
            Some(ReviewTerminal::Interrupted { .. })
        ));
        runtime
            .lifecycle_owner(tokio_util::sync::CancellationToken::new())?
            .settle(ApplicationLifecycleReason::Shutdown, None)
            .await
            .into_result()?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_refuses_foreign_workspace_review_without_publishing_runtime()
    -> anyhow::Result<()> {
        use crate::evolution::background_review_owner::ReviewReceiptStore;
        use crate::workspace::{
            WorkspaceKind, layout::WorkspaceLayout, registry::WorkspaceRegistry,
        };

        if run_isolated_bootstrap_test(
            "runtime::tests::bootstrap_refuses_foreign_workspace_review_without_publishing_runtime",
        )? {
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))?;
        let workspace = registry.create_at(
            "foreign-review",
            WorkspaceKind::General,
            temp.path().join("workspace"),
        )?;
        let state_dir = WorkspaceLayout::state_dir(&workspace.root);
        let receipts = ReviewReceiptStore::open(&state_dir).map_err(anyhow::Error::msg)?;
        receipts
            .admit(
                "foreign-before-exit".to_string(),
                echo_agent::evolution::ReviewIdentity::for_run("foreign-review"),
                workspace.id.as_str(),
                "foreign-generation",
                None,
            )
            .map_err(anyhow::Error::msg)?;
        let error = match bootstrap_review_fixture(Some(workspace.root), temp.path()).await {
            Ok(runtime) => {
                runtime
                    .lifecycle_owner(tokio_util::sync::CancellationToken::new())?
                    .settle(ApplicationLifecycleReason::Shutdown, None)
                    .await
                    .into_result()?;
                anyhow::bail!("bootstrap published an Agent with foreign recovery debt");
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("different workspace generation"));
        assert!(
            receipts
                .receipt("foreign-before-exit")
                .map_err(anyhow::Error::msg)?
                .is_some_and(|receipt| receipt.terminal.is_none())
        );
        Ok(())
    }

    #[test]
    fn bootstrap_cwd_recovers_workspace_and_global_review_owners() -> anyhow::Result<()> {
        use crate::evolution::background_review_owner::{ReviewReceiptStore, ReviewTerminal};
        use crate::workspace::{
            WorkspaceKind, layout::WorkspaceLayout, registry::WorkspaceRegistry,
        };

        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))?;
        let workspace = registry.create_at(
            "cwd-review",
            WorkspaceKind::General,
            temp.path().join("workspace"),
        )?;
        let state_dir = WorkspaceLayout::state_dir(&workspace.root);
        let receipts = ReviewReceiptStore::open(&state_dir).map_err(anyhow::Error::msg)?;
        receipts
            .admit(
                "cwd-before-exit".to_string(),
                echo_agent::evolution::ReviewIdentity::for_run("cwd-review"),
                workspace.id.as_str(),
                &workspace.opaque_product_data_generation(),
                None,
            )
            .map_err(anyhow::Error::msg)?;
        drop(receipts);

        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "runtime::tests::bootstrap_workspace_cwd_child",
                "--nocapture",
            ])
            .current_dir(&workspace.root)
            .env("EKO_BOOTSTRAP_WORKSPACE_FIXTURE", temp.path())
            .env("EKO_DATA_DIR", temp.path().join("child-data"))
            .output()?;
        assert!(
            output.status.success(),
            "fresh-process bootstrap failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let receipt = ReviewReceiptStore::open(&state_dir)
            .map_err(anyhow::Error::msg)?
            .receipt("cwd-before-exit")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("fresh-process recovery lost its receipt"))?;
        assert_eq!(
            receipt.workspace_generation,
            workspace.opaque_product_data_generation()
        );
        assert!(matches!(
            receipt.terminal,
            Some(ReviewTerminal::Interrupted { .. })
        ));

        let global_cwd = temp.path().join("outside-project");
        std::fs::create_dir_all(&global_cwd)?;
        let global_root = temp.path().join("global-data");
        let receipts = ReviewReceiptStore::open(&global_root).map_err(anyhow::Error::msg)?;
        receipts
            .admit(
                "global-cwd-before-exit".to_string(),
                echo_agent::evolution::ReviewIdentity::for_run("global-cwd-review"),
                "global",
                "global",
                None,
            )
            .map_err(anyhow::Error::msg)?;
        drop(receipts);
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "runtime::tests::bootstrap_workspace_cwd_child",
                "--nocapture",
            ])
            .current_dir(&global_cwd)
            .env("EKO_BOOTSTRAP_WORKSPACE_FIXTURE", temp.path())
            .env("EKO_DATA_DIR", &global_root)
            .output()?;
        assert!(
            output.status.success(),
            "global-root bootstrap failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt = ReviewReceiptStore::open(&global_root)
            .map_err(anyhow::Error::msg)?
            .receipt("global-cwd-before-exit")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("global-root bootstrap lost its receipt"))?;
        assert_eq!(receipt.authority_scope, "global");
        assert_eq!(receipt.workspace_generation, "global");
        assert!(matches!(
            receipt.terminal,
            Some(ReviewTerminal::Interrupted { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_workspace_cwd_child() -> anyhow::Result<()> {
        let Some(fixture_dir) = std::env::var_os("EKO_BOOTSTRAP_WORKSPACE_FIXTURE") else {
            return Ok(());
        };
        let runtime = bootstrap_review_fixture(None, &PathBuf::from(fixture_dir)).await?;
        runtime
            .lifecycle_owner(tokio_util::sync::CancellationToken::new())?
            .settle(ApplicationLifecycleReason::Shutdown, None)
            .await
            .into_result()?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_rollback_settles_command_flow_before_product_data_join() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
            temp.path().join("events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?);
        let command_cells =
            crate::tasks::task_runtime::command_cells::CommandCellRuntimeService::new(
                Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
                chat_events,
                product_data_io.clone(),
            )
            .map_err(anyhow::Error::msg)?;
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.bind_product_data_io(product_data_io);
        owner.bind_command_cell_runtime(command_cells);
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            owner.settle(
                ApplicationLifecycleReason::BootstrapRollback,
                Some(anyhow::anyhow!("injected recovery failure")),
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("rollback waited for its own command-cell flow"))?;
        assert_eq!(
            receipt.primary_error.as_deref(),
            Some("injected recovery failure")
        );
        assert!(
            receipt.failures.is_empty(),
            "rollback left cleanup failures: {receipt}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_broadcasts_root_cancel_before_joining_background_tasks() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let task_cancel = cancel.clone();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_observed = Arc::clone(&observed);
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            task_observed.store(true, std::sync::atomic::Ordering::Release);
        });
        let mut owner = ApplicationLifecycleOwner::new(cancel);
        owner.track_background_task("cancellation observer", task);

        let receipt = owner
            .settle(ApplicationLifecycleReason::Shutdown, None)
            .await;

        assert!(receipt.is_clean(), "unexpected receipt: {receipt}");
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn lifecycle_receipt_aggregates_primary_and_join_failures() {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.track_background_task("aborted fixture", task);

        let receipt = owner
            .settle(
                ApplicationLifecycleReason::BootstrapRollback,
                Some(anyhow::anyhow!("injected bootstrap failure")),
            )
            .await;

        assert_eq!(
            receipt.primary_error.as_deref(),
            Some("injected bootstrap failure")
        );
        assert_eq!(receipt.failures.len(), 1);
        assert_eq!(
            receipt
                .failures
                .first()
                .map(|failure| failure.owner.as_str()),
            Some("aborted fixture")
        );
        assert!(receipt.into_result().is_err());
    }

    #[test]
    fn lifecycle_receipt_aggregates_analysis_cleanup_failures() {
        use crate::product_data_io::{AnalysisCancelReceipt, AnalysisRunReceipt};

        let run = |owner_id: &str| AnalysisRunReceipt {
            workspace_id: "workspace-a".to_string(),
            workspace_generation: "generation-a".to_string(),
            analysis_id: "analysis-a".to_string(),
            owner_id: owner_id.to_string(),
        };
        let mut receipt =
            ApplicationLifecycleReceipt::new(ApplicationLifecycleReason::Shutdown, None);
        record_analysis_cleanup_outcomes(
            &mut receipt,
            vec![
                AnalysisCancelReceipt::Joined {
                    receipt: run("joined"),
                    execution_error: Some("cancelled as requested".to_string()),
                },
                AnalysisCancelReceipt::CleanupTimedOut {
                    receipt: run("timed-out"),
                    timeout_seconds: 30,
                },
                AnalysisCancelReceipt::CleanupFailed {
                    receipt: run("failed"),
                    error: "join failed".to_string(),
                },
            ],
        );

        assert_eq!(receipt.failures.len(), 2);
        assert_eq!(
            receipt
                .failures
                .first()
                .map(|failure| failure.owner.as_str()),
            Some("analysis run timed-out")
        );
        assert!(
            receipt
                .failures
                .first()
                .is_some_and(|failure| failure.error.contains("30 seconds"))
        );
        assert_eq!(
            receipt
                .failures
                .get(1)
                .map(|failure| failure.owner.as_str()),
            Some("analysis run failed")
        );
        assert_eq!(
            receipt
                .failures
                .get(1)
                .map(|failure| failure.error.as_str()),
            Some("join failed")
        );
    }

    #[tokio::test]
    async fn lifecycle_begin_closes_external_admission_before_join() {
        let begun = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let begin_observed = Arc::clone(&begun);
        let join_observed = Arc::clone(&begun);
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.track_external_owner(
            "surface fixture",
            move || {
                begin_observed.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            },
            async move {
                if join_observed.load(std::sync::atomic::Ordering::Acquire) {
                    Ok(())
                } else {
                    Err("join started before admission closed".to_string())
                }
            },
        );

        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        assert!(begun.load(std::sync::atomic::Ordering::Acquire));
        let receipt = owner.join(receipt).await;
        assert!(receipt.is_clean(), "unexpected receipt: {receipt}");
    }

    #[tokio::test]
    async fn lifecycle_joins_extension_settlement_before_specialist_teardown() -> Result<(), String>
    {
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let extension_settlement = product_data_io
            .begin_owned_flow("extension settlement fixture")
            .map_err(|error| error.to_string())?;
        let (teardown_started_tx, mut teardown_started_rx) = tokio::sync::oneshot::channel();
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.bind_product_data_io(product_data_io.clone());
        owner.install_specialist_teardown_probe(teardown_started_tx);

        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        if product_data_io
            .begin_owned_flow("late extension settlement fixture")
            .is_ok()
        {
            return Err("phase one left extension settlement admission open".to_string());
        }
        let settlement = owner.start_join(receipt);

        if tokio::time::timeout(
            std::time::Duration::from_millis(50),
            &mut teardown_started_rx,
        )
        .await
        .is_ok()
        {
            return Err(
                "specialist teardown started before accepted extension settlement completed"
                    .to_string(),
            );
        }

        extension_settlement.settle(None);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut teardown_started_rx)
            .await
            .map_err(|_| {
                "specialist teardown did not start after extension settlement".to_string()
            })?
            .map_err(|_| "specialist teardown probe closed without a signal".to_string())?;
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(1), settlement.wait())
            .await
            .map_err(|_| "application lifecycle settlement timed out".to_string())?;
        if !receipt.is_clean() {
            return Err(format!("unexpected lifecycle receipt: {receipt}"));
        }
        Ok(())
    }

    struct RootDropProof(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for RootDropProof {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for RootDropProof {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                drop(self);
            })
        }
    }

    async fn runtime_services_for_retention_test()
    -> anyhow::Result<(tempfile::TempDir, AgentRuntime, ApplicationServices)> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project)?;
        let mut config = EkoConfig::default();
        config.model.default_model_id = Some("fixture:test-model".to_string());
        config.model_providers.insert(
            "fixture".to_string(),
            crate::config::ModelProviderConfig {
                base_url: Some("http://127.0.0.1:9/v1".to_string()),
                requires_api_key: false,
                ..Default::default()
            },
        );
        config
            .configured_models
            .push(crate::config::ConfiguredModel {
                id: "fixture:test-model".to_string(),
                provider: "fixture".to_string(),
                model: "test-model".to_string(),
                ..Default::default()
            });
        let browser = crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig {
            enabled: false,
            extension_enabled: false,
            session_dir: temp.path().join("browser"),
            ..Default::default()
        })
        .await;
        let runtime = AgentRuntime::bootstrap(
            &config,
            AgentCreateParams {
                working_dir: Some(project),
                browser_runtime: Some(browser),
                ..Default::default()
            },
            temp.path().join("missing-mcp.json"),
        )
        .await?;
        let services = ApplicationServices::compose(
            &runtime,
            None,
            None,
            echo_agent::tools::permission::PermissionMode::Default,
        )
        .await?;
        Ok((temp, runtime, services))
    }

    #[tokio::test]
    async fn aggregate_scheduler_unknown_preserves_original_slot_on_reobservation()
    -> anyhow::Result<()> {
        const NAME: &str =
            "runtime::tests::aggregate_scheduler_unknown_preserves_original_slot_on_reobservation";
        if run_isolated_bootstrap_test(NAME)? {
            return Ok(());
        }
        use crate::state::scheduler_shutdown_tests::SchedulerFixture;
        use std::sync::atomic::Ordering;

        let directory = tempfile::tempdir()?;
        let mut scheduler = SchedulerFixture::new(true).await?;
        let original = scheduler.original_result().await?;
        let primary = AgentHandle::new(
            echo_agent::prelude::ReactAgentBuilder::new()
                .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                .build()?,
        );
        let weak_agent = Arc::downgrade(primary.inner());
        let io = crate::product_data_io::ProductDataIoService::new();
        let mut state = AppState::from_shared(
            primary.clone(),
            None,
            Arc::new(HitlDispatcher::new()),
            None,
            None,
            EkoConfig::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                directory.path().join("mcp.json"),
                Default::default(),
            )),
            io.clone(),
        )?;
        state.scheduler = scheduler.take_scheduler()?;
        let state = Arc::new(state);
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.bind_app_state(state.clone());
        owner.bind_primary_agent(primary);
        owner.bind_product_data_io(io.clone());
        let root = owner
            .retention
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("original Root absent"))?;
        let normal = io.begin_owned_flow("scheduler-independent normal future")?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let normal_task = tokio::spawn(async move {
            let result = normal
                .run_async("held original normal operation", async move {
                    let _ = entered_tx.send(());
                    release_rx.await.map_err(|error| error.to_string())
                })
                .await;
            normal.settle(result.as_ref().err().map(ToString::to_string));
            result
        });
        entered_rx.await?;
        let initial = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        let settlement = owner.start_join(initial);
        let mut observer = Box::pin(settlement.clone().wait());
        let normal_was_pending = futures::poll!(observer.as_mut()).is_pending();
        // Always release and join the same ordinary source before reporting a
        // regression, including on the original false-final implementation.
        release_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("ordinary source vanished"))?;
        let normal_result = normal_task.await??;
        normal_result.map_err(anyhow::Error::msg)?;
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), observer).await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if !root
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .polling
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let first_retained = first.continuation.is_some();
        let owner_normal_drained = root
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .owner
            .as_ref()
            .is_some_and(|(owner, _)| owner.normal_drained);
        let first_final = root
            .publications
            .borrow()
            .as_ref()
            .is_some_and(|p| p.final_receipt);
        let has_scheduler_diagnostic = first
            .failures
            .iter()
            .any(|failure| failure.owner == "scheduler");
        let mut second_retained = false;
        if first_retained {
            let mut publication = root.publications.subscribe();
            publication.borrow_and_update();
            root.note_progress();
            tokio::time::timeout(std::time::Duration::from_secs(3), publication.changed())
                .await??;
            second_retained = publication
                .borrow()
                .as_ref()
                .is_some_and(|p| !p.final_receipt);
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if !root
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .polling
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await?;
        }
        let callback_held = scheduler.capacity.available_permits() == 0
            && scheduler.drops.load(Ordering::SeqCst) == 0;
        let agent_reachable = weak_agent.upgrade().is_some();
        let calls = scheduler.calls.load(Ordering::SeqCst);
        let identity = scheduler
            .identity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original identity absent"))?;
        // This joins the actual test-owned backend only. No canonical
        // occurrence completion is injected and Unknown is never turned Ok.
        scheduler.finish_backend().await?;
        drop(first);
        drop(settlement);
        drop(root);
        drop(state);
        let final_drops = scheduler.drops.load(Ordering::SeqCst);
        println!(
            "scheduler aggregate: ordinary_pending={normal_was_pending} first_retained={first_retained} first_final={first_final} normal_drained={owner_normal_drained} second_retained={second_retained} callback_held={callback_held} agent_reachable={agent_reachable} calls={calls} final_drops={final_drops}"
        );
        assert!(
            matches!(original, Err(echo_agent::error::ReactError::ExecutionNotSettled { run_id, execution_id: Some(execution_id) }) if run_id == identity.0 && execution_id == identity.1)
        );
        assert!(normal_was_pending);
        assert!(has_scheduler_diagnostic);
        assert!(
            first_retained && !first_final && owner_normal_drained,
            "scheduler Unknown escaped the original lifecycle retention decision"
        );
        assert!(
            second_retained,
            "normal_drained skipped the original scheduler on the next poll"
        );
        assert!(callback_held && agent_reachable);
        assert_eq!(calls, 1);
        assert_eq!(final_drops, 1);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_no_runtime_observation_keeps_single_flight_until_publication()
    -> anyhow::Result<()> {
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        let root = owner
            .initial_retention
            .take()
            .ok_or_else(|| anyhow::anyhow!("original root missing"))?;
        owner.armed = false;
        root.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .owner = Some((owner, receipt));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *root
            .before_no_runtime_publication
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Box::new(move || {
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
        }));
        let foreign = root.clone();
        let thread = std::thread::spawn(move || foreign.kick());
        entered_rx.await?;
        root.note_progress();
        tokio::task::yield_now().await;
        assert!(
            root.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .polling
        );
        assert!(root.publications.borrow().is_none());
        release_tx.send(())?;
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("test owner thread panicked"))?;
        root.kick();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !root
                .publications
                .borrow()
                .as_ref()
                .is_some_and(|published| published.final_receipt)
            {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(
            root.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .owner
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_foreign_join_unwind_retains_explicit_bundle_and_drains_later_owner()
    -> anyhow::Result<()> {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let original = RootDropProof(dropped.clone());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completion = completed.clone();
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        let store = Arc::new(crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()?);
        owner.bind_task_runtime(store.clone());
        store.create_run(
            "panic-original",
            "global",
            "conversation",
            "message",
            crate::tasks::task_runtime::DomainProfile::General,
            "goal",
            "task",
            crate::tasks::task_runtime::AttendedMode::Attended,
        )?;
        store.transition_run(
            "panic-original",
            crate::tasks::task_runtime::TaskRunStatus::Running,
        )?;
        let client =
            Arc::new(echo_agent::testing::MockLlmClient::new().with_model_name("test-model"));
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("test-model")
                .llm_client(client.clone())
                .build()?,
        );
        let weak_agent = Arc::downgrade(agent.inner());
        let pool =
            Arc::new(crate::agent_pool::AgentPool::new_for_test(agent, None, None, 2, false).await);
        pool.set_llm_client_override_for_test(client).await;
        owner.bind_pool(pool.clone());
        let execution = pool.acquire("panic-original").await?;
        let explicit_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let explicit_proof = explicit_dropped.clone();
        let admission = store.reserve_run_driver_admission(
            "panic-original".into(),
            echo_agent::agent::CancellationToken::new(),
        )?;
        let registered = store.register_observed_run_driver::<()>(
            admission,
            store.lease_active_workspace_generation()?,
        )?;
        let observed = registered
            .start(move |mut resources| async move {
                resources.retain(RootDropProof(explicit_proof))?;
                resources.retain(execution)?;
                Err(crate::chat_driver::ChatTurnError::Observation {
                    run_id: "panic-original".into(),
                    execution_id: None,
                })
            })
            .await?;
        assert!(matches!(
            observed,
            Err(crate::chat_driver::ChatTurnError::Observation { .. })
        ));
        owner.track_external_owner(
            "foreign original",
            || Ok(()),
            futures::future::poll_fn(move |_| {
                // This specific poll_fn fixture retains its own capture. The
                // framework makes no such guarantee for arbitrary async unwinding.
                std::hint::black_box(&original);
                std::panic::resume_unwind(Box::new("actual foreign poll unwind"))
            }),
        );
        owner.track_external_owner("independent actual owner", || Ok(()), async move {
            completion.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        });
        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        let settlement = owner.start_join(receipt);
        let receipt =
            tokio::time::timeout(std::time::Duration::from_secs(2), settlement.clone().wait())
                .await?;
        assert!(!receipt.is_clean());
        assert!(receipt.continuation.is_some());
        assert!(!explicit_dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(pool.active_execution_count(), 1);
        assert!(weak_agent.upgrade().is_some());
        assert_eq!(
            store
                .active_run_driver_receipt_count()
                .map_err(anyhow::Error::msg)?,
            2
        );
        assert!(completed.load(std::sync::atomic::Ordering::Acquire));
        assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));
        let repeated =
            tokio::time::timeout(std::time::Duration::from_secs(2), settlement.clone().wait())
                .await?;
        assert!(repeated.continuation.is_some());
        assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));
        // No matching proof exists: only dropping ALL test-owned roots releases
        // captures; this is not a physical-completion or recovery assertion.
        drop(receipt);
        drop(repeated);
        drop(settlement);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_analysis_final_publication_resumes_original_root_without_waiter_retry()
    -> anyhow::Result<()> {
        const NAME: &str = "runtime::tests::aggregate_analysis_final_publication_resumes_original_root_without_waiter_retry";
        if run_isolated_bootstrap_test(NAME)? {
            return Ok(());
        }
        let (_temp, runtime, mut services) = runtime_services_for_retention_test().await?;
        let analysis = services.app_state.session.analysis_runs.clone();
        let data = services
            .app_state
            .product_data_for_scope("global", "global")
            .await?;
        let (runner_release, runner_held) = tokio::sync::oneshot::channel();
        let (at_publication, publication_held) = tokio::sync::oneshot::channel();
        let (publish, publication_release) = tokio::sync::oneshot::channel();
        let _original = data.start_analysis_monitor_barrier_fixture(
            "original-monitor",
            runner_held,
            Box::pin(async move {
                let _ = at_publication.send(());
                let _ = publication_release.await;
            }),
        )?;
        runner_release
            .send(())
            .map_err(|_| anyhow::anyhow!("original runner vanished"))?;
        publication_held.await?;
        let initial = services.begin_shutdown(ApplicationLifecycleReason::Shutdown, None)?;
        // Exercise the original 30-second monitor drain budget, unchanged.
        let observation =
            tokio::time::timeout(std::time::Duration::from_secs(35), services.join(initial))
                .await?;
        let root = observation.continuation.as_ref().ok_or_else(|| {
            anyhow::anyhow!("root did not retain unfinished analysis publication")
        })?;
        assert!(
            runtime
                .lifecycle_retention
                .publications
                .borrow()
                .as_ref()
                .is_some_and(|published| !published.final_receipt)
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while root
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .polling
            {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        publish
            .send(())
            .map_err(|_| anyhow::anyhow!("original monitor vanished"))?;
        // Observe the original publication only. Do not call Settlement.wait(),
        // kick() or note_progress() to hide a missing post-commit wakeup.
        let mut final_publication = root.publications.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if final_publication
                    .borrow()
                    .as_ref()
                    .is_some_and(|publication| publication.final_receipt)
                {
                    break;
                }
                final_publication
                    .changed()
                    .await
                    .map_err(|error| anyhow::anyhow!(error))?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await??;
        assert!(!analysis.has_retained_owners());
        drop(data);
        drop(observation);
        drop(runtime);
        Ok(())
    }

    #[tokio::test]
    async fn typed_shutdown_error_alone_retains_original_root_until_matching_backend_completion()
    -> anyhow::Result<()> {
        const NAME: &str = "runtime::tests::typed_shutdown_error_alone_retains_original_root_until_matching_backend_completion";
        if run_isolated_bootstrap_test(NAME)? {
            return Ok(());
        }
        let (temp, runtime, mut services) = runtime_services_for_retention_test().await?;
        let store = services
            .app_state
            .tasks
            .runtime
            .clone()
            .ok_or_else(|| anyhow::anyhow!("store absent"))?;
        let weak_root = Arc::downgrade(&runtime.lifecycle_retention);
        let weak_state = Arc::downgrade(&services.app_state);
        let weak_analysis = Arc::downgrade(&services.app_state.session.analysis_runs);
        let weak_agent = Arc::downgrade(runtime.agent_handle.inner());
        store.create_run(
            "root-unknown",
            "global",
            "conversation",
            "message",
            crate::tasks::task_runtime::DomainProfile::General,
            "goal",
            "task",
            crate::tasks::task_runtime::AttendedMode::Attended,
        )?;
        store.transition_run(
            "root-unknown",
            crate::tasks::task_runtime::TaskRunStatus::Running,
        )?;
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scope_tx, scope_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend_effects = effects.clone();
        let backend = tokio::spawn(async move {
            let owner = scope_rx.await?;
            finish_rx.await?;
            backend_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<String, tokio::sync::oneshot::error::RecvError>(owner)
        });
        let admission = store.reserve_run_driver_admission(
            "root-unknown".to_string(),
            echo_agent::agent::CancellationToken::new(),
        )?;
        let registration = store.register_observed_run_driver::<()>(
            admission,
            store.lease_active_workspace_generation()?,
        )?;
        let proof = dropped.clone();
        let observed = registration
            .start(move |mut owner| async move {
                owner.retain(RootDropProof(proof))?;
                let _ = scope_tx.send(owner.execution_context_id());
                Err(crate::chat_driver::ChatTurnError::Observation {
                    run_id: "root-unknown".to_string(),
                    execution_id: None,
                })
            })
            .await?;
        assert!(
            matches!(observed, Err(crate::chat_driver::ChatTurnError::Observation { ref run_id, .. }) if run_id == "root-unknown")
        );
        let normal = runtime
            .product_data_io
            .begin_owned_flow("independent actual normal write")?;
        let (normal_entered, entered) = tokio::sync::oneshot::channel();
        let (normal_release, normal_hold) = tokio::sync::oneshot::channel();
        let output = temp.path().join("normal-output");
        let output_task = output.clone();
        let normal_task = tokio::spawn(async move {
            let result = normal
                .run_async("write original normal output", async move {
                    let _ = normal_entered.send(());
                    normal_hold.await.map_err(|error| error.to_string())?;
                    tokio::fs::write(output_task, "normal 中文🙂")
                        .await
                        .map_err(|error| error.to_string())
                })
                .await;
            normal.settle(result.as_ref().err().map(ToString::to_string));
            result
        });
        entered.await?;
        let initial = services.begin_shutdown(ApplicationLifecycleReason::Shutdown, None)?;
        let receipt = {
            let join = services.join(initial);
            tokio::pin!(join);
            assert!(futures::poll!(join.as_mut()).is_pending());
            normal_release
                .send(())
                .map_err(|_| anyhow::anyhow!("normal producer vanished"))?;
            let completed_normal_write = normal_task.await??;
            completed_normal_write.map_err(anyhow::Error::msg)?;
            assert_eq!(tokio::fs::read_to_string(output).await?, "normal 中文🙂");
            tokio::time::timeout(std::time::Duration::from_secs(3), join).await?
        };
        let error = anyhow::Error::new(receipt.into_error());
        drop(runtime);
        let lifecycle = error
            .downcast_ref::<ApplicationLifecycleError>()
            .ok_or_else(|| anyhow::anyhow!("typed lifecycle error lost"))?;
        let root = lifecycle
            .receipt
            .continuation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("typed error lost original root"))?;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while Arc::strong_count(root) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(weak_state.upgrade().is_some());
        assert!(weak_analysis.upgrade().is_some());
        assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(store.begin_workspace_transition().await.is_err());
        assert!(
            store
                .reserve_run_driver_admission(
                    "root-unknown".to_string(),
                    echo_agent::agent::CancellationToken::new()
                )
                .is_err()
        );
        assert_eq!(
            store.get_run("root-unknown")?.map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Running)
        );
        {
            let original_agent = weak_agent
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("original Agent was destroyed"))?;
            assert!(
                original_agent
                    .read()
                    .await
                    .tool_names()
                    .iter()
                    .any(|name| name == "task_create")
            );
        }
        let observer = ApplicationLifecycleSettlement {
            result: root.publications.subscribe(),
            fallback: lifecycle.receipt.clone(),
            retention: root.clone(),
        };
        finish_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("original backend disappeared"))?;
        let owner = backend.await??;
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
        store
            .complete_joined_owner_for_test("root-unknown", &owner)
            .await?;
        let _receipt =
            tokio::time::timeout(std::time::Duration::from_secs(3), observer.wait()).await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let final_published = root
                    .publications
                    .borrow()
                    .as_ref()
                    .is_some_and(|publication| publication.final_receipt);
                if final_published {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            store.get_run("root-unknown")?.map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Completed)
        );
        drop(store);
        drop(_receipt);
        drop(error);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while weak_root.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(weak_state.upgrade().is_none());
        assert!(weak_analysis.upgrade().is_none());
        assert!(weak_agent.upgrade().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dropping_join_waiter_does_not_abandon_owned_settlement() {
        let join_started = Arc::new(tokio::sync::Notify::new());
        let release_join = Arc::new(tokio::sync::Notify::new());
        let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let join_started_task = Arc::clone(&join_started);
        let release_join_task = Arc::clone(&release_join);
        let settled_task = Arc::clone(&settled);
        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.track_external_owner("parked settlement", || Ok(()), async move {
            join_started_task.notify_one();
            release_join_task.notified().await;
            settled_task.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        });
        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        let settlement = owner.start_join(receipt);
        let observer = settlement.clone();
        let waiter = tokio::spawn(settlement.wait());
        join_started.notified().await;
        waiter.abort();
        let _ = waiter.await;
        release_join.notify_one();

        let receipt = tokio::time::timeout(std::time::Duration::from_secs(1), observer.wait())
            .await
            .unwrap_or_else(|_| {
                ApplicationLifecycleReceipt::new(
                    ApplicationLifecycleReason::Shutdown,
                    Some(anyhow::anyhow!("owned settlement timed out")),
                )
            });
        assert!(receipt.is_clean(), "unexpected receipt: {receipt}");
        assert!(settled.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn application_shutdown_joins_blocking_operation_after_caller_abort() -> Result<(), String>
    {
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let operation = crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            operation
                .run_owned("application shutdown barrier", move || {
                    let _ = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            crate::tasks::task_runtime::StoreError::InvalidPlan(error.to_string())
                        })?;
                    Ok(())
                })
                .await
        });
        entered_rx
            .await
            .map_err(|_| "blocking operation did not enter".to_string())?;
        caller.abort();
        let _ = caller.await;

        let mut owner = ApplicationLifecycleOwner::new(tokio_util::sync::CancellationToken::new());
        owner.bind_task_runtime(store.clone());
        let receipt = owner.begin_shutdown(ApplicationLifecycleReason::Shutdown, None);
        let rejected = crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone())
            .run_owned("late application operation", || Ok(()))
            .await;
        if !rejected.is_err_and(|error| error.to_string().contains("admission is closed")) {
            return Err("application phase one did not close TaskRuntime operations".to_string());
        }
        let settlement = owner.start_join(receipt);
        let waiter = tokio::spawn(settlement.wait());
        tokio::task::yield_now().await;
        if waiter.is_finished() {
            return Err("application shutdown crossed an active TaskRuntime operation".to_string());
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release blocking operation: {error}"))?;
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .map_err(|_| "application settlement timed out".to_string())?
            .map_err(|error| format!("application settlement failed to join: {error}"))?;
        if !receipt.is_clean() {
            return Err(format!("unexpected lifecycle receipt: {receipt}"));
        }
        if store.active_operation_count() != 0 {
            return Err("TaskRuntime operation supervisor remained active".to_string());
        }
        Ok(())
    }

    fn make_test_classifier() -> KeywordClassifier {
        let mut c = KeywordClassifier::new();
        c.add_skill_keywords("coding", &["写代码", "编程", "调试", "debug", "实现"]);
        c.add_skill_keywords("paper-search", &["论文检索", "arxiv", "文献检索", "找论文"]);
        c.add_skill_keywords(
            "evidence-medicine",
            &["医学文献", "pubmed", "临床试验", "循证"],
        );
        c
    }

    #[test]
    fn test_classifier_routes_coding_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let intent =
            tokio::runtime::Runtime::new()?.block_on(c.classify("帮我写代码实现排序", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "coding"),
            "Should route to coding, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_routes_research_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("帮我搜索 arxiv 上的论文", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "paper-search"),
            "arxiv should match paper-search, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_routes_medical_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("搜索 pubmed 上关于骨质疏松的文献", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "evidence-medicine"),
            "PubMed should route to evidence-medicine, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_no_match_returns_fallback() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("今天天气怎么样", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::Fallback),
            "Weather should be Fallback, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_empty_returns_fallback() -> anyhow::Result<()> {
        let c = KeywordClassifier::new();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("帮我写代码", &[]));
        assert!(matches!(intent, echo_agent::intent::Intent::Fallback));
        Ok(())
    }

    #[test]
    fn test_classifier_word_boundary_no_false_positive() -> anyhow::Result<()> {
        let mut c = KeywordClassifier::new();
        c.add_skill_keywords("coding", &["bug"]);
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("I am debugging the code", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::Fallback),
            "'debugging' should not trigger 'bug', got {:?}",
            intent
        );
        let intent = rt.block_on(c.classify("there is a bug in my code", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { .. }),
            "Standalone 'bug' should trigger coding, got {:?}",
            intent
        );
        Ok(())
    }
}
