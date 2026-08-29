//! AgentPool — multi-agent parallel execution pool.
//!
//! Enables multiple conversations/tasks to execute concurrently by managing
//! a pool of `ReactAgent` instances that share expensive resources (LLM client,
//! tool manager, hooks, etc.) while maintaining isolated execution contexts.
//!
//! # Architecture
//!
//! ```text
//! AgentPool
//! ├── SharedResources (Arc-shared across all pool agents)
//! │   ├── ToolManager, HookRegistry, SandboxManager
//! │   ├── Store, ConversationStore, RunStore, RuntimeStateStore
//! │   └── TokenUsageTracker, PermissionService, ToolExecutionPipeline, ReviewIntegration
//! │
//! └── agents: RwLock<HashMap<String, PooledAgent>>
//!     ├── "conv-001" → Agent (independent execution_mutex + ContextManager)
//!     ├── "conv-002" → Agent (independent execution_mutex + ContextManager)
//!     └── "__background__" → dedicated background task agent
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! // After bootstrap:
//! let pool = AgentPool::from_runtime(&runtime, PoolConfig::default(), None).await;
//!
//! // Acquire an agent for a conversation:
//! let lease = pool.acquire("conv-001").await?;
//! let agent = lease.agent();
//! agent.chat_stream("Hello").await;  // Keep `lease` until execution settles.
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use echo_agent::agent::AgentHandle;
use echo_agent::agent::CancellationToken;
use tokio::sync::{Notify, RwLock};

use crate::config::EkoConfig;
use crate::infra;
use crate::model_config::ModelRuntimeConfig;
use crate::plugin_components::{PreparedPluginAgent, register_plugin_agents};
use crate::workspace::WorkspaceKind;
use echo_agent::mcp::McpConfigFile;
use echo_agent::tools::permission::PermissionMode;

const PROCESS_AGENT_EXECUTION_LIMIT: usize = 10;
static PROCESS_AGENT_EXECUTION: std::sync::LazyLock<Arc<AgentExecutionGovernor>> =
    std::sync::LazyLock::new(|| {
        Arc::new(AgentExecutionGovernor::new(PROCESS_AGENT_EXECUTION_LIMIT))
    });

struct AgentExecutionGovernor {
    limit: usize,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl AgentExecutionGovernor {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            semaphore: Arc::new(tokio::sync::Semaphore::new(limit.max(1))),
        }
    }

    fn snapshot(&self) -> AgentExecutionResourceSnapshot {
        AgentExecutionResourceSnapshot {
            active: self
                .limit
                .saturating_sub(self.semaphore.available_permits()),
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentExecutionResourceSnapshot {
    pub active: usize,
    pub limit: usize,
}

pub fn agent_execution_resource_snapshot() -> AgentExecutionResourceSnapshot {
    PROCESS_AGENT_EXECUTION.snapshot()
}

/// Immutable EKO projection of the plugin catalog installed into primary,
/// existing pooled, and future pooled agents as one generation.
#[derive(Clone, Default)]
pub(crate) struct AgentPluginGeneration {
    revision: u64,
    skill_descriptors: Vec<echo_agent::skills::external::SkillDescriptor>,
    plugin_agents: Vec<PreparedPluginAgent>,
    output_style: Option<String>,
    framework_generation: Option<Arc<echo_agent::plugin::PreparedPluginSet>>,
}

#[derive(Clone)]
struct ApplicationSkillProjectionRepair {
    name: String,
    source: String,
}

impl AgentPluginGeneration {
    pub(crate) fn new(
        revision: u64,
        skill_descriptors: Vec<echo_agent::skills::external::SkillDescriptor>,
        plugin_agents: Vec<PreparedPluginAgent>,
        output_style: Option<String>,
    ) -> Self {
        Self {
            revision,
            skill_descriptors,
            plugin_agents,
            output_style,
            framework_generation: None,
        }
    }

    pub(crate) fn with_framework_generation(
        mut self,
        generation: Option<Arc<echo_agent::plugin::PreparedPluginSet>>,
    ) -> Self {
        self.framework_generation = generation;
        self
    }

    pub(crate) fn framework_generation(
        &self,
    ) -> Option<Arc<echo_agent::plugin::PreparedPluginSet>> {
        self.framework_generation.clone()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// Configuration for the agent pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of concurrent agents in the pool.
    pub max_agents: usize,
    /// Duration after which an idle agent is eligible for eviction.
    pub idle_timeout: Duration,
    /// Whether to pre-create a dedicated background task agent.
    pub enable_background_agent: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_agents: 10,
            idle_timeout: Duration::from_secs(1800), // 30 minutes
            enable_background_agent: true,
        }
    }
}

/// Errors that can occur during pool operations.
#[derive(Debug)]
pub enum PoolError {
    /// The pool has reached its maximum number of agents.
    PoolFull { max: usize },
    /// Failed to create a new agent.
    AgentCreation(String),
    /// Workspace transition owns the pool admission boundary.
    WorkspaceTransition,
    /// Application shutdown permanently closed pool admission.
    ShuttingDown,
    /// The in-process execution lease counter cannot admit another owner.
    ExecutionLeaseCapacity,
    /// The caller attempted to retire a pool entry with another key/pool's receipt.
    ExecutionLeaseMismatch,
    /// The caller completed a retirement receipt against another pool.
    RetirementReceiptMismatch,
    /// Durable conversation deletion owns this identity until its finalizer retires.
    ConversationDeletionPending {
        conversation_id: String,
        reason: String,
    },
    /// An exact cached generation is settling before the key can be reused.
    ConversationRetirementPending { conversation_id: String },
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::PoolFull { max } => {
                write!(f, "Agent pool full (max: {})", max)
            }
            PoolError::AgentCreation(msg) => {
                write!(f, "Failed to create pool agent: {}", msg)
            }
            PoolError::WorkspaceTransition => {
                write!(
                    f,
                    "Agent pool admission is suspended for a workspace transition"
                )
            }
            PoolError::ShuttingDown => {
                write!(f, "Agent pool is shutting down")
            }
            PoolError::ExecutionLeaseCapacity => {
                write!(f, "Agent pool execution lease capacity exhausted")
            }
            PoolError::ExecutionLeaseMismatch => {
                write!(f, "Agent pool execution lease does not own this pool entry")
            }
            PoolError::RetirementReceiptMismatch => {
                write!(f, "Agent pool retirement receipt belongs to another pool")
            }
            PoolError::ConversationDeletionPending {
                conversation_id,
                reason,
            } => write!(
                f,
                "Conversation {conversation_id} cannot acquire an agent: {reason}"
            ),
            PoolError::ConversationRetirementPending { conversation_id } => write!(
                f,
                "Conversation {conversation_id} is retiring its cached agent generation"
            ),
        }
    }
}

impl std::error::Error for PoolError {}

/// Resources extracted from the primary agent that can be shared across
/// multiple pool agents. All fields are `Arc`-wrapped for thread-safe sharing.
pub struct SharedResources {
    pub tool_manager: Option<Arc<echo_agent::tools::ToolManager>>,
    pub hook_registry: Option<Arc<tokio::sync::RwLock<echo_agent::skills::hooks::HookRegistry>>>,
    pub sandbox_manager: Option<Arc<echo_agent::sandbox::SandboxManager>>,
    pub store: Option<Arc<dyn echo_agent::memory::Store>>,
    pub conversation_store: Option<Arc<dyn echo_agent::memory::ConversationStore>>,
    pub run_store: Option<Arc<dyn echo_agent::trace::RunStore>>,
    pub token_tracker: Option<Arc<echo_agent::tokenizer::TokenUsageTracker>>,
    pub permission_service: Option<Arc<echo_agent::human_loop::service::PermissionService>>,
    pub state_store: Option<Arc<dyn echo_agent::state::RuntimeStateStore>>,
    pub tool_execution_pipeline:
        Option<Arc<echo_agent::agent::react::run::pipeline::ToolExecutionPipeline>>,
    pub review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    /// TaskRuntime store handle. When present, pool agents get the task
    /// management tools (task_create/task_update/task_list) registered so
    /// the main agent can autonomously manage its plan during execution.
    pub task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    pub browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    pub command_cell_runtime:
        Option<Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>>,
    pub product_data_io: Option<crate::product_data_io::ProductDataIoService>,
    pub execution_scope: Option<crate::workspace::WorkspaceExecutionScope>,
}

pub(crate) struct WorkspaceAgentPoolResources {
    pub root: std::path::PathBuf,
    pub kind: WorkspaceKind,
    pub conversation_store: Arc<dyn echo_agent::memory::ConversationStore>,
    pub state_store: Arc<dyn echo_agent::state::RuntimeStateStore>,
    pub memory_store: Arc<dyn echo_agent::memory::Store>,
    pub task_runtime_store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    pub review_integration: Arc<crate::evolution::ReviewIntegration>,
    pub execution_scope: crate::workspace::WorkspaceExecutionScope,
    pub workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
}

impl SharedResources {
    /// Extract shareable resources from a fully-initialized agent handle.
    ///
    /// This reads through the agent's subsystems and clones the `Arc` handles
    /// without duplicating the underlying data.
    pub async fn extract_from(
        agent: &AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    ) -> Self {
        agent
            .read(|a| {
                let tool_manager = Some(a.tool_manager().clone());
                let hook_registry = Some(a.hook_registry().clone());
                let sandbox_manager = a.sandbox_manager().cloned();
                let token_tracker = Some(a.token_tracker().clone());
                let store = a.store().cloned();
                let conversation_store = a.conversation_store().clone();
                let state_store = a.state_store().clone();
                let run_store = a.run_store().cloned();
                let tool_execution_pipeline = a.tool_execution_pipeline().clone();
                let permission_service = a.permission_service().cloned();

                SharedResources {
                    tool_manager,
                    hook_registry,
                    sandbox_manager,
                    store,
                    conversation_store,
                    run_store,
                    token_tracker,
                    permission_service,
                    state_store,
                    tool_execution_pipeline,
                    review_integration,
                    // TaskRuntimeStore is not part of the agent handle — it lives
                    // in AppState. extract_from leaves it None; the caller
                    // (AppState / pool init) injects it separately so pooled
                    // agents can register task-management tools.
                    task_runtime_store: None,
                    browser_runtime: None,
                    command_cell_runtime: None,
                    product_data_io: None,
                    execution_scope: None,
                }
            })
            .await
    }
}

/// Internal wrapper around a pooled agent with metadata.
struct PooledAgent {
    handle: AgentHandle,
    model_consumers: infra::AgentModelConsumers,
    _conversation_id: String,
    created_at: Instant,
    last_used: Instant,
}

struct AgentPoolAdmission {
    active: Mutex<AgentPoolAdmissionState>,
    idle: Notify,
}

struct AgentPoolAdmissionState {
    accepting: bool,
    total: usize,
    by_key: HashMap<String, usize>,
    process_permits: HashMap<String, tokio::sync::OwnedSemaphorePermit>,
    retiring: HashSet<String>,
}

impl Default for AgentPoolAdmission {
    fn default() -> Self {
        Self {
            active: Mutex::new(AgentPoolAdmissionState {
                accepting: true,
                total: 0,
                by_key: HashMap::new(),
                process_permits: HashMap::new(),
                retiring: HashSet::new(),
            }),
            idle: Notify::new(),
        }
    }
}

impl AgentPoolAdmission {
    fn issue(
        self: &Arc<Self>,
        key: &str,
        agent: AgentHandle,
        process_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.accepting {
            return Err(PoolError::ShuttingDown);
        }
        if active.retiring.contains(key) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: key.to_string(),
            });
        }
        let total = active
            .total
            .checked_add(1)
            .ok_or(PoolError::ExecutionLeaseCapacity)?;
        let key_count = active
            .by_key
            .get(key)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or(PoolError::ExecutionLeaseCapacity)?;
        if key_count == 1 {
            let permit = process_permit.ok_or(PoolError::ExecutionLeaseCapacity)?;
            active.process_permits.insert(key.to_string(), permit);
        }
        active.total = total;
        active.by_key.insert(key.to_string(), key_count);
        drop(active);
        Ok(AgentPoolExecutionLease {
            agent,
            admission: Some((Arc::clone(self), key.to_string())),
        })
    }

    fn issue_process_scoped(
        self: &Arc<Self>,
        key: &str,
        agent: AgentHandle,
        governor: &Arc<AgentExecutionGovernor>,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        if self.is_active(key) {
            match self.issue(key, agent.clone(), None) {
                Ok(lease) => return Ok(lease),
                Err(PoolError::ExecutionLeaseCapacity) => {}
                Err(error) => return Err(error),
            }
        }
        let permit = match governor.semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::NoPermits) if self.is_active(key) => {
                return self.issue(key, agent, None);
            }
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                return Err(PoolError::ExecutionLeaseCapacity);
            }
            Err(tokio::sync::TryAcquireError::Closed) => return Err(PoolError::ShuttingDown),
        };
        self.issue(key, agent, Some(permit))
    }

    fn is_active(&self, key: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_key
            .get(key)
            .is_some_and(|count| *count != 0)
    }

    fn is_retiring(&self, key: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retiring
            .contains(key)
    }

    fn begin_retirement(
        self: &Arc<Self>,
        key: &str,
    ) -> Result<AgentPoolRetirementAdmission, PoolError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.accepting {
            return Err(PoolError::ShuttingDown);
        }
        if !active.retiring.insert(key.to_string()) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: key.to_string(),
            });
        }
        drop(active);
        Ok(AgentPoolRetirementAdmission {
            admission: Arc::clone(self),
            key: key.to_string(),
            active: true,
        })
    }

    async fn wait_key_idle(&self, key: &str) {
        loop {
            let notified = self.idle.notified();
            if !self.is_active(key) {
                return;
            }
            notified.await;
        }
    }

    fn close(&self) {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .accepting = false;
    }

    async fn wait_until_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .total
                == 0
            {
                return;
            }
            notified.await;
        }
    }
}

struct AgentPoolRetirementAdmission {
    admission: Arc<AgentPoolAdmission>,
    key: String,
    active: bool,
}

/// Exclusive admission receipt for retiring one exact conversation key.
///
/// While this value is alive, new leases for the key fail closed. Dropping it
/// before completion reopens admission without claiming that the old Agent was
/// removed, so callers can safely cancel and retry a higher-level barrier.
#[must_use]
pub struct AgentPoolConversationRetirement {
    key: String,
    admission: AgentPoolRetirementAdmission,
}

impl Drop for AgentPoolRetirementAdmission {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.admission
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retiring
            .remove(&self.key);
        self.admission.idle.notify_waiters();
        self.active = false;
    }
}

/// Application execution receipt for one pooled agent generation.
///
/// The lease is intentionally not cloneable. Callers may clone the contained
/// framework handle for APIs that require ownership, but must retain this
/// receipt until the corresponding chat/run has reached its terminal state.
/// Workspace transition closes pool admission and waits for every issued
/// receipt to drop before clearing or rebinding pooled agents.
#[must_use]
pub struct AgentPoolExecutionLease {
    agent: AgentHandle,
    admission: Option<(Arc<AgentPoolAdmission>, String)>,
}

impl AgentPoolExecutionLease {
    pub fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    pub(crate) fn unpooled(agent: AgentHandle) -> Self {
        Self {
            agent,
            admission: None,
        }
    }

    fn owns(&self, admission: &Arc<AgentPoolAdmission>, key: &str) -> bool {
        self.admission
            .as_ref()
            .is_some_and(|(owner, owned_key)| Arc::ptr_eq(owner, admission) && owned_key == key)
    }
}

impl Drop for AgentPoolExecutionLease {
    fn drop(&mut self) {
        let Some((admission, key)) = self.admission.take() else {
            return;
        };
        let mut active = admission
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.total = active.total.saturating_sub(1);
        let mut release_process_permit = false;
        if let Some(count) = active.by_key.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                active.by_key.remove(&key);
                release_process_permit = true;
            }
        }
        let process_permit = release_process_permit
            .then(|| active.process_permits.remove(&key))
            .flatten();
        let released_key = !active.by_key.contains_key(&key);
        let released_last = active.total == 0;
        drop(active);
        drop(process_permit);
        if released_key || released_last {
            admission.idle.notify_waiters();
        }
    }
}

impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for AgentPoolExecutionLease {
    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            drop(self);
        })
    }
}

/// TaskRuntime-owned pool receipt. The canonical RunDriver supervisor awaits
/// release after durable run settlement while this value retains execution
/// admission through task-specific pool removal.
pub(crate) struct OwnedRunPoolReceipt {
    pool: Arc<AgentPool>,
    key: String,
    execution: Option<AgentPoolExecutionLease>,
}

impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for OwnedRunPoolReceipt {
    fn release(mut self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            if let Some(execution) = self.execution.take() {
                self.pool
                    .release_supervised_execution(&self.key, execution)
                    .await;
            }
        })
    }
}

impl PooledAgent {
    fn new(
        handle: AgentHandle,
        model_consumers: infra::AgentModelConsumers,
        conversation_id: String,
    ) -> Self {
        let now = Instant::now();
        Self {
            handle,
            model_consumers,
            _conversation_id: conversation_id,
            created_at: now,
            last_used: now,
        }
    }
}

/// Pool of `ReactAgent` instances that share expensive resources while
/// maintaining isolated execution contexts.
///
/// Each agent in the pool has its own `execution_mutex` and `ContextManager`,
/// enabling true parallel execution of multiple conversations.
pub struct AgentPool {
    shared: SharedResources,
    agents: RwLock<HashMap<String, PooledAgent>>,
    /// Primary Agent owned by this pool generation. Workspace forks create a
    /// dedicated primary; the bootstrap pool references the process primary.
    primary_agent: RwLock<Option<AgentHandle>>,
    /// Model consumers for a primary that is owned by this pool. The bootstrap
    /// primary remains owned by `AppState`; workspace primary Agents are
    /// published through the same pool transaction as cached conversation Agents.
    primary_model_consumers: RwLock<Option<infra::AgentModelConsumers>>,
    /// Latest durable user MCP snapshot for future Agents and future workspace
    /// forks. Live ToolManagers are reconciled separately by McpConfigRuntime.
    mcp_config_snapshot: RwLock<Option<McpConfigFile>>,
    workspace_transitioning: AtomicBool,
    shutting_down: AtomicBool,
    admission: Arc<AgentPoolAdmission>,
    process_agent_execution: Arc<AgentExecutionGovernor>,
    config: PoolConfig,
    app_config: RwLock<EkoConfig>,
    /// Working directory applied to existing and future pooled agents.
    working_dir: RwLock<Option<std::path::PathBuf>>,
    permission_mode: RwLock<PermissionMode>,
    /// Exact plugin generation projected into existing and future agents.
    agent_generation: RwLock<AgentPluginGeneration>,
    /// Cancellation token for the cleanup monitor task.
    cleanup_cancel: CancellationToken,
    /// Sole owned cleanup monitor settlement handle. The monitor holds only a
    /// weak pool reference so a failed bootstrap cannot keep the pool alive.
    cleanup_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Workspace-scoped conversation store used by existing and future agents.
    conversation_store_override: RwLock<Option<Arc<dyn echo_agent::memory::ConversationStore>>>,
    /// Workspace-scoped runtime-state store used by existing and future agents.
    state_store_override: RwLock<Option<Arc<dyn echo_agent::state::RuntimeStateStore>>>,
    /// Product-owned complete tool-output artifact policy for existing and
    /// future pooled agents. Updated together with workspace routing.
    tool_output_artifacts: RwLock<echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    /// Active workspace profile applied to existing and future pooled agents.
    workspace_kind: RwLock<WorkspaceKind>,
    /// Last strictly-read instruction generation. Existing and future pool
    /// agents are always projected from this same snapshot.
    instruction_projection: RwLock<Option<crate::unified_memory::InstructionProjectionSnapshot>>,
    /// Shared EKO user policy for tool visibility. Workspace forks retain the
    /// same service; each pool projects its generation into live/future Agents.
    tool_control: Arc<crate::tool_control::ToolControlService>,
    /// Explicit Mock transport used only by integration tests that must fork
    /// real workspace pools without contacting an external model provider.
    #[cfg(test)]
    llm_client_override: RwLock<Option<Arc<dyn echo_agent::llm::LlmClient>>>,
}

pub(crate) struct AgentPoolWorkspaceTransition<'a> {
    pool: &'a AgentPool,
    committed: bool,
}

/// Exact pool-wide receipt prepared before config persistence.
///
/// The agents write guard prevents eviction or creation while every existing
/// agent generation is admitted. Dropping this value rolls back all prepared
/// context receipts without touching live or future pool state.
pub(crate) struct PreparedAgentPoolModelPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    _agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    publications: Vec<infra::PreparedAgentModelPublication>,
    app_config: EkoConfig,
    runtime: ModelRuntimeConfig,
}

pub(crate) struct PreparedAgentPoolModelDeactivation<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    _agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    publications: Vec<infra::PreparedAgentModelDeactivation>,
    app_config: EkoConfig,
}

/// Pool-wide plugin publication. The existing workspace-transition admission
/// guard prevents new leases and waits for current executions to settle while
/// every cached agent is moved to the same candidate generation.
pub(crate) struct PreparedAgentPoolPluginPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    previous: AgentPluginGeneration,
    candidate: Option<AgentPluginGeneration>,
    application_skill_repair: Option<ApplicationSkillProjectionRepair>,
}

/// Pool-wide instruction publication under the existing execution admission.
pub(crate) struct PreparedAgentPoolInstructionPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    candidate: Option<crate::unified_memory::InstructionProjectionSnapshot>,
}

impl PreparedAgentPoolModelPublication<'_> {
    pub(crate) async fn commit(self) {
        let Self {
            pool,
            _transition,
            _agents,
            publications,
            app_config,
            runtime,
        } = self;
        for publication in publications {
            publication.commit().await;
        }
        *pool.app_config.write().await = app_config;
        tracing::info!(
            provider = %runtime.provider,
            model = %runtime.model,
            pooled_agents = _agents.len(),
            "AgentPool: prepared runtime generation committed"
        );
    }
}

impl PreparedAgentPoolModelDeactivation<'_> {
    pub(crate) async fn commit(self) {
        let Self {
            pool,
            _transition,
            _agents,
            publications,
            app_config,
        } = self;
        for publication in publications {
            publication.commit().await;
        }
        *pool.app_config.write().await = app_config;
        tracing::info!(
            pooled_agents = _agents.len(),
            "AgentPool: active model removed from pooled agents"
        );
    }
}

impl PreparedAgentPoolPluginPublication<'_> {
    pub(crate) async fn prepare(&mut self, candidate: AgentPluginGeneration) -> Result<(), String> {
        self.prepare_inner(candidate, None).await
    }

    pub(crate) async fn prepare_application_skill(
        &mut self,
        candidate: AgentPluginGeneration,
        name: &str,
        source: &str,
    ) -> Result<(), String> {
        self.prepare_inner(
            candidate,
            Some(ApplicationSkillProjectionRepair {
                name: name.to_string(),
                source: source.to_string(),
            }),
        )
        .await
    }

    async fn prepare_inner(
        &mut self,
        candidate: AgentPluginGeneration,
        application_skill_repair: Option<ApplicationSkillProjectionRepair>,
    ) -> Result<(), String> {
        if self.candidate.is_some() {
            return Err("AgentPool plugin publication is already prepared".to_string());
        }
        if let Some(repair) = application_skill_repair.as_ref()
            && candidate.skill_descriptors.iter().any(|descriptor| {
                descriptor.source.as_deref() == Some(repair.source.as_str())
                    && descriptor.name != repair.name
            })
        {
            return Err(format!(
                "application skill source '{}' contains a descriptor other than '{}'",
                repair.source, repair.name
            ));
        }

        let mut applied = Vec::new();
        let mut pooled_agents = self.agents.iter().collect::<Vec<_>>();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (conversation_id, pooled) in pooled_agents {
            if let Err(error) = replace_agent_plugin_generation(
                &pooled.handle,
                &self.previous,
                &candidate,
                application_skill_repair.as_ref(),
            )
            .await
            {
                let mut errors = vec![format!("{conversation_id}: {error}")];
                for (applied_id, applied_handle) in applied.into_iter().rev() {
                    if let Err(rollback_error) = replace_agent_plugin_generation(
                        &applied_handle,
                        &candidate,
                        &self.previous,
                        application_skill_repair.as_ref(),
                    )
                    .await
                    {
                        errors.push(format!("rollback {applied_id}: {rollback_error}"));
                    }
                }
                return Err(format!(
                    "AgentPool plugin generation preparation failed: {}",
                    errors.join("; ")
                ));
            }
            applied.push((conversation_id.clone(), pooled.handle.clone()));
        }

        self.candidate = Some(candidate);
        self.application_skill_repair = application_skill_repair;
        Ok(())
    }

    pub(crate) async fn commit(&mut self) -> Result<(), String> {
        let candidate = self.candidate.as_ref().ok_or_else(|| {
            "AgentPool plugin publication cannot commit before preparation".to_string()
        })?;
        let revision = candidate.revision;
        *self.pool.agent_generation.write().await = candidate.clone();
        tracing::info!(
            revision,
            pooled_agents = self.agents.len(),
            "AgentPool: plugin generation committed"
        );
        Ok(())
    }

    pub(crate) async fn rollback(&mut self) -> Result<(), String> {
        let Some(candidate) = self.candidate.take() else {
            return Ok(());
        };
        let mut errors = Vec::new();
        let mut pooled_agents = self.agents.iter().collect::<Vec<_>>();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (conversation_id, pooled) in pooled_agents {
            if let Err(error) = replace_agent_plugin_generation(
                &pooled.handle,
                &candidate,
                &self.previous,
                self.application_skill_repair.as_ref(),
            )
            .await
            {
                errors.push(format!("{conversation_id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "AgentPool plugin generation rollback failed: {}",
                errors.join("; ")
            ))
        }
    }
}

impl PreparedAgentPoolInstructionPublication<'_> {
    pub(crate) async fn prepare(
        &mut self,
        candidate: crate::unified_memory::InstructionProjectionSnapshot,
    ) -> Result<(), String> {
        if self.candidate.is_some() {
            return Err("AgentPool instruction publication is already prepared".to_string());
        }
        for pooled in self.agents.values() {
            let snapshot = candidate.clone();
            pooled
                .handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::unified_memory::apply_instruction_projection_snapshot(
                            agent, &snapshot,
                        )
                        .await;
                    })
                })
                .await;
        }
        self.candidate = Some(candidate);
        Ok(())
    }

    pub(crate) async fn commit(mut self) -> Result<(), String> {
        let candidate = self.candidate.take().ok_or_else(|| {
            "AgentPool instruction publication cannot commit before preparation".to_string()
        })?;
        tracing::info!(
            revision = candidate.revision(),
            pooled_agents = self.agents.len(),
            "AgentPool: instruction projection generation committed"
        );
        *self.pool.instruction_projection.write().await = Some(candidate);
        Ok(())
    }
}

impl AgentPoolWorkspaceTransition<'_> {
    #[cfg(test)]
    pub(crate) async fn commit(&mut self) {
        if self.committed {
            return;
        }
        let mut agents = self.pool.agents.write().await;
        let count = agents.len();
        agents.clear();
        self.committed = true;
        tracing::info!(
            agents_cleared = count,
            "AgentPool: cleared for workspace transition"
        );
    }

    pub(crate) async fn publish_instruction_snapshot(
        &self,
        expected_pool: &Arc<AgentPool>,
        snapshot: crate::unified_memory::InstructionProjectionSnapshot,
    ) -> Result<(), String> {
        if !std::ptr::eq(self.pool, expected_pool.as_ref()) {
            return Err("instruction snapshot targets a different AgentPool".to_string());
        }
        if !self.committed {
            return Err(
                "instruction snapshot cannot publish before the pool transition commits"
                    .to_string(),
            );
        }
        if !self.pool.agents.read().await.is_empty() {
            return Err(
                "instruction snapshot cannot publish while retired pool agents remain".to_string(),
            );
        }
        tracing::info!(
            revision = snapshot.revision(),
            "AgentPool: workspace instruction projection generation committed"
        );
        *self.pool.instruction_projection.write().await = Some(snapshot);
        Ok(())
    }
}

impl Drop for AgentPoolWorkspaceTransition<'_> {
    fn drop(&mut self) {
        self.pool
            .workspace_transitioning
            .store(false, Ordering::Release);
    }
}

impl AgentPool {
    pub(crate) fn retain_for_supervised_run(
        self: &Arc<Self>,
        key: String,
        execution: AgentPoolExecutionLease,
    ) -> OwnedRunPoolReceipt {
        OwnedRunPoolReceipt {
            pool: Arc::clone(self),
            key,
            execution: Some(execution),
        }
    }

    /// Create a pool from an already-bootstrapped `AgentRuntime`.
    ///
    /// Extracts shared resources from the runtime's primary agent and
    /// optionally pre-creates a background task agent.
    pub async fn from_runtime(
        runtime: &crate::runtime::AgentRuntime,
        config: PoolConfig,
        task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Arc<Self>> {
        let shared = SharedResources::extract_from(
            &runtime.agent_handle,
            runtime.review_integration.clone(),
        )
        .await;
        let mut shared = shared;
        shared.browser_runtime = Some(runtime.browser_runtime.clone());
        shared.task_runtime_store = task_runtime_store;
        shared.command_cell_runtime = Some(runtime.command_cell_runtime.clone());
        shared.product_data_io = Some(runtime.product_data_io.clone());

        // Extract skill descriptors from primary agent (avoids re-reading from disk)
        let skill_descriptors = runtime.agent_handle.read(|a| a.skill_descriptors()).await;
        let tool_output_artifacts = runtime
            .agent_handle
            .read(|agent| agent.tool_output_artifacts())
            .await
            .unwrap_or_else(|| crate::infra::tool_output_artifact_config(None));
        let working_dir = runtime.agent_handle.read(|agent| agent.working_dir()).await;
        shared.execution_scope = Some(crate::workspace::WorkspaceExecutionScope::global(
            working_dir
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        ));

        let pool = Arc::new(Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(Some(runtime.agent_handle.clone())),
            primary_model_consumers: RwLock::new(Some(runtime.model_consumers.clone())),
            mcp_config_snapshot: RwLock::new(Some(runtime.mcp_config_runtime.snapshot().await)),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: PROCESS_AGENT_EXECUTION.clone(),
            config,
            app_config: RwLock::new(runtime.session_app_config.clone()),
            working_dir: RwLock::new(working_dir),
            permission_mode: RwLock::new(PermissionMode::Default),
            agent_generation: RwLock::new(AgentPluginGeneration::new(
                0,
                skill_descriptors,
                Vec::new(),
                None,
            )),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(tool_output_artifacts),
            workspace_kind: RwLock::new(WorkspaceKind::General),
            instruction_projection: RwLock::new(None),
            tool_control: Arc::new(crate::tool_control::ToolControlService::default()),
            #[cfg(test)]
            llm_client_override: RwLock::new(None),
        });

        // Bind before creating the background agent so it and every later
        // conversation start from PluginRuntimeService's committed catalog.
        runtime
            .plugin_runtime
            .bind_agent_pool(Arc::downgrade(&pool))
            .await?;
        if let Some(review_integration) = runtime.review_integration.as_ref() {
            review_integration.bind_rule_projection_pool(&pool).await?;
        }

        // Pre-create background agent if enabled
        if pool.config.enable_background_agent {
            match pool.create_agent("__background__").await {
                Ok(pooled) => {
                    let mut agents = pool.agents.write().await;
                    agents.insert("__background__".to_string(), pooled);
                    tracing::info!("AgentPool: background agent created");
                }
                Err(e) => {
                    tracing::warn!("AgentPool: failed to create background agent: {e}");
                }
            }
        }

        Ok(pool)
    }

    /// Fork an independently admitted pool for one immutable workspace host.
    ///
    /// Expensive process-safe primitives remain shared, while every resource
    /// whose contents or tool behavior depend on workspace identity is replaced
    /// by the host-owned instance. Agents inside one host share that host's
    /// ToolManager (including its MCP clients); different hosts never share it.
    pub(crate) async fn fork_for_workspace(
        &self,
        resources: WorkspaceAgentPoolResources,
    ) -> anyhow::Result<(
        Arc<Self>,
        Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
        Arc<crate::mcp_config_runtime::McpNameOwnershipRegistry>,
    )> {
        let WorkspaceAgentPoolResources {
            root,
            kind,
            conversation_store,
            state_store,
            memory_store,
            task_runtime_store,
            review_integration,
            execution_scope,
            workspace_io_identity,
        } = resources;
        let plugin_target_scope = format!(
            "{}@{}",
            execution_scope.workspace_id(),
            workspace_io_identity.host_generation()
        );
        let authority_plugin_generation = self.agent_generation.read().await.clone();
        let mcp_config_snapshot = self.mcp_config_snapshot.read().await.clone();
        let shared = SharedResources {
            tool_manager: None,
            hook_registry: None,
            sandbox_manager: self.shared.sandbox_manager.clone(),
            store: Some(memory_store),
            conversation_store: Some(conversation_store),
            run_store: self.shared.run_store.clone(),
            token_tracker: self.shared.token_tracker.clone(),
            permission_service: self.shared.permission_service.clone(),
            state_store: Some(state_store),
            tool_execution_pipeline: self.shared.tool_execution_pipeline.clone(),
            review_integration: Some(review_integration),
            task_runtime_store: Some(task_runtime_store.clone()),
            browser_runtime: self.shared.browser_runtime.clone(),
            command_cell_runtime: self.shared.command_cell_runtime.clone(),
            product_data_io: self.shared.product_data_io.clone(),
            execution_scope: Some(execution_scope),
        };
        let workspace_product_data_io = shared.product_data_io.clone();
        let mut pool = Arc::new(Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(None),
            primary_model_consumers: RwLock::new(None),
            mcp_config_snapshot: RwLock::new(mcp_config_snapshot.clone()),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: self.process_agent_execution.clone(),
            config: self.config.clone(),
            app_config: RwLock::new(self.app_config.read().await.clone()),
            working_dir: RwLock::new(Some(root.clone())),
            permission_mode: RwLock::new(*self.permission_mode.read().await),
            agent_generation: RwLock::new(authority_plugin_generation.clone()),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(crate::infra::tool_output_artifact_config(Some(
                &root,
            ))),
            workspace_kind: RwLock::new(kind),
            instruction_projection: RwLock::new(self.instruction_projection.read().await.clone()),
            tool_control: crate::tool_control::shared(&self.tool_control),
            #[cfg(test)]
            llm_client_override: RwLock::new(self.llm_client_override.read().await.clone()),
        });

        let primary = pool.create_agent("__workspace_primary__").await?;
        let app_config = self.app_config.read().await.clone();
        crate::infra::load_user_hooks(&primary.handle, &app_config, Some(root.as_path())).await;
        let lsp_runtime = if mcp_config_snapshot.is_some() {
            Some(crate::runtime::register_lsp_tools(&primary.handle, &root).await)
        } else {
            None
        };
        primary
            .handle
            .write(move |agent| {
                if let Some(product_data_io) = workspace_product_data_io {
                    crate::research_connectors::install_auto_ingest_tools(
                        agent,
                        workspace_io_identity.clone(),
                        product_data_io.clone(),
                    );
                    agent.add_tool(Box::new(crate::research_tool::ResearchLibraryTool::new(
                        product_data_io,
                        workspace_io_identity,
                    )));
                }
            })
            .await;
        let primary_tool_manager = primary
            .handle
            .read(|agent| agent.tool_manager().clone())
            .await;
        let primary_hook_registry = primary
            .handle
            .read(|agent| agent.hook_registry().clone())
            .await;
        let pool_mut = Arc::get_mut(&mut pool).ok_or_else(|| {
            anyhow::anyhow!("workspace AgentPool escaped before host resources were installed")
        })?;
        pool_mut.shared.tool_manager = Some(primary_tool_manager);
        pool_mut.shared.hook_registry = Some(primary_hook_registry);
        *pool.primary_agent.write().await = Some(primary.handle.clone());
        *pool.primary_model_consumers.write().await = Some(primary.model_consumers.clone());
        crate::tasks::task_runtime::bind_task_execute_to_pool(
            &primary.handle,
            task_runtime_store,
            &pool,
        )
        .await;
        let (plugin_runtime, mcp_ownership) = match (lsp_runtime, mcp_config_snapshot.as_ref()) {
            (Some(lsp_runtime), Some(mcp_config)) => {
                let ownership = crate::mcp_config_runtime::McpNameOwnershipRegistry::new(
                    mcp_config.mcp_servers.keys().cloned(),
                );
                let runtime = crate::plugin_runtime::PluginRuntimeService::new_for_scope(
                    primary.handle.clone(),
                    lsp_runtime,
                    Arc::clone(&ownership),
                    plugin_target_scope,
                    Some(authority_plugin_generation.clone()),
                )
                .await?;
                runtime.bind_agent_pool(Arc::downgrade(&pool)).await?;
                (Some(runtime), ownership)
            }
            _ => (
                None,
                crate::mcp_config_runtime::McpNameOwnershipRegistry::new(Vec::<String>::new()),
            ),
        };
        crate::infra::fire_startup_hook(&primary.handle).await;

        if pool.config.enable_background_agent {
            match pool.create_agent("__background__").await {
                Ok(pooled) => {
                    pool.agents
                        .write()
                        .await
                        .insert("__background__".to_string(), pooled);
                }
                Err(error) => {
                    tracing::warn!(%error, "workspace AgentPool background agent unavailable");
                }
            }
        }
        pool.spawn_cleanup_monitor().await;
        Ok((pool, plugin_runtime, mcp_ownership))
    }

    #[cfg(test)]
    pub(crate) async fn for_model_mutation_test(
        primary: &AgentHandle,
        app_config: EkoConfig,
    ) -> Self {
        Self::new_for_test_with_config(primary, None, None, 8, false, app_config).await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent: AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
        store: Option<Arc<dyn echo_agent::memory::Store>>,
        max_agents: usize,
        enable_background_agent: bool,
    ) -> Self {
        let mut app_config = EkoConfig::default();
        app_config.model.provider = "test".to_string();
        app_config.model.name = "test-model".to_string();
        app_config.model.base_url = Some("http://127.0.0.1:11434/v1/chat/completions".to_string());
        Self::new_for_test_with_config(
            &agent,
            review_integration,
            store,
            max_agents,
            enable_background_agent,
            app_config,
        )
        .await
    }

    #[cfg(test)]
    async fn new_for_test_with_config(
        agent: &AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
        store: Option<Arc<dyn echo_agent::memory::Store>>,
        max_agents: usize,
        enable_background_agent: bool,
        app_config: EkoConfig,
    ) -> Self {
        let mut shared = SharedResources::extract_from(agent, review_integration).await;
        if let Some(store) = store {
            shared.store = Some(store);
        }
        Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(Some(agent.clone())),
            primary_model_consumers: RwLock::new(None),
            mcp_config_snapshot: RwLock::new(None),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: Arc::new(AgentExecutionGovernor::new(
                PROCESS_AGENT_EXECUTION_LIMIT,
            )),
            config: PoolConfig {
                max_agents,
                idle_timeout: Duration::from_secs(1800),
                enable_background_agent,
            },
            app_config: RwLock::new(app_config),
            working_dir: RwLock::new(None),
            permission_mode: RwLock::new(PermissionMode::Default),
            agent_generation: RwLock::new(AgentPluginGeneration::default()),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(crate::infra::tool_output_artifact_config(None)),
            workspace_kind: RwLock::new(WorkspaceKind::General),
            instruction_projection: RwLock::new(None),
            tool_control: Arc::new(crate::tool_control::ToolControlService::default()),
            #[cfg(test)]
            llm_client_override: RwLock::new(None),
        }
    }

    #[cfg(test)]
    pub async fn set_llm_client_override_for_test(
        &self,
        client: Arc<dyn echo_agent::llm::LlmClient>,
    ) {
        *self.llm_client_override.write().await = Some(client);
    }

    /// Whether this key consumes one user-conversation capacity slot.
    fn is_conversation_agent(key: &str) -> bool {
        key != "__background__"
            && key != "__workspace_primary__"
            && !key.starts_with("__task__:")
            && !key.starts_with("__continuation__:")
    }

    /// Whether this key consumes one internal continuation capacity slot.
    fn is_continuation_agent(key: &str) -> bool {
        key.starts_with("__continuation__:")
    }

    /// Capacity is isolated by product ownership: foreground conversations
    /// cannot evict continuations, and continuations cannot evict conversations.
    fn shares_capacity_class(candidate: &str, requested: &str) -> bool {
        (Self::is_conversation_agent(requested) && Self::is_conversation_agent(candidate))
            || (Self::is_continuation_agent(requested) && Self::is_continuation_agent(candidate))
    }

    /// Acquire an agent for a given conversation ID.
    ///
    /// If an agent already exists for this ID, it is returned (with updated
    /// `last_used` timestamp). Otherwise, a new agent is created and added
    /// to the pool. Foreground conversations and internal continuations each
    /// have an independent capacity limit; task subagents and the background
    /// agent have separate product ownership.
    ///
    /// The write lock is held across the entire operation (including async
    /// agent creation) to prevent TOCTOU races between concurrent acquirers.
    pub async fn acquire(
        &self,
        conversation_id: &str,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let mut agents = self.agents.write().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        if self.admission.is_retiring(conversation_id) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: conversation_id.to_string(),
            });
        }

        // Fast path: reuse existing agent
        if let Some(existing) = agents.get_mut(conversation_id) {
            existing.last_used = Instant::now();
            let permission_mode = *self.permission_mode.read().await;
            let _updated = existing.handle.try_write(|agent| {
                if agent.get_permission_mode() != permission_mode {
                    agent.set_permission_mode(permission_mode);
                }
            });
            let handle = existing.handle.clone();
            let lease = self.admission.issue_process_scoped(
                conversation_id,
                handle,
                &self.process_agent_execution,
            )?;
            drop(agents);
            return Ok(lease);
        }

        // Enforce the requested class limit and evict only from that class.
        // Dedicated background and task subagents own separate admission paths.
        let capacity_limited = Self::is_conversation_agent(conversation_id)
            || Self::is_continuation_agent(conversation_id);
        let active_count = agents
            .keys()
            .filter(|candidate| Self::shares_capacity_class(candidate, conversation_id))
            .count();
        if capacity_limited && active_count >= self.config.max_agents {
            // Find the oldest inactive agent in the requested capacity class.
            let mut candidates: Vec<(String, Instant)> = agents
                .iter()
                .filter(|(id, _)| {
                    Self::shares_capacity_class(id, conversation_id)
                        && !self.admission.is_active(id)
                })
                .map(|(id, agent)| (id.clone(), agent.last_used))
                .collect();
            candidates.sort_by_key(|(_, ts)| *ts);

            let mut evicted = false;
            for (candidate_id, _) in &candidates {
                // Check if the agent is currently executing by trying to
                // acquire its execution_mutex. If try_lock succeeds, the
                // agent is idle and safe to evict.
                let is_idle = agents
                    .get(candidate_id)
                    .and_then(|pa| pa.handle.inner().try_read().ok())
                    .map(|guard| guard.execution_mutex().try_lock().is_ok())
                    .unwrap_or(false);

                if is_idle {
                    agents.remove(candidate_id);
                    tracing::info!(
                        conv_id = %candidate_id,
                        "AgentPool: evicted idle agent to make room"
                    );
                    evicted = true;
                    break;
                }
            }

            if !evicted {
                return Err(PoolError::PoolFull {
                    max: self.config.max_agents,
                });
            }
        }

        // Create new agent (lock is held — prevents concurrent insert races)
        let pooled = self
            .create_agent(conversation_id)
            .await
            .map_err(|e| PoolError::AgentCreation(e.to_string()))?;
        let handle = pooled.handle.clone();

        let lease = self.admission.issue_process_scoped(
            conversation_id,
            handle,
            &self.process_agent_execution,
        )?;
        agents.insert(conversation_id.to_string(), pooled);

        tracing::info!(
            conv_id = %conversation_id,
            pool_size = agents.len(),
            "AgentPool: new agent created"
        );
        drop(agents);
        Ok(lease)
    }

    /// Lease an existing agent without creating a new one.
    ///
    /// Returns `None` if no agent is allocated for this conversation ID.
    pub async fn lease_existing(
        &self,
        conversation_id: &str,
    ) -> Result<Option<AgentPoolExecutionLease>, PoolError> {
        let agents = self.agents.write().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        if self.admission.is_retiring(conversation_id) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: conversation_id.to_string(),
            });
        }
        let lease = agents
            .get(conversation_id)
            .map(|pooled| {
                self.admission.issue_process_scoped(
                    conversation_id,
                    pooled.handle.clone(),
                    &self.process_agent_execution,
                )
            })
            .transpose()?;
        drop(agents);
        Ok(lease)
    }

    /// Retire one cached agent using the exact execution receipt that owns it.
    /// The receipt and cache decision settle under the same pool lock, so reset
    /// cannot remove a generation still used by another accepted execution.
    pub async fn retire_execution(
        &self,
        conversation_id: &str,
        execution: AgentPoolExecutionLease,
    ) -> Result<bool, PoolError> {
        if !execution.owns(&self.admission, conversation_id) {
            return Err(PoolError::ExecutionLeaseMismatch);
        }
        Ok(self
            .release_supervised_execution(conversation_id, execution)
            .await)
    }

    /// Close admission for one conversation key, await every previously issued
    /// execution receipt, and remove that exact cached generation.
    ///
    /// New acquisitions fail with [`PoolError::ConversationRetirementPending`]
    /// until the operation settles. The admission guard is cancellation-safe:
    /// dropping the waiter reopens the key without claiming retirement, so a
    /// caller can retry rather than consuming a false terminal receipt.
    pub async fn retire_conversation_and_wait(
        &self,
        conversation_id: &str,
    ) -> Result<bool, PoolError> {
        let retirement = self.begin_conversation_retirement(conversation_id)?;
        self.complete_conversation_retirement(retirement).await
    }

    /// Close new admission for one exact conversation key before a caller
    /// settles its foreground owner and previously issued execution leases.
    pub fn begin_conversation_retirement(
        &self,
        conversation_id: &str,
    ) -> Result<AgentPoolConversationRetirement, PoolError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        let admission = self.admission.begin_retirement(conversation_id)?;
        Ok(AgentPoolConversationRetirement {
            key: conversation_id.to_string(),
            admission,
        })
    }

    /// Await old leases and remove the exact cached Agent protected by a
    /// receipt from [`Self::begin_conversation_retirement`].
    pub async fn complete_conversation_retirement(
        &self,
        retirement: AgentPoolConversationRetirement,
    ) -> Result<bool, PoolError> {
        let removed = self.drain_conversation_retirement(&retirement).await?;
        drop(retirement);
        Ok(removed)
    }

    /// Drain and remove one cached generation while retaining its closed
    /// admission receipt in the caller.
    ///
    /// Aggregate reset/delete owners use this form to keep the exact key closed
    /// through persisted runtime cleanup. Dropping `retirement` reopens the key
    /// only after their commit boundary.
    pub async fn drain_conversation_retirement(
        &self,
        retirement: &AgentPoolConversationRetirement,
    ) -> Result<bool, PoolError> {
        if !Arc::ptr_eq(&self.admission, &retirement.admission.admission) {
            return Err(PoolError::RetirementReceiptMismatch);
        }
        let conversation_id = retirement.key.clone();
        self.admission.wait_key_idle(&conversation_id).await;
        let mut agents = self.agents.write().await;
        let removed = agents.remove(&conversation_id).is_some();
        drop(agents);
        if removed {
            tracing::info!(
                conv_id = %conversation_id,
                "AgentPool: exact conversation generation retired after settlement"
            );
        }
        Ok(removed)
    }

    /// Release one exact supervised execution receipt. Dropping the receipt
    /// and deciding whether to remove the cached agent happen under the same
    /// agents lock used by acquire, so overlapping drivers for one key cannot
    /// remove each other's live agent.
    async fn release_supervised_execution(
        &self,
        conversation_id: &str,
        execution: AgentPoolExecutionLease,
    ) -> bool {
        let mut agents = self.agents.write().await;
        drop(execution);
        if self.admission.is_active(conversation_id) {
            return false;
        }
        let removed = agents.remove(conversation_id);
        if let Some(agent) = removed.as_ref() {
            tracing::info!(
                conv_id = %conversation_id,
                age_secs = agent.created_at.elapsed().as_secs(),
                "AgentPool: supervised agent released"
            );
        }
        removed.is_some()
    }

    #[cfg(test)]
    async fn background_agent(&self) -> Option<AgentHandle> {
        let agents = self.agents.read().await;
        agents.get("__background__").map(|pa| pa.handle.clone())
    }

    /// Update the pool's app config snapshot used for future agents.
    pub async fn update_app_config(&self, app_config: EkoConfig) {
        let _agents = self.agents.write().await;
        *self.app_config.write().await = app_config;
    }

    /// Publish the durable user MCP snapshot used by future conversation Agents
    /// and by workspace hosts opened after this generation commits.
    pub(crate) async fn update_mcp_config_snapshot(&self, snapshot: McpConfigFile) {
        *self.mcp_config_snapshot.write().await = Some(snapshot);
    }

    #[cfg(test)]
    pub(crate) async fn mcp_config_snapshot_for_test(&self) -> Option<McpConfigFile> {
        self.mcp_config_snapshot.read().await.clone()
    }

    /// Number of exact execution receipts currently retaining this pool.
    pub(crate) fn active_execution_count(&self) -> usize {
        self.admission
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .total
    }

    /// Admit every existing and future pool consumer before persistence.
    pub(crate) async fn prepare_model_publication(
        &self,
        app_config: EkoConfig,
        runtime: ModelRuntimeConfig,
        prepared: infra::PreparedRuntimeLlm,
    ) -> Result<PreparedAgentPoolModelPublication<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let token_limit = infra::effective_token_limit(&app_config, Some(&runtime));
        let primary_consumers = self.primary_model_consumers.read().await.clone();
        let primary_agent = self.primary_agent.read().await.clone();
        let mut publications = Vec::with_capacity(
            agents
                .len()
                .saturating_add(usize::from(primary_consumers.is_some())),
        );
        if let (Some(primary), Some(consumers)) = (primary_agent, primary_consumers) {
            publications.push(
                infra::prepare_agent_model_publication(
                    &primary,
                    consumers,
                    &runtime,
                    &prepared,
                    token_limit,
                )
                .await?,
            );
        }
        let mut pooled_agents: Vec<(&String, &PooledAgent)> = agents.iter().collect();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (_, pooled) in pooled_agents {
            publications.push(
                infra::prepare_agent_model_publication(
                    &pooled.handle,
                    pooled.model_consumers.clone(),
                    &runtime,
                    &prepared,
                    token_limit,
                )
                .await?,
            );
        }
        Ok(PreparedAgentPoolModelPublication {
            pool: self,
            _transition: transition,
            _agents: agents,
            publications,
            app_config,
            runtime,
        })
    }

    /// Admit every pooled agent before removing the final active model.
    pub(crate) async fn prepare_model_deactivation(
        &self,
        app_config: EkoConfig,
    ) -> Result<PreparedAgentPoolModelDeactivation<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let primary_consumers = self.primary_model_consumers.read().await.clone();
        let primary_agent = self.primary_agent.read().await.clone();
        let mut publications = Vec::with_capacity(
            agents
                .len()
                .saturating_add(usize::from(primary_consumers.is_some())),
        );
        if let (Some(primary), Some(consumers)) = (primary_agent, primary_consumers) {
            publications.push(infra::prepare_agent_model_deactivation(&primary, consumers).await);
        }
        let mut pooled_agents: Vec<(&String, &PooledAgent)> = agents.iter().collect();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (_, pooled) in pooled_agents {
            publications.push(
                infra::prepare_agent_model_deactivation(
                    &pooled.handle,
                    pooled.model_consumers.clone(),
                )
                .await,
            );
        }
        Ok(PreparedAgentPoolModelDeactivation {
            pool: self,
            _transition: transition,
            _agents: agents,
            publications,
            app_config,
        })
    }

    /// Publish the current permission mode without waiting for an active turn.
    ///
    /// The shared permission service is the authority used by tool execution,
    /// so it is updated first. Idle agents mirror the mode immediately; a busy
    /// agent refreshes its informational config on its next pool acquisition.
    pub async fn apply_permission_mode(&self, mode: PermissionMode) {
        *self.permission_mode.write().await = mode;

        if let Some(service) = &self.shared.permission_service {
            service.set_mode(mode).await;
            service.clear_cache();
        }

        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();

        let mut updated_agents = 0usize;
        for handle in agents {
            if handle
                .try_write(|agent| {
                    if agent.get_permission_mode() != mode {
                        agent.set_permission_mode(mode);
                    }
                })
                .is_some()
            {
                updated_agents = updated_agents.saturating_add(1);
            }
        }

        let pooled_agents = self.agents.read().await.len();
        let deferred_agents = pooled_agents.saturating_sub(updated_agents);
        tracing::info!(
            mode = %mode,
            pooled_agents,
            updated_agents,
            deferred_agents,
            "AgentPool: permission mode published"
        );
    }

    /// Publish a product system prompt to the primary, every existing pooled
    /// Agent, and the config template used for future pool admissions.
    pub async fn apply_system_prompt(&self, system_prompt: String) {
        self.app_config.write().await.agent.system_prompt = system_prompt.clone();
        let mut handles = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect::<Vec<_>>();
        if let Some(primary) = self.primary_agent.read().await.clone()
            && !handles
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate.inner(), primary.inner()))
        {
            handles.push(primary);
        }
        for handle in handles {
            let system_prompt = system_prompt.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_system_prompt(system_prompt).await;
                    })
                })
                .await;
        }
    }

    /// Project the current EKO tool-control generation into the primary and
    /// every cached Agent. Runs already holding a snapshot remain unchanged;
    /// the next run observes the new generation.
    pub(crate) async fn publish_tool_control_generation(
        &self,
    ) -> Result<(), crate::tool_control::ToolControlError> {
        let agents = self.agents.write().await;
        // Read the authority only after publication owns the pool generation.
        // Concurrent older publishers therefore observe the newest revision
        // instead of overwriting a later mutation with a stale snapshot.
        let snapshot = self.tool_control.snapshot()?;
        let disabled = crate::tool_control::disabled_option(&snapshot);
        let mut handles = agents
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect::<Vec<_>>();
        let mut model_consumers = agents
            .values()
            .map(|pooled| pooled.model_consumers.clone())
            .collect::<Vec<_>>();
        if let Some(primary) = self.primary_agent.read().await.clone()
            && !handles
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate.inner(), primary.inner()))
        {
            handles.push(primary);
        }
        if let Some(primary_consumers) = self.primary_model_consumers.read().await.clone() {
            model_consumers.push(primary_consumers);
        }
        for handle in handles {
            let disabled = disabled.clone();
            handle
                .read(|agent| agent.set_disabled_tools(disabled))
                .await;
        }
        for consumers in model_consumers {
            consumers.apply_disabled_tools(disabled.clone()).await;
        }
        tracing::info!(
            revision = snapshot.revision,
            disabled_tools = snapshot.disabled_tools.len(),
            pooled_agents = agents.len(),
            "AgentPool: tool-control generation published"
        );
        Ok(())
    }

    pub(crate) fn tool_control(&self) -> Arc<crate::tool_control::ToolControlService> {
        crate::tool_control::shared(&self.tool_control)
    }

    /// Propagate `working_dir` to all pooled agents.
    ///
    /// Called after a workspace switch so that background tasks and
    /// multi-conversation agents operate in the new workspace root.
    pub async fn apply_working_dir(&self, path: Option<std::path::PathBuf>) {
        *self.working_dir.write().await = path.clone();
        let artifact_config = crate::infra::tool_output_artifact_config(path.as_deref());
        *self.tool_output_artifacts.write().await = artifact_config.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        for handle in agents {
            let path = path.clone();
            let artifact_config = artifact_config.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_working_dir(path.clone());
                        agent.set_tool_output_artifacts(Some(artifact_config));
                        crate::infra::refresh_dynamic_context(agent, path.as_deref()).await;
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(?path, pooled_agents, "AgentPool: working_dir applied");
    }

    /// Apply one workspace prompt/skill profile to existing and future agents.
    pub async fn apply_workspace_routing(&self, kind: WorkspaceKind) {
        *self.workspace_kind.write().await = kind.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let kind = kind.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::workspace_routing::configure_agent_for_workspace(agent, &kind).await;
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(?kind, pooled_agents, "AgentPool: workspace routing applied");
    }

    /// Rebind existing and future pooled agents to the active conversation store.
    pub async fn apply_conversation_store(
        &self,
        store: Arc<dyn echo_agent::memory::ConversationStore>,
    ) {
        *self.conversation_store_override.write().await = Some(store.clone());
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let store = store.clone();
            handle
                .write(|agent| agent.set_conversation_store(store))
                .await;
        }
    }

    /// Rebind existing and future pooled agents to the active checkpoint store.
    pub async fn apply_state_store(&self, store: Arc<dyn echo_agent::state::RuntimeStateStore>) {
        *self.state_store_override.write().await = Some(store.clone());
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let store = store.clone();
            handle.write(|agent| agent.set_state_store(store)).await;
        }
    }

    /// Current number of agents in the pool (including background).
    pub async fn pool_size(&self) -> usize {
        self.agents.read().await.len()
    }

    /// Return the primary Agent for this pool generation.
    pub(crate) async fn primary_agent(&self) -> anyhow::Result<AgentHandle> {
        self.primary_agent
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("AgentPool primary Agent is unavailable"))
    }

    /// Maximum number of non-background agents this pool may create.
    pub fn max_agents(&self) -> usize {
        self.config.max_agents
    }

    /// Conservative default parallelism for background tasks backed by this pool.
    ///
    /// Keep one slot notionally reserved for foreground/multi-session work and
    /// cap the initial task fan-out to avoid overwhelming tools, LLM calls, and
    /// workspace writes.
    pub fn background_task_concurrency(&self) -> usize {
        self.config
            .max_agents
            .saturating_sub(self.foreground_agent_reserve())
            .clamp(1, 4)
    }

    /// Number of pool slots reserved for foreground/multi-session work.
    pub fn foreground_agent_reserve(&self) -> usize {
        1
    }

    /// Conservative default fan-out for a single composite parallel task.
    pub fn composite_parallelism(&self) -> usize {
        self.background_task_concurrency().clamp(1, 3)
    }

    /// Start a periodic cleanup task that evicts idle agents.
    ///
    /// The cleanup runs every 5 minutes, removing agents that have been
    /// idle longer than `config.idle_timeout`. The `__background__` agent
    /// is never evicted. Call `shutdown()` to stop the monitor.
    pub async fn spawn_cleanup_monitor(self: &Arc<Self>) {
        let mut cleanup_handle = self
            .cleanup_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.shutting_down.load(Ordering::Acquire) || cleanup_handle.is_some() {
            return;
        }

        let pool = Arc::downgrade(self);
        let cancel = self.cleanup_cancel.clone();
        *cleanup_handle = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 min
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("AgentPool: cleanup monitor stopped");
                        return;
                    }
                    _ = interval.tick() => {}
                }

                let Some(pool) = pool.upgrade() else {
                    return;
                };
                let idle_timeout = pool.config.idle_timeout;
                let mut agents = pool.agents.write().await;
                // First pass: find agents that exceed idle timeout (except background).
                let timed_out: Vec<String> = agents
                    .iter()
                    .filter(|(id, agent)| {
                        id.as_str() != "__background__" && agent.last_used.elapsed() > idle_timeout
                    })
                    .map(|(id, _)| id.clone())
                    .collect();

                // Second pass: only evict agents that are NOT currently executing.
                // Uses the same try_lock(execution_mutex) check as the acquire() path
                // so long-running tasks (e.g. TaskRuntime DAG subagents) aren't killed.
                let to_remove: Vec<String> = timed_out
                    .into_iter()
                    .filter(|id| {
                        if pool.admission.is_active(id) {
                            return false;
                        }
                        let is_idle = agents
                            .get(id)
                            .and_then(|pa| pa.handle.inner().try_read().ok())
                            .map(|guard| guard.execution_mutex().try_lock().is_ok())
                            .unwrap_or(false);
                        if !is_idle {
                            tracing::debug!(
                                conv_id = %id,
                                "AgentPool: skipping eviction — agent is executing"
                            );
                        }
                        is_idle
                    })
                    .collect();

                for id in to_remove {
                    if let Some(pa) = agents.remove(&id) {
                        tracing::info!(
                            conv_id = %id,
                            idle_secs = pa.last_used.elapsed().as_secs(),
                            "AgentPool: evicted idle agent"
                        );
                    }
                }
            }
        }));
    }

    /// Stop the cleanup monitor and release all pool agents.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.admission.close();
        self.cleanup_cancel.cancel();
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let agents = self.agents.write().await;
        self.begin_shutdown();
        drop(agents);
        let cleanup_handle = self
            .cleanup_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let monitor_error = if let Some(cleanup_handle) = cleanup_handle {
            cleanup_handle
                .await
                .err()
                .map(|error| format!("AgentPool cleanup monitor failed: {error}"))
        } else {
            None
        };
        self.admission.wait_until_idle().await;
        let mut agents = self.agents.write().await;
        let count = agents.len();
        agents.clear();
        tracing::info!(agents_cleared = count, "AgentPool: shutdown complete");
        match monitor_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Verify that cached conversations can be retired without mutating them.
    pub(crate) async fn preflight_workspace_transition(
        &self,
    ) -> anyhow::Result<AgentPoolWorkspaceTransition<'_>> {
        let agents = self.agents.write().await;
        if self.workspace_transitioning.swap(true, Ordering::AcqRel) {
            anyhow::bail!("Agent pool workspace transition is already in progress");
        }
        drop(agents);
        let transition = AgentPoolWorkspaceTransition {
            pool: self,
            committed: false,
        };

        // An issued handle is execution ownership even before its framework
        // execution mutex is locked. Closing admission under the agents write
        // lock above makes the counter stable in the downward direction; wait
        // for every existing receipt to reach its real settlement.
        self.admission.wait_until_idle().await;

        let agents = self.agents.write().await;
        for (conversation_id, pooled) in agents.iter() {
            let Ok(agent) = pooled.handle.inner().try_read() else {
                anyhow::bail!(
                    "Cannot change workspace while pooled conversation {conversation_id} is busy"
                );
            };
            if agent.execution_mutex().try_lock().is_err() {
                anyhow::bail!(
                    "Cannot change workspace while pooled conversation {conversation_id} is executing"
                );
            }
        }
        drop(agents);
        Ok(transition)
    }

    /// Reuse the pool's existing generation admission boundary for an active
    /// model publication. Dropping the returned guard reopens admission
    /// without clearing cached agents.
    pub(crate) async fn preflight_model_mutation(
        &self,
    ) -> anyhow::Result<AgentPoolWorkspaceTransition<'_>> {
        self.preflight_workspace_transition().await
    }

    pub(crate) async fn begin_plugin_publication(
        &self,
    ) -> Result<PreparedAgentPoolPluginPublication<'_>, String> {
        let transition = self
            .preflight_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let previous = self.agent_generation.read().await.clone();
        Ok(PreparedAgentPoolPluginPublication {
            pool: self,
            _transition: transition,
            agents,
            previous,
            candidate: None,
            application_skill_repair: None,
        })
    }

    /// Close pool execution/creation admission and retain the agents write
    /// guard until one instruction snapshot is committed.
    pub(crate) async fn begin_instruction_publication(
        &self,
    ) -> Result<PreparedAgentPoolInstructionPublication<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        Ok(PreparedAgentPoolInstructionPublication {
            pool: self,
            _transition: transition,
            agents,
            candidate: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn transition_admission_closed_for_test(&self) -> bool {
        self.workspace_transitioning.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) async fn plugin_generation_revision_for_test(&self) -> u64 {
        self.agent_generation.read().await.revision
    }

    #[cfg(test)]
    fn conversation_retiring_for_test(&self, conversation_id: &str) -> bool {
        self.admission.is_retiring(conversation_id)
    }

    #[cfg(test)]
    pub(crate) async fn instruction_projection_revision_for_test(&self) -> Option<String> {
        self.instruction_projection
            .read()
            .await
            .as_ref()
            .map(|snapshot| snapshot.revision().to_string())
    }

    /// Internal: create a new agent with shared resources injected.
    ///
    /// `conversation_id` is used both as the pool key and as the
    /// `AgentConfig.conversation_id` — the latter is required by
    /// `save_runtime_checkpoint` and `ConversationStore` projection. We also
    /// keep it as `session_id` so existing `session_id`-keyed paths (e.g.
    /// background tasks) continue to work.
    async fn create_agent(&self, conversation_id: &str) -> anyhow::Result<PooledAgent> {
        // 1. Create a base agent — pass conversation_id + state_store at build
        //    time so the agent boots with everything the framework's checkpoint
        //    helpers need. (Previously the pool called `set_state_store` here,
        //    but `self.shared.state_store` was always None because the primary
        //    agent never had a store wired in — `extract_from` would only ever
        //    see None and the runtime checkpoint loop silently no-op'd.)
        let app_config = self.app_config.read().await.clone();
        let working_dir = self.working_dir.read().await.clone();
        let state_store = self
            .state_store_override
            .read()
            .await
            .clone()
            .or_else(|| self.shared.state_store.clone());
        let params = infra::AgentCreateParams {
            model: None, // will use app_config default
            system_prompt: None,
            project: None,
            session_id: Some(conversation_id.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            react_checkpoint_interval: None,
            state_store,
            memory_context_suffix: None,
            working_dir,
            // Thread the TaskRuntimeStore so pooled agents get task-management
            // tools registered (matches the primary agent wiring).
            // Formal Subagents created by TaskRuntime still have task_execute
            // disabled by invocation policy; pool conversation agents may drive it.
            task_runtime_store: self.shared.task_runtime_store.clone(),
            browser_runtime: self.shared.browser_runtime.clone(),
            command_cell_runtime: self.shared.command_cell_runtime.clone(),
            product_data_io: self.shared.product_data_io.clone(),
            execution_scope: self.shared.execution_scope.clone(),
        };
        let created = infra::create_agent_with_diagnostics(&params, &app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut agent = created.agent;
        let model_consumers = created.model_consumers;
        #[cfg(test)]
        if let Some(client) = self.llm_client_override.read().await.clone() {
            agent.set_llm_client(client);
        }
        if self.shared.tool_manager.is_none()
            && let Some(snapshot) = self.mcp_config_snapshot.read().await.clone()
            && let Err(error) = agent.load_mcp_config(snapshot).await
        {
            tracing::warn!(conversation_id, %error, "workspace pooled agent MCP connection failed");
        }
        agent.set_tool_output_artifacts(Some(self.tool_output_artifacts.read().await.clone()));

        // 2. Inject non-model shared resources. The model transport produced by
        // create_agent_with_diagnostics is authoritative and must not be
        // overwritten by the primary agent's startup client.
        if let Some(ref tm) = self.shared.tool_manager {
            agent.set_tool_manager(tm.clone());
        }
        if let Some(ref hr) = self.shared.hook_registry {
            agent.set_hook_registry(hr.clone());
        }
        if let Some(ref sm) = self.shared.sandbox_manager {
            agent.set_sandbox_manager(sm.clone());
        }
        if let Some(ref tt) = self.shared.token_tracker {
            agent.set_token_tracker(tt.clone());
        }
        // state_store is now injected via the builder above; nothing to set here.
        if let Some(ref rs) = self.shared.run_store {
            agent.set_run_store(rs.clone());
        }
        if let Some(ref tep) = self.shared.tool_execution_pipeline {
            agent.set_tool_execution_pipeline(tep.clone());
        }
        let conversation_store = self
            .conversation_store_override
            .read()
            .await
            .clone()
            .or_else(|| self.shared.conversation_store.clone());
        if let Some(ref cs) = conversation_store {
            agent.set_conversation_store(cs.clone());
        }
        if let Some(ref st) = self.shared.store {
            agent.install_store(st.clone()).await;
        }
        if let Some(ref review_integration) = self.shared.review_integration {
            let memory_generation = review_integration
                .lease_generation()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let layer_manager = memory_generation.layer_manager()?;
            agent.install_memory_layer_manager(layer_manager);
            agent.set_memory_trigger_sink(Some(review_integration.clone()));
            agent.set_skill_load_policy(Some(review_integration.clone()));
            agent.set_skill_curator(Some(review_integration.curator()));
            let mut projector = crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            )
            .with_hot_memory_source(review_integration.hot_memory_projection_source());
            if let (Some(command_cells), Some(execution_scope)) = (
                self.shared.command_cell_runtime.clone(),
                self.shared.execution_scope.clone(),
            ) {
                projector = projector.with_awaiter_results(command_cells, execution_scope);
            }
            agent.set_pre_model_context_projector(Some(Arc::new(projector)));
        }
        if let Some(ref ps) = self.shared.permission_service {
            agent.set_permission_service(ps.clone());
        }
        let permission_mode = *self.permission_mode.read().await;
        agent.set_permission_mode(permission_mode);
        let tool_control = self
            .tool_control
            .snapshot()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let disabled_tools = crate::tool_control::disabled_option(&tool_control);
        agent.set_disabled_tools(disabled_tools.clone());
        model_consumers.apply_disabled_tools(disabled_tools).await;

        // 3. Install the exact plugin generation committed by PluginRuntime.
        let agent_generation = self.agent_generation.read().await.clone();
        for desc in &agent_generation.skill_descriptors {
            agent.skill_registry_mut().register_descriptor(desc.clone());
        }
        register_plugin_agents(&mut agent, &agent_generation.plugin_agents)
            .await
            .map_err(anyhow::Error::msg)?;
        agent
            .replace_system_context_projection(
                crate::plugin_runtime::OUTPUT_STYLE_PROJECTION,
                agent_generation.output_style.clone(),
            )
            .await;
        crate::runtime::configure_intent_router(&mut agent);

        if let Some(snapshot) = self.instruction_projection.read().await.clone() {
            crate::unified_memory::apply_instruction_projection_snapshot(&mut agent, &snapshot)
                .await;
        }

        let workspace_kind = self.workspace_kind.read().await.clone();
        crate::workspace_routing::configure_agent_for_workspace(&mut agent, &workspace_kind).await;

        // 3b. Auto-compression — pooled agents must not rely solely on the
        // 200-msg hard cap. Mirror the primary agent wiring (runtime.rs) so
        // long GUI multi-session runs are protected by the configured strategy.
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::debug!(conversation_id, "pooled agent auto-compression configured");
        }

        // 4. Wrap in AgentHandle
        let handle = AgentHandle::new(agent);
        if let (Some(runtime), Some(scope)) = (
            self.shared.command_cell_runtime.as_ref(),
            self.shared.execution_scope.as_ref(),
        ) {
            runtime.bind_agent(scope.workspace_id(), conversation_id, &handle);
        }

        // Workspace pools own their ToolManagers, so complete the same task
        // tool suite used by the bootstrap primary. The execute tool captures
        // this exact Agent and host store; no process-global pool lookup is
        // needed or allowed here.
        if self.shared.tool_manager.is_none()
            && let Some(store) = self.shared.task_runtime_store.as_ref()
        {
            crate::tasks::task_runtime::register_task_tools_on_agent(&handle, store.clone()).await;
        }

        // TaskRuntime's formal Subagents are created by the framework registry,
        // not by this conversation pool. Their invocation policy continues to
        // disable task_execute so nested dispatch cannot recurse into L2.

        // 5. Configure HITL for this agent.
        // Use an empty HitlDispatcher (no REPL provider!) so that if the caller
        // hasn't yet called set_human_loop_provider, approval requests
        // auto-reject instead of blocking on terminal stdin (which hangs GUI).
        // The real provider (Tauri/TUI/REPL) is injected per-use via
        // set_human_loop_provider, which now does an in-place replace.
        {
            let dispatcher = Arc::new(crate::hitl::HitlDispatcher::new());
            handle
                .write_async(|a| {
                    let d = dispatcher.clone();
                    Box::pin(async move {
                        a.set_human_loop_provider(d);
                    })
                })
                .await;
        }

        Ok(PooledAgent::new(
            handle,
            model_consumers,
            conversation_id.to_string(),
        ))
    }
}

async fn replace_agent_plugin_generation(
    handle: &AgentHandle,
    previous: &AgentPluginGeneration,
    candidate: &AgentPluginGeneration,
    application_skill_repair: Option<&ApplicationSkillProjectionRepair>,
) -> Result<(), String> {
    let previous = previous.clone();
    let candidate = candidate.clone();
    let application_skill_repair = application_skill_repair.cloned();
    handle
        .write_async(|agent| {
            Box::pin(async move {
                remove_agent_plugin_generation(agent, &previous).await;
                if let Some(repair) = application_skill_repair.as_ref() {
                    agent.unregister_skills_by_source(&repair.source).await;
                }
                for descriptor in &candidate.skill_descriptors {
                    agent
                        .skill_registry_mut()
                        .register_descriptor(descriptor.clone());
                }
                if let Err(error) = register_plugin_agents(agent, &candidate.plugin_agents).await {
                    remove_agent_plugin_generation(agent, &candidate).await;
                    if let Some(repair) = application_skill_repair.as_ref() {
                        agent.unregister_skills_by_source(&repair.source).await;
                    }
                    for descriptor in &previous.skill_descriptors {
                        agent
                            .skill_registry_mut()
                            .register_descriptor(descriptor.clone());
                    }
                    let restore_error = register_plugin_agents(agent, &previous.plugin_agents)
                        .await
                        .err();
                    crate::runtime::configure_intent_router(agent);
                    return Err(match restore_error {
                        Some(restore_error) => {
                            format!("{error}; previous generation restore failed: {restore_error}")
                        }
                        None => error,
                    });
                }
                agent
                    .replace_system_context_projection(
                        crate::plugin_runtime::OUTPUT_STYLE_PROJECTION,
                        candidate.output_style.clone(),
                    )
                    .await;
                crate::runtime::configure_intent_router(agent);
                Ok(())
            })
        })
        .await
}

pub(crate) async fn remove_agent_plugin_generation(
    agent: &mut echo_agent::agent::react::ReactAgent,
    generation: &AgentPluginGeneration,
) {
    for plugin_agent in &generation.plugin_agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
    for descriptor in &generation.skill_descriptors {
        agent
            .skill_registry_mut()
            .remove_descriptor(&descriptor.name);
    }
}

impl Drop for AgentPool {
    fn drop(&mut self) {
        self.cleanup_cancel.cancel();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::Agent;

    type TestResult<T = ()> = Result<T, String>;

    struct MemoryReleaseProbe {
        pool: Arc<AgentPool>,
        released_after_pool: Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for MemoryReleaseProbe {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                let pool_is_idle = self
                    .pool
                    .admission
                    .active
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .total
                    == 0;
                self.released_after_pool
                    .store(pool_is_idle, Ordering::SeqCst);
            })
        }
    }

    #[test]
    fn test_pool_config_default() {
        let config = PoolConfig::default();
        assert_eq!(config.max_agents, 10);
        assert_eq!(config.idle_timeout, Duration::from_secs(1800));
        assert!(config.enable_background_agent);
    }

    #[test]
    fn test_pool_config_custom() {
        let config = PoolConfig {
            max_agents: 5,
            idle_timeout: Duration::from_secs(60),
            enable_background_agent: false,
        };
        assert_eq!(config.max_agents, 5);
        assert!(!config.enable_background_agent);
    }

    #[tokio::test]
    async fn test_pool_exposes_max_agents() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        assert_eq!(pool.max_agents(), 4);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_task_concurrency_is_conservative() -> TestResult {
        let small = create_test_pool(1, false).await?;
        assert_eq!(small.background_task_concurrency(), 1);

        let medium = create_test_pool(3, false).await?;
        assert_eq!(medium.background_task_concurrency(), 2);

        let large = create_test_pool(10, false).await?;
        assert_eq!(large.background_task_concurrency(), 4);
        assert_eq!(large.foreground_agent_reserve(), 1);
        assert_eq!(large.composite_parallelism(), 3);
        Ok(())
    }

    #[test]
    fn test_pool_error_display() {
        let err = PoolError::PoolFull { max: 5 };
        assert!(err.to_string().contains("5"));
        assert!(err.to_string().contains("full"));

        let err = PoolError::AgentCreation("test error".to_string());
        assert!(err.to_string().contains("test error"));
    }

    #[test]
    fn test_pool_error_is_std_error() {
        // Verify PoolError implements std::error::Error
        let err: Box<dyn std::error::Error> = Box::new(PoolError::PoolFull { max: 3 });
        assert!(err.to_string().contains("3"));
    }

    #[tokio::test]
    async fn test_pooled_agent_timestamps() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let lease = pool
            .acquire("test-conv")
            .await
            .map_err(|error| error.to_string())?;
        drop(lease);
        let agents = pool.agents.read().await;
        let pa = agents
            .get("test-conv")
            .ok_or_else(|| "pooled agent was not retained".to_string())?;
        assert!(pa.created_at.elapsed().as_millis() < 100);
        assert!(pa.last_used.elapsed().as_millis() < 100);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_creates_agent() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        assert_eq!(pool.pool_size().await, 0);

        let handle = pool.acquire("conv-1").await;
        assert!(handle.is_ok());
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_reuses_existing() -> TestResult {
        let pool = create_test_pool(3, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;

        // Same conversation_id should return the same agent (pool size stays 1)
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_different_ids() -> TestResult {
        let pool = create_test_pool(5, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        let _h3 = pool.acquire("conv-3").await.map_err(|e| e.to_string())?;

        assert_eq!(pool.pool_size().await, 3);
        Ok(())
    }

    #[tokio::test]
    async fn process_agent_execution_is_bounded_across_workspace_pools() -> TestResult {
        let governor = Arc::new(AgentExecutionGovernor::new(PROCESS_AGENT_EXECUTION_LIMIT));
        let mut pools = Vec::new();
        for _ in 0..3 {
            let mut pool = create_test_pool(10, false).await?;
            pool.process_agent_execution = governor.clone();
            pools.push(Arc::new(pool));
        }
        let mut leases = Vec::new();
        for index in 0..PROCESS_AGENT_EXECUTION_LIMIT {
            let pool = pools
                .get(index % pools.len())
                .ok_or_else(|| "workspace pool is missing".to_string())?;
            leases.push(
                pool.acquire(&format!("workspace-conversation-{index}"))
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        assert_eq!(
            governor.snapshot(),
            AgentExecutionResourceSnapshot {
                active: PROCESS_AGENT_EXECUTION_LIMIT,
                limit: PROCESS_AGENT_EXECUTION_LIMIT,
            }
        );

        let waiting_pool = pools
            .first()
            .cloned()
            .ok_or_else(|| "waiting workspace pool is missing".to_string())?;
        assert!(matches!(
            waiting_pool.acquire("workspace-conversation-waiting").await,
            Err(PoolError::ExecutionLeaseCapacity)
        ));
        leases.pop();
        let admitted = waiting_pool
            .acquire("workspace-conversation-waiting")
            .await
            .map_err(|error| error.to_string())?;
        drop(admitted);
        drop(leases);
        assert!(governor.snapshot().active <= governor.snapshot().limit);
        Ok(())
    }

    #[tokio::test]
    async fn test_exact_execution_retirement_removes_agent() -> TestResult {
        let pool = create_test_pool(5, false).await?;

        let execution = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        assert!(
            pool.retire_execution("conv-1", execution)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn exact_execution_retirement_rejects_wrong_key() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let execution = pool.acquire("owned").await.map_err(|e| e.to_string())?;
        assert!(matches!(
            pool.retire_execution("other", execution).await,
            Err(PoolError::ExecutionLeaseMismatch)
        ));
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn exact_execution_retirement_waits_for_overlapping_same_key_receipt() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let first = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let second = pool.acquire("shared").await.map_err(|e| e.to_string())?;

        assert!(
            !pool
                .retire_execution("shared", first)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 1);
        assert!(
            pool.retire_execution("shared", second)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn awaited_conversation_retirement_blocks_aba_and_replaces_exact_generation() -> TestResult
    {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let old_execution = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let old_agent = old_execution.agent();
        let retirement_pool = Arc::clone(&pool);
        let mut retirement =
            tokio::spawn(
                async move { retirement_pool.retire_conversation_and_wait("shared").await },
            );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.conversation_retiring_for_test("shared") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "conversation retirement admission did not close".to_string())?;
        assert!(matches!(
            pool.acquire("shared").await,
            Err(PoolError::ConversationRetirementPending { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut retirement)
                .await
                .is_err(),
            "retirement completed before the old execution receipt settled"
        );

        drop(old_execution);
        assert!(
            retirement
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
        );
        assert!(!pool.conversation_retiring_for_test("shared"));
        let replacement = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        assert!(!Arc::ptr_eq(old_agent.inner(), replacement.agent().inner()));
        drop(replacement);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_conversation_retirement_reopens_without_claiming_settlement() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let old_execution = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let retirement_pool = Arc::clone(&pool);
        let retirement =
            tokio::spawn(
                async move { retirement_pool.retire_conversation_and_wait("shared").await },
            );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.conversation_retiring_for_test("shared") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "conversation retirement admission did not close".to_string())?;
        retirement.abort();
        let _join = retirement.await;
        assert!(!pool.conversation_retiring_for_test("shared"));
        let overlapping = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(overlapping);
        drop(old_execution);
        Ok(())
    }

    #[tokio::test]
    async fn drained_retirement_keeps_admission_closed_until_aggregate_commit() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let cached = pool
            .acquire("shared")
            .await
            .map_err(|error| error.to_string())?;
        drop(cached);
        let retirement = pool
            .begin_conversation_retirement("shared")
            .map_err(|error| error.to_string())?;
        assert!(
            pool.drain_conversation_retirement(&retirement)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(matches!(
            pool.acquire("shared").await,
            Err(PoolError::ConversationRetirementPending { .. })
        ));
        drop(retirement);
        let replacement = pool
            .acquire("shared")
            .await
            .map_err(|error| error.to_string())?;
        drop(replacement);
        Ok(())
    }

    #[tokio::test]
    async fn retirement_receipt_cannot_complete_against_another_pool() -> TestResult {
        let pool_a = create_test_pool(5, false).await?;
        let pool_b = create_test_pool(5, false).await?;
        let cached_b = pool_b.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(cached_b);

        let retirement = pool_a
            .begin_conversation_retirement("shared")
            .map_err(|error| error.to_string())?;
        let result = pool_b.complete_conversation_retirement(retirement).await;
        assert!(matches!(result, Err(PoolError::RetirementReceiptMismatch)));
        assert_eq!(pool_b.pool_size().await, 1);
        assert!(!pool_a.conversation_retiring_for_test("shared"));
        let still_cached = pool_b.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(still_cached);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_checkpoint_restores_same_incarnation_but_not_rotated_key() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state_store = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(temp.path())
                .map_err(|error| error.to_string())?,
        );
        let pool = create_test_pool(5, false).await?;
        pool.apply_state_store(state_store).await;

        let first = pool
            .acquire("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;
        let first_agent = first.agent();
        first_agent
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .load_messages(vec![echo_agent::llm::types::Message::user(
                            "incarnation-a history".to_string(),
                        )])
                        .await;
                    agent.force_checkpoint().await
                })
            })
            .await
            .map_err(|error| error.to_string())?;
        drop(first);
        pool.retire_conversation_and_wait("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;

        let same_incarnation = pool
            .acquire("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;
        let restored = same_incarnation
            .agent()
            .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
            .await
            .map_err(|error| error.to_string())?;
        if restored.is_none() {
            return Err("same incarnation did not restore its checkpoint".into());
        }
        drop(same_incarnation);

        let rotated = pool
            .acquire("runtime-incarnation-b")
            .await
            .map_err(|error| error.to_string())?;
        let rotated_agent = rotated.agent();
        let rotated_restore = rotated_agent
            .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
            .await
            .map_err(|error| error.to_string())?;
        let rotated_messages = rotated_agent
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        if rotated_restore.is_some()
            || rotated_messages.iter().any(|message| {
                message
                    .text_content()
                    .is_some_and(|text| text.contains("incarnation-a history"))
            })
        {
            return Err("rotated incarnation restored the previous model context".into());
        }
        drop(rotated);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_lease_existing_returns_none_for_unknown() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        assert!(
            pool.lease_existing("unknown")
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_lease_existing_returns_some_for_known() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let _h = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        assert!(
            pool.lease_existing("conv-1")
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_admission_linearizes_with_pool_lock_before_lease_publication() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let initial = pool.acquire("reserved").await.map_err(|e| e.to_string())?;
        drop(initial);

        let agents = pool.agents.write().await;
        let handle = agents
            .get("reserved")
            .map(|pooled| pooled.handle.clone())
            .ok_or_else(|| "reserved pooled Agent is missing".to_string())?;
        let accepted = pool
            .admission
            .issue_process_scoped("reserved", handle.clone(), &pool.process_agent_execution)
            .map_err(|error| error.to_string())?;
        pool.begin_shutdown();
        assert!(matches!(
            pool.admission
                .issue_process_scoped("reserved", handle, &pool.process_agent_execution,),
            Err(PoolError::ShuttingDown)
        ));
        drop(agents);

        let shutdown_pool = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { shutdown_pool.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(accepted);
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
            .await
            .map_err(|_| "pool shutdown did not wait for the accepted reservation".to_string())?
            .map_err(|error| error.to_string())??;
        Ok(())
    }

    #[tokio::test]
    async fn task_execute_resolves_the_current_conversation_agent() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let pooled = pool
            .acquire("conv-1")
            .await
            .map_err(|error| error.to_string())?;
        let fallback = create_test_agent_handle()?;
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let tool = crate::tasks::task_runtime::ExecuteTaskTool::new(store, fallback.clone())
            .with_agent_pool(Arc::downgrade(&pool));

        let resolved = tool
            .execution_agent_for_test(Some("conv-1".to_string()))
            .await
            .map_err(|error| error.to_string())?;
        let pooled_agent = pooled.agent();
        assert!(Arc::ptr_eq(resolved.inner(), pooled_agent.inner()));

        let unresolved = tool
            .execution_agent_for_test(Some("missing".to_string()))
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(unresolved.inner(), fallback.inner()));
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_rejects_overflow_while_all_receipts_are_active() -> TestResult {
        let pool = create_test_pool(2, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 2);

        assert!(matches!(
            pool.acquire("conv-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_evicts_idle_after_execution_receipts_drop() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        drop(h1);
        drop(h2);

        let _h3 = pool.acquire("conv-3").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn continuation_capacity_is_bounded_without_consuming_conversation_slots() -> TestResult {
        let pool = create_test_pool(2, false).await?;

        let _continuation_one = pool
            .acquire("__continuation__:run-1")
            .await
            .map_err(|error| error.to_string())?;
        let _continuation_two = pool
            .acquire("__continuation__:run-2")
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            pool.acquire("__continuation__:run-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));

        let _conversation_one = pool
            .acquire("conv-1")
            .await
            .map_err(|error| error.to_string())?;
        let _conversation_two = pool
            .acquire("conv-2")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 4);
        assert!(matches!(
            pool.acquire("conv-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn continuation_capacity_reuses_an_idle_slot() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let first = pool
            .acquire("__continuation__:run-1")
            .await
            .map_err(|error| error.to_string())?;
        let second = pool
            .acquire("__continuation__:run-2")
            .await
            .map_err(|error| error.to_string())?;
        drop(first);
        drop(second);

        let _replacement = pool
            .acquire("__continuation__:run-3")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_precreated() -> TestResult {
        // Background agent pre-creation only happens in from_runtime().
        // With manual construction, no background agent exists until acquired.
        let pool = create_test_pool(5, true).await?;
        // Manually created pool has no pre-created agents
        assert_eq!(pool.pool_size().await, 0);
        // But background_agent() returns None since __background__ wasn't pre-created
        assert!(pool.background_agent().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_not_created_when_disabled() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        assert!(pool.background_agent().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_acquire_on_demand() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        // Can acquire __background__ on demand even without pre-creation
        let bg = pool.acquire("__background__").await;
        assert!(bg.is_ok());
        assert!(pool.background_agent().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_shared_resources_extraction() -> TestResult {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        // ToolManager should be extracted
        assert!(shared.tool_manager.is_some());
        // HookRegistry should be extracted
        assert!(shared.hook_registry.is_some());
        // TokenTracker should be extracted
        assert!(shared.token_tracker.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_shared_resources_arc_sharing() -> TestResult {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        // Verify Arc reference counts indicate sharing
        let tm = shared
            .tool_manager
            .as_ref()
            .ok_or_else(|| "tool manager should be extracted".to_string())?;
        // At least 2 references: one in original agent, one in shared
        assert!(
            Arc::strong_count(tm) >= 2,
            "ToolManager Arc should be shared (count={})",
            Arc::strong_count(tm)
        );

        let tt = shared
            .token_tracker
            .as_ref()
            .ok_or_else(|| "token tracker should be extracted".to_string())?;
        assert!(
            Arc::strong_count(tt) >= 2,
            "TokenUsageTracker Arc should be shared (count={})",
            Arc::strong_count(tt)
        );
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn create_test_agent() -> TestResult<echo_agent::agent::ReactAgent> {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;

        let mock_llm = Arc::new(MockLlmClient::new().with_model_name("test-model"));

        ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(mock_llm)
            .build()
            .map_err(|error| error.to_string())
    }

    fn create_test_agent_handle() -> TestResult<AgentHandle> {
        create_test_agent().map(AgentHandle::new)
    }

    async fn create_test_pool(max_agents: usize, enable_bg: bool) -> TestResult<AgentPool> {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        Ok(AgentPool::new_for_test(handle, None, None, max_agents, enable_bg).await)
    }

    async fn create_test_pool_with_review_integration() -> TestResult<AgentPool> {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let echo_agent_dir = std::env::temp_dir()
            .join(format!("echo-agent-pool-test-{}", uuid::Uuid::new_v4()))
            .join(".echo-agent");
        let review_integration = Arc::new(crate::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            echo_agent_dir,
            store.clone(),
        ));
        Ok(AgentPool::new_for_test(handle, Some(review_integration), Some(store), 3, false).await)
    }

    #[tokio::test]
    async fn runtime_state_store_rebind_reaches_existing_and_future_agents() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let existing_lease = pool
            .acquire("existing-state-binding")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store: Arc<dyn echo_agent::state::RuntimeStateStore> = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(temp.path())
                .map_err(|error| error.to_string())?,
        );

        pool.apply_state_store(store.clone()).await;
        let existing_store = existing
            .read(|agent| agent.state_store().clone())
            .await
            .ok_or_else(|| "existing agent has no runtime state store".to_string())?;
        assert!(Arc::ptr_eq(&existing_store, &store));

        let future_lease = pool
            .acquire("future-state-binding")
            .await
            .map_err(|error| error.to_string())?;
        let future_store = future_lease
            .agent()
            .read(|agent| agent.state_store().clone())
            .await
            .ok_or_else(|| "future agent has no runtime state store".to_string())?;
        assert!(Arc::ptr_eq(&future_store, &store));
        Ok(())
    }

    #[tokio::test]
    async fn future_agent_uses_committed_local_config_without_api_key() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let runtime = ModelRuntimeConfig {
            id: "local:model".to_string(),
            display_name: "Local model".to_string(),
            provider: "local".to_string(),
            model: "model".to_string(),
            api_protocol: echo_agent::llm::LlmApiProtocol::ChatCompletions,
            input_modalities: echo_agent::llm::ModelInputModality::text_only(),
            auth_token: None,
            auth_source: "none".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            api_key_env: None,
            requires_api_key: false,
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::core::capabilities::ThinkingProfile::unknown(),
        };

        let prepared = infra::prepare_runtime_llm(&runtime)?;
        let mut candidate = pool.app_config.read().await.clone();
        candidate.model_providers.insert(
            runtime.provider.clone(),
            crate::config::ModelProviderConfig {
                base_url: runtime.base_url.clone(),
                ..Default::default()
            },
        );
        candidate
            .configured_models
            .push(crate::config::ConfiguredModel {
                id: runtime.id.clone(),
                display_name: runtime.display_name.clone(),
                provider: runtime.provider.clone(),
                model: runtime.model.clone(),
                api_protocol: runtime.api_protocol,
                ..Default::default()
            });
        crate::model_config::set_default_model(&mut candidate, &runtime.id)?;
        pool.prepare_model_publication(candidate, runtime, prepared)
            .await?
            .commit()
            .await;
        let lease = pool
            .acquire("future-local")
            .await
            .map_err(|error| error.to_string())?;
        let handle = lease.agent();
        let applied = handle
            .read(|agent| agent.llm_config().cloned())
            .await
            .ok_or_else(|| "future agent has no LLM config".to_string())?;
        assert!(applied.api_key.is_empty());
        assert_eq!(
            applied.base_url,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        Ok(())
    }

    #[tokio::test]
    async fn future_pool_agent_uses_the_session_model_selection() -> TestResult {
        use echo_agent::agent::Agent;

        let agent = create_test_agent_handle()?;
        let mut config = EkoConfig {
            configured_models: vec![
                crate::config::ConfiguredModel {
                    id: "local:a".to_string(),
                    display_name: "A".to_string(),
                    provider: "local".to_string(),
                    model: "a".to_string(),
                    context_window: Some(100_000),
                    ..crate::config::ConfiguredModel::default()
                },
                crate::config::ConfiguredModel {
                    id: "local:b".to_string(),
                    display_name: "B".to_string(),
                    provider: "local".to_string(),
                    model: "b".to_string(),
                    context_window: Some(200_000),
                    ..crate::config::ConfiguredModel::default()
                },
            ],
            ..EkoConfig::default()
        };
        config.model.default_model_id = Some("local:a".to_string());
        config.model_providers.insert(
            "local".to_string(),
            crate::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        let selected = crate::model_config::resolve_runtime_model(&config, Some("local:b"));
        let session = crate::model_config::session_config_for_runtime(&config, &selected)?;
        let pool = AgentPool::new_for_test_with_config(&agent, None, None, 3, false, session).await;

        let lease = pool
            .acquire("future-session-selection")
            .await
            .map_err(|error| error.to_string())?;
        let handle = lease.agent();
        let (model, token_limit) = handle
            .read(|pooled| {
                (
                    pooled.model_name().to_string(),
                    pooled.config().get_token_limit(),
                )
            })
            .await;

        assert_eq!(model, "b");
        assert_eq!(token_limit, 200_000);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_transition_gate_rejects_acquire_until_publication_finishes() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let mut transition = pool
            .preflight_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            pool.acquire("blocked-before-commit").await,
            Err(PoolError::WorkspaceTransition)
        ));
        transition.commit().await;
        assert!(matches!(
            pool.acquire("blocked-after-commit").await,
            Err(PoolError::WorkspaceTransition)
        ));
        drop(transition);

        let published = pool
            .acquire("published")
            .await
            .map_err(|error| error.to_string())?;
        drop(published);
        Ok(())
    }

    #[tokio::test]
    async fn failed_pool_preflight_reopens_admission() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let active_lease = pool
            .acquire("active")
            .await
            .map_err(|error| error.to_string())?;
        let active = active_lease.agent();
        drop(active_lease);
        let execution = active.read(|agent| agent.execution_mutex().clone()).await;
        let execution_guard = execution.lock().await;
        let error = pool
            .preflight_workspace_transition()
            .await
            .err()
            .ok_or_else(|| "busy pool transition unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("executing"));
        drop(execution_guard);

        let reopened = pool
            .acquire("reopened")
            .await
            .map_err(|error| error.to_string())?;
        drop(reopened);
        Ok(())
    }

    #[tokio::test]
    async fn issued_lease_blocks_transition_even_before_execution_mutex_is_locked() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let issued = pool
            .acquire("issued-before-execution")
            .await
            .map_err(|error| error.to_string())?;
        let mut transition = Box::pin(pool.preflight_workspace_transition());

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut transition)
                .await
                .is_err(),
            "transition must wait for the issued execution receipt"
        );
        assert!(matches!(
            pool.acquire("blocked-while-draining").await,
            Err(PoolError::WorkspaceTransition)
        ));

        drop(issued);
        let mut transition = tokio::time::timeout(Duration::from_secs(1), transition)
            .await
            .map_err(|_| "transition did not observe the released lease".to_string())?
            .map_err(|error| error.to_string())?;
        transition.commit().await;
        drop(transition);

        let new_generation = pool
            .acquire("new-generation")
            .await
            .map_err(|error| error.to_string())?;
        drop(new_generation);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_preflight_wait_reopens_pool_admission() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let issued = pool
            .acquire("issued-before-cancel")
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                pool.preflight_workspace_transition(),
            )
            .await
            .is_err(),
            "preflight should still be draining the issued lease"
        );
        let admitted_after_cancel = pool
            .acquire("admitted-after-cancel")
            .await
            .map_err(|error| error.to_string())?;

        drop(admitted_after_cancel);
        drop(issued);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_routing_applies_to_existing_and_future_pool_agents() -> Result<(), String> {
        let pool = create_test_pool(4, false).await?;
        let existing = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;

        pool.apply_workspace_routing(WorkspaceKind::DataAnalysis { datasets: vec![] })
            .await;
        let existing_context = existing.agent().read(|agent| agent.context().clone()).await;
        assert!(
            existing_context
                .lock()
                .await
                .has_projection("eko:workspace-profile")
        );

        let future = pool
            .acquire("future")
            .await
            .map_err(|error| error.to_string())?;
        let future_context = future.agent().read(|agent| agent.context().clone()).await;
        assert!(
            future_context
                .lock()
                .await
                .has_projection("eko:workspace-profile")
        );
        Ok(())
    }

    #[tokio::test]
    async fn working_dir_applies_to_existing_and_future_pool_agents() -> Result<(), String> {
        let pool = create_test_pool(4, false).await?;
        let existing = pool
            .acquire("existing-working-dir")
            .await
            .map_err(|error| error.to_string())?;
        let root = std::env::temp_dir().join("eko-pool-working-dir");

        pool.apply_working_dir(Some(root.clone())).await;
        assert_eq!(
            existing.agent().read(|agent| agent.working_dir()).await,
            Some(root.clone())
        );

        let future = pool
            .acquire("future-working-dir")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            future.agent().read(|agent| agent.working_dir()).await,
            Some(root)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_agent_installs_layered_memory_runtime() -> TestResult {
        let pool = create_test_pool_with_review_integration().await?;
        let handle = pool
            .acquire("conv-memory")
            .await
            .map_err(|error| error.to_string())?;

        let has_layer_manager = handle
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        assert!(
            has_layer_manager,
            "pooled agents must install MemoryLayerManager so TriggerDetector writes real memory"
        );
        Ok(())
    }

    #[tokio::test]
    async fn primary_existing_and_future_agents_share_exact_scoped_memory_arc() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store_a = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration_a = Arc::new(crate::evolution::ReviewIntegration::new_scoped(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store_a.clone(),
            "workspace-a".to_string(),
            "generation-a".to_string(),
        ));
        let manager_a = integration_a
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        let primary = create_test_agent_handle()?;
        let primary_manager = manager_a.clone();
        primary
            .write(|agent| agent.install_memory_layer_manager(primary_manager))
            .await;
        let pool = AgentPool::new_for_test(
            primary.clone(),
            Some(integration_a.clone()),
            Some(store_a),
            3,
            false,
        )
        .await;

        let existing = pool
            .acquire("existing-memory-generation")
            .await
            .map_err(|error| error.to_string())?;
        let existing_manager = existing
            .agent()
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "existing pooled Agent has no memory manager".to_string())?;
        drop(existing);
        let future = pool
            .acquire("future-memory-generation")
            .await
            .map_err(|error| error.to_string())?;
        let future_manager = future
            .agent()
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "future pooled Agent has no memory manager".to_string())?;
        let installed_primary = primary
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "primary Agent has no memory manager".to_string())?;

        assert!(Arc::ptr_eq(&manager_a, &installed_primary));
        assert!(Arc::ptr_eq(&manager_a, &existing_manager));
        assert!(Arc::ptr_eq(&manager_a, &future_manager));

        let integration_b = crate::evolution::ReviewIntegration::new_scoped(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join("workspace-b/.eko"),
            Arc::new(echo_agent::memory::InMemoryStore::new()),
            "workspace-b".to_string(),
            "generation-b".to_string(),
        );
        let manager_b = integration_b
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        assert!(!Arc::ptr_eq(&manager_a, &manager_b));
        assert!(!Arc::ptr_eq(
            &integration_a.hot_memory_projection_source(),
            &integration_b.hot_memory_projection_source(),
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_task_subagents_are_isolated_and_have_memory_runtime() -> TestResult {
        let pool = create_test_pool_with_review_integration().await?;

        let task_a = pool
            .acquire("__task__:task-a")
            .await
            .map_err(|error| error.to_string())?;
        let task_b = pool
            .acquire("__task__:task-b")
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            !Arc::ptr_eq(task_a.agent().inner(), task_b.agent().inner()),
            "parallel background tasks must use distinct subagent instances"
        );

        let task_a_has_memory = task_a
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        let task_b_has_memory = task_b
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        assert!(task_a_has_memory);
        assert!(task_b_has_memory);
        Ok(())
    }

    #[tokio::test]
    async fn test_released_task_subagent_frees_pool_capacity() -> TestResult {
        let pool = create_test_pool(1, false).await?;

        let task_a = pool
            .acquire("__task__:task-a")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        assert!(
            pool.retire_execution("__task__:task-a", task_a)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);

        let task_b = pool.acquire("__task__:task-b").await;
        assert!(
            task_b.is_ok(),
            "released task subagent should free capacity for a later task"
        );
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_permission_mode_applies_to_existing_and_future_pool_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;

        pool.apply_permission_mode(PermissionMode::BypassPermissions)
            .await;

        let first_mode = first
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(first_mode, PermissionMode::BypassPermissions);

        let second = pool
            .acquire("conv-b")
            .await
            .map_err(|error| error.to_string())?;
        let second_mode = second
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(second_mode, PermissionMode::BypassPermissions);
        Ok(())
    }

    #[tokio::test]
    async fn tool_control_generation_reaches_primary_existing_and_future_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let existing = pool
            .acquire("tool-control-existing")
            .await
            .map_err(|error| error.to_string())?;
        let receipt = pool
            .tool_control()
            .set_enabled("shell", false)
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.revision, 1);
        pool.publish_tool_control_generation()
            .await
            .map_err(|error| error.to_string())?;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            assert!(
                crate::tool_control::snapshot_disabled_tools(&handle)
                    .await
                    .contains("shell")
            );
        }

        let future = pool
            .acquire("tool-control-future")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            crate::tool_control::snapshot_disabled_tools(&future.agent())
                .await
                .contains("shell")
        );
        let agents = pool.agents.read().await;
        for pooled in agents.values() {
            assert!(
                pooled
                    .model_consumers
                    .tool_control_is_projected_for_test("shell")
                    .await
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn delayed_tool_control_publisher_cannot_overwrite_a_newer_generation() -> TestResult {
        let pool = Arc::new(create_test_pool(3, false).await?);
        let existing = pool
            .acquire("tool-control-race")
            .await
            .map_err(|error| error.to_string())?;
        let agents_guard = pool.agents.write().await;
        pool.tool_control()
            .set_enabled("shell", false)
            .map_err(|error| error.to_string())?;
        let delayed_pool = Arc::clone(&pool);
        let delayed =
            tokio::spawn(async move { delayed_pool.publish_tool_control_generation().await });
        tokio::task::yield_now().await;
        let latest = pool
            .tool_control()
            .set_enabled("read_file", false)
            .map_err(|error| error.to_string())?;
        assert_eq!(latest.revision, 2);
        drop(agents_guard);
        delayed
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        pool.publish_tool_control_generation()
            .await
            .map_err(|error| error.to_string())?;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            let disabled = crate::tool_control::snapshot_disabled_tools(&handle).await;
            assert!(disabled.contains("shell"));
            assert!(disabled.contains("read_file"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn system_prompt_applies_to_primary_existing_and_future_pool_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        pool.apply_system_prompt("Shared EKO prompt".to_string())
            .await;

        assert!(
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        assert!(
            first
                .agent()
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        let future = pool
            .acquire("conv-b")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future
                .agent()
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permission_mode_does_not_wait_for_busy_pool_agent() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        let first_handle = first.agent();
        let busy_guard = first_handle.inner().read().await;

        tokio::time::timeout(
            Duration::from_secs(1),
            pool.apply_permission_mode(PermissionMode::BypassPermissions),
        )
        .await
        .map_err(|_| "permission update waited for a busy agent".to_string())?;
        drop(busy_guard);

        let refreshed = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        let refreshed_mode = refreshed
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(refreshed_mode, PermissionMode::BypassPermissions);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_monitor_start_is_idempotent_and_shutdown_awaits_exit() -> TestResult {
        let pool = Arc::new(create_test_pool(2, false).await?);
        pool.spawn_cleanup_monitor().await;
        let first_id = pool
            .cleanup_handle
            .lock()
            .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
            .as_ref()
            .map(tokio::task::JoinHandle::id);

        pool.spawn_cleanup_monitor().await;
        let second_id = pool
            .cleanup_handle
            .lock()
            .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
            .as_ref()
            .map(tokio::task::JoinHandle::id);
        assert!(first_id.is_some());
        assert_eq!(second_id, first_id);

        pool.shutdown().await?;
        assert!(pool.cleanup_cancel.is_cancelled());
        assert!(
            pool.cleanup_handle
                .lock()
                .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn supervised_release_keeps_overlapping_same_key_agent_until_last_receipt() -> TestResult
    {
        let pool = Arc::new(create_test_pool(2, false).await?);
        let first = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        let first_receipt = pool.retain_for_supervised_run("shared-run".to_string(), first);
        let second = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        let second_agent = second.agent();
        let second_receipt = pool.retain_for_supervised_run("shared-run".to_string(), second);

        crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
            first_receipt,
        ))
        .await;
        assert_eq!(pool.pool_size().await, 1);

        let third = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(second_agent.inner(), third.agent().inner()));
        drop(third);

        crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
            second_receipt,
        ))
        .await;
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_waits_for_active_receipts_and_rejects_later_acquire() -> TestResult {
        let pool = Arc::new(create_test_pool(2, false).await?);
        let active = pool
            .acquire("active-during-shutdown")
            .await
            .map_err(|error| error.to_string())?;
        let shutdown_pool = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { shutdown_pool.shutdown().await });

        while !pool.shutting_down.load(Ordering::Acquire) {
            if shutdown.is_finished() {
                return Err("pool shutdown finished while an execution receipt was active".into());
            }
            tokio::task::yield_now().await;
        }
        assert!(!shutdown.is_finished());
        assert!(matches!(
            pool.acquire("after-shutdown").await,
            Err(PoolError::ShuttingDown)
        ));

        drop(active);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_terminal_debt_reports_abandonment_and_unblocks_pool_shutdown() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                root.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        let canonical_root = std::fs::canonicalize(temp.path())
            .map_err(|error| error.to_string())?
            .join("tasks");
        store
            .create_run(
                "permanent-terminal-debt",
                "workspace-a",
                "conversation",
                "message",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve non-terminal disk truth",
                "",
                crate::tasks::task_runtime::AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "permanent-terminal-debt",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;

        let pool = Arc::new(create_test_pool(2, false).await?);
        let pool_execution = pool
            .acquire("permanent-terminal-debt")
            .await
            .map_err(|error| error.to_string())?;
        let pool_for_driver = Arc::clone(&pool);
        let released_after_pool = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_probe = Arc::clone(&released_after_pool);
        let admission = store
            .reserve_run_driver_admission(
                "permanent-terminal-debt".to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(MemoryReleaseProbe {
                        pool: Arc::clone(&pool_for_driver),
                        released_after_pool: release_probe,
                    });
                    receipt_owner.retain(pool_for_driver.retain_for_supervised_run(
                        "permanent-terminal-debt".to_string(),
                        pool_execution,
                    ));
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected permanent driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let driver_error = waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "permanent driver failure was not reported".to_string())?;
        assert!(driver_error.contains("terminal settlement failed"));

        let shutdown_error =
            tokio::time::timeout(Duration::from_secs(2), store.shutdown_run_drivers())
                .await
                .map_err(|_| "TaskRun driver shutdown timed out on permanent debt".to_string())?
                .err()
                .ok_or_else(|| "permanent settlement debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "abandoned settlement diagnostic is missing".to_string())?;
        let driver_token = diagnostic
            .driver_token
            .ok_or_else(|| "abandoned settlement driver token is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "permanent-terminal-debt");
        assert_eq!(diagnostic.root, canonical_root);
        assert!(!diagnostic.error.is_empty());
        let shutdown_text = shutdown_error.to_string();
        assert!(shutdown_text.contains("run=permanent-terminal-debt"));
        assert!(shutdown_text.contains(&format!("driver_token={driver_token}")));
        assert!(shutdown_text.contains(&diagnostic.root.display().to_string()));
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert!(released_after_pool.load(Ordering::SeqCst));
        let transition = store
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        drop(transition);

        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "AgentPool shutdown remained blocked by abandoned debt".to_string())??;
        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("permanent-terminal-debt")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "non-terminal run disappeared from disk".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn aborted_reporter_and_waiter_do_not_abort_owned_driver_settlement() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                root.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "aborted-shutdown-waiter",
                "workspace-a",
                "conversation",
                "message",
                crate::tasks::task_runtime::DomainProfile::General,
                "retain settlement ownership after waiter abort",
                "",
                crate::tasks::task_runtime::AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "aborted-shutdown-waiter",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;

        let pool = Arc::new(create_test_pool(2, false).await?);
        let execution = pool
            .acquire("aborted-shutdown-waiter")
            .await
            .map_err(|error| error.to_string())?;
        let cancel = echo_agent::agent::CancellationToken::new();
        let driver_cancel = cancel.clone();
        let admission = store
            .reserve_run_driver_admission("aborted-shutdown-waiter".to_string(), cancel)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let (cancel_observed_tx, cancel_observed_rx) = tokio::sync::oneshot::channel::<()>();
        let (continue_driver_tx, continue_driver_rx) = tokio::sync::oneshot::channel::<()>();
        let pool_for_driver = Arc::clone(&pool);
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(pool_for_driver.retain_for_supervised_run(
                        "aborted-shutdown-waiter".to_string(),
                        execution,
                    ));
                    driver_cancel.cancelled().await;
                    cancel_observed_tx
                        .send(())
                        .map_err(|_| "shutdown cancel observer closed".to_string())?;
                    continue_driver_rx
                        .await
                        .map_err(|error| error.to_string())?;
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected failure after shutdown waiter abort".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        drop(waiter);

        store.abort_next_run_driver_shutdown_reporter_for_test();
        let first_shutdown_store = Arc::clone(&store);
        let first_shutdown =
            tokio::spawn(async move { first_shutdown_store.shutdown_run_drivers().await });
        tokio::time::timeout(Duration::from_secs(2), cancel_observed_rx)
            .await
            .map_err(|_| "owned shutdown did not cancel the driver".to_string())?
            .map_err(|_| "driver cancel observer closed".to_string())?;
        first_shutdown.abort();
        if first_shutdown.await.is_ok() {
            return Err("first shutdown waiter was not aborted".to_string());
        }
        continue_driver_tx
            .send(())
            .map_err(|_| "parked driver receiver closed".to_string())?;

        let shutdown_error =
            tokio::time::timeout(Duration::from_secs(2), store.shutdown_run_drivers())
                .await
                .map_err(|_| "second shutdown waiter did not observe owned settlement".to_string())?
                .err()
                .ok_or_else(|| "permanent debt was hidden after waiter abort".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        assert!(
            shutdown_error
                .driver_errors
                .iter()
                .any(|error| error.contains("shutdown reporter failed"))
        );
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "abandoned settlement diagnostic is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "aborted-shutdown-waiter");
        assert!(diagnostic.driver_token.is_some());
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        let repeated_error = store
            .shutdown_run_drivers()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost its typed degradation".to_string())?;
        assert_eq!(repeated_error, shutdown_error);
        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "pool shutdown remained blocked after waiter abort".to_string())??;

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("aborted-shutdown-waiter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "abandoned run disappeared".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        Ok(())
    }
}
