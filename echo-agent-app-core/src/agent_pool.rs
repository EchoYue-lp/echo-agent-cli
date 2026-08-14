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
//! │   ├── LlmClient, ToolManager, HookRegistry, SandboxManager
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use echo_agent::agent::AgentHandle;
use echo_agent::agent::CancellationToken;
use echo_agent::llm::LlmClient;
use tokio::sync::{Notify, RwLock};

use crate::infra;
use crate::model_config::ModelRuntimeConfig;
use crate::workspace::WorkspaceKind;
use echo_agent::config::AppConfig;

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
        }
    }
}

impl std::error::Error for PoolError {}

/// Resources extracted from the primary agent that can be shared across
/// multiple pool agents. All fields are `Arc`-wrapped for thread-safe sharing.
pub struct SharedResources {
    pub llm_client: Option<Arc<dyn LlmClient>>,
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
                let llm_client = a.llm_client().cloned();
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
                    llm_client,
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
                }
            })
            .await
    }
}

/// Internal wrapper around a pooled agent with metadata.
struct PooledAgent {
    handle: AgentHandle,
    _conversation_id: String,
    created_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
struct AgentPoolAdmission {
    active: Mutex<AgentPoolAdmissionState>,
    idle: Notify,
}

#[derive(Default)]
struct AgentPoolAdmissionState {
    total: usize,
    by_key: HashMap<String, usize>,
}

impl AgentPoolAdmission {
    fn issue(
        self: &Arc<Self>,
        key: &str,
        agent: AgentHandle,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        active.total = total;
        active.by_key.insert(key.to_string(), key_count);
        drop(active);
        Ok(AgentPoolExecutionLease {
            agent,
            admission: Some((Arc::clone(self), key.to_string())),
        })
    }

    fn is_active(&self, key: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_key
            .get(key)
            .is_some_and(|count| *count != 0)
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
        if let Some(count) = active.by_key.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                active.by_key.remove(&key);
            }
        }
        let released_last = active.total == 0;
        drop(active);
        if released_last {
            admission.idle.notify_waiters();
        }
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
    fn new(handle: AgentHandle, conversation_id: String) -> Self {
        let now = Instant::now();
        Self {
            handle,
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
    workspace_transitioning: AtomicBool,
    shutting_down: AtomicBool,
    admission: Arc<AgentPoolAdmission>,
    config: PoolConfig,
    app_config: RwLock<AppConfig>,
    /// Working directory applied to existing and future pooled agents.
    working_dir: RwLock<Option<std::path::PathBuf>>,
    runtime_llm_config: RwLock<Option<echo_agent::llm::LlmConfig>>,
    permission_mode: RwLock<String>,
    /// Skill descriptors extracted from the primary agent.
    /// Pool agents register these instead of re-reading from disk.
    skill_descriptors: RwLock<Vec<echo_agent::skills::external::SkillDescriptor>>,
    /// Cancellation token for the cleanup monitor task.
    cleanup_cancel: CancellationToken,
    /// Sole owned cleanup monitor settlement handle. The monitor holds only a
    /// weak pool reference so a failed bootstrap cannot keep the pool alive.
    cleanup_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Workspace-scoped memory store override. Set by `apply_memory_store`
    /// on workspace switch so newly-created pool agents also bind to the
    /// current workspace's memory store (not the stale shared.store captured
    /// at bootstrap). `None` means "use shared.store" (pre-switch behavior).
    memory_store_override: RwLock<Option<Arc<dyn echo_agent::memory::Store>>>,
    /// Workspace-scoped conversation store used by existing and future agents.
    conversation_store_override: RwLock<Option<Arc<dyn echo_agent::memory::ConversationStore>>>,
    /// Product-owned complete tool-output artifact policy for existing and
    /// future pooled agents. Updated together with workspace routing.
    tool_output_artifacts: RwLock<echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    /// Active workspace profile applied to existing and future pooled agents.
    workspace_kind: RwLock<WorkspaceKind>,
}

pub(crate) struct AgentPoolWorkspaceTransition<'a> {
    pool: &'a AgentPool,
    committed: bool,
}

impl AgentPoolWorkspaceTransition<'_> {
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
    ) -> Self {
        let shared = SharedResources::extract_from(
            &runtime.agent_handle,
            runtime.review_integration.clone(),
        )
        .await;
        let mut shared = shared;
        shared.browser_runtime = Some(runtime.browser_runtime.clone());
        shared.task_runtime_store = task_runtime_store;

        // Extract skill descriptors from primary agent (avoids re-reading from disk)
        let skill_descriptors = runtime.agent_handle.read(|a| a.skill_descriptors()).await;
        let tool_output_artifacts = runtime
            .agent_handle
            .read(|agent| agent.tool_output_artifacts())
            .await
            .unwrap_or_else(|| crate::infra::tool_output_artifact_config(None));
        let working_dir = runtime.agent_handle.read(|agent| agent.working_dir()).await;

        let pool = Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            config,
            app_config: RwLock::new(runtime.app_config.clone()),
            working_dir: RwLock::new(working_dir),
            runtime_llm_config: RwLock::new(None),
            permission_mode: RwLock::new("default".to_string()),
            skill_descriptors: RwLock::new(skill_descriptors),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            memory_store_override: RwLock::new(None),
            conversation_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(tool_output_artifacts),
            workspace_kind: RwLock::new(WorkspaceKind::General),
        };

        // Pre-create background agent if enabled
        if pool.config.enable_background_agent {
            match pool.create_agent("__background__").await {
                Ok(handle) => {
                    let mut agents = pool.agents.write().await;
                    agents.insert(
                        "__background__".to_string(),
                        PooledAgent::new(handle, "__background__".to_string()),
                    );
                    tracing::info!("AgentPool: background agent created");
                }
                Err(e) => {
                    tracing::warn!("AgentPool: failed to create background agent: {e}");
                }
            }
        }

        pool
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent: AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
        store: Option<Arc<dyn echo_agent::memory::Store>>,
        max_agents: usize,
        enable_background_agent: bool,
    ) -> Self {
        let mut shared = SharedResources::extract_from(&agent, review_integration).await;
        if let Some(store) = store {
            shared.store = Some(store);
        }
        let mut app_config = AppConfig::default();
        app_config.model.provider = "test".to_string();
        app_config.model.name = "test-model".to_string();
        Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            config: PoolConfig {
                max_agents,
                idle_timeout: Duration::from_secs(1800),
                enable_background_agent,
            },
            app_config: RwLock::new(app_config),
            working_dir: RwLock::new(None),
            runtime_llm_config: RwLock::new(None),
            permission_mode: RwLock::new("default".to_string()),
            skill_descriptors: RwLock::new(Vec::new()),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            memory_store_override: RwLock::new(None),
            conversation_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(crate::infra::tool_output_artifact_config(None)),
            workspace_kind: RwLock::new(WorkspaceKind::General),
        }
    }

    /// Whether this key consumes one user-conversation capacity slot.
    fn is_conversation_agent(key: &str) -> bool {
        key != "__background__" && !key.starts_with("__task__:")
    }

    /// Acquire an agent for a given conversation ID.
    ///
    /// If an agent already exists for this ID, it is returned (with updated
    /// `last_used` timestamp). Otherwise, a new agent is created and added
    /// to the pool. Pool capacity counts conversation agents only; task
    /// subagents and the background agent have separate product ownership.
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

        // Fast path: reuse existing agent
        if let Some(existing) = agents.get_mut(conversation_id) {
            existing.last_used = Instant::now();
            return self
                .admission
                .issue(conversation_id, existing.handle.clone());
        }

        // Enforce pool limit — evict oldest idle agent that is NOT executing
        // P1-13: 只计对话 agent, 排除 __background__ 和 __task__ subagent。
        let active_count = agents
            .keys()
            .filter(|k| Self::is_conversation_agent(k))
            .count();
        if active_count >= self.config.max_agents {
            // Find oldest non-background, non-executing conversation agent
            let mut candidates: Vec<(String, Instant)> = agents
                .iter()
                .filter(|(id, _)| Self::is_conversation_agent(id) && !self.admission.is_active(id))
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
        let handle = self
            .create_agent(conversation_id)
            .await
            .map_err(|e| PoolError::AgentCreation(e.to_string()))?;

        agents.insert(
            conversation_id.to_string(),
            PooledAgent::new(handle.clone(), conversation_id.to_string()),
        );

        tracing::info!(
            conv_id = %conversation_id,
            pool_size = agents.len(),
            "AgentPool: new agent created"
        );

        self.admission.issue(conversation_id, handle)
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
        agents
            .get(conversation_id)
            .map(|pooled| self.admission.issue(conversation_id, pooled.handle.clone()))
            .transpose()
    }

    /// Release an agent from the pool (marks for cleanup).
    pub async fn release(&self, conversation_id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(pa) = agents.remove(conversation_id) {
            tracing::info!(
                conv_id = %conversation_id,
                age_secs = pa.created_at.elapsed().as_secs(),
                "AgentPool: agent released"
            );
        }
    }

    /// Release one exact supervised execution receipt. Dropping the receipt
    /// and deciding whether to remove the cached agent happen under the same
    /// agents lock used by acquire, so overlapping drivers for one key cannot
    /// remove each other's live agent.
    async fn release_supervised_execution(
        &self,
        conversation_id: &str,
        execution: AgentPoolExecutionLease,
    ) {
        let mut agents = self.agents.write().await;
        drop(execution);
        if self.admission.is_active(conversation_id) {
            return;
        }
        if let Some(agent) = agents.remove(conversation_id) {
            tracing::info!(
                conv_id = %conversation_id,
                age_secs = agent.created_at.elapsed().as_secs(),
                "AgentPool: supervised agent released"
            );
        }
    }

    #[cfg(test)]
    async fn background_agent(&self) -> Option<AgentHandle> {
        let agents = self.agents.read().await;
        agents.get("__background__").map(|pa| pa.handle.clone())
    }

    /// Update the pool's app config snapshot used for future agents.
    pub async fn update_app_config(&self, app_config: AppConfig) {
        *self.app_config.write().await = app_config;
    }

    /// Apply a runtime model to all existing pooled agents and remember it for
    /// future agents. This prevents pooled GUI conversations from continuing to
    /// use stale env-derived credentials after the user saves a GUI API key.
    pub async fn apply_runtime_model(&self, runtime: ModelRuntimeConfig) {
        let llm_config = runtime.auth_token.as_ref().map(|token| {
            infra::build_llm_config(
                &runtime.provider,
                token,
                &runtime.model,
                runtime.base_url.as_deref(),
            )
        });
        *self.runtime_llm_config.write().await = llm_config.clone();

        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        for handle in agents {
            let runtime = runtime.clone();
            let llm_config = llm_config.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        if let Some(config) = llm_config {
                            agent.set_llm_config(config);
                        } else {
                            agent.set_model(&runtime.model);
                        }
                        agent.set_temperature(runtime.temperature);
                        agent.set_max_tokens(runtime.max_tokens);
                        // Apply context_window as token_limit when set.
                        if let Some(cw) = runtime.context_window
                            && let Err(error) = agent.set_token_limit(cw as usize)
                        {
                            tracing::error!(
                                error = %error,
                                "AgentPool: failed to apply model context window"
                            );
                        }
                        match runtime.thinking.as_deref() {
                            Some(spec) if !spec.trim().is_empty() => {
                                match echo_agent::llm::ThinkingConfig::parse_spec(spec) {
                                    Ok(config) => agent.set_thinking(config),
                                    Err(error) => tracing::warn!(
                                        thinking_spec = spec,
                                        error = %error,
                                        "AgentPool: ignoring invalid thinking configuration"
                                    ),
                                }
                            }
                            _ => agent.set_thinking(None),
                        }
                    })
                })
                .await;
        }

        let pooled_agents = self.agents.read().await.len();
        tracing::info!(
            provider = %runtime.provider,
            model = %runtime.model,
            auth_source = %runtime.auth_source,
            pooled_agents = pooled_agents,
            "AgentPool: runtime model applied"
        );
    }

    /// Apply the current permission mode to all existing pooled agents and
    /// remember it for future agents.
    pub async fn apply_permission_mode(&self, mode: String) {
        *self.permission_mode.write().await = mode.clone();

        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();

        for handle in agents {
            let mode = mode.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_permission_mode(&mode);
                    })
                })
                .await;
        }

        let pooled_agents = self.agents.read().await.len();
        tracing::info!(mode = %mode, pooled_agents, "AgentPool: permission mode applied");
    }

    /// Refresh available file-based skill descriptors for future and existing
    /// pooled agents after the primary agent discovers or loads skills.
    pub async fn refresh_skill_descriptors(
        &self,
        descriptors: Vec<echo_agent::skills::external::SkillDescriptor>,
    ) {
        *self.skill_descriptors.write().await = descriptors.clone();

        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        let descriptor_count = descriptors.len();
        for handle in agents {
            let descriptors = descriptors.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        for desc in descriptors {
                            agent.skill_registry_mut().register_descriptor(desc);
                        }
                    })
                })
                .await;
        }

        let pooled_agents = self.agents.read().await.len();
        tracing::info!(
            descriptor_count,
            pooled_agents,
            "AgentPool: skill descriptors refreshed"
        );
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

    /// Rebind all pooled agents to a workspace-scoped memory store.
    ///
    /// Called after `switch_workspace` so that pooled agents (background +
    /// multi-session) read/write memories from the new workspace's store
    /// (`{root}/.eko/memory/store.json`), not the stale bootstrap store.
    /// Also sets the `memory_store_override` so *future* pool agents created
    /// post-switch bind to the same store.
    ///
    /// Mirrors the primary-agent store swap done in `AppState::switch_workspace`.
    /// `ReviewIntegration` is expected to have been `rebind`-ed by the caller
    /// already — `create_layer_manager` reads the rebound dir/store.
    pub async fn apply_memory_store(&self, workspace_root: &std::path::Path) {
        let store = match crate::infra::create_memory_store_for_workspace(workspace_root) {
            Some(s) => s,
            None => {
                tracing::warn!(
                    root = %workspace_root.display(),
                    "AgentPool: failed to create workspace memory store; pool unchanged"
                );
                return;
            }
        };
        let echo_agent_dir = crate::workspace::layout::WorkspaceLayout::state_dir(workspace_root);
        self.apply_memory_store_inner(store, echo_agent_dir).await;
    }

    /// Rebind all pooled agents to the global memory store (post-`exit_workspace`).
    pub async fn apply_memory_store_global(&self) {
        let store = match crate::infra::create_global_memory_store() {
            Some(s) => s,
            None => {
                tracing::warn!("AgentPool: failed to create global memory store; pool unchanged");
                return;
            }
        };
        let (_, echo_agent_dir) = crate::infra::global_memory_paths();
        self.apply_memory_store_inner(store, echo_agent_dir).await;
    }

    /// Shared implementation: swap store + rebuild layer manager on every
    /// pooled agent, and record the override for future pool agents.
    async fn apply_memory_store_inner(
        &self,
        store: Arc<dyn echo_agent::memory::Store>,
        echo_agent_dir: std::path::PathBuf,
    ) {
        // (1) Record override so future create_agent calls in the pool bind
        //     to this store instead of the stale `shared.store`.
        {
            let mut ovr = self.memory_store_override.write().await;
            *ovr = Some(store.clone());
        }
        // (2) Hot-swap every existing pooled agent's store + layer manager.
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        for handle in agents {
            let store_clone = store.clone();
            let evolution_observer = crate::evolution::evolution_hook_observer(&handle).await;
            let skill_curator = self
                .shared
                .review_integration
                .as_ref()
                .map(|integration| integration.curator());
            let layer_manager = self
                .shared
                .review_integration
                .as_ref()
                .map(|integration| {
                    integration.create_layer_manager_with_observer(evolution_observer)
                })
                .unwrap_or_else(|| {
                    echo_agent::evolution::MemoryRuntimeIntegrationBuilder::new(
                        echo_agent_dir.clone(),
                        store_clone.clone(),
                    )
                    .build_layer_manager()
                });
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.install_memory_store(store_clone.clone()).await;
                        agent.install_memory_layer_manager(Arc::new(layer_manager));
                        agent.set_skill_curator(skill_curator);
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(
            dir = %echo_agent_dir.display(),
            pooled_agents,
            "AgentPool: memory store applied"
        );
    }

    /// Refresh the AGENTS/instructions projection on every existing agent.
    pub async fn refresh_instruction_context(&self) {
        let root = self.working_dir.read().await.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let root = root.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::unified_memory::refresh_instruction_projection(
                            agent,
                            root.as_deref(),
                        )
                        .await;
                    })
                })
                .await;
        }
    }

    /// Refresh the independently-owned MEMORY.md projection for every pooled agent.
    pub async fn refresh_hot_memory_context(&self) {
        let root = self.working_dir.read().await.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let root = root.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::unified_memory::refresh_hot_memory_projection(
                            agent,
                            root.as_deref(),
                        )
                        .await;
                    })
                })
                .await;
        }
    }

    /// Current number of agents in the pool (including background).
    pub async fn pool_size(&self) -> usize {
        self.agents.read().await.len()
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
    pub async fn shutdown(&self) -> Result<(), String> {
        let agents = self.agents.write().await;
        self.shutting_down.store(true, Ordering::Release);
        drop(agents);
        self.cleanup_cancel.cancel();
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

    /// Internal: create a new agent with shared resources injected.
    ///
    /// `conversation_id` is used both as the pool key and as the
    /// `AgentConfig.conversation_id` — the latter is required by
    /// `save_runtime_checkpoint` and `ConversationStore` projection. We also
    /// keep it as `session_id` so existing `session_id`-keyed paths (e.g.
    /// background tasks) continue to work.
    async fn create_agent(&self, conversation_id: &str) -> anyhow::Result<AgentHandle> {
        // 1. Create a base agent — pass conversation_id + state_store at build
        //    time so the agent boots with everything the framework's checkpoint
        //    helpers need. (Previously the pool called `set_state_store` here,
        //    but `self.shared.state_store` was always None because the primary
        //    agent never had a store wired in — `extract_from` would only ever
        //    see None and the runtime checkpoint loop silently no-op'd.)
        let app_config = self.app_config.read().await.clone();
        let working_dir = self.working_dir.read().await.clone();
        let params = infra::AgentCreateParams {
            model: None, // will use app_config default
            system_prompt: None,
            project: None,
            session_id: Some(conversation_id.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            react_checkpoint_interval: None,
            state_store: self.shared.state_store.clone(),
            memory_context_suffix: None,
            working_dir,
            // Thread the TaskRuntimeStore so pooled agents get task-management
            // tools registered (matches the primary agent wiring).
            // Formal Subagents created by TaskRuntime still have task_execute
            // disabled by invocation policy; pool conversation agents may drive it.
            task_runtime_store: self.shared.task_runtime_store.clone(),
            browser_runtime: self.shared.browser_runtime.clone(),
        };
        let mut agent = infra::create_agent(&params, &app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        agent.set_tool_output_artifacts(Some(self.tool_output_artifacts.read().await.clone()));

        // 2. Inject shared resources (replace independently-created ones)
        if let Some(ref llm) = self.shared.llm_client {
            agent.set_llm_client(llm.clone());
        }
        if let Some(llm_config) = self.runtime_llm_config.read().await.clone() {
            // Translate the optional thinking spec (e.g. "high", "4000",
            // "disabled") into a ThinkingConfig and inject it so every chat
            // request the agent makes carries the configured reasoning depth.
            // Unparseable specs are logged and dropped (config typos shouldn't
            // wedge the agent). "auto"/empty → None (model default).
            if let Some(spec) = llm_config.thinking.as_deref()
                && !spec.trim().is_empty()
            {
                match echo_agent::llm::ThinkingConfig::parse_spec(spec) {
                    Ok(Some(cfg)) => agent.set_thinking(Some(cfg)),
                    Ok(None) => agent.set_thinking(None),
                    Err(e) => {
                        tracing::warn!(
                            thinking_spec = spec,
                            error = %e,
                            "ignoring unparseable thinking config; using model default"
                        );
                    }
                }
            }
            agent.set_llm_config(llm_config);
        }
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
        // Prefer the workspace-scoped override (set by apply_memory_store after
        // a workspace switch) over the stale shared.store captured at bootstrap.
        let effective_store = self
            .memory_store_override
            .read()
            .await
            .clone()
            .or_else(|| self.shared.store.clone());
        if let Some(ref st) = effective_store {
            agent.install_store(st.clone()).await;
        }
        if let Some(ref review_integration) = self.shared.review_integration {
            let evolution_observer = Arc::new(echo_agent::evolution::HookEvolutionObserver::new(
                agent.hook_registry().clone(),
                agent.config().get_session_id().unwrap_or(""),
                agent.config().get_agent_name(),
            ));
            let layer_manager =
                Arc::new(review_integration.create_layer_manager_with_observer(evolution_observer));
            agent.install_memory_layer_manager(layer_manager);
            agent.set_memory_trigger_sink(Some(review_integration.clone()));
            agent.set_skill_load_policy(Some(review_integration.clone()));
            agent.set_skill_curator(Some(review_integration.curator()));
        }
        if let Some(ref ps) = self.shared.permission_service {
            agent.set_permission_service(ps.clone());
        }
        let permission_mode = self.permission_mode.read().await.clone();
        agent.set_permission_mode(&permission_mode);

        // 3. Register skill descriptors extracted from primary agent
        //    (avoids re-reading SKILL.md files from disk for each pool agent)
        let skill_descriptors = self.skill_descriptors.read().await.clone();
        for desc in &skill_descriptors {
            agent.skill_registry_mut().register_descriptor(desc.clone());
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

        Ok(handle)
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

    #[test]
    fn test_pooled_agent_timestamps() -> TestResult {
        // Verify PooledAgent records creation time
        let mock_handle = create_test_agent_handle()?;
        let pa = PooledAgent::new(mock_handle, "test-conv".to_string());
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
    async fn test_pool_release_removes_agent() -> TestResult {
        let pool = create_test_pool(5, false).await?;

        let _h = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        pool.release("conv-1").await;
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_release_nonexistent_is_noop() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        pool.release("nonexistent").await;
        assert_eq!(pool.pool_size().await, 0);
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

        // LlmClient should be extracted
        assert!(shared.llm_client.is_some());
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

        let _task_a = pool
            .acquire("__task__:task-a")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        pool.release("__task__:task-a").await;
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

        pool.apply_permission_mode("full-auto".to_string()).await;

        let first_mode = first
            .agent()
            .read(|agent| agent.get_permission_mode().to_string())
            .await;
        assert_eq!(first_mode, "full-auto");

        let second = pool
            .acquire("conv-b")
            .await
            .map_err(|error| error.to_string())?;
        let second_mode = second
            .agent()
            .read(|agent| agent.get_permission_mode().to_string())
            .await;
        assert_eq!(second_mode, "full-auto");
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
        assert_eq!(diagnostic.root, temp.path().join("tasks"));
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
