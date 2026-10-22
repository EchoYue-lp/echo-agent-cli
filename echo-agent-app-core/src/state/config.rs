// 应用状态管理
//
// 支持两种运行模式的状态共享：
// - 单模式（Web 或 CLI）：独立的 Agent 实例
// - 双模式（Web + CLI）：共享的 Agent 实例

use chrono::{DateTime, Utc};
use echo_agent::agent::CancellationToken;
use echo_agent::memory::{Conversation, ConversationStore, NewConversation};
use echo_agent::prelude::*;
use echo_agent::state::RuntimeStateStore;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::hitl::HitlDispatcher;
use tokio::sync::{Mutex, RwLock};

use crate::agent_handle::AgentHandle;
use crate::workspace::Workspace;
use crate::workspace::registry::WorkspaceRegistry;

type Result<T, E = echo_agent::error::ReactError> = std::result::Result<T, E>;

/// Web 配置（支持热更新）
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub model: String,
    pub system_prompt: String,
    pub token_limit: usize,
    /// 文件上传大小限制（字节），默认 10MB
    pub max_upload_size_bytes: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            system_prompt: "你是 EKO，运行在用户本机的 AI 工作台，面向编程、研究、数据分析和专业写作。你的核心原则：先读后做、连续推进、事实可追溯、如实汇报。具体行为规范参见你的完整系统提示。".to_string(),
            token_limit: 8000,
            max_upload_size_bytes: 10 * 1024 * 1024,
        }
    }
}

// ── 审计日志 ──

/// 审计日志最大条目数（FIFO 淘汰，可经由环境变量 ECHO_AUDIT_MAX_ENTRIES 覆盖）
pub fn max_audit_log_entries() -> usize {
    std::env::var("ECHO_AUDIT_MAX_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

/// 审计日志条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub decision: AuditDecision,
    pub reason: String,
    pub source: String,
    pub duration_us: u64,
    pub elapsed_ms: u64,
    pub timestamp: String,
}

/// 审计决策类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditDecision {
    Allow,
    Deny,
    Ask,
}

impl std::fmt::Display for AuditDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditDecision::Allow => write!(f, "allow"),
            AuditDecision::Deny => write!(f, "deny"),
            AuditDecision::Ask => write!(f, "ask"),
        }
    }
}

// ── 权限管理 ──

// ── 沙箱配置 ──

/// Sandbox safety tier for the CLI's local-execution sandbox.
///
/// This controls the runtime restrictions applied to shell/code execution.
/// Distinct from `echo_agent::prelude::SecurityLevel` in the framework,
/// which is a 4-level sandbox *isolation* policy (Trusted → Maximum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Ord)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SandboxTier {
    Low,
    #[default]
    Medium,
    High,
}

/// 沙箱运行时配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfigData {
    pub security_level: SandboxTier,
    pub max_memory_mb: u32,
    pub max_cpu_seconds: u32,
    pub network_enabled: bool,
}

impl Default for SandboxConfigData {
    fn default() -> Self {
        Self {
            security_level: SandboxTier::default(),
            max_memory_mb: 512,
            max_cpu_seconds: 30,
            network_enabled: false,
        }
    }
}

/// MCP 服务端健康状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHealthStatus {
    pub name: String,
    pub healthy: bool,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    pub last_check: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

// ── 子状态拆分 ──

/// 连接管理状态：Agent 句柄 + HITL Dispatcher + 可选 Agent Pool
pub struct ConnectionState {
    pub agent: AgentHandle,
    /// EKO-owned consumers coupled to the primary agent's model generation.
    pub model_consumers: Option<crate::infra::AgentModelConsumers>,
    /// HITL dispatcher — 多 Provider 协作（repl, ws, webhook 等）
    /// WS handler 注册到 dispatcher 而非替换 agent 的 provider，
    /// 确保多模式下 HITL 请求能路由到正确的 Provider。
    pub hitl_dispatcher: Arc<crate::hitl::HitlDispatcher>,
    /// Agent pool for multi-conversation parallel execution.
    /// When `Some`, `agent_for()` routes to pool agents by conversation_id.
    /// When `None`, all requests use the single `agent` (backward compatible).
    pub pool: Option<Arc<crate::agent_pool::AgentPool>>,
    conversation_binding: Arc<RwLock<ConversationStorageBinding>>,
}

impl ConnectionState {
    /// Get the agent for a given conversation ID.
    ///
    /// If a pool is active, acquires (or reuses) a pool agent for the ID.
    /// Pool admission failures are observable; in particular, a workspace
    /// transition must not silently fall back to the old primary generation.
    pub async fn agent_for(
        &self,
        conversation_id: &str,
    ) -> std::result::Result<crate::agent_pool::AgentPoolExecutionLease, crate::agent_pool::PoolError>
    {
        let binding = self.conversation_binding.read().await;
        if let Err(error) = binding
            .deletions
            .ensure_admission_allowed(conversation_id, None)
            .await
        {
            return Err(crate::agent_pool::PoolError::ConversationDeletionPending {
                conversation_id: conversation_id.to_string(),
                reason: error.to_string(),
            });
        }
        drop(binding);
        if let Some(ref pool) = self.pool {
            pool.acquire(conversation_id).await
        } else {
            Ok(crate::agent_pool::AgentPoolExecutionLease::unpooled(
                self.agent.clone(),
            ))
        }
    }

    /// Get the primary agent (bypass pool routing).
    ///
    /// Used by commands that don't participate in multi-conversation routing.
    pub fn primary_agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    /// Whether the agent pool is active.
    pub fn has_pool(&self) -> bool {
        self.pool.is_some()
    }
}

/// 配置状态：应用 / Web / 沙箱 / 权限
pub struct ConfigState {
    pub app_config: RwLock<crate::config::EkoConfig>,
    /// Runtime model currently published to primary and pooled agents. This
    /// remains distinct from the durable default when startup used `--model`.
    active_model_id: RwLock<String>,
    /// Immutable startup source used for every application-side config commit.
    pub config_path: std::path::PathBuf,
    pub web_config: RwLock<WebConfig>,
    pub sandbox_config: RwLock<SandboxConfigData>,
    pub permission_mode: RwLock<echo_agent::tools::permission::PermissionMode>,
    pub permission_rules: RwLock<Vec<echo_agent::tools::permission::PermissionRule>>,
    model_mutations: Mutex<ModelMutationOwnerState>,
    model_mutation_admission_open: std::sync::atomic::AtomicBool,
}

type ModelMutationSettlement =
    Shared<BoxFuture<'static, Result<ModelMutationReceipt, ModelMutationError>>>;

struct ModelMutationOwnerState {
    lifecycle: ModelMutationOwnerLifecycle,
}

enum ModelMutationOwnerLifecycle {
    Running(ModelMutationSettlement),
    Settled(Box<Result<Option<ModelMutationReceipt>, ModelMutationError>>),
    Closed(Result<(), ModelMutationError>),
}

impl Default for ModelMutationOwnerState {
    fn default() -> Self {
        Self {
            lifecycle: ModelMutationOwnerLifecycle::Settled(Box::new(Ok(None))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfiguredModelMutation {
    pub model: crate::config::ConfiguredModel,
    pub set_default: bool,
}

#[derive(Debug, Clone)]
pub struct ModelProviderMutation {
    pub id: String,
    pub provider: crate::config::ModelProviderConfig,
    pub preserve_auth_token: bool,
}

/// Linearized result returned only after disk, snapshot, primary, and pool
/// publication have completed for an active-model mutation.
#[derive(Clone)]
pub struct ModelMutationReceipt {
    pub config: crate::config::EkoConfig,
    pub model_id: String,
    pub runtime: Option<crate::model_config::ModelRuntimeConfig>,
    pub activated: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelMutationError {
    #[error("model mutation rejected: {0}")]
    Validation(String),
    #[error("model mutation persistence failed: {0}")]
    Persistence(String),
    #[error("model mutation publication failed: {0}")]
    Publication(String),
    #[error("model mutation owner is shutting down")]
    ShuttingDown,
    #[error("model mutation settlement task failed: {0}")]
    Settlement(String),
}

type OwnedConfigUpdate =
    Box<dyn FnOnce(&mut crate::config::EkoConfig) -> Result<(), String> + Send + 'static>;

enum ModelMutationRequest {
    UpsertModel(ConfiguredModelMutation),
    UpsertProvider(ModelProviderMutation),
    SetDefault(String),
    DeleteModel(String),
    DeleteProvider(String),
    UpdateConfig {
        update: OwnedConfigUpdate,
        reapply_active_model: bool,
    },
    #[cfg(test)]
    AbortSettlementForTest,
}

struct PreparedModelMutation {
    config: crate::config::EkoConfig,
    model_id: String,
    runtime: Option<crate::model_config::ModelRuntimeConfig>,
    prepared: Option<crate::infra::PreparedRuntimeLlm>,
    activated: bool,
    deactivated: bool,
    deleted: bool,
}

/// 会话状态：非聊天操作取消和前台 turn 控制。
pub struct SessionState {
    /// Cancellation registry for non-chat operations such as analysis jobs.
    pub analysis_runs: Arc<crate::product_data_io::AnalysisRunSupervisor>,
    pub product_data_io: crate::product_data_io::ProductDataIoService,
    /// Application authority for foreground chat admission and cancellation.
    pub foreground_turns: crate::foreground_turn::ForegroundTurnControl,
}

/// 插件状态：MCP 服务管理
pub struct PluginState {
    pub mcp_config: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    /// Health is scoped by workspace host generation. A process-global or
    /// workspace-id-only map can expose an old host after same-id recreation.
    pub mcp_health: RwLock<HashMap<String, HashMap<String, McpHealthStatus>>>,
}

/// 持久化存储状态
#[derive(Clone)]
pub struct ConversationStorageBinding {
    pub store: Option<Arc<dyn ConversationStore>>,
    pub runtime_state: Option<Arc<dyn RuntimeStateStore>>,
    pub deletions: Arc<crate::conversation_deletion::ConversationDeletionService>,
}

pub struct StorageState {
    pub conversation: Arc<RwLock<ConversationStorageBinding>>,
    /// EKO-owned visibility projection shared by GUI/TUI/channel surfaces.
    pub conversation_archive: Arc<crate::conversation_archive::ConversationArchiveStore>,
    pub tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
    /// Bounded ordinary-chat replay stream. Formal task history remains owned
    /// by TaskRuntimeStore; long-term transcript remains ConversationStore.
    pub chat_events: Arc<crate::chat_event_log::ChatEventLog>,
}

/// 历史记录状态：审计日志 + 工作流
pub struct HistoryState {
    pub audit_logs: RwLock<Vec<AuditLogEntry>>,
    pub workflows: Arc<crate::workflow_service::WorkflowService>,
    pub structured_extraction: Arc<crate::structured_extraction::StructuredExtractionService>,
}

/// 调度器状态
pub struct SchedulerState {
    pub runner: Option<Arc<crate::scheduler::SchedulerRunner>>,
    pub cancel_token: echo_agent::agent::CancellationToken,
    /// Owned scheduler loop. Keeping the framework handle lets every EKO
    /// surface cancel and await the same long-lived task during shutdown.
    handle: Mutex<Option<echo_agent::scheduler::SchedulerHandle>>,
}

impl SchedulerState {
    async fn shutdown(&self) -> echo_agent::error::Result<()> {
        // Borrow the original handle under the same slot lock. Unknown or a
        // cancelled observer cannot discard its Runner/JoinHandle parent.
        let mut slot = self.handle.lock().await;
        if let Some(handle) = slot.as_mut() {
            handle.shutdown().await?;
            slot.take();
        }
        Ok(())
    }
}

/// 后台任务状态
pub struct TaskState {
    pub service: Option<Arc<crate::tasks::BackgroundTaskService>>,
    pub cancel_token: CancellationToken,
    /// TaskRuntime canonical file-backed store for complex-task runs, plans,
    /// todos, events, artifacts, reviews, and execution summaries.
    /// Backs TaskRuntime query commands. Bootstrap propagates authority errors;
    /// `None` is retained only for explicit embedding/test construction.
    pub runtime: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TaskRunBootReport {
    recovered: usize,
    resumed: usize,
    blocked: usize,
    failed_scopes: Vec<String>,
}

impl TaskRunBootReport {
    /// Whether the boot scan settled every TaskRun it observed.
    ///
    /// A fence-rejected run is counted as `blocked`, so a zero `recovered`
    /// count can never make an unsettled boot look settled (R1).
    fn is_settled(&self) -> bool {
        self.blocked == 0 && self.failed_scopes.is_empty()
    }
}

/// Whether a TaskRun belongs to a cron occurrence slot.
///
/// Cron runs carry the scheduler's conversation prefix, derived from the
/// durable occurrence id. Ordinary recovery must not resume them: the slot is
/// owned by the cron delivery that admitted it, not by the ordinary
/// continuation launcher.
fn is_cron_owned_task_run(run: &crate::tasks::task_runtime::TaskRun) -> bool {
    run.conversation_id
        .starts_with(crate::scheduler::CRON_CONVERSATION_PREFIX)
}

/// Webhook 状态
pub struct WebhookState {
    pub emitter: Arc<crate::webhook::WebhookEmitter>,
}

/// Product-level observability inputs not owned by the framework run store.
pub struct ObservabilityState {
    /// Static prompt-module report for the primary EKO agent.
    pub prompt_assembly: RwLock<Option<crate::project::prompt::PromptAssembly>>,
}

#[cfg(test)]
pub(crate) mod scheduler_shutdown_tests {
    use super::SchedulerState;
    use echo_agent::agent::CancellationToken;
    use echo_agent::error::ReactError;
    use echo_agent::scheduler::{CronTask, CronTaskStore, OccurrenceFireFn, SchedulerRunner};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Mutex, Notify};

    struct CallbackResource {
        drops: Arc<AtomicUsize>,
        _permit: tokio::sync::OwnedSemaphorePermit,
    }

    impl Drop for CallbackResource {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    // A real public framework occurrence/Handle, shared by the slot and Root
    // regressions. The backend is test-owned async work, not an OS process.
    pub(crate) struct SchedulerFixture {
        _directory: tempfile::TempDir,
        scheduler: Option<SchedulerState>,
        release: Arc<Notify>,
        backend: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
        manual: Option<tokio::task::JoinHandle<echo_agent::error::Result<String>>>,
        pub(crate) calls: Arc<AtomicUsize>,
        pub(crate) drops: Arc<AtomicUsize>,
        pub(crate) capacity: Arc<tokio::sync::Semaphore>,
        pub(crate) identity: Arc<std::sync::Mutex<Option<(String, String)>>>,
    }

    impl Drop for SchedulerFixture {
        fn drop(&mut self) {
            self.release.notify_one();
        }
    }

    impl SchedulerFixture {
        pub(crate) async fn new(unknown: bool) -> anyhow::Result<Self> {
            let directory = tempfile::tempdir()?;
            let capacity = Arc::new(tokio::sync::Semaphore::new(1));
            let drops = Arc::new(AtomicUsize::new(0));
            let resource = Arc::new(CallbackResource {
                drops: drops.clone(),
                _permit: capacity.clone().acquire_owned().await?,
            });
            let release = Arc::new(Notify::new());
            let entered = Arc::new(Notify::new());
            let backend = Arc::new(std::sync::Mutex::new(None));
            let calls = Arc::new(AtomicUsize::new(0));
            let identity = Arc::new(std::sync::Mutex::new(None));
            let fire_fn: OccurrenceFireFn = {
                let release = release.clone();
                let entered = entered.clone();
                let backend = backend.clone();
                let calls = calls.clone();
                let identity = identity.clone();
                Arc::new(move |invocation| {
                    let resource = resource.clone();
                    let release = release.clone();
                    let entered = entered.clone();
                    let backend = backend.clone();
                    calls.fetch_add(1, Ordering::SeqCst);
                    *identity.lock().unwrap_or_else(|error| error.into_inner()) = Some((
                        invocation.occurrence_id.clone(),
                        invocation.attempt_id.clone(),
                    ));
                    Box::pin(async move {
                        let (started, ready) = tokio::sync::oneshot::channel();
                        let physical = tokio::spawn(async move {
                            let _original_resource = resource;
                            let _ = started.send(());
                            release.notified().await;
                        });
                        ready
                            .await
                            .map_err(|error| ReactError::Other(error.to_string()))?;
                        entered.notify_one();
                        if unknown {
                            *backend.lock().unwrap_or_else(|error| error.into_inner()) =
                                Some(physical);
                            Err(ReactError::ExecutionNotSettled {
                                run_id: invocation.occurrence_id,
                                execution_id: Some(invocation.attempt_id),
                            })
                        } else {
                            physical
                                .await
                                .map_err(|error| ReactError::Other(error.to_string()))?;
                            Ok("same original callback physically completed".to_string())
                        }
                    })
                })
            };
            let cancel = CancellationToken::new();
            let runner = Arc::new(
                SchedulerRunner::new_with_occurrence_context(
                    CronTaskStore::new().with_path(directory.path().join("tasks.json")),
                    cancel.clone(),
                    fire_fn,
                )
                .await?,
            );
            let task = CronTask::new("shutdown owner", "* * * * *", "original callback");
            let task_id = task.id.clone();
            runner.add_task(task).await?;
            let handle = runner.clone().spawn();
            let manual_runner = runner.clone();
            let manual = tokio::spawn(async move { manual_runner.run_once(&task_id).await });
            let fixture = Self {
                _directory: directory,
                scheduler: Some(SchedulerState {
                    runner: Some(runner),
                    cancel_token: cancel,
                    handle: Mutex::new(Some(handle)),
                }),
                release,
                backend,
                manual: Some(manual),
                calls,
                drops,
                capacity,
                identity,
            };
            tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified()).await?;
            Ok(fixture)
        }

        pub(crate) fn take_scheduler(&mut self) -> anyhow::Result<SchedulerState> {
            self.scheduler
                .take()
                .ok_or_else(|| anyhow::anyhow!("same scheduler already transferred"))
        }

        pub(crate) async fn original_result(
            &mut self,
        ) -> anyhow::Result<echo_agent::error::Result<String>> {
            let result = {
                let manual = self
                    .manual
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("original observer absent"))?;
                tokio::time::timeout(std::time::Duration::from_secs(3), manual).await??
            };
            self.manual.take();
            Ok(result)
        }

        pub(crate) fn release_backend(&self) {
            self.release.notify_one();
        }

        pub(crate) async fn finish_backend(&mut self) -> anyhow::Result<()> {
            self.release_backend();
            if self.manual.is_some() {
                let _original_result = self.original_result().await?;
            }
            let backend = self
                .backend
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(backend) = backend {
                tokio::time::timeout(std::time::Duration::from_secs(3), backend).await??;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn scheduler_slot_preserves_original_handle_across_unknown_and_cancelled_waits()
    -> anyhow::Result<()> {
        // Ordinary held work supplies the cancellation-safe-wait and true final
        // completion control. No later physical receipt is synthesized.
        let mut normal = SchedulerFixture::new(false).await?;
        let normal_state = normal.take_scheduler()?;
        let mut borrowed = Box::pin(normal_state.shutdown());
        let was_pending = futures::poll!(borrowed.as_mut()).is_pending();
        drop(borrowed);
        let slot_after_cancelled_wait = normal_state.handle.lock().await.is_some();
        // The loop ending is not proof that an independently admitted callback
        // committed. Await that same callback's real public result first.
        normal.release_backend();
        let normal_completed = normal.original_result().await?;
        let (normal_first, normal_second) = {
            let first = normal_state.shutdown();
            let second = normal_state.shutdown();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                tokio::join!(first, second)
            })
            .await?
        };
        normal.finish_backend().await?;
        let normal_slot_cleared = normal_state.handle.lock().await.is_none();
        drop(normal_state);
        let normal_drops = normal.drops.load(Ordering::SeqCst);

        let mut unknown = SchedulerFixture::new(true).await?;
        let state = unknown.take_scheduler()?;
        let original = unknown.original_result().await?;
        let first = state.shutdown().await;
        let (second, third) = tokio::join!(state.shutdown(), state.shutdown());
        let identity = unknown
            .identity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original occurrence identity absent"))?;
        let exact = |result: &echo_agent::error::Result<()>| {
            matches!(result,
            Err(ReactError::ExecutionNotSettled { run_id, execution_id: Some(execution_id) })
                if run_id == &identity.0 && execution_id == &identity.1)
        };
        let exact_observations = exact(&first) && exact(&second) && exact(&third);
        let unknown_slot_retained = state.handle.lock().await.is_some();
        let resource_held =
            unknown.capacity.available_permits() == 0 && unknown.drops.load(Ordering::SeqCst) == 0;
        let calls = unknown.calls.load(Ordering::SeqCst);
        // Test-owned backend is genuinely joined before the assertions report
        // old behavior; this is not a fabricated framework journal completion.
        unknown.finish_backend().await?;
        drop(state);
        let final_drops = unknown.drops.load(Ordering::SeqCst);
        println!(
            "scheduler slot: pending={was_pending} kept_after_wait_drop={slot_after_cancelled_wait} normal_cleared={normal_slot_cleared} unknown_slot={unknown_slot_retained} exact_observations={exact_observations} calls={calls} resources_held={resource_held} final_drops={final_drops}"
        );
        assert!(
            matches!(original, Err(ReactError::ExecutionNotSettled { run_id, execution_id: Some(execution_id) }) if run_id == identity.0 && execution_id == identity.1)
        );
        assert!(
            was_pending && slot_after_cancelled_wait,
            "borrowed wait lost the original scheduler slot"
        );
        assert_eq!(
            normal_completed?,
            "same original callback physically completed"
        );
        assert!(normal_first.is_ok() && normal_second.is_ok() && normal_slot_cleared);
        assert_eq!(normal_drops, 1);
        assert!(
            unknown_slot_retained && exact_observations,
            "Unknown was mistaken for empty-slot success"
        );
        assert!(resource_held);
        assert_eq!(calls, 1);
        assert_eq!(final_drops, 1);
        Ok(())
    }
}
