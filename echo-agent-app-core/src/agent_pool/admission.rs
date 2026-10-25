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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use echo_agent::agent::AgentHandle;
use echo_agent::agent::CancellationToken;
use echo_agent::agent::admission::{
    KeyedExecutionAdmission, KeyedExecutionAdmissionError, KeyedExecutionLease,
    KeyedExecutionRetirement,
};
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
    domain_components: Arc<crate::domain_plugin::PreparedDomainComponents>,
    domain_output_style: Option<String>,
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
            domain_components: Arc::new(Default::default()),
            domain_output_style: None,
            framework_generation: None,
        }
    }

    pub(crate) fn with_domain_components(
        mut self,
        components: crate::domain_plugin::PreparedDomainComponents,
        active_style: Option<String>,
    ) -> Self {
        self.domain_components = Arc::new(components);
        self.domain_output_style = active_style;
        self
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
/// multiple pool agents. Cloneable handles preserve the underlying
/// thread-safe resource owners.
pub struct SharedResources {
    /// One framework Subagent event transport shared by every Agent generation
    /// in the process, including workspace-owned pool forks.
    pub(crate) subagent_event_bus: echo_agent::subagent::SubagentEventBus,
    pub tool_manager: Option<Arc<echo_agent::tools::ToolManager>>,
    pub hook_registry: Option<Arc<tokio::sync::RwLock<echo_agent::skills::hooks::HookRegistry>>>,
    pub sandbox_manager: Option<Arc<dyn echo_agent::sandbox::SandboxExecutor>>,
    pub local_skill_executor: Option<Arc<dyn echo_agent::sandbox::SandboxExecutor>>,
    pub file_system: Option<Arc<dyn echo_agent::tools::filesystem::FileSystem>>,
    pub workspace_data_root: Option<std::path::PathBuf>,
    pub store: Option<Arc<dyn echo_agent::memory::Store>>,
    pub conversation_store: Option<Arc<dyn echo_agent::memory::ConversationStore>>,
    pub run_store: Option<Arc<dyn echo_agent::trace::RunStore>>,
    pub token_tracker: Option<Arc<echo_agent::tokenizer::TokenUsageTracker>>,
    pub permission_service: Option<Arc<echo_agent::human_loop::service::PermissionService>>,
    pub state_store: Option<Arc<dyn echo_agent::state::RuntimeStateStore>>,
    pub tool_execution_pipeline: Option<Arc<echo_agent::agent::ToolExecutionPipeline>>,
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
    pub execution_root: std::path::PathBuf,
    pub file_system: Arc<dyn echo_agent::tools::filesystem::FileSystem>,
    pub executor: Arc<dyn echo_agent::sandbox::SandboxExecutor>,
    pub project_root: Option<std::path::PathBuf>,
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
                let subagent_event_bus = a.subagent_registry().event_bus().clone();
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
                    subagent_event_bus,
                    tool_manager,
                    hook_registry,
                    local_skill_executor: sandbox_manager.clone(),
                    sandbox_manager,
                    file_system: None,
                    workspace_data_root: None,
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
    subagent_registry: Arc<echo_agent::subagent::SubagentRegistry>,
    tool_manager: Arc<echo_agent::tools::ToolManager>,
    _conversation_id: String,
    created_at: Instant,
    last_used: Instant,
}

struct AgentPoolAdmission {
    framework: Arc<KeyedExecutionAdmission>,
    // Observation projection of exact existing leases; framework still owns
    // every actual keyed/process permit and its release.
    leases: std::sync::Mutex<PoolLeaseObservations>,
    progress: tokio::sync::watch::Sender<u64>,
    lifecycle_retention:
        std::sync::RwLock<std::sync::Weak<crate::runtime::ApplicationLifecycleRetention>>,
}

#[derive(Default)]
struct PoolLeaseObservations {
    next: u64,
    live: std::collections::BTreeMap<u64, PoolLeaseObservation>,
}

struct PoolLeaseObservation {
    key: String,
    owner: Option<(String, Option<String>)>,
}

impl Default for AgentPoolAdmission {
    fn default() -> Self {
        Self {
            framework: Arc::new(KeyedExecutionAdmission::default()),
            leases: std::sync::Mutex::new(PoolLeaseObservations::default()),
            progress: tokio::sync::watch::channel(0).0,
            lifecycle_retention: std::sync::RwLock::new(std::sync::Weak::new()),
        }
    }
}

impl AgentPoolAdmission {
    fn notify_progress(&self) {
        self.progress
            .send_modify(|revision| *revision = revision.saturating_add(1));
        let root = self
            .lifecycle_retention
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade();
        if let Some(root) = root {
            root.note_progress();
        }
    }

    fn issue_process_scoped(
        self: &Arc<Self>,
        key: &str,
        agent: AgentHandle,
        governor: &Arc<AgentExecutionGovernor>,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| PoolError::ExecutionLeaseCapacity)?;
        if leases
            .live
            .values()
            .any(|entry| entry.key == key && entry.owner.is_some())
        {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: key.to_string(),
            });
        }
        let token = leases
            .next
            .checked_add(1)
            .ok_or(PoolError::ExecutionLeaseCapacity)?;
        let framework_lease = self
            .framework
            .issue_process_scoped(key, &governor.semaphore)
            .map_err(PoolError::from)?;
        leases.next = token;
        leases.live.insert(
            token,
            PoolLeaseObservation {
                key: key.to_string(),
                owner: None,
            },
        );
        drop(leases);
        Ok(AgentPoolExecutionLease {
            physical: Arc::new(AgentPoolPhysicalLease {
                token: Some(token),
                admission: Some((Arc::clone(self), key.to_string())),
                framework_lease: Some(framework_lease),
                projections: AtomicUsize::new(0),
                projection_progress: Arc::new(tokio::sync::Notify::new()),
            }),
            agent,
            domain_generation: None,
            admitted_catalogs: None,
            admitted_tool_control: None,
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
        let framework = self
            .framework
            .begin_retirement(key)
            .map_err(PoolError::from)?;
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

    fn observe_unsettled(&self, token: u64, run: &str, execution: Option<&str>) {
        let changed = {
            let mut leases = self
                .leases
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = leases.live.get_mut(&token) {
                let owner = Some((run.to_string(), execution.map(str::to_string)));
                if entry.owner == owner {
                    false
                } else {
                    entry.owner = owner;
                    true
                }
            } else {
                false
            }
        };
        if changed {
            self.notify_progress();
        }
    }

    fn has_unsettled(&self) -> bool {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .values()
            .any(|entry| entry.owner.is_some())
    }

    async fn join_known(&self) -> Result<(), String> {
        let mut progress = self.progress.subscribe();
        loop {
            let observation = {
                let leases = self.leases.lock().map_err(|error| error.to_string())?;
                if leases.live.values().any(|entry| entry.owner.is_none()) {
                    None
                } else {
                    Some(
                        leases
                            .live
                            .values()
                            .filter_map(|entry| entry.owner.as_ref())
                            .map(|(run, execution)| {
                                format!(
                                    "run={run} execution={}",
                                    execution.as_deref().unwrap_or("unknown")
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                }
            };
            if let Some(unknown) = observation {
                if !unknown.is_empty() {
                    return Err(format!(
                        "original pool execution remains physically unsettled: {}",
                        unknown.join("; ")
                    ));
                }
                // Metadata cannot replace the actual framework permit drain.
                self.framework.wait_until_idle().await;
                return Ok(());
            }
            progress
                .changed()
                .await
                .map_err(|_| "original pool progress channel closed".to_string())?;
        }
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
    physical: Arc<AgentPoolPhysicalLease>,
    agent: AgentHandle,
    domain_generation: Option<(
        Arc<crate::domain_plugin::PreparedDomainComponents>,
        Option<String>,
    )>,
    admitted_tool_control: Option<Arc<crate::tool_control::ToolControlService>>,
    admitted_catalogs: Option<(
        Arc<echo_agent::subagent::SubagentRegistry>,
        Arc<echo_agent::tools::ToolManager>,
    )>,
}

/// Read-only, short-lived view of an already issued foreground lease. It
/// cannot issue a token or release/retire the original receipt. Drop it before
/// awaiting driver settlement; only actual invocation guards cross that point.
pub(crate) struct AgentPoolExecutionProjection {
    // Field order matters: drop the borrowed snapshot before notifying the
    // final physical-reference waiter through the retention field.
    execution: AgentPoolExecutionLease,
    _retention: AgentPoolPhysicalProjection,
}

impl AgentPoolExecutionProjection {
    pub(crate) fn execution(&self) -> &AgentPoolExecutionLease {
        &self.execution
    }
}

/// The original lease accounting, retained until its final physical projection.
/// It deliberately has no Agent/factory reference and cannot form an Agent cycle.
struct AgentPoolPhysicalLease {
    token: Option<u64>,
    admission: Option<(Arc<AgentPoolAdmission>, String)>,
    framework_lease: Option<KeyedExecutionLease>,
    projections: AtomicUsize,
    projection_progress: Arc<tokio::sync::Notify>,
}

pub(crate) struct AgentPoolPhysicalProjection {
    physical: Option<Arc<AgentPoolPhysicalLease>>,
    // The original projection registration is not another TaskRun authority.
    // It keeps cross-spawn reads bound to that same RunCreated/store lifetime.
    _run_projection:
        Option<Arc<crate::tasks::task_runtime::compact_context::TaskRuntimeProjectionRegistration>>,
}

impl Drop for AgentPoolPhysicalProjection {
    fn drop(&mut self) {
        let Some(physical) = self.physical.take() else {
            return;
        };
        let progress = physical.projection_progress.clone();
        if physical
            .projections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .is_err()
        {
            tracing::error!("AgentPool physical projection accounting was already released");
        }
        // The last supervised remover must observe the actual reference gone,
        // not a zero counter followed by a still-live keyed lease.
        drop(physical);
        progress.notify_waiters();
    }
}

impl AgentPoolExecutionLease {
    pub fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    fn with_plugin_generation(
        mut self,
        generation: &AgentPluginGeneration,
        registry: Arc<echo_agent::subagent::SubagentRegistry>,
        tools: Arc<echo_agent::tools::ToolManager>,
        tool_control: Arc<crate::tool_control::ToolControlService>,
    ) -> Self {
        self.domain_generation = Some((
            generation.domain_components.clone(),
            generation.domain_output_style.clone(),
        ));
        self.admitted_catalogs = Some((registry, tools));
        self.admitted_tool_control = Some(tool_control);
        self
    }

    /// Validate the actual target generation while this original lease keeps
    /// publication/retirement closed. Cross-workspace dispatch keeps its own
    /// target receipt; a leader pool cannot vouch for another workspace.
    pub(crate) async fn verify_domain_binding(
        &self,
        binding: &crate::domain_plugin::DomainBinding,
    ) -> Result<(), String> {
        let (owner, key) = self.physical.admission.as_ref().ok_or_else(|| {
            "domain execution cannot use an unpooled handle as a receipt".to_string()
        })?;
        if !self.owns(owner, key) {
            return Err("domain execution lease is not live".to_string());
        }
        let (components, style) = self.domain_generation.as_ref().ok_or_else(|| {
            "domain execution lease has no published plugin generation".to_string()
        })?;
        let (registry, tools) = self
            .admitted_catalogs
            .as_ref()
            .ok_or_else(|| "domain lease lost its original Agent catalog handles".to_string())?;
        let policy = self
            .admitted_tool_control
            .as_ref()
            .ok_or_else(|| "domain lease lost its original tool policy owner".to_string())?
            .snapshot()
            .map_err(|error| error.to_string())?;
        binding.check_capabilities(&tools.list_tools(), &policy.disabled_tools)?;
        components.verify_binding(binding, style.as_deref(), &registry.list_available().await)
    }

    pub(crate) fn project_for_invocation(&self) -> Result<AgentPoolExecutionProjection, String> {
        let retention = self.retain_physical_projection(None)?;
        Ok(AgentPoolExecutionProjection {
            execution: Self {
                physical: self.physical.clone(),
                agent: self.agent.clone(),
                domain_generation: self.domain_generation.clone(),
                admitted_tool_control: self.admitted_tool_control.clone(),
                admitted_catalogs: self.admitted_catalogs.clone(),
            },
            _retention: retention,
        })
    }

    fn retain_physical_projection(
        &self,
        run_projection: Option<
            Arc<crate::tasks::task_runtime::compact_context::TaskRuntimeProjectionRegistration>,
        >,
    ) -> Result<AgentPoolPhysicalProjection, String> {
        let (owner, key) =
            self.physical.admission.as_ref().ok_or_else(|| {
                "domain physical retention requires a pooled admission".to_string()
            })?;
        if !self.owns(owner, key) {
            return Err(
                "domain physical retention requires the original live pool lease".to_string(),
            );
        }
        self.physical
            .projections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| "domain physical retention capacity exhausted".to_string())?;
        Ok(AgentPoolPhysicalProjection {
            physical: Some(self.physical.clone()),
            _run_projection: run_projection,
        })
    }

    pub(crate) fn domain_resource_guard(
        &self,
        run_id: &str,
        binding: &crate::domain_plugin::DomainBinding,
        run_projection: Option<
            Arc<crate::tasks::task_runtime::compact_context::TaskRuntimeProjectionRegistration>,
        >,
    ) -> Result<echo_agent::tools::InvocationResourceGuard, String> {
        let (owner, _) =
            self.physical.admission.as_ref().ok_or_else(|| {
                "domain physical retention requires a pooled admission".to_string()
            })?;
        Ok(echo_agent::tools::InvocationResourceGuard::new_identified(
            self.retain_physical_projection(run_projection)?,
            crate::domain_plugin::DomainInvocationIdentity::new(
                run_id,
                binding.reference(),
                Arc::as_ptr(owner) as usize,
            ),
        ))
    }

    async fn wait_for_physical_projections(&self) {
        loop {
            let progress = self.physical.projection_progress.notified();
            tokio::pin!(progress);
            progress.as_mut().enable();
            if self.physical.projections.load(Ordering::Acquire) == 0
                && Arc::strong_count(&self.physical) == 1
            {
                return;
            }
            progress.await;
        }
    }

    pub(crate) fn unpooled(agent: AgentHandle) -> Self {
        Self {
            physical: Arc::new(AgentPoolPhysicalLease {
                token: None,
                admission: None,
                framework_lease: None,
                projections: AtomicUsize::new(0),
                projection_progress: Arc::new(tokio::sync::Notify::new()),
            }),
            agent,
            domain_generation: None,
            admitted_catalogs: None,
            admitted_tool_control: None,
        }
    }

    fn owns(&self, admission: &Arc<AgentPoolAdmission>, key: &str) -> bool {
        self.physical
            .admission
            .as_ref()
            .is_some_and(|(owner, owned_key)| {
                Arc::ptr_eq(owner, admission)
                    && owned_key == key
                    && self
                        .physical
                        .framework_lease
                        .as_ref()
                        .is_some_and(|lease| lease.owns(&admission.framework, key))
            })
    }
}

impl Drop for AgentPoolPhysicalLease {
    fn drop(&mut self) {
        let admission = self.admission.take();
        if let (Some((owner, _)), Some(token)) = (admission.as_ref(), self.token.take()) {
            owner
                .leases
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .live
                .remove(&token);
        }
        self.framework_lease.take();
        if let Some((owner, _)) = admission {
            owner.notify_progress();
        }
    }
}

impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for AgentPoolExecutionLease {
    fn observe_unsettled(&self, run_id: &str, execution_id: Option<&str>) {
        if let (Some((admission, _)), Some(token)) =
            (self.physical.admission.as_ref(), self.physical.token)
        {
            admission.observe_unsettled(token, run_id, execution_id);
        }
    }

    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            self.wait_for_physical_projections().await;
            drop(self);
        })
    }
}

// GUI holds this same lease cell until original turn/HITL cleanup. The
// observation projection must mark its actual token without taking the lease.
impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt
    for Arc<std::sync::Mutex<Option<AgentPoolExecutionLease>>>
{
    fn observe_unsettled(&self, run_id: &str, execution_id: Option<&str>) {
        if let Some(execution) = self
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            crate::tasks::task_runtime::store::RunDriverExecutionReceipt::observe_unsettled(
                execution,
                run_id,
                execution_id,
            );
        }
    }
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
    fn observe_unsettled(&self, run_id: &str, execution_id: Option<&str>) {
        if let Some(execution) = self.execution.as_ref() {
            crate::tasks::task_runtime::store::RunDriverExecutionReceipt::observe_unsettled(
                execution,
                run_id,
                execution_id,
            );
        }
    }
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

/// Thin EKO policy on the framework's original selected registration. No
/// registry lookup or second dispatcher may replace the record passed here.
struct DomainRegistrationAdmission {
    registry: std::sync::Weak<echo_agent::subagent::SubagentRegistry>,
    generation: std::sync::Weak<RwLock<AgentPluginGeneration>>,
    admission: std::sync::Weak<AgentPoolAdmission>,
    store: Option<std::sync::Weak<crate::tasks::task_runtime::store::TaskRuntimeStore>>,
}

#[async_trait::async_trait]
impl echo_agent::subagent::SubagentRegistrationAdmission for DomainRegistrationAdmission {
    async fn admit(
        &self,
        registration: &echo_agent::subagent::SubagentRegistration,
        request: &echo_agent::subagent::DispatchRequest,
    ) -> echo_agent::error::Result<()> {
        let fail = |message: String| echo_agent::error::ReactError::Other(message);
        let Some(context) = request.runtime_context.as_ref() else {
            return Ok(());
        };
        let physical = context
            .resource_guards
            .iter()
            .any(|guard| guard.retains::<AgentPoolPhysicalProjection>());
        let Some(run_id) = context.run_id.as_deref() else {
            return if physical {
                Err(fail(
                    "bound domain Subagent lost its original TaskRun identity".to_string(),
                ))
            } else {
                Ok(())
            };
        };
        if run_id.trim().is_empty() {
            return Err(fail("Subagent TaskRun identity is empty".to_string()));
        }
        let store = crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
            .store(run_id)
            .or_else(|| self.store.as_ref().and_then(std::sync::Weak::upgrade))
            .ok_or_else(|| fail("Subagent TaskRun has no original runtime owner".to_string()))?;
        let owned_run_id = run_id.to_string();
        let snapshot = crate::tasks::task_runtime::TaskRuntimeOperation::new(store)
            .run_store("admit exact domain Subagent registration", move |store| {
                store.get_run_state(&owned_run_id)?.ok_or(
                    crate::tasks::task_runtime::StoreError::RunNotFound(owned_run_id),
                )
            })
            .await
            .map_err(|error| fail(error.to_string()))?;
        let Some(binding) = snapshot.domain_binding else {
            if physical
                || snapshot.run.domain_profile != crate::tasks::task_runtime::DomainProfile::General
            {
                return Err(fail(
                    "Subagent TaskRun is missing its original domain binding".to_string(),
                ));
            }
            return Ok(());
        };
        if snapshot.run.domain_binding.as_ref() != Some(&binding.reference()) {
            return Err(fail(
                "Subagent TaskRun domain projection does not match RunCreated".to_string(),
            ));
        }
        let owner = self
            .admission
            .upgrade()
            .ok_or_else(|| fail("Subagent pool admission owner is unavailable".to_string()))?;
        let expected = crate::domain_plugin::DomainInvocationIdentity::new(
            run_id,
            binding.reference(),
            Arc::as_ptr(&owner) as usize,
        );
        if !context.resource_guards.iter().any(|guard| {
            guard.retains::<AgentPoolPhysicalProjection>() && guard.matches_identity(&expected)
        }) {
            return Err(fail("domain Subagent has no physical retention for its actual target pool and original binding".to_string()));
        }
        // Keep the actual registry alive through this check, but do not resolve
        // the selected name again. F already proves this record's owner.
        let _registry = self
            .registry
            .upgrade()
            .ok_or_else(|| fail("Subagent registry owner is unavailable".to_string()))?;
        let generation = self
            .generation
            .upgrade()
            .ok_or_else(|| fail("Subagent plugin generation is unavailable".to_string()))?;
        let (components, style) = {
            let generation = generation.read().await;
            (
                generation.domain_components.clone(),
                generation.domain_output_style.clone(),
            )
        };
        components
            .admit_registration(&binding, style.as_deref(), registration.definition())
            .map_err(fail)
    }
}

impl PooledAgent {
    fn new(
        handle: AgentHandle,
        model_consumers: infra::AgentModelConsumers,
        conversation_id: String,
        subagent_registry: Arc<echo_agent::subagent::SubagentRegistry>,
        tool_manager: Arc<echo_agent::tools::ToolManager>,
    ) -> Self {
        let now = Instant::now();
        Self {
            handle,
            model_consumers,
            subagent_registry,
            tool_manager,
            _conversation_id: conversation_id,
            created_at: now,
            last_used: now,
        }
    }
}
