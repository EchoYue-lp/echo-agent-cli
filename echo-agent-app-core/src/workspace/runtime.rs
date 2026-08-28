//! Workspace-scoped runtime hosts and their immutable file resources.
//!
//! Workspace roots, stores, and execution authorities are immutable per host.
//! `AppState` may change UI focus without rebinding or stopping another host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::memory::{ConversationStore, FileConversationStore, FileStore, Store};
use echo_agent::state::{FileRuntimeStateStore, RuntimeStateStore};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use tokio::sync::{Mutex, OnceCell, RwLock};

use super::layout::WorkspaceLayout;
use super::{Workspace, WorkspaceExecutionScope, WorkspaceId};
use crate::agent_pool::{AgentPool, WorkspaceAgentPoolResources};
use crate::conversation_deletion::ConversationDeletionService;
use crate::evolution::ReviewIntegration;
use crate::tasks::task_runtime::TaskRuntimeStore;

type WorkspaceShutdownSettlement = Shared<BoxFuture<'static, Result<(), String>>>;

/// One coherently prepared set of workspace-scoped runtime resources.
///
/// The roots and stores are immutable after construction. Runtime publication
/// consumes these resources through their owning [`WorkspaceRuntimeHost`].
pub(crate) struct WorkspaceRuntimeResources {
    workspace: Workspace,
    state_dir: PathBuf,
    tasks_dir: PathBuf,
    conversation_store: Arc<dyn ConversationStore>,
    runtime_state_store: Arc<dyn RuntimeStateStore>,
    memory_store: Arc<dyn Store>,
    deletion_service: Arc<ConversationDeletionService>,
}

/// Stable application-layer owner for one workspace's file resources.
///
/// The workspace ID and canonical root never change. Display metadata can be
/// refreshed after registry operations such as linking a project without
/// replacing the host or reopening its stores.
pub(crate) struct WorkspaceRuntimeHost {
    workspace: RwLock<Workspace>,
    resources: WorkspaceRuntimeResources,
    task_runtime: OnceCell<Arc<TaskRuntimeStore>>,
    execution: OnceCell<Arc<WorkspaceExecutionRuntime>>,
    control_lifecycle: std::sync::Mutex<WorkspaceControlLifecycle>,
    operation_stores: Arc<std::sync::Mutex<Vec<std::sync::Weak<TaskRuntimeStore>>>>,
    operation_admission_open: Arc<std::sync::atomic::AtomicBool>,
    shutdown_settlement: std::sync::Mutex<Option<WorkspaceShutdownSettlement>>,
    #[cfg(test)]
    shutdown_after_operations_barrier: std::sync::Mutex<Option<WorkspaceControlAcquireTestBarrier>>,
}

#[derive(Default)]
struct WorkspaceControlLifecycle {
    active: usize,
    closing: bool,
}

#[derive(Clone)]
pub(crate) struct WorkspaceControlLease {
    _receipt: Arc<WorkspaceControlReceipt>,
}

struct WorkspaceControlReceipt {
    host: Arc<WorkspaceRuntimeHost>,
}

struct WorkspaceClosingGuard {
    host: Arc<WorkspaceRuntimeHost>,
    active_controls: usize,
    committed: bool,
}

impl WorkspaceClosingGuard {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for WorkspaceClosingGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.host.abort_closing();
        }
    }
}

impl Drop for WorkspaceControlReceipt {
    fn drop(&mut self) {
        let mut lifecycle = self
            .host
            .control_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lifecycle.active = lifecycle.active.saturating_sub(1);
    }
}

/// Workspace-owned execution authorities used by foreground and background
/// turns after focus has moved elsewhere.
pub(crate) struct WorkspaceExecutionRuntime {
    primary_agent: crate::agent_handle::AgentHandle,
    pool: Arc<AgentPool>,
    task_runtime: Arc<TaskRuntimeStore>,
    review_integration: Arc<ReviewIntegration>,
    plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRuntimeActivity {
    pub workspace_id: WorkspaceId,
    pub execution_loaded: bool,
    pub active_pool_executions: usize,
    pub active_run_drivers: usize,
    pub active_run_driver_receipts: usize,
    pub active_task_runtime_operations: usize,
    pub active_controls: usize,
}

impl WorkspaceRuntimeActivity {
    pub(crate) fn is_idle(&self) -> bool {
        self.active_pool_executions == 0
            && self.active_run_drivers == 0
            && self.active_run_driver_receipts == 0
            && self.active_task_runtime_operations == 0
            && self.active_controls == 0
    }
}

/// Sole process-level owner for loaded workspace hosts.
///
/// Host creation is serialized so concurrent opens of the same workspace
/// cannot build two independent in-process store owners. Loaded hosts remain
/// resident until application shutdown; eviction requires an explicit idle
/// proof and is intentionally deferred.
pub(crate) struct WorkspaceRuntimeRegistry {
    hosts: Mutex<HashMap<WorkspaceId, Arc<WorkspaceRuntimeHost>>>,
    product_data_io: crate::product_data_io::ProductDataIoService,
    operation_stores: Arc<std::sync::Mutex<Vec<std::sync::Weak<TaskRuntimeStore>>>>,
    operation_admission_open: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    control_acquire_barrier: std::sync::Mutex<Option<WorkspaceControlAcquireTestBarrier>>,
    #[cfg(test)]
    close_barrier: std::sync::Mutex<Option<WorkspaceControlAcquireTestBarrier>>,
}

#[cfg(test)]
struct WorkspaceControlAcquireTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

impl WorkspaceRuntimeResources {
    /// Validate a workspace root and open every file-backed store needed by a
    /// focused workspace generation before any live Agent binding is changed.
    pub(crate) async fn prepare(
        workspace: Workspace,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> anyhow::Result<Self> {
        let flow = product_data_io
            .begin_owned_flow("prepare workspace runtime resources")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let prepare_product_data_io = flow.service();
        let resources = flow
            .run("prepare workspace runtime file stores", move || {
                let mut workspace = workspace;
                let root = validated_workspace_root(&workspace.root)?;
                WorkspaceLayout::ensure_dirs(&root).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to prepare workspace layout at {}: {error}",
                        root.display()
                    )
                })?;
                let state_dir = WorkspaceLayout::state_dir(&root);
                let sessions_dir = WorkspaceLayout::sessions(&root);
                let tasks_dir = WorkspaceLayout::tasks(&root);
                let conversation_store: Arc<dyn ConversationStore> =
                    Arc::new(FileConversationStore::new(&state_dir).map_err(|error| {
                        anyhow::anyhow!(
                            "Failed to prepare workspace conversation store at {}: {error}",
                            state_dir.display()
                        )
                    })?);
                let runtime_state_store: Arc<dyn RuntimeStateStore> =
                    Arc::new(FileRuntimeStateStore::new(&sessions_dir).map_err(|error| {
                        anyhow::anyhow!(
                            "Failed to prepare workspace runtime state store at {}: {error}",
                            sessions_dir.display()
                        )
                    })?);
                let memory_path = WorkspaceLayout::memory_store(&root);
                let memory_store: Arc<dyn Store> =
                    Arc::new(FileStore::new(&memory_path).map_err(|error| {
                        anyhow::anyhow!(
                            "Failed to prepare workspace memory store at {}: {error}",
                            memory_path.display()
                        )
                    })?);
                let deletion_service =
                    Arc::new(ConversationDeletionService::new_with_product_data_io(
                        state_dir.join("conversation-deletions"),
                        prepare_product_data_io,
                    ));
                workspace.root = root;
                workspace.refresh_product_data_generation();
                Ok::<Self, anyhow::Error>(Self {
                    workspace,
                    state_dir,
                    tasks_dir,
                    conversation_store,
                    runtime_state_store,
                    memory_store,
                    deletion_service,
                })
            })
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))??;
        // Aggregate deletion recovery also owns AgentRouter retirement. That
        // authority belongs to AppState, so boot reconciliation runs it after
        // the scoped resources have been published instead of finalizing only
        // the transcript/runtime-state half here.
        flow.settle(None);
        Ok(resources)
    }

    pub(crate) fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub(crate) fn root(&self) -> &Path {
        &self.workspace.root
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub(crate) fn tasks_dir(&self) -> &Path {
        &self.tasks_dir
    }

    pub(crate) fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        self.conversation_store.clone()
    }

    pub(crate) fn runtime_state_store(&self) -> Arc<dyn RuntimeStateStore> {
        self.runtime_state_store.clone()
    }

    pub(crate) fn memory_store(&self) -> Arc<dyn Store> {
        self.memory_store.clone()
    }

    pub(crate) fn deletion_service(&self) -> Arc<ConversationDeletionService> {
        self.deletion_service.clone()
    }
}

impl WorkspaceRuntimeHost {
    async fn open_with_operation_stores(
        workspace: Workspace,
        operation_stores: Arc<std::sync::Mutex<Vec<std::sync::Weak<TaskRuntimeStore>>>>,
        operation_admission_open: Arc<std::sync::atomic::AtomicBool>,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> anyhow::Result<Arc<Self>> {
        let resources = WorkspaceRuntimeResources::prepare(workspace, product_data_io).await?;
        let workspace = resources.workspace().clone();
        Ok(Arc::new(Self {
            workspace: RwLock::new(workspace),
            resources,
            task_runtime: OnceCell::new(),
            execution: OnceCell::new(),
            control_lifecycle: std::sync::Mutex::new(WorkspaceControlLifecycle::default()),
            operation_stores,
            operation_admission_open,
            shutdown_settlement: std::sync::Mutex::new(None),
            #[cfg(test)]
            shutdown_after_operations_barrier: std::sync::Mutex::new(None),
        }))
    }

    pub(crate) async fn workspace(&self) -> Workspace {
        self.workspace.read().await.clone()
    }

    pub(crate) fn execution_if_loaded(&self) -> Option<Arc<WorkspaceExecutionRuntime>> {
        self.execution.get().map(Arc::clone)
    }

    pub(crate) fn id(&self) -> &WorkspaceId {
        &self.resources.workspace().id
    }

    pub(crate) fn root(&self) -> &Path {
        self.resources.root()
    }

    pub(crate) fn resources(&self) -> &WorkspaceRuntimeResources {
        &self.resources
    }

    pub(crate) fn execution_scope(&self) -> WorkspaceExecutionScope {
        WorkspaceExecutionScope::workspace(self.id(), self.root())
    }

    pub(crate) fn workspace_io_identity(&self) -> super::WorkspaceIoIdentity {
        super::WorkspaceIoIdentity::workspace(self.resources.workspace())
    }

    fn acquire_control_lease(self: &Arc<Self>) -> anyhow::Result<WorkspaceControlLease> {
        let mut lifecycle = self
            .control_lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace control lifecycle lock is poisoned"))?;
        if lifecycle.closing {
            anyhow::bail!("workspace '{}' runtime is closing", self.id());
        }
        lifecycle.active = lifecycle
            .active
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("workspace control lease counter exhausted"))?;
        drop(lifecycle);
        Ok(WorkspaceControlLease {
            _receipt: Arc::new(WorkspaceControlReceipt {
                host: Arc::clone(self),
            }),
        })
    }

    fn active_control_count(&self) -> anyhow::Result<usize> {
        self.control_lifecycle
            .lock()
            .map(|lifecycle| lifecycle.active)
            .map_err(|_| anyhow::anyhow!("workspace control lifecycle lock is poisoned"))
    }

    fn ensure_open(&self) -> anyhow::Result<()> {
        let lifecycle = self
            .control_lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace control lifecycle lock is poisoned"))?;
        if lifecycle.closing {
            anyhow::bail!("workspace '{}' runtime is closing", self.id());
        }
        Ok(())
    }

    fn begin_closing(self: &Arc<Self>) -> anyhow::Result<WorkspaceClosingGuard> {
        let mut lifecycle = self
            .control_lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace control lifecycle lock is poisoned"))?;
        if lifecycle.closing {
            anyhow::bail!("workspace '{}' runtime is already closing", self.id());
        }
        lifecycle.closing = true;
        let active_controls = lifecycle.active;
        drop(lifecycle);
        Ok(WorkspaceClosingGuard {
            host: Arc::clone(self),
            active_controls,
            committed: false,
        })
    }

    fn abort_closing(&self) {
        let mut lifecycle = self
            .control_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lifecycle.closing = false;
    }

    #[cfg(test)]
    fn park_shutdown_after_operations(
        &self,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ),
        String,
    > {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut barrier = self
            .shutdown_after_operations_barrier
            .lock()
            .map_err(|_| "workspace shutdown test barrier is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("workspace shutdown test barrier is already installed".to_string());
        }
        *barrier = Some(WorkspaceControlAcquireTestBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        Ok((entered_rx, release_tx))
    }

    /// Open the host's TaskRuntime authority without constructing AgentPool,
    /// plugin, MCP, or review generations.
    pub(crate) async fn task_runtime(&self) -> anyhow::Result<Arc<TaskRuntimeStore>> {
        let store = self
            .task_runtime
            .get_or_try_init(|| async {
                if !self
                    .operation_admission_open
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    anyhow::bail!("workspace TaskRuntime operation admission is closed");
                }
                let store = Arc::new(TaskRuntimeStore::open_for_workspace(
                    self.resources.tasks_dir(),
                    self.id().to_string(),
                )?);
                self.operation_stores
                    .lock()
                    .map_err(|_| {
                        anyhow::anyhow!("workspace TaskRuntime operation registry lock is poisoned")
                    })?
                    .push(Arc::downgrade(&store));
                if !self
                    .operation_admission_open
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    anyhow::bail!("workspace TaskRuntime operation admission closed during open");
                }
                crate::tasks::task_runtime::TaskRunBootReconciler::for_store(&store)
                    .recover_once()
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok::<Arc<TaskRuntimeStore>, anyhow::Error>(store)
            })
            .await?;
        Ok(Arc::clone(store))
    }

    /// Lazily build the one execution generation owned by this host.
    ///
    /// `seed_pool` supplies process-safe model/plugin/tool primitives. All
    /// workspace-bearing stores and task tools are replaced by host resources
    /// before the pool can admit its first conversation.
    pub(crate) async fn get_or_open_execution(
        &self,
        seed_pool: &Arc<AgentPool>,
    ) -> anyhow::Result<Arc<WorkspaceExecutionRuntime>> {
        let runtime = self
            .execution
            .get_or_try_init(|| async {
                let task_runtime = self.task_runtime().await?;
                let review_integration = Arc::new(ReviewIntegration::new(
                    echo_agent::evolution::ReviewConfig::default(),
                    self.resources.state_dir().to_path_buf(),
                    self.resources.memory_store(),
                ));
                let workspace = self.workspace().await;
                let workspace_io_identity = self.workspace_io_identity();
                let (pool, plugin_runtime, _mcp_ownership) = seed_pool
                    .fork_for_workspace(WorkspaceAgentPoolResources {
                        root: self.root().to_path_buf(),
                        kind: workspace.kind,
                        conversation_store: self.resources.conversation_store(),
                        state_store: self.resources.runtime_state_store(),
                        memory_store: self.resources.memory_store(),
                        task_runtime_store: task_runtime.clone(),
                        review_integration: review_integration.clone(),
                        execution_scope: self.execution_scope(),
                        workspace_io_identity,
                    })
                    .await?;
                let primary_agent = pool.primary_agent().await?;
                review_integration.bind_rule_projection_primary(primary_agent.clone());
                review_integration.bind_rule_projection_pool(&pool).await?;
                Ok::<Arc<WorkspaceExecutionRuntime>, anyhow::Error>(Arc::new(
                    WorkspaceExecutionRuntime {
                        primary_agent,
                        pool,
                        task_runtime,
                        review_integration,
                        plugin_runtime,
                    },
                ))
            })
            .await?;
        Ok(Arc::clone(runtime))
    }

    pub(crate) async fn refresh_workspace(&self, mut workspace: Workspace) -> anyhow::Result<()> {
        if workspace.id != *self.id() {
            anyhow::bail!(
                "Workspace host identity mismatch: expected {}, received {}",
                self.id(),
                workspace.id
            );
        }
        let root = validated_workspace_root(&workspace.root)?;
        if root != self.root() {
            anyhow::bail!(
                "Workspace '{}' is already loaded from {}; refusing root change to {}",
                self.id(),
                self.root().display(),
                root.display()
            );
        }
        workspace.root = root;
        *self.workspace.write().await = workspace;
        Ok(())
    }

    async fn shutdown_runtime_uncached(&self) -> anyhow::Result<()> {
        if let Some(runtime) = self.execution.get() {
            return runtime.shutdown().await;
        }
        let Some(store) = self.task_runtime.get() else {
            return Ok(());
        };
        let mut errors = Vec::new();
        if let Err(error) = store.shutdown_run_drivers().await {
            errors.push(format!("TaskRun drivers: {error}"));
        }
        if let Err(error) = store.shutdown_operations().await {
            errors.push(format!("TaskRuntime operations: {error}"));
        }
        #[cfg(test)]
        {
            let barrier = self
                .shutdown_after_operations_barrier
                .lock()
                .map_err(|_| anyhow::anyhow!("workspace shutdown test barrier is poisoned"))?
                .take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                barrier.release.await.map_err(|_| {
                    anyhow::anyhow!("workspace shutdown test barrier release was dropped")
                })?;
            }
        }
        if let Err(error) = store.shutdown_hook_events().await {
            errors.push(format!("task hooks: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }

    async fn shutdown_runtime(self: &Arc<Self>) -> anyhow::Result<()> {
        let settlement = {
            let mut owner = self
                .shutdown_settlement
                .lock()
                .map_err(|_| anyhow::anyhow!("workspace shutdown owner lock is poisoned"))?;
            if let Some(settlement) = owner.as_ref() {
                settlement.clone()
            } else {
                let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
                    anyhow::anyhow!(
                        "Tokio runtime is unavailable during workspace shutdown: {error}"
                    )
                })?;
                let host = Arc::clone(self);
                let settlement = async move {
                    match std::panic::AssertUnwindSafe(host.shutdown_runtime_uncached())
                        .catch_unwind()
                        .await
                    {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(_) => Err("workspace runtime shutdown panicked".to_string()),
                    }
                }
                .boxed()
                .shared();
                drop(runtime.spawn(settlement.clone()));
                *owner = Some(settlement.clone());
                settlement
            }
        };
        settlement.await.map_err(anyhow::Error::msg)
    }
}

impl WorkspaceExecutionRuntime {
    pub(crate) fn primary_agent(&self) -> crate::agent_handle::AgentHandle {
        self.primary_agent.clone()
    }

    pub(crate) fn pool(&self) -> Arc<AgentPool> {
        Arc::clone(&self.pool)
    }

    pub(crate) fn task_runtime(&self) -> Arc<TaskRuntimeStore> {
        Arc::clone(&self.task_runtime)
    }

    pub(crate) fn review_integration(&self) -> Arc<ReviewIntegration> {
        Arc::clone(&self.review_integration)
    }

    pub(crate) fn plugin_runtime(
        &self,
    ) -> Option<Arc<crate::plugin_runtime::PluginRuntimeService>> {
        self.plugin_runtime.clone()
    }

    pub(crate) fn activity(
        &self,
        workspace_id: WorkspaceId,
        active_controls: usize,
    ) -> anyhow::Result<WorkspaceRuntimeActivity> {
        Ok(WorkspaceRuntimeActivity {
            workspace_id,
            execution_loaded: true,
            active_pool_executions: self.pool.active_execution_count(),
            active_run_drivers: self
                .task_runtime
                .active_run_driver_count()
                .map_err(anyhow::Error::msg)?,
            active_run_driver_receipts: self
                .task_runtime
                .active_run_driver_receipt_count()
                .map_err(anyhow::Error::msg)?,
            active_task_runtime_operations: self.task_runtime.active_operation_count(),
            active_controls,
        })
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        if let Err(error) = self.task_runtime.shutdown_run_drivers().await {
            errors.push(format!("TaskRun drivers: {error}"));
        }
        if let Err(error) = self.task_runtime.shutdown_operations().await {
            errors.push(format!("TaskRuntime operations: {error}"));
        }
        if let Err(error) = self.review_integration.shutdown_background_reviews().await {
            errors.push(format!("memory review: {error}"));
        }
        if let Err(error) = self.pool.shutdown().await {
            errors.push(format!("AgentPool: {error}"));
        }
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = plugin_runtime.shutdown().await
        {
            errors.push(format!("plugin runtime: {error}"));
        }
        if let Err(error) = self.task_runtime.shutdown_hook_events().await {
            errors.push(format!("task hooks: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }
}

impl WorkspaceRuntimeRegistry {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::new_with_product_data_io(crate::product_data_io::ProductDataIoService::new())
    }

    pub(crate) fn new_with_product_data_io(
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> Self {
        Self {
            hosts: Mutex::new(HashMap::new()),
            product_data_io,
            operation_stores: Arc::new(std::sync::Mutex::new(Vec::new())),
            operation_admission_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            #[cfg(test)]
            control_acquire_barrier: std::sync::Mutex::new(None),
            #[cfg(test)]
            close_barrier: std::sync::Mutex::new(None),
        }
    }

    fn product_data_io(&self) -> crate::product_data_io::ProductDataIoService {
        self.product_data_io.clone()
    }

    /// Return the one loaded host for a workspace, opening it on first use.
    pub(crate) async fn get_or_open(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<Arc<WorkspaceRuntimeHost>> {
        let workspace_id = workspace.id.clone();
        let mut hosts = self.hosts.lock().await;
        if let Some(host) = hosts.get(&workspace_id) {
            host.ensure_open()?;
            host.refresh_workspace(workspace).await?;
            return Ok(Arc::clone(host));
        }

        let host = WorkspaceRuntimeHost::open_with_operation_stores(
            workspace,
            Arc::clone(&self.operation_stores),
            Arc::clone(&self.operation_admission_open),
            self.product_data_io(),
        )
        .await?;
        hosts.insert(workspace_id, Arc::clone(&host));
        Ok(host)
    }

    /// Return an already loaded host without creating a second runtime owner.
    pub(crate) async fn loaded_host(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<Arc<WorkspaceRuntimeHost>> {
        self.hosts.lock().await.get(workspace_id).cloned()
    }

    /// Resolve and pin one host while the registry membership lock is held.
    /// Eviction cannot mark the host Closing between lookup and lease acquire.
    pub(crate) async fn get_or_open_control(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<(Arc<WorkspaceRuntimeHost>, WorkspaceControlLease)> {
        let workspace_id = workspace.id.clone();
        let mut hosts = self.hosts.lock().await;
        let host = match hosts.get(&workspace_id) {
            Some(host) => {
                host.ensure_open()?;
                host.refresh_workspace(workspace).await?;
                Arc::clone(host)
            }
            None => {
                let host = WorkspaceRuntimeHost::open_with_operation_stores(
                    workspace,
                    Arc::clone(&self.operation_stores),
                    Arc::clone(&self.operation_admission_open),
                    self.product_data_io(),
                )
                .await?;
                hosts.insert(workspace_id, Arc::clone(&host));
                host
            }
        };
        let lease = host.acquire_control_lease()?;
        Ok((host, lease))
    }

    /// Pin a focused host only if it is still the exact registered generation.
    pub(crate) async fn acquire_control_for_host(
        &self,
        host: &Arc<WorkspaceRuntimeHost>,
    ) -> anyhow::Result<WorkspaceControlLease> {
        let hosts = self.hosts.lock().await;
        let registered = hosts.get(host.id()).ok_or_else(|| {
            anyhow::anyhow!("workspace '{}' runtime is no longer registered", host.id())
        })?;
        if !Arc::ptr_eq(registered, host) {
            anyhow::bail!("workspace '{}' runtime generation was replaced", host.id());
        }
        #[cfg(test)]
        {
            let barrier = self
                .control_acquire_barrier
                .lock()
                .map_err(|_| anyhow::anyhow!("control acquire test barrier is poisoned"))?
                .take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                barrier.release.await.map_err(|_| {
                    anyhow::anyhow!("control acquire test barrier release was dropped")
                })?;
            }
        }
        host.acquire_control_lease()
    }

    pub(crate) fn begin_task_runtime_operation_shutdown(&self) -> Result<(), String> {
        self.operation_admission_open
            .store(false, std::sync::atomic::Ordering::Release);
        let mut stores = self
            .operation_stores
            .lock()
            .map_err(|_| "workspace TaskRuntime operation registry lock is poisoned".to_string())?;
        let mut failures = Vec::new();
        stores.retain(|store| {
            let Some(store) = store.upgrade() else {
                return false;
            };
            if let Err(error) = store.begin_operation_shutdown() {
                failures.push(format!(
                    "workspace {}: {error}",
                    store.active_workspace_id()
                ));
            }
            true
        });
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    #[cfg(test)]
    pub(crate) fn park_next_control_acquire(
        &self,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ),
        String,
    > {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut barrier = self
            .control_acquire_barrier
            .lock()
            .map_err(|_| "control acquire test barrier is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("control acquire test barrier is already installed".to_string());
        }
        *barrier = Some(WorkspaceControlAcquireTestBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        Ok((entered_rx, release_tx))
    }

    #[cfg(test)]
    fn park_next_close(
        &self,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ),
        String,
    > {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut barrier = self
            .close_barrier
            .lock()
            .map_err(|_| "workspace close test barrier is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("workspace close test barrier is already installed".to_string());
        }
        *barrier = Some(WorkspaceControlAcquireTestBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        Ok((entered_rx, release_tx))
    }

    /// Stable, sorted snapshot of every initialized host generation. The map
    /// lock is released before callers await any runtime publication.
    pub(crate) async fn loaded_execution_runtimes(
        &self,
    ) -> Vec<(WorkspaceId, Arc<WorkspaceExecutionRuntime>)> {
        let hosts = self.hosts.lock().await;
        let mut runtimes = hosts
            .values()
            .filter_map(|host| {
                host.execution
                    .get()
                    .map(|runtime| (host.id().clone(), Arc::clone(runtime)))
            })
            .collect::<Vec<_>>();
        runtimes.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        runtimes
    }

    /// Pin every initialized workspace execution generation for one global
    /// control mutation. Acquiring leases while holding registry membership
    /// prevents delete/eviction from racing the returned snapshot.
    pub(crate) async fn loaded_execution_controls(
        &self,
    ) -> anyhow::Result<
        Vec<(
            WorkspaceId,
            String,
            Arc<WorkspaceExecutionRuntime>,
            WorkspaceControlLease,
        )>,
    > {
        let hosts = self.hosts.lock().await;
        let mut controls = Vec::new();
        for host in hosts.values() {
            let Some(runtime) = host.execution.get() else {
                continue;
            };
            controls.push((
                host.id().clone(),
                host.workspace().await.opaque_product_data_generation(),
                Arc::clone(runtime),
                host.acquire_control_lease()?,
            ));
        }
        controls.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        Ok(controls)
    }

    pub(crate) async fn activity_snapshot(&self) -> anyhow::Result<Vec<WorkspaceRuntimeActivity>> {
        let hosts = self.hosts.lock().await;
        let mut activity = Vec::with_capacity(hosts.len());
        for host in hosts.values() {
            match host.execution.get() {
                Some(runtime) => activity
                    .push(runtime.activity(host.id().clone(), host.active_control_count()?)?),
                None => activity.push(WorkspaceRuntimeActivity {
                    workspace_id: host.id().clone(),
                    execution_loaded: false,
                    active_pool_executions: 0,
                    active_run_drivers: 0,
                    active_run_driver_receipts: 0,
                    active_task_runtime_operations: host
                        .task_runtime
                        .get()
                        .map(|store| store.active_operation_count())
                        .unwrap_or(0),
                    active_controls: host.active_control_count()?,
                }),
            }
        }
        activity.sort_by(|left, right| left.workspace_id.as_str().cmp(right.workspace_id.as_str()));
        Ok(activity)
    }

    /// Shut down and remove one loaded host only after its runtime proves idle.
    /// An unloaded workspace needs no runtime settlement and returns `false`.
    pub(crate) async fn shutdown_and_evict_if_idle(
        &self,
        workspace_id: &WorkspaceId,
    ) -> anyhow::Result<bool> {
        let mut hosts = self.hosts.lock().await;
        let Some(host) = hosts.get(workspace_id).cloned() else {
            return Ok(false);
        };
        let closing = host.begin_closing()?;
        #[cfg(test)]
        {
            let barrier = self
                .close_barrier
                .lock()
                .map_err(|_| anyhow::anyhow!("workspace close test barrier is poisoned"))?
                .take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                barrier.release.await.map_err(|_| {
                    anyhow::anyhow!("workspace close test barrier release was dropped")
                })?;
            }
        }
        let active_controls = closing.active_controls;
        let activity_result = match host.execution.get() {
            Some(runtime) => runtime.activity(workspace_id.clone(), active_controls),
            None => Ok(WorkspaceRuntimeActivity {
                workspace_id: workspace_id.clone(),
                execution_loaded: false,
                active_pool_executions: 0,
                active_run_drivers: 0,
                active_run_driver_receipts: 0,
                active_task_runtime_operations: host
                    .task_runtime
                    .get()
                    .map(|store| store.active_operation_count())
                    .unwrap_or(0),
                active_controls,
            }),
        };
        let activity = match activity_result {
            Ok(activity) => activity,
            Err(error) => return Err(error),
        };
        if !activity.is_idle() {
            anyhow::bail!(
                "workspace '{}' is busy (pool executions: {}, run drivers: {}, driver receipts: {}, TaskRuntime operations: {}, controls: {})",
                workspace_id,
                activity.active_pool_executions,
                activity.active_run_drivers,
                activity.active_run_driver_receipts,
                activity.active_task_runtime_operations,
                activity.active_controls
            );
        }
        closing.commit();
        host.shutdown_runtime().await?;
        hosts.remove(workspace_id);
        Ok(true)
    }

    pub(crate) async fn shutdown(&self) -> anyhow::Result<()> {
        let hosts = self
            .hosts
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for host in hosts {
            if let Err(error) = host.shutdown_runtime().await {
                errors.push(format!("workspace {}: {error}", host.id()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }

    #[cfg(test)]
    async fn host_count(&self) -> usize {
        self.hosts.lock().await.len()
    }
}

fn validated_workspace_root(root: &Path) -> anyhow::Result<PathBuf> {
    let root = root.canonicalize().map_err(|error| {
        anyhow::anyhow!(
            "Workspace root is missing or cannot be resolved ({}): {error}",
            root.display()
        )
    })?;
    if !root.is_dir() {
        anyhow::bail!("Workspace root is not a directory: {}", root.display());
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use echo_agent::memory::NewConversation;
    use echo_agent::testing::MockLlmClient;

    use super::*;
    use crate::agent_handle::AgentHandle;
    use crate::workspace::{WorkspaceId, WorkspaceKind, WorkspaceMetadata};

    fn workspace(name: &str, root: PathBuf) -> Workspace {
        Workspace {
            id: WorkspaceId::from_name(name),
            name: name.to_string(),
            root,
            project_root: None,
            kind: WorkspaceKind::General,
            metadata: WorkspaceMetadata::default(),
            product_data_generation: String::new(),
            created_at: Utc::now(),
            last_active: Utc::now(),
        }
    }

    #[tokio::test]
    async fn prepare_rejects_missing_and_non_directory_roots() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let file = temp.path().join("workspace-file");
        std::fs::write(&file, "not a directory").map_err(|error| error.to_string())?;

        assert!(
            WorkspaceRuntimeResources::prepare(
                workspace("missing", temp.path().join("missing")),
                crate::product_data_io::ProductDataIoService::new(),
            )
            .await
            .is_err()
        );
        assert!(
            WorkspaceRuntimeResources::prepare(
                workspace("file", file),
                crate::product_data_io::ProductDataIoService::new(),
            )
            .await
            .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn prepare_builds_canonical_workspace_layout() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;

        let resources = WorkspaceRuntimeResources::prepare(
            workspace("alpha", root.clone()),
            crate::product_data_io::ProductDataIoService::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;

        assert_eq!(resources.root(), canonical_root);
        assert_eq!(
            resources.state_dir(),
            WorkspaceLayout::state_dir(&canonical_root)
        );
        assert!(WorkspaceLayout::sessions(&canonical_root).is_dir());
        assert_eq!(
            resources.tasks_dir(),
            WorkspaceLayout::tasks(&canonical_root)
        );
        assert!(WorkspaceLayout::conversations(&canonical_root).is_dir());
        assert!(WorkspaceLayout::memory(&canonical_root).is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn task_runtime_recovery_does_not_eagerly_load_execution_generation() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("lazy-runtime");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();
        let host = registry
            .get_or_open(workspace("lazy", root))
            .await
            .map_err(|error| error.to_string())?;

        let first = host
            .task_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let second = host
            .task_runtime()
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(&first, &second));
        assert!(host.execution.get().is_none());
        assert_eq!(first.active_workspace_id(), "lazy");
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caller_abort_keeps_workspace_busy_until_taskruntime_operation_settles()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("operation-barrier");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();
        let host = registry
            .get_or_open(workspace("operation-barrier", root))
            .await
            .map_err(|error| error.to_string())?;
        let workspace_id = host.id().clone();
        let store = host
            .task_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let adapter = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            adapter
                .run_owned("workspace teardown barrier", move || {
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
            .map_err(|_| "TaskRuntime operation did not enter".to_string())?;
        caller.abort();
        let _ = caller.await;

        let error = registry
            .shutdown_and_evict_if_idle(&workspace_id)
            .await
            .err()
            .ok_or_else(|| "workspace teardown crossed an active operation".to_string())?;
        if !error.to_string().contains("TaskRuntime operations: 1") {
            return Err(format!("unexpected workspace busy receipt: {error}"));
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release TaskRuntime operation: {error}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while store.active_operation_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "TaskRuntime operation did not settle".to_string())?;
        if !registry
            .shutdown_and_evict_if_idle(&workspace_id)
            .await
            .map_err(|error| error.to_string())?
        {
            return Err("settled workspace host was not evicted".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn independent_resources_do_not_share_conversations() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("a");
        let root_b = temp.path().join("b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;

        let resources_a = WorkspaceRuntimeResources::prepare(
            workspace("a", root_a),
            crate::product_data_io::ProductDataIoService::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let resources_b = WorkspaceRuntimeResources::prepare(
            workspace("b", root_b),
            crate::product_data_io::ProductDataIoService::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let conversation_id = "shared-conversation-id";
        resources_a
            .conversation_store()
            .ensure_conversation(NewConversation {
                conversation_id: conversation_id.to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some("Workspace A".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            resources_a
                .conversation_store()
                .get_conversation(conversation_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        assert!(
            resources_b
                .conversation_store()
                .get_conversation(conversation_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn two_hosts_own_independent_concurrent_execution_runtimes() -> Result<(), String> {
        let process_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("a");
        let root_b = temp.path().join("b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_b = root_b.canonicalize().map_err(|error| error.to_string())?;

        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace runtime seed")
            .build()
            .map_err(|error| error.to_string())?;
        let seed = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                4,
                false,
            )
            .await,
        );
        let registry = WorkspaceRuntimeRegistry::new();
        let host_a = registry
            .get_or_open(workspace("a", root_a))
            .await
            .map_err(|error| error.to_string())?;
        let host_b = registry
            .get_or_open(workspace("b", root_b))
            .await
            .map_err(|error| error.to_string())?;

        let (runtime_a, runtime_b) = tokio::try_join!(
            host_a.get_or_open_execution(&seed),
            host_b.get_or_open_execution(&seed)
        )
        .map_err(|error| error.to_string())?;
        assert!(!Arc::ptr_eq(&runtime_a, &runtime_b));
        assert!(!Arc::ptr_eq(&runtime_a.pool(), &runtime_b.pool()));
        assert_eq!(runtime_a.task_runtime().active_workspace_id(), "a");
        assert_eq!(runtime_b.task_runtime().active_workspace_id(), "b");
        assert_eq!(
            runtime_a.task_runtime().active_shadow_root(),
            WorkspaceLayout::tasks(&canonical_a)
        );
        assert_eq!(
            runtime_b.task_runtime().active_shadow_root(),
            WorkspaceLayout::tasks(&canonical_b)
        );

        let pool_a = runtime_a.pool();
        let pool_b = runtime_b.pool();
        let (lease_a, lease_b) = tokio::try_join!(
            pool_a.acquire("same-conversation"),
            pool_b.acquire("same-conversation")
        )
        .map_err(|error| error.to_string())?;
        let agent_a = lease_a.agent();
        let agent_b = lease_b.agent();
        assert!(!Arc::ptr_eq(agent_a.inner(), agent_b.inner()));

        let working_dir_a = agent_a.read(|agent| agent.working_dir()).await;
        let working_dir_b = agent_b.read(|agent| agent.working_dir()).await;
        assert_eq!(working_dir_a.as_deref(), Some(canonical_a.as_path()));
        assert_eq!(working_dir_b.as_deref(), Some(canonical_b.as_path()));

        let tools_a = agent_a.read(|agent| agent.tool_names()).await;
        let tools_b = agent_b.read(|agent| agent.tool_names()).await;
        for expected in ["task_create", "task_update", "task_list", "task_execute"] {
            assert!(tools_a.iter().any(|name| name == expected));
            assert!(tools_b.iter().any(|name| name == expected));
        }
        let tool_manager_a = agent_a.read(|agent| agent.tool_manager().clone()).await;
        let tool_manager_b = agent_b.read(|agent| agent.tool_manager().clone()).await;
        assert!(!Arc::ptr_eq(&tool_manager_a, &tool_manager_b));

        let artifacts_a = agent_a
            .read(|agent| agent.tool_output_artifacts())
            .await
            .ok_or_else(|| "workspace A artifact config missing".to_string())?;
        let artifacts_b = agent_b
            .read(|agent| agent.tool_output_artifacts())
            .await
            .ok_or_else(|| "workspace B artifact config missing".to_string())?;
        assert!(artifacts_a.root_dir.starts_with(&canonical_a));
        assert!(artifacts_b.root_dir.starts_with(&canonical_b));
        assert_eq!(
            std::env::current_dir().map_err(|error| error.to_string())?,
            process_cwd
        );
        Ok(())
    }

    #[tokio::test]
    async fn workspace_host_evicts_only_after_execution_is_idle() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("evict");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace eviction seed")
            .build()
            .map_err(|error| error.to_string())?;
        let seed = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                2,
                false,
            )
            .await,
        );
        let registry = WorkspaceRuntimeRegistry::new();
        let workspace = workspace("evict", root);
        let host = registry
            .get_or_open(workspace.clone())
            .await
            .map_err(|error| error.to_string())?;
        let runtime = host
            .get_or_open_execution(&seed)
            .await
            .map_err(|error| error.to_string())?;
        let lease = runtime
            .pool()
            .acquire("conversation")
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace.id)
                .await
                .is_err()
        );
        assert_eq!(registry.host_count().await, 1);

        drop(lease);
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace.id)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(registry.host_count().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn degraded_workspace_shutdown_stays_sealed_and_replays_its_debt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("degraded-eviction");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();
        let workspace = workspace("degraded", root);
        let host = registry
            .get_or_open(workspace.clone())
            .await
            .map_err(|error| error.to_string())?;
        let store = host
            .task_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let adapter = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store);
        let debt = crate::tasks::task_runtime::StoreError::InvalidPlan(
            "terminal projection debt".to_string(),
        );
        adapter.record_lifecycle_debt("degraded workspace fixture", &debt);

        let first = registry
            .shutdown_and_evict_if_idle(&workspace.id)
            .await
            .err()
            .map(|error| error.to_string())
            .ok_or_else(|| "degraded workspace shutdown unexpectedly succeeded".to_string())?;
        assert!(first.contains("terminal projection debt"));
        assert_eq!(registry.host_count().await, 1);
        assert!(registry.get_or_open(workspace.clone()).await.is_err());

        let replay = host
            .shutdown_runtime()
            .await
            .err()
            .map(|error| error.to_string())
            .ok_or_else(|| "cached degraded settlement was lost".to_string())?;
        assert!(replay.contains("terminal projection debt"));
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace.id)
                .await
                .is_err()
        );
        assert_eq!(registry.host_count().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn eviction_caller_drop_cannot_cancel_or_erase_shutdown_debt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("caller-drop-eviction");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = Arc::new(WorkspaceRuntimeRegistry::new());
        let workspace = workspace("caller-drop", root);
        let host = registry
            .get_or_open(workspace.clone())
            .await
            .map_err(|error| error.to_string())?;
        let store = host
            .task_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let adapter = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store);
        let debt = crate::tasks::task_runtime::StoreError::InvalidPlan(
            "caller-drop terminal debt".to_string(),
        );
        adapter.record_lifecycle_debt("caller-drop workspace fixture", &debt);
        let (entered, release) = host.park_shutdown_after_operations()?;

        let eviction_registry = registry.clone();
        let workspace_id = workspace.id.clone();
        let eviction = tokio::spawn(async move {
            eviction_registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
        });
        entered.await.map_err(|_| {
            "workspace shutdown never reached its post-operation barrier".to_string()
        })?;
        eviction.abort();
        let eviction_result = eviction.await;
        if !eviction_result.is_err_and(|error| error.is_cancelled()) {
            return Err("workspace eviction caller was not cancelled".to_string());
        }
        release
            .send(())
            .map_err(|_| "failed to release workspace shutdown owner".to_string())?;

        let replay = host
            .shutdown_runtime()
            .await
            .err()
            .map(|error| error.to_string())
            .ok_or_else(|| "caller-drop shutdown debt was lost".to_string())?;
        assert!(replay.contains("caller-drop terminal debt"));
        assert!(registry.get_or_open(workspace.clone()).await.is_err());
        assert_eq!(registry.host_count().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn three_hosts_converge_mcp_generations_without_sharing_activity_or_tools()
    -> Result<(), String> {
        let process_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace generation seed")
            .build()
            .map_err(|error| error.to_string())?;
        let seed = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                4,
                false,
            )
            .await,
        );
        let registry = WorkspaceRuntimeRegistry::new();
        let mut runtimes = Vec::new();
        for position in 0..3 {
            let name = format!("workspace-{position}");
            let root = temp.path().join(&name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            let host = registry
                .get_or_open(workspace(&name, root))
                .await
                .map_err(|error| error.to_string())?;
            runtimes.push(
                host.get_or_open_execution(&seed)
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }

        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let ownerships = runtimes
            .iter()
            .map(|_| crate::mcp_config_runtime::McpNameOwnershipRegistry::new(Vec::<String>::new()))
            .collect::<Vec<_>>();
        let mut final_name = String::new();
        for generation in 1..=24 {
            final_name = format!("fixture-{generation}");
            let mut candidate = echo_agent::mcp::McpConfigFile::default();
            candidate.mcp_servers.insert(
                final_name.clone(),
                echo_agent::mcp::McpServerEntry {
                    command: Some("fixture-command".to_string()),
                    disabled: true,
                    ..Default::default()
                },
            );
            let targets = runtimes
                .iter()
                .zip(ownerships.iter())
                .map(|(runtime, ownership)| {
                    crate::mcp_config_runtime::McpReconcileTarget::new(
                        runtime.primary_agent(),
                        Arc::clone(ownership),
                        Some(runtime.pool()),
                    )
                })
                .collect();
            let committed = mcp
                .replace_and_reconcile(targets, candidate)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(committed, generation);
        }

        let expected =
            serde_json::to_value(mcp.snapshot().await).map_err(|error| error.to_string())?;
        let mut tool_managers = Vec::new();
        for (runtime, ownership) in runtimes.iter().zip(&ownerships) {
            let snapshot = runtime
                .pool()
                .mcp_config_snapshot_for_test()
                .await
                .ok_or_else(|| "workspace MCP snapshot missing".to_string())?;
            assert_eq!(
                serde_json::to_value(snapshot).map_err(|error| error.to_string())?,
                expected
            );
            assert!(ownership.is_user_owned(&final_name).await);
            tool_managers.push(
                runtime
                    .primary_agent()
                    .read(|agent| agent.tool_manager().clone())
                    .await,
            );
        }
        for (position, left) in tool_managers.iter().enumerate() {
            for right in tool_managers.iter().skip(position.saturating_add(1)) {
                assert!(!Arc::ptr_eq(left, right));
            }
        }

        let mut leases = Vec::new();
        for (position, runtime) in runtimes.iter().enumerate() {
            leases.push(
                runtime
                    .pool()
                    .acquire(&format!("conversation-{position}"))
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        let active = registry
            .activity_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|activity| {
            activity.execution_loaded
                && activity.active_pool_executions == 1
                && activity.active_run_drivers == 0
                && activity.active_run_driver_receipts == 0
        }));
        drop(leases);
        let idle = registry
            .activity_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        assert!(idle.iter().all(WorkspaceRuntimeActivity::is_idle));
        assert_eq!(
            std::env::current_dir().map_err(|error| error.to_string())?,
            process_cwd
        );
        mcp.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn control_lease_blocks_host_eviction_until_last_clone_drops() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("control-lease");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();
        let workspace = workspace("control-lease", root);
        let workspace_id = workspace.id.clone();
        let host = registry
            .get_or_open(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let lease = registry
            .acquire_control_for_host(&host)
            .await
            .map_err(|error| error.to_string())?;
        let lease_clone = lease.clone();

        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
                .is_err_and(|error| error.to_string().contains("controls: 1"))
        );
        drop(lease);
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
                .is_err()
        );
        drop(lease_clone);
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(
            registry
                .acquire_control_for_host(&host)
                .await
                .is_err_and(|error| error.to_string().contains("no longer registered"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn registry_serializes_control_pin_before_eviction_closing() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("control-race");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = Arc::new(WorkspaceRuntimeRegistry::new());
        let workspace = workspace("control-race", root);
        let workspace_id = workspace.id.clone();
        let host = registry
            .get_or_open(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let (entered, release) = registry.park_next_control_acquire()?;
        let acquire_registry = Arc::clone(&registry);
        let acquire_host = Arc::clone(&host);
        let acquire = tokio::spawn(async move {
            acquire_registry
                .acquire_control_for_host(&acquire_host)
                .await
        });
        entered
            .await
            .map_err(|_| "control acquire did not reach test barrier".to_string())?;

        let delete_registry = Arc::clone(&registry);
        let delete_workspace_id = workspace_id.clone();
        let delete = tokio::spawn(async move {
            delete_registry
                .shutdown_and_evict_if_idle(&delete_workspace_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!delete.is_finished());

        release
            .send(())
            .map_err(|_| "control acquire release receiver was dropped".to_string())?;
        let lease = acquire
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(
            delete
                .await
                .map_err(|error| error.to_string())?
                .is_err_and(|error| error.to_string().contains("controls: 1"))
        );
        drop(lease);
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
                .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[tokio::test]
    async fn aborted_close_future_reopens_host_lifecycle() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("abort-close");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = Arc::new(WorkspaceRuntimeRegistry::new());
        let workspace = workspace("abort-close", root);
        let workspace_id = workspace.id.clone();
        let host = registry
            .get_or_open(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let (entered, release) = registry.park_next_close()?;
        let closing_registry = Arc::clone(&registry);
        let closing_workspace_id = workspace_id.clone();
        let closing = tokio::spawn(async move {
            closing_registry
                .shutdown_and_evict_if_idle(&closing_workspace_id)
                .await
        });
        entered
            .await
            .map_err(|_| "close did not reach test barrier".to_string())?;
        closing.abort();
        let _ = closing.await;
        assert!(release.send(()).is_err());

        let lease = registry
            .acquire_control_for_host(&host)
            .await
            .map_err(|error| error.to_string())?;
        drop(lease);
        assert!(
            registry
                .shutdown_and_evict_if_idle(&workspace_id)
                .await
                .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[tokio::test]
    async fn registry_reuses_one_host_and_refreshes_workspace_metadata() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();

        let first = registry
            .get_or_open(workspace("alpha", root.clone()))
            .await
            .map_err(|error| error.to_string())?;
        let mut updated = workspace("alpha", root);
        updated.project_root = Some(temp.path().join("project"));
        let second = registry
            .get_or_open(updated.clone())
            .await
            .map_err(|error| error.to_string())?;

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.host_count().await, 1);
        assert_eq!(second.workspace().await.project_root, updated.project_root);
        Ok(())
    }

    #[tokio::test]
    async fn registry_rejects_a_loaded_identity_at_another_root() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        std::fs::create_dir_all(&first_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second_root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();

        registry
            .get_or_open(workspace("alpha", first_root))
            .await
            .map_err(|error| error.to_string())?;
        let error = registry
            .get_or_open(workspace("alpha", second_root))
            .await
            .err()
            .ok_or_else(|| "root drift should be rejected".to_string())?;

        assert!(error.to_string().contains("refusing root change"));
        assert_eq!(registry.host_count().await, 1);
        Ok(())
    }
}
