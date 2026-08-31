// AgentPool — multi-agent parallel execution pool.
//
// Enables multiple conversations/tasks to execute concurrently by managing
// a pool of `ReactAgent` instances that share expensive resources (LLM client,
// tool manager, hooks, etc.) while maintaining isolated execution contexts.
//
// # Architecture
//
// ```text
// AgentPool
// ├── SharedResources (Arc-shared across all pool agents)
// │   ├── ToolManager, HookRegistry, SandboxManager
// │   ├── Store, ConversationStore, RunStore, RuntimeStateStore
// │   └── TokenUsageTracker, PermissionService, ToolExecutionPipeline, ReviewIntegration
// │
// └── agents: RwLock<HashMap<String, PooledAgent>>
//     ├── "conv-001" → Agent (independent execution_mutex + ContextManager)
//     ├── "conv-002" → Agent (independent execution_mutex + ContextManager)
//     └── "__background__" → dedicated background task agent
// ```
//
// # Usage
//
// ```rust,ignore
// // After bootstrap:
// let pool = AgentPool::from_runtime(&runtime, PoolConfig::default(), None).await;
//
// // Acquire an agent for a conversation:
// let lease = pool.acquire("conv-001").await?;
// let agent = lease.agent();
// agent.chat_stream("Hello").await;  // Keep `lease` until execution settles.
// ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use echo_agent::agent::admission::{
    KeyedExecutionAdmission, KeyedExecutionAdmissionError, KeyedExecutionLease,
    KeyedExecutionRetirement,
};
use echo_agent::agent::AgentHandle;
use echo_agent::agent::CancellationToken;
use tokio::sync::RwLock;

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
        Option<Arc<echo_agent::agent::ToolExecutionPipeline>>,
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
    framework: Arc<KeyedExecutionAdmission>,
}

impl Default for AgentPoolAdmission {
    fn default() -> Self {
        Self {
            framework: Arc::new(KeyedExecutionAdmission::default()),
        }
    }
}

impl AgentPoolAdmission {
    fn issue_process_scoped(
        self: &Arc<Self>,
        key: &str,
        agent: AgentHandle,
        governor: &Arc<AgentExecutionGovernor>,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let framework_lease = self
            .framework
            .issue_process_scoped(key, &governor.semaphore)
            .map_err(PoolError::from)?;
        Ok(AgentPoolExecutionLease {
            agent,
            admission: Some((Arc::clone(self), key.to_string())),
            framework_lease: Some(framework_lease),
        })
    }

    fn is_active(&self, key: &str) -> bool {
        self.framework.is_active(key)
    }

    fn is_retiring(&self, key: &str) -> bool {
        self.framework.is_retiring(key)
    }

    fn begin_retirement(
        self: &Arc<Self>,
        key: &str,
    ) -> Result<AgentPoolRetirementAdmission, PoolError> {
        let framework = self.framework.begin_retirement(key).map_err(PoolError::from)?;
        Ok(AgentPoolRetirementAdmission {
            admission: Arc::clone(self),
            framework: Some(framework),
            active: true,
        })
    }

    async fn wait_key_idle(&self, key: &str) {
        self.framework.wait_key_idle(key).await;
    }

    fn close(&self) {
        self.framework.close();
    }

    async fn wait_until_idle(&self) {
        self.framework.wait_until_idle().await;
    }

    fn active_count(&self) -> usize {
        self.framework.active_count()
    }
}

impl From<KeyedExecutionAdmissionError> for PoolError {
    fn from(error: KeyedExecutionAdmissionError) -> Self {
        match error {
            KeyedExecutionAdmissionError::Closed => Self::ShuttingDown,
            KeyedExecutionAdmissionError::Retiring { key }
            | KeyedExecutionAdmissionError::RetirementAlreadyActive { key } => {
                Self::ConversationRetirementPending {
                    conversation_id: key,
                }
            }
            KeyedExecutionAdmissionError::CapacityOverflow { .. }
            | KeyedExecutionAdmissionError::ProcessCapacity => Self::ExecutionLeaseCapacity,
        }
    }
}

struct AgentPoolRetirementAdmission {
    admission: Arc<AgentPoolAdmission>,
    framework: Option<KeyedExecutionRetirement>,
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
        drop(self.framework.take());
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
    framework_lease: Option<KeyedExecutionLease>,
}

impl AgentPoolExecutionLease {
    pub fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    pub(crate) fn unpooled(agent: AgentHandle) -> Self {
        Self {
            agent,
            admission: None,
            framework_lease: None,
        }
    }

    fn owns(&self, admission: &Arc<AgentPoolAdmission>, key: &str) -> bool {
        self.admission
            .as_ref()
            .is_some_and(|(owner, owned_key)| {
                Arc::ptr_eq(owner, admission)
                    && owned_key == key
                    && self
                        .framework_lease
                        .as_ref()
                        .is_some_and(|lease| lease.owns(&admission.framework, key))
            })
    }
}

impl Drop for AgentPoolExecutionLease {
    fn drop(&mut self) {
        self.admission.take();
        self.framework_lease.take();
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
