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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationLifecycleReceipt {
    pub reason: ApplicationLifecycleReason,
    pub primary_error: Option<String>,
    pub failures: Vec<ApplicationLifecycleFailure>,
}

impl ApplicationLifecycleReceipt {
    fn new(reason: ApplicationLifecycleReason, primary_error: Option<anyhow::Error>) -> Self {
        Self {
            reason,
            primary_error: primary_error.map(|error| error.to_string()),
            failures: Vec::new(),
        }
    }

    fn record(&mut self, owner: impl Into<String>, error: impl std::fmt::Display) {
        self.failures.push(ApplicationLifecycleFailure {
            owner: owner.into(),
            error: error.to_string(),
        });
    }

    pub fn is_clean(&self) -> bool {
        self.primary_error.is_none() && self.failures.is_empty()
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
}

#[derive(Clone)]
pub struct ApplicationLifecycleSettlement {
    result: tokio::sync::watch::Receiver<Option<ApplicationLifecycleReceipt>>,
    fallback: ApplicationLifecycleReceipt,
}

impl ApplicationLifecycleSettlement {
    pub async fn wait(mut self) -> ApplicationLifecycleReceipt {
        loop {
            if let Some(receipt) = self.result.borrow().clone() {
                return receipt;
            }
            if self.result.changed().await.is_err() {
                let mut fallback = self.fallback;
                fallback.record(
                    "application lifecycle",
                    "settlement owner ended before publishing its receipt",
                );
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
        Self {
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
        self.app_state = Some(state);
    }

    pub fn bind_primary_agent(&mut self, agent: AgentHandle) {
        self.primary_agent = Some(agent);
    }

    pub fn bind_pool(&mut self, pool: Arc<crate::agent_pool::AgentPool>) {
        self.pool = Some(pool);
    }

    pub fn bind_task_runtime(&mut self, store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>) {
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
        receipt
    }

    /// Start the state-owned settlement task. The task owns every resource and
    /// publishes through a shared receiver, so dropping one caller's wait future
    /// cannot abandon shutdown or its typed receipt.
    pub fn start_join(
        mut self,
        receipt: ApplicationLifecycleReceipt,
    ) -> ApplicationLifecycleSettlement {
        self.armed = false;
        let fallback = receipt.clone();
        let (result_tx, result) = tokio::sync::watch::channel(None);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let receipt = self.join_owned(receipt).await;
                    result_tx.send_replace(Some(receipt));
                });
            }
            Err(error) => {
                let mut receipt = receipt;
                receipt.record(
                    "application lifecycle",
                    format!("settlement requires a Tokio runtime: {error}"),
                );
                result_tx.send_replace(Some(receipt));
            }
        }
        ApplicationLifecycleSettlement { result, fallback }
    }

    /// Await all work accepted before [`Self::begin_shutdown`]. Earlier failures
    /// never suppress later joins; all of them are appended to one typed receipt.
    pub async fn join(self, receipt: ApplicationLifecycleReceipt) -> ApplicationLifecycleReceipt {
        self.start_join(receipt).wait().await
    }

    async fn join_owned(
        mut self,
        mut receipt: ApplicationLifecycleReceipt,
    ) -> ApplicationLifecycleReceipt {
        if !self.shutdown_begun {
            receipt.record(
                "application lifecycle",
                "join requested before shutdown admission closed",
            );
            let begin_receipt = self.begin_shutdown(receipt.reason, None);
            receipt.failures.extend(begin_receipt.failures);
        }

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
        for owner in std::mem::take(&mut self.external_owners) {
            if let Err(error) = owner.join.await {
                receipt.record(owner.name, error);
            }
        }
        if let Some(integration) = self.review_integration.as_ref()
            && let Err(error) = integration.shutdown_background_reviews().await
        {
            receipt.record("background reviews", error);
        }
        if let Some(state) = self.app_state.as_ref() {
            if let Err(error) = state.join_workspace_transition().await {
                receipt.record("workspace transition", error);
            }
            if let Err(error) = state.shutdown_scheduler().await {
                receipt.record("scheduler", error);
            }
        }

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
        if let Some(state) = self.app_state.as_ref()
            && let Err(error) = state.shutdown_command_cells().await
        {
            receipt.record("command cells", error);
        }
        // Product-data admission was sealed in phase one. Extension mutations
        // hold an owned flow while their durable commit and specialist fanout
        // settle, so this join must complete while the pool and specialist
        // runtimes they captured are still alive.
        if let Some(product_data_io) = self.product_data_io.as_ref()
            && let Err(error) = product_data_io.join_shutdown().await
        {
            receipt.record("product-data I/O", error);
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
        if let Some(state) = self.app_state.as_ref() {
            if let Err(error) = state.terminal.close_all().await {
                receipt.record("terminal sessions", error);
            }
        } else if let Some(runtime) = self.command_cell_runtime.as_ref()
            && let Err(error) = runtime.shutdown().await
        {
            receipt.record("command cells", error);
        }

        if let Some(runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = runtime.shutdown().await
        {
            receipt.record("plugin runtime", error);
        }
        if let Some(watcher) = self.config_watcher.as_ref()
            && let Err(error) = watcher.shutdown().await
        {
            receipt.record("config watcher", error);
        }
        if let Some(runtime) = self.mcp_config_runtime.as_ref() {
            runtime.shutdown().await;
        }
        if let Some(runtime) = self.browser_runtime.as_ref() {
            runtime.shutdown().await;
        }
        if let Some(agent) = self.primary_agent.as_ref()
            && let Err(error) = agent
                .read_async(|agent| Box::pin(async move { agent.close().await }))
                .await
        {
            receipt.record("primary Agent", error);
        }
        if let Some(service) = self.subagent_projection.as_ref()
            && let Err(error) = service.shutdown_and_join().await
        {
            receipt.record("Subagent event projection", error);
        }
        receipt
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
        self.root_cancel.cancel();
        for task in self.background_tasks.drain(..) {
            task.handle.abort();
        }
        if let Some(service) = self.subagent_projection.as_ref() {
            service.abort();
        }
        self.external_owners.clear();
    }
}

/// Shared agent runtime context.
///
/// Created once at application startup and lives for the entire process lifetime.
pub struct AgentRuntime {
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
        let mut lifecycle = runtime.lifecycle_owner(root_cancel.clone());
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
    ) -> ApplicationLifecycleOwner {
        let mut owner = ApplicationLifecycleOwner::new(root_cancel);
        owner.bind_primary_agent(self.agent_handle.clone());
        owner.bind_plugin_runtime(self.plugin_runtime.clone());
        owner.bind_mcp_config_runtime(self.mcp_config_runtime.clone());
        owner.bind_browser_runtime(self.browser_runtime.clone());
        owner.bind_product_data_io(self.product_data_io.clone());
        owner.bind_command_cell_runtime(self.command_cell_runtime.clone());
        if let Some(integration) = self.review_integration.as_ref() {
            owner.bind_review_integration(integration.clone());
        }
        owner
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
    /// 6. Load built-in skills
    /// 7. Load user hooks
    /// 8. Create hook bridges (task + subagent lifecycle)
    /// 9. Initialize unified memory
    /// 10. Load plugins (skills / hooks / MCP)
    /// 11. Register LSP tools
    /// 12. Fire startup hook
    pub async fn bootstrap(
        app_config: &EkoConfig,
        mut params: AgentCreateParams,
        mcp_config_path: PathBuf,
    ) -> anyhow::Result<Self> {
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

        // ── 5. Built-in skills ──
        // The durable enabled-skills file is the activation authority. All
        // bundled files remain discoverable in SkillsHub, but disabled entries
        // never register descriptors, hooks, or intent-routing candidates.
        {
            let builtin_skills_dir = crate::skills_hub::builtin_skills_root();
            let enabled_config_path = crate::data_root::user_data_path("enabled-skills.json");
            let active_policy = Arc::new(crate::skills_hub::ActiveSkillLoadPolicy::new(
                enabled_config_path,
                builtin_skills_dir.clone(),
                None,
            ));
            agent_handle
                .write(|agent| agent.set_skill_load_policy(Some(active_policy.clone())))
                .await;
            if builtin_skills_dir.is_dir() {
                agent_handle
                    .write_async(|a| {
                        Box::pin(async move {
                            match a.load_skills_from_dir(&builtin_skills_dir).await {
                                Ok(names) => {
                                    tracing::info!(count = names.len(), skills = ?names, "Built-in skills loaded");
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to load built-in skills: {e}");
                                }
                            }
                        })
                    })
                    .await;
            }
        }

        // ── 5b. Methodology baseline injection ──
        // The same authority is called by pooled conversation Agent creation.
        let baseline_config_path = crate::data_root::user_data_path("enabled-skills.json");
        let baseline_names = agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    crate::skills_hub::apply_methodology_baseline(agent, &baseline_config_path)
                        .await
                })
            })
            .await;
        tracing::info!(
            count = baseline_names.len(),
            skills = ?baseline_names,
            "Methodology baseline injected into primary Agent"
        );

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
        let review_integration = agent_handle
            .read(|a| a.store().cloned())
            .await
            .map(|store| {
                Arc::new(ReviewIntegration::new(
                    ReviewConfig::default(),
                    review_echo_agent_dir.clone(),
                    store,
                ))
            });
        if review_integration.is_some() {
            tracing::info!("ReviewIntegration created for session");
        }
        if let Some(review_integration) = &review_integration {
            bootstrap_lifecycle.bind_review_integration(review_integration.clone());
            review_integration.bind_rule_projection_primary(agent_handle.clone());
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
            let trigger_sink = review_integration.clone();
            let skill_policy = Arc::new(crate::skills_hub::ActiveSkillLoadPolicy::new(
                crate::data_root::user_data_path("enabled-skills.json"),
                crate::skills_hub::builtin_skills_root(),
                Some(review_integration.clone()),
            ));
            let skill_curator = review_integration.curator();
            let workspace_skills = review_echo_agent_dir.join("skills");
            agent_handle
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_layer_manager(layer_manager);
                        a.set_memory_trigger_sink(Some(trigger_sink));
                        a.set_skill_load_policy(Some(skill_policy));
                        a.set_skill_curator(Some(skill_curator));
                        let _ = a.reconcile_skill_load_policy().await;
                        if workspace_skills.is_dir()
                            && let Err(error) = a.load_skills_from_dir(workspace_skills).await
                        {
                            tracing::warn!(%error, "Failed to load workspace-curated skills");
                        }
                    })
                })
                .await;
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

        bootstrap_lifecycle.disarm();

        Ok(Self {
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
    lsp_manager.load_config(&config);
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
    use echo_agent::skills::external::{SkillLoader, tool_matcher};

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

    #[tokio::test]
    async fn bundled_skill_allowlists_match_registered_tool_names() -> anyhow::Result<()> {
        let agent = echo_agent::agent::ReactAgent::new(echo_agent::agent::AgentConfig::standard(
            "test-model",
            "skill-audit",
            "test",
        ));
        let mut tool_names = agent.tool_names();
        tool_names.extend(
            ["task_create", "task_update", "task_list", "task_execute"]
                .into_iter()
                .map(str::to_string),
        );

        let skill_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills");
        let mut loader = SkillLoader::new();
        let descriptors = loader.discover_directory(skill_root).await?;
        assert!(
            !descriptors.is_empty(),
            "bundled skills were not discovered"
        );

        for descriptor in descriptors {
            for matcher in descriptor.allowed_tools {
                assert!(
                    tool_names
                        .iter()
                        .any(|tool_name| tool_matcher(&matcher, tool_name)),
                    "Skill '{}' allowed-tools entry '{}' matches no registered tool",
                    descriptor.name,
                    matcher
                );
            }
        }
        Ok(())
    }
}
