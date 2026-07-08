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
//! let pool = AgentPool::from_runtime(&runtime, PoolConfig::default()).await;
//!
//! // Acquire an agent for a conversation:
//! let agent = pool.acquire("conv-001").await?;
//! agent.chat_stream("Hello").await;  // Executes in parallel with other agents
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use echo_agent::agent::AgentHandle;
use echo_agent::agent::CancellationToken;
use echo_agent::llm::LlmClient;
use tokio::sync::RwLock;

use crate::config::AppConfig;
use crate::infra;
use crate::model_config::ModelRuntimeConfig;

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
    /// management tools (plan_create/update/complete/skip/list) registered so
    /// the main agent can autonomously manage its plan during execution.
    pub task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
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
    config: PoolConfig,
    app_config: RwLock<AppConfig>,
    runtime_llm_config: RwLock<Option<echo_agent::llm::LlmConfig>>,
    permission_mode: RwLock<String>,
    /// Skill descriptors extracted from the primary agent.
    /// Pool agents register these instead of re-reading from disk.
    skill_descriptors: RwLock<Vec<echo_agent::skills::external::SkillDescriptor>>,
    /// Cancellation token for the cleanup monitor task.
    cleanup_cancel: CancellationToken,
    /// Workspace-scoped memory store override. Set by `apply_memory_store`
    /// on workspace switch so newly-created pool agents also bind to the
    /// current workspace's memory store (not the stale shared.store captured
    /// at bootstrap). `None` means "use shared.store" (pre-switch behavior).
    memory_store_override: RwLock<Option<Arc<dyn echo_agent::memory::Store>>>,
}

impl AgentPool {
    /// Inject the TaskRuntimeStore so subsequently-created pool agents get
    /// the task-management tools (plan_create/update/complete/skip/list)
    /// registered. Must be called before any pool agent is created (i.e. right
    /// after AppState builds the store). Existing pool agents are unaffected.
    pub fn set_task_runtime_store(
        &mut self,
        store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    ) {
        self.shared.task_runtime_store = Some(store);
    }

    /// Create a pool from an already-bootstrapped `AgentRuntime`.
    ///
    /// Extracts shared resources from the runtime's primary agent and
    /// optionally pre-creates a background task agent.
    pub async fn from_runtime(runtime: &crate::runtime::AgentRuntime, config: PoolConfig) -> Self {
        let shared = SharedResources::extract_from(
            &runtime.agent_handle,
            runtime.review_integration.clone(),
        )
        .await;

        // Extract skill descriptors from primary agent (avoids re-reading from disk)
        let skill_descriptors = runtime.agent_handle.read(|a| a.skill_descriptors()).await;

        let pool = Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            config,
            app_config: RwLock::new(runtime.app_config.clone()),
            runtime_llm_config: RwLock::new(None),
            permission_mode: RwLock::new("default".to_string()),
            skill_descriptors: RwLock::new(skill_descriptors),
            cleanup_cancel: CancellationToken::new(),
            memory_store_override: RwLock::new(None),
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

    /// Acquire an agent for a given conversation ID.
    ///
    /// If an agent already exists for this ID, it is returned (with updated
    /// `last_used` timestamp). Otherwise, a new agent is created and added
    /// to the pool.
    ///
    /// Pool 容量计数 (`max_agents`) 只计**对话 agent**, 不计 task worker
    /// (`__task__:` 前缀) 和 `__background__` (P1-13 修复)。否则后台任务多了
    /// 会挤占用户交互 agent 的并发槽位, 导致用户发消息被拒。
    fn is_conversation_agent(key: &str) -> bool {
        key != "__background__" && !key.starts_with("__task__:")
    }

    ///
    /// The write lock is held across the entire operation (including async
    /// agent creation) to prevent TOCTOU races between concurrent acquirers.
    pub async fn acquire(&self, conversation_id: &str) -> Result<AgentHandle, PoolError> {
        let mut agents = self.agents.write().await;

        // Fast path: reuse existing agent
        if let Some(existing) = agents.get_mut(conversation_id) {
            existing.last_used = Instant::now();
            return Ok(existing.handle.clone());
        }

        // Enforce pool limit — evict oldest idle agent that is NOT executing
        // P1-13: 只计对话 agent, 排除 __background__ 和 __task__ worker。
        let active_count = agents
            .keys()
            .filter(|k| Self::is_conversation_agent(k))
            .count();
        if active_count >= self.config.max_agents {
            // Find oldest non-background, non-executing conversation agent
            let mut candidates: Vec<(String, Instant)> = agents
                .iter()
                .filter(|(id, _)| Self::is_conversation_agent(id))
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

        Ok(handle)
    }

    /// Get an agent handle without creating a new one.
    ///
    /// Returns `None` if no agent is allocated for this conversation ID.
    pub async fn get(&self, conversation_id: &str) -> Option<AgentHandle> {
        let agents = self.agents.read().await;
        agents.get(conversation_id).map(|pa| pa.handle.clone())
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

    /// Get the dedicated background task agent.
    ///
    /// Falls back to the primary agent if no background agent was created.
    pub async fn background_agent(&self) -> Option<AgentHandle> {
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
                        if let Some(cw) = runtime.context_window {
                            agent.set_token_limit(cw as usize);
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
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        for handle in agents {
            let path = path.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_working_dir(path);
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(?path, pooled_agents, "AgentPool: working_dir applied");
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
            let dir_clone = echo_agent_dir.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.install_memory_store(store_clone.clone()).await;
                        let mgr = echo_agent::evolution::MemoryRuntimeIntegrationBuilder::new(
                            dir_clone,
                            store_clone,
                        )
                        .build_layer_manager();
                        agent.install_memory_layer_manager(Arc::new(mgr));
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
        let pool = self.clone();
        let cancel = pool.cleanup_cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 min
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("AgentPool: cleanup monitor stopped");
                        return;
                    }
                    _ = interval.tick() => {}
                }

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
                // so long-running tasks (e.g. TaskRuntime DAG workers) aren't killed.
                let to_remove: Vec<String> = timed_out
                    .into_iter()
                    .filter(|id| {
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
        });
    }

    /// Stop the cleanup monitor and release all pool agents.
    pub async fn shutdown(&self) {
        self.cleanup_cancel.cancel();
        let mut agents = self.agents.write().await;
        let count = agents.len();
        agents.clear();
        tracing::info!(agents_cleared = count, "AgentPool: shutdown complete");
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
        let params = infra::AgentCreateParams {
            model: None, // will use app_config default
            system_prompt: None,
            project: None,
            session_id: Some(conversation_id.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            react_checkpoint_interval: None,
            state_store: self.shared.state_store.clone(),
            memory_context_suffix: None,
            // Stage 2 will bind a per-conversation worktree here; for now the
            // pooled agent runs in the process cwd.
            working_dir: None,
            // Thread the TaskRuntimeStore so pooled agents get task-management
            // tools registered (matches the primary agent wiring).
            // route is intentionally None for pooled agents (workers never get
            // plan_execute per §10.2).
            task_runtime_store: self.shared.task_runtime_store.clone(),
            route: None,
        };
        let mut agent = infra::create_agent(&params, &app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

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
        if let Some(ref cs) = self.shared.conversation_store {
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
            let layer_manager = Arc::new(
                review_integration
                    .create_layer_manager()
                    .with_write_observer(review_integration.clone()),
            );
            agent.install_memory_layer_manager(layer_manager);
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

        // 3b. Auto-compression — pooled agents must not rely solely on the
        // 200-msg hard cap. Mirror the primary agent wiring (runtime.rs) so
        // long GUI multi-session runs are protected by the configured strategy.
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::debug!(conversation_id, "pooled agent auto-compression configured");
        }

        // 4. Wrap in AgentHandle
        let handle = AgentHandle::new(agent);

        // (delegate_readonly 工具已删除:单步派发内联进 plan_execute。
        // worker 不再注册 plan_execute——§10.2 防死锁;故 worker 无派发工具,
        // 需要子任务时自己用文件工具完成。)

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

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn test_pool_exposes_max_agents() {
        let pool = create_test_pool(4, false).await;
        assert_eq!(pool.max_agents(), 4);
    }

    #[tokio::test]
    async fn test_pool_background_task_concurrency_is_conservative() {
        let small = create_test_pool(1, false).await;
        assert_eq!(small.background_task_concurrency(), 1);

        let medium = create_test_pool(3, false).await;
        assert_eq!(medium.background_task_concurrency(), 2);

        let large = create_test_pool(10, false).await;
        assert_eq!(large.background_task_concurrency(), 4);
        assert_eq!(large.foreground_agent_reserve(), 1);
        assert_eq!(large.composite_parallelism(), 3);
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
    fn test_pooled_agent_timestamps() {
        // Verify PooledAgent records creation time
        let mock_handle = create_test_agent_handle();
        let pa = PooledAgent::new(mock_handle, "test-conv".to_string());
        assert!(pa.created_at.elapsed().as_millis() < 100);
        assert!(pa.last_used.elapsed().as_millis() < 100);
    }

    #[tokio::test]
    async fn test_pool_acquire_creates_agent() {
        let pool = create_test_pool(3, false).await;
        assert_eq!(pool.pool_size().await, 0);

        let handle = pool.acquire("conv-1").await;
        assert!(handle.is_ok());
        assert_eq!(pool.pool_size().await, 1);
    }

    #[tokio::test]
    async fn test_pool_acquire_reuses_existing() {
        let pool = create_test_pool(3, false).await;

        let _h1 = pool.acquire("conv-1").await.unwrap();
        let _h2 = pool.acquire("conv-1").await.unwrap();

        // Same conversation_id should return the same agent (pool size stays 1)
        assert_eq!(pool.pool_size().await, 1);
    }

    #[tokio::test]
    async fn test_pool_acquire_different_ids() {
        let pool = create_test_pool(5, false).await;

        let _h1 = pool.acquire("conv-1").await.unwrap();
        let _h2 = pool.acquire("conv-2").await.unwrap();
        let _h3 = pool.acquire("conv-3").await.unwrap();

        assert_eq!(pool.pool_size().await, 3);
    }

    #[tokio::test]
    async fn test_pool_release_removes_agent() {
        let pool = create_test_pool(5, false).await;

        let _h = pool.acquire("conv-1").await.unwrap();
        assert_eq!(pool.pool_size().await, 1);

        pool.release("conv-1").await;
        assert_eq!(pool.pool_size().await, 0);
    }

    #[tokio::test]
    async fn test_pool_release_nonexistent_is_noop() {
        let pool = create_test_pool(5, false).await;
        pool.release("nonexistent").await;
        assert_eq!(pool.pool_size().await, 0);
    }

    #[tokio::test]
    async fn test_pool_get_returns_none_for_unknown() {
        let pool = create_test_pool(5, false).await;
        assert!(pool.get("unknown").await.is_none());
    }

    #[tokio::test]
    async fn test_pool_get_returns_some_for_known() {
        let pool = create_test_pool(5, false).await;
        let _h = pool.acquire("conv-1").await.unwrap();
        assert!(pool.get("conv-1").await.is_some());
    }

    #[tokio::test]
    async fn test_pool_evicts_idle_on_overflow() {
        let pool = create_test_pool(2, false).await;

        let _h1 = pool.acquire("conv-1").await.unwrap();
        let _h2 = pool.acquire("conv-2").await.unwrap();
        assert_eq!(pool.pool_size().await, 2);

        // Pool is full — acquiring a 3rd should evict the oldest
        let h3 = pool.acquire("conv-3").await;
        assert!(h3.is_ok());
        // Pool should still be at max capacity
        assert!(pool.pool_size().await <= 3);
    }

    #[tokio::test]
    async fn test_pool_background_agent_precreated() {
        // Background agent pre-creation only happens in from_runtime().
        // With manual construction, no background agent exists until acquired.
        let pool = create_test_pool(5, true).await;
        // Manually created pool has no pre-created agents
        assert_eq!(pool.pool_size().await, 0);
        // But background_agent() returns None since __background__ wasn't pre-created
        assert!(pool.background_agent().await.is_none());
    }

    #[tokio::test]
    async fn test_pool_background_agent_not_created_when_disabled() {
        let pool = create_test_pool(5, false).await;
        assert!(pool.background_agent().await.is_none());
    }

    #[tokio::test]
    async fn test_pool_background_agent_acquire_on_demand() {
        let pool = create_test_pool(5, false).await;
        // Can acquire __background__ on demand even without pre-creation
        let bg = pool.acquire("__background__").await;
        assert!(bg.is_ok());
        assert!(pool.background_agent().await.is_some());
    }

    #[tokio::test]
    async fn test_shared_resources_extraction() {
        let agent = create_test_agent();
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
    }

    #[tokio::test]
    async fn test_shared_resources_arc_sharing() {
        let agent = create_test_agent();
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        // Verify Arc reference counts indicate sharing
        let tm = shared.tool_manager.as_ref().unwrap();
        // At least 2 references: one in original agent, one in shared
        assert!(
            Arc::strong_count(tm) >= 2,
            "ToolManager Arc should be shared (count={})",
            Arc::strong_count(tm)
        );

        let tt = shared.token_tracker.as_ref().unwrap();
        assert!(
            Arc::strong_count(tt) >= 2,
            "TokenUsageTracker Arc should be shared (count={})",
            Arc::strong_count(tt)
        );
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn create_test_agent() -> echo_agent::agent::ReactAgent {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;

        let mock_llm = Arc::new(MockLlmClient::new().with_model_name("test-model"));

        ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(mock_llm)
            .build()
            .expect("test agent should build")
    }

    fn create_test_agent_handle() -> AgentHandle {
        AgentHandle::new(create_test_agent())
    }

    async fn create_test_pool(max_agents: usize, enable_bg: bool) -> AgentPool {
        let agent = create_test_agent();
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        AgentPool {
            shared,
            agents: RwLock::new(HashMap::new()),
            config: PoolConfig {
                max_agents,
                idle_timeout: Duration::from_secs(1800),
                enable_background_agent: enable_bg,
            },
            app_config: RwLock::new(AppConfig::default()),
            runtime_llm_config: RwLock::new(None),
            permission_mode: RwLock::new("default".to_string()),
            skill_descriptors: RwLock::new(vec![]),
            cleanup_cancel: CancellationToken::new(),
            memory_store_override: RwLock::new(None),
        }
    }

    async fn create_test_pool_with_review_integration() -> AgentPool {
        let agent = create_test_agent();
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
        let mut shared = SharedResources::extract_from(&handle, Some(review_integration)).await;
        shared.store = Some(store);

        AgentPool {
            shared,
            agents: RwLock::new(HashMap::new()),
            config: PoolConfig {
                max_agents: 3,
                idle_timeout: Duration::from_secs(1800),
                enable_background_agent: false,
            },
            app_config: RwLock::new(AppConfig::default()),
            runtime_llm_config: RwLock::new(None),
            permission_mode: RwLock::new("default".to_string()),
            skill_descriptors: RwLock::new(vec![]),
            cleanup_cancel: CancellationToken::new(),
            memory_store_override: RwLock::new(None),
        }
    }

    #[tokio::test]
    async fn test_pool_agent_installs_layered_memory_runtime() {
        let pool = create_test_pool_with_review_integration().await;
        let handle = pool.acquire("conv-memory").await.unwrap();

        let has_layer_manager = handle.read(|agent| agent.has_memory_layer_manager()).await;
        assert!(
            has_layer_manager,
            "pooled agents must install MemoryLayerManager so TriggerDetector writes real memory"
        );
    }

    #[tokio::test]
    async fn test_task_workers_are_isolated_and_have_memory_runtime() {
        let pool = create_test_pool_with_review_integration().await;

        let task_a = pool.acquire("__task__:task-a").await.unwrap();
        let task_b = pool.acquire("__task__:task-b").await.unwrap();

        assert!(
            !Arc::ptr_eq(task_a.inner(), task_b.inner()),
            "parallel background tasks must use distinct subagent agents"
        );

        let task_a_has_memory = task_a.read(|agent| agent.has_memory_layer_manager()).await;
        let task_b_has_memory = task_b.read(|agent| agent.has_memory_layer_manager()).await;
        assert!(task_a_has_memory);
        assert!(task_b_has_memory);
    }

    #[tokio::test]
    async fn test_released_task_worker_frees_pool_capacity() {
        let pool = create_test_pool(1, false).await;

        let _task_a = pool.acquire("__task__:task-a").await.unwrap();
        assert_eq!(pool.pool_size().await, 1);

        pool.release("__task__:task-a").await;
        assert_eq!(pool.pool_size().await, 0);

        let task_b = pool.acquire("__task__:task-b").await;
        assert!(
            task_b.is_ok(),
            "released task subagent should free capacity for a later task"
        );
        assert_eq!(pool.pool_size().await, 1);
    }

    #[tokio::test]
    async fn test_permission_mode_applies_to_existing_and_future_pool_agents() {
        let pool = create_test_pool(3, false).await;
        let first = pool.acquire("conv-a").await.unwrap();

        pool.apply_permission_mode("full-auto".to_string()).await;

        let first_mode = first
            .read(|agent| agent.get_permission_mode().to_string())
            .await;
        assert_eq!(first_mode, "full-auto");

        let second = pool.acquire("conv-b").await.unwrap();
        let second_mode = second
            .read(|agent| agent.get_permission_mode().to_string())
            .await;
        assert_eq!(second_mode, "full-auto");
    }
}
