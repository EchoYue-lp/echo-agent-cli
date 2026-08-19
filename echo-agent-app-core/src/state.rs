//! 应用状态管理
//!
//! 支持两种运行模式的状态共享：
//! - 单模式（Web 或 CLI）：独立的 Agent 实例
//! - 双模式（Web + CLI）：共享的 Agent 实例

use chrono::{DateTime, Utc};
use dashmap::DashMap;
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

/// 工具状态
#[derive(Debug, Clone)]
pub struct ToolState {
    pub enabled: bool,
    pub need_approval: bool,
}

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

/// CLI permission rule — simplified serializable form for the REST API.
///
/// This serves as an adapter over the framework's richer [`PermissionRule`]
/// type (in `echo_core::tools::permission`). The `matcher` field is a string
/// that maps to `RuleMatcher` variants (`tool:<name>`, `pattern:<glob>`,
/// `permission:<flag>`, or `*` for catch-all). The `behavior` field maps to
/// `RuleBehavior` (see [`PermissionBehavior`]).
///
/// [`PermissionRule`]: https://docs.rs/echo_agent/latest/echo_agent/tools/permission/struct.PermissionRule.html
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    pub matcher: String,
    pub behavior: PermissionBehavior,
    pub source: String,
}

/// Permission behavior — mirrors `echo_core::tools::permission::RuleBehavior`.
///
/// | Variant | Framework `RuleBehavior` equivalent |
/// |---------|-------------------------------------|
/// | `Allow` | `RuleBehavior::Allow` |
/// | `Deny`  | `RuleBehavior::Deny { reason: String }` |
/// | `Ask`   | `RuleBehavior::Ask { suggestions: Vec<String> }` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

impl std::fmt::Display for PermissionBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionBehavior::Allow => write!(f, "allow"),
            PermissionBehavior::Deny => write!(f, "deny"),
            PermissionBehavior::Ask => write!(f, "ask"),
        }
    }
}

impl PermissionBehavior {
    /// Convert to the framework's `PermissionDecision` (re-exported in
    /// `echo_agent::prelude`).
    ///
    /// The `Ask` and `Deny` variants carry data — the caller is responsible
    /// for providing reason/suggestions where needed.
    pub fn to_permission_decision(
        &self,
        reason: Option<&str>,
    ) -> echo_agent::prelude::PermissionDecision {
        match self {
            PermissionBehavior::Allow => echo_agent::prelude::PermissionDecision::Allow,
            PermissionBehavior::Deny => echo_agent::prelude::PermissionDecision::Deny {
                reason: reason.unwrap_or("denied by rule").to_string(),
            },
            PermissionBehavior::Ask => echo_agent::prelude::PermissionDecision::Ask {
                suggestions: reason.map(|r| vec![r.to_string()]).unwrap_or_default(),
            },
        }
    }
}

impl PermissionRuleConfig {
    pub fn to_framework_rule(
        &self,
    ) -> std::result::Result<echo_agent::tools::permission::PermissionRule, String> {
        use echo_agent::tools::permission::{
            PermissionRule, RuleBehavior, RuleMatcher, RuleSource, ToolPermission,
        };

        let matcher = if self.matcher == "*" {
            RuleMatcher::All
        } else if let Some(name) = self.matcher.strip_prefix("tool:") {
            if name.is_empty() {
                return Err("tool permission matcher requires a name".to_string());
            }
            RuleMatcher::Tool {
                name: name.to_string(),
            }
        } else if let Some(pattern) = self.matcher.strip_prefix("pattern:") {
            if pattern.is_empty() {
                return Err("pattern permission matcher cannot be empty".to_string());
            }
            RuleMatcher::Pattern {
                pattern: pattern.to_string(),
            }
        } else if let Some(flag) = self
            .matcher
            .strip_prefix("perm:")
            .or_else(|| self.matcher.strip_prefix("permission:"))
        {
            let permission = match flag {
                "read" => ToolPermission::Read,
                "write" => ToolPermission::Write,
                "network" => ToolPermission::Network,
                "execute" => ToolPermission::Execute,
                "sensitive" => ToolPermission::Sensitive,
                _ => return Err(format!("unknown permission matcher: {flag}")),
            };
            RuleMatcher::Permission { permission }
        } else {
            return Err(format!("unsupported permission matcher: {}", self.matcher));
        };
        let behavior = match self.behavior {
            PermissionBehavior::Allow => RuleBehavior::Allow,
            PermissionBehavior::Deny => RuleBehavior::Deny {
                reason: "denied by EKO permission rule".to_string(),
            },
            PermissionBehavior::Ask => RuleBehavior::Ask {
                suggestions: vec!["allow".to_string(), "deny".to_string()],
            },
        };
        let source = match self.source.as_str() {
            "session" => RuleSource::Session,
            "cliArg" | "cli_arg" => RuleSource::CliArg,
            "projectSettings" | "project_settings" => RuleSource::ProjectSettings,
            "localSettings" | "local_settings" => RuleSource::LocalSettings,
            "managed" => RuleSource::Managed,
            _ => RuleSource::UserSettings,
        };
        Ok(PermissionRule {
            matcher,
            behavior,
            source,
            description: Some("EKO application permission rule".to_string()),
        })
    }

    /// Check whether this rule applies to a given tool by name.
    ///
    /// Supported matcher patterns:
    /// - `tool:<name>` — exact tool name match
    /// - `perm:<flag>` — matches if the tool declares the given permission
    ///   (this method returns `true` for all `perm:` matchers; the caller
    ///   performs the actual permission check)
    /// - `*` — catch-all, matches every tool
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        if self.matcher == "*" {
            return true;
        }
        if let Some(name) = self.matcher.strip_prefix("tool:") {
            return name == tool_name;
        }
        // perm:<flag> — tool-level; caller checks the flag
        if self.matcher.starts_with("perm:") {
            return true;
        }
        false
    }

    /// Parse a `perm:<flag>` matcher into the corresponding [`ToolPermission`].
    ///
    /// Returns `None` for non-permission matchers (`tool:`, `*`).
    /// Returns `None` for unrecognized flag names.
    pub fn parse_permission_flag(&self) -> Option<echo_agent::prelude::ToolPermission> {
        let flag = self.matcher.strip_prefix("perm:")?;
        match flag {
            "read" => Some(echo_agent::prelude::ToolPermission::Read),
            "write" => Some(echo_agent::prelude::ToolPermission::Write),
            "network" => Some(echo_agent::prelude::ToolPermission::Network),
            "execute" => Some(echo_agent::prelude::ToolPermission::Execute),
            "sensitive" => Some(echo_agent::prelude::ToolPermission::Sensitive),
            _ => {
                tracing::warn!(%flag, "Unrecognized permission flag in matcher");
                None
            }
        }
    }
}

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
        if let Err(error) = binding.deletions.ensure_admission_allowed(conversation_id) {
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
    pub app_config: RwLock<echo_agent::config::AppConfig>,
    /// Runtime model currently published to primary and pooled agents. This
    /// remains distinct from the durable default when startup used `--model`.
    active_model_id: RwLock<String>,
    /// Immutable startup source used for every application-side config commit.
    pub config_path: std::path::PathBuf,
    pub web_config: RwLock<WebConfig>,
    pub sandbox_config: RwLock<SandboxConfigData>,
    pub permission_mode: RwLock<String>,
    pub permission_rules: RwLock<Vec<PermissionRuleConfig>>,
    model_mutations: Mutex<ModelMutationOwnerState>,
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
    pub model: echo_agent::config::ConfiguredModel,
    pub set_default: bool,
}

#[derive(Debug, Clone)]
pub struct ModelProviderMutation {
    pub id: String,
    pub provider: echo_agent::config::ModelProviderConfig,
    pub preserve_auth_token: bool,
}

/// Linearized result returned only after disk, snapshot, primary, and pool
/// publication have completed for an active-model mutation.
#[derive(Clone)]
pub struct ModelMutationReceipt {
    pub config: echo_agent::config::AppConfig,
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
    Box<dyn FnOnce(&mut echo_agent::config::AppConfig) -> Result<(), String> + Send + 'static>;

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
    config: echo_agent::config::AppConfig,
    model_id: String,
    runtime: Option<crate::model_config::ModelRuntimeConfig>,
    prepared: Option<crate::infra::PreparedRuntimeLlm>,
    activated: bool,
    deactivated: bool,
    deleted: bool,
}

/// 会话状态：工具状态、非聊天操作取消和前台 turn 控制。
pub struct SessionState {
    pub tool_states: RwLock<HashMap<String, ToolState>>,
    /// Cancellation registry for non-chat operations such as analysis jobs.
    pub operation_cancel_tokens: Arc<DashMap<String, CancellationToken>>,
    /// Application authority for foreground chat admission and cancellation.
    pub foreground_turns: crate::foreground_turn::ForegroundTurnControl,
}

/// 插件状态：MCP 服务管理
pub struct PluginState {
    pub mcp_config: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    pub mcp_health: RwLock<HashMap<String, McpHealthStatus>>,
}

/// 持久化存储状态
pub struct ConversationStorageBinding {
    pub store: Option<Arc<dyn ConversationStore>>,
    pub runtime_state: Option<Arc<dyn RuntimeStateStore>>,
    pub deletions: Arc<crate::conversation_deletion::ConversationDeletionService>,
}

pub struct StorageState {
    pub conversation: Arc<RwLock<ConversationStorageBinding>>,
    pub tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
    /// Bounded ordinary-chat replay stream. Formal task history remains owned
    /// by TaskRuntimeStore; long-term transcript remains ConversationStore.
    pub chat_events: Arc<crate::chat_event_log::ChatEventLog>,
}

/// 历史记录状态：审计日志 + 工作流
pub struct HistoryState {
    pub audit_logs: RwLock<Vec<AuditLogEntry>>,
    pub workflows: Arc<crate::workflow_service::WorkflowService>,
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
        let handle = self.handle.lock().await.take();
        if let Some(handle) = handle {
            handle.shutdown().await?;
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
    /// Backs TaskRuntime query commands. `None` only if both the
    /// on-disk open and the in-memory fallback failed (extreme OOM).
    pub runtime: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    /// Manual interaction mode override (Chat/Task/Auto). `Auto` chooses
    /// between direct work and a formal run; `Chat` disables formal task tools
    /// for the turn; `Task` requires a formal run and plan lifecycle.
    /// Toggleable at runtime via Tauri command.
    pub interaction_mode: std::sync::atomic::AtomicU8,
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

/// 工作区状态
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "WorkspaceTransitionStatus")]
pub enum WorkspaceTransitionStatus {
    Committed,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "WorkspaceSubsystemTransition")]
pub struct WorkspaceSubsystemTransition {
    pub subsystem: String,
    pub target_root: std::path::PathBuf,
    #[serde(default)]
    pub stale_roots: Vec<std::path::PathBuf>,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "WorkspaceTransitionReceipt")]
pub struct WorkspaceTransitionReceipt {
    pub status: WorkspaceTransitionStatus,
    pub previous_workspace_id: Option<String>,
    pub target_workspace_id: Option<String>,
    pub target_root: std::path::PathBuf,
    pub degraded_subsystems: Vec<WorkspaceSubsystemTransition>,
}

impl WorkspaceTransitionReceipt {
    fn committed(
        previous_workspace_id: Option<String>,
        target_workspace_id: Option<String>,
        target_root: std::path::PathBuf,
        degraded_subsystems: Vec<WorkspaceSubsystemTransition>,
    ) -> Self {
        let status = if degraded_subsystems.is_empty() {
            WorkspaceTransitionStatus::Committed
        } else {
            WorkspaceTransitionStatus::Degraded
        };
        Self {
            status,
            previous_workspace_id,
            target_workspace_id,
            target_root,
            degraded_subsystems,
        }
    }
}

fn memory_rebind_degradation(
    target_root: std::path::PathBuf,
    receipt: &crate::evolution::MemoryRebindReceipt,
) -> WorkspaceSubsystemTransition {
    let detail = receipt
        .delivery_error
        .clone()
        .unwrap_or_else(|| "old-root trigger delivery remains pending".to_string());
    WorkspaceSubsystemTransition {
        subsystem: "memory_evolution".to_string(),
        target_root,
        stale_roots: receipt.pending_roots.clone(),
        error: format!(
            "{detail}; {} trigger(s) remain owned for retry",
            receipt.pending_old
        ),
    }
}

fn memory_rule_projection_degradation(
    target_root: std::path::PathBuf,
    error: impl std::fmt::Display,
) -> WorkspaceSubsystemTransition {
    WorkspaceSubsystemTransition {
        subsystem: "memory_rule_projection".to_string(),
        target_root,
        stale_roots: Vec::new(),
        error: error.to_string(),
    }
}

enum WorkspaceTransitionRequest {
    Switch(Workspace),
    Exit,
}

type WorkspaceSettlementHandle =
    tokio::task::JoinHandle<anyhow::Result<WorkspaceTransitionReceipt>>;

pub struct WorkspaceState {
    /// 当前活跃工作区（None 表示使用全局默认路径）。
    pub current: RwLock<Option<Workspace>>,
    /// 工作区注册表。
    pub registry: Arc<WorkspaceRegistry>,
    /// Process directory to restore when leaving workspace mode.
    pub global_cwd: std::path::PathBuf,
    /// Serializes generation changes so two UI or automation requests cannot
    /// interleave primary/pool/store rebinding.
    pub transition: Mutex<()>,
    /// Owned non-abortable settlement after a transition request is accepted.
    /// Dropping an IPC/CLI waiter detaches only that waiter; the application
    /// retains this handle until publication or shutdown has awaited it.
    settlement: Mutex<Option<WorkspaceSettlementHandle>>,
    /// Last committed transition, including degraded subsystem settlement.
    pub last_transition: RwLock<Option<WorkspaceTransitionReceipt>>,
}

/// 全局应用状态
///
/// 按功能域拆分为子状态，通过 `Arc<AppState>` 共享。
pub struct AppState {
    /// 连接管理（Agent 句柄）
    pub connection: ConnectionState,
    /// 配置（应用 / Web / 安全 / 沙箱 / 权限）
    pub config: ConfigState,
    /// 会话状态（工具 / 取消 / 限速）
    pub session: SessionState,
    /// 插件（MCP）
    pub plugins: PluginState,
    /// 持久化存储
    pub storage: StorageState,
    /// 历史记录（审计 / 工作流）
    pub history: HistoryState,
    /// 调度器（定时任务）
    pub scheduler: SchedulerState,
    /// 后台任务系统
    pub tasks: TaskState,
    /// Webhook 事件回调
    pub webhook: WebhookState,
    /// Run diagnostics product projection state.
    pub observability: ObservabilityState,
    /// 工作区管理
    pub workspace: WorkspaceState,
    /// Skills Hub（本地技能市场）
    pub skills_hub: Arc<RwLock<crate::skills_hub::SkillsHub>>,
    /// Shared memory review integration for GUI/IPC paths that write real memory.
    pub review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    /// Process-level shared plugin runtime (P0-4). `None` until bootstrap
    /// completes the primary agent; populated via
    /// [`Self::with_plugin_runtime`].
    pub plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    /// Sole acknowledged hook/config watcher lifecycle handle.
    pub config_watcher: Option<Arc<crate::config_watcher::ConfigWatcherHandle>>,
    /// Interactive terminal authority shared by GUI, TUI, CLI, and channels.
    pub terminal: Arc<crate::terminal::TerminalService>,
}

impl AppState {
    /// 从共享的 Agent 和 HITL Dispatcher 创建状态（用于双模式）
    pub fn from_shared(
        agent: AgentHandle,
        model_consumers: Option<crate::infra::AgentModelConsumers>,
        hitl_dispatcher: Arc<crate::hitl::HitlDispatcher>,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        runtime_state_store: Option<Arc<dyn RuntimeStateStore>>,
        app_config: echo_agent::config::AppConfig,
        mcp_config_runtime: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    ) -> Self {
        let config = agent
            .try_write(|guard| WebConfig {
                model: guard.model_name().to_string(),
                system_prompt: guard.system_prompt().to_string(),
                token_limit: 8000,
                ..Default::default()
            })
            .unwrap_or_default();
        let initial_tool_output_artifacts = agent
            .try_write(|guard| guard.tool_output_artifacts())
            .flatten()
            .unwrap_or_else(|| crate::infra::tool_output_artifact_config(None));

        let active_model_id = app_config
            .model
            .default_model_id
            .as_deref()
            .and_then(|id| {
                app_config
                    .configured_models
                    .iter()
                    .find(|model| model.id == id && model.enabled)
            })
            .or_else(|| {
                app_config
                    .configured_models
                    .iter()
                    .find(|model| model.enabled)
            })
            .map(|model| model.id.clone())
            .unwrap_or_default();
        let webhook_emitter = Arc::new(crate::webhook::WebhookEmitter::from_config(&app_config));
        let conversation_binding = Arc::new(RwLock::new(ConversationStorageBinding {
            store: conversation_store,
            runtime_state: runtime_state_store,
            deletions: Arc::new(
                crate::conversation_deletion::ConversationDeletionService::at_default_root(),
            ),
        }));

        Self {
            connection: ConnectionState {
                agent,
                model_consumers,
                hitl_dispatcher,
                pool: None,
                conversation_binding: conversation_binding.clone(),
            },
            config: ConfigState {
                app_config: RwLock::new(app_config),
                active_model_id: RwLock::new(active_model_id),
                config_path: crate::config_watcher::resolve_config_save_path(None),
                web_config: RwLock::new(config),
                sandbox_config: RwLock::new(SandboxConfigData::default()),
                permission_mode: RwLock::new("default".to_string()),
                permission_rules: RwLock::new(Vec::new()),
                model_mutations: Mutex::new(ModelMutationOwnerState::default()),
            },
            session: SessionState {
                tool_states: RwLock::new(HashMap::new()),
                operation_cancel_tokens: Arc::new(DashMap::new()),
                foreground_turns: crate::foreground_turn::ForegroundTurnControl::default(),
            },
            plugins: PluginState {
                mcp_config: mcp_config_runtime,
                mcp_health: RwLock::new(HashMap::new()),
            },
            storage: StorageState {
                conversation: conversation_binding,
                tool_executions: {
                    let root = crate::tool_execution::ToolExecutionRepository::default_root();
                    let repository =
                        crate::tool_execution::ToolExecutionRepository::open(root.clone())
                            .or_else(|error| {
                                tracing::warn!(
                                    path = %root.display(),
                                    %error,
                                    "Failed to open tool execution repository; using temporary storage"
                                );
                                let fallback = std::env::temp_dir().join("eko-tool-executions");
                                crate::tool_execution::ToolExecutionRepository::open(fallback.clone())
                                    .map_err(|fallback_error| {
                                        tracing::warn!(
                                            path = %fallback.display(),
                                            error = %fallback_error,
                                            "Failed to open fallback tool execution repository"
                                        );
                                        fallback
                                    })
                            })
                            .unwrap_or_else(|fallback| {
                                crate::tool_execution::ToolExecutionRepository::without_initialization(
                                    fallback,
                                )
                            });
                    repository.register_artifact_config(initial_tool_output_artifacts);
                    Arc::new(repository)
                },
                chat_events: Arc::new(crate::chat_event_log::ChatEventLog::at_default_root()),
            },
            history: HistoryState {
                audit_logs: RwLock::new(Vec::new()),
                workflows: Arc::new(crate::workflow_service::WorkflowService::at_default_path()),
            },
            scheduler: SchedulerState {
                runner: None,
                cancel_token: echo_agent::agent::CancellationToken::new(),
                handle: Mutex::new(None),
            },
            tasks: TaskState {
                service: None,
                cancel_token: CancellationToken::new(),
                runtime: {
                    let store = crate::tasks::task_runtime::TaskRuntimeStore::new().or_else(|e| {
                        tracing::warn!(
                            "Failed to open file-backed TaskRuntime store: {e}; falling back to in-memory"
                        );
                        crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                    });
                    store.ok().map(|store| {
                        // P1-8: proactively recover runs interrupted by a previous
                        // process crash into resumable Paused runs.
                        match store.recover_incomplete() {
                            Ok(recovered) if recovered > 0 => {
                                tracing::info!(
                                    count = recovered,
                                    "Recovered interrupted task-runtime runs at boot"
                                );
                            }
                            Ok(_) => {}
                            Err(error) => tracing::warn!(
                                %error,
                                "Failed to recover interrupted task-runtime runs at boot"
                            ),
                        }
                        Arc::new(store)
                    })
                },
                interaction_mode: std::sync::atomic::AtomicU8::new(0), // 0 = Auto
            },
            webhook: WebhookState {
                emitter: webhook_emitter,
            },
            observability: ObservabilityState {
                prompt_assembly: RwLock::new(None),
            },
            workspace: WorkspaceState {
                current: RwLock::new(None),
                transition: Mutex::new(()),
                settlement: Mutex::new(None),
                last_transition: RwLock::new(None),
                global_cwd: std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                registry: Arc::new(WorkspaceRegistry::new().unwrap_or_else(|e| {
                    tracing::warn!("Failed to init workspace registry: {e}");
                    let fallback_dir = std::env::temp_dir().join("echo-workspaces");
                    WorkspaceRegistry::with_base_dir(fallback_dir.clone()).unwrap_or_else(
                        |fallback_error| {
                            tracing::warn!(
                                error = %fallback_error,
                                path = %fallback_dir.display(),
                                "Failed to create fallback workspace directory; registry writes may fail"
                            );
                            WorkspaceRegistry::without_initialization(fallback_dir)
                        },
                    )
                })),
            },
            skills_hub: Arc::new(RwLock::new(crate::skills_hub::SkillsHub::new())),
            review_integration: None,
            plugin_runtime: None,
            config_watcher: None,
            terminal: crate::terminal::TerminalService::new(),
        }
    }

    /// Record the non-persistent model generation selected during bootstrap.
    pub fn with_active_model_id(mut self, active_model_id: impl Into<String>) -> Self {
        *self.config.active_model_id.get_mut() = active_model_id.into();
        self
    }

    /// Bind config persistence to the source selected during bootstrap.
    pub fn with_config_path(mut self, path: std::path::PathBuf) -> Self {
        self.config.config_path = path;
        self
    }

    /// Persist one complete config snapshot to the immutable bootstrap source.
    fn save_app_config(
        &self,
        config: &echo_agent::config::AppConfig,
    ) -> std::result::Result<(), String> {
        echo_agent::config::save_config_file(&self.config.config_path, config)
    }

    /// Upsert one configured model through the sole application-owned config
    /// mutation settlement path.
    pub async fn upsert_configured_model_owned(
        self: &Arc<Self>,
        mutation: ConfiguredModelMutation,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::UpsertModel(mutation))
            .await
    }

    /// Upsert one provider and refresh the active generation when it uses the
    /// edited provider.
    pub async fn upsert_model_provider_owned(
        self: &Arc<Self>,
        mutation: ModelProviderMutation,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::UpsertProvider(mutation))
            .await
    }

    /// Resolve an id or unambiguous model selector, persist it as the default,
    /// and publish the exact prepared client to primary and pooled agents.
    pub async fn set_default_model_owned(
        self: &Arc<Self>,
        selector: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::SetDefault(selector.into()))
            .await
    }

    /// Delete a configured model. Deleting the active default is accepted only
    /// when another enabled model has passed the real client preflight.
    pub async fn delete_configured_model_owned(
        self: &Arc<Self>,
        model_id: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::DeleteModel(model_id.into()))
            .await
    }

    /// Delete a provider after all of its models have been removed.
    pub async fn delete_model_provider_owned(
        self: &Arc<Self>,
        provider_id: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::DeleteProvider(provider_id.into()))
            .await
    }

    /// Serialize a broader AppConfig edit with model mutations so a stale
    /// whole-config snapshot cannot overwrite an accepted model publication.
    /// When model runtime fields change, the active model is preflighted and
    /// republished within the same owned settlement.
    pub async fn update_app_config_owned<Update>(
        self: &Arc<Self>,
        reapply_active_model: bool,
        update: Update,
    ) -> Result<echo_agent::config::AppConfig, ModelMutationError>
    where
        Update: FnOnce(&mut echo_agent::config::AppConfig) -> Result<(), String> + Send + 'static,
    {
        self.run_owned_model_mutation(ModelMutationRequest::UpdateConfig {
            update: Box::new(update),
            reapply_active_model,
        })
        .await
        .map(|receipt| receipt.config)
    }

    async fn run_owned_model_mutation(
        self: &Arc<Self>,
        request: ModelMutationRequest,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        let mut owner = self.config.model_mutations.lock().await;
        if let ModelMutationOwnerLifecycle::Closed(_) = &owner.lifecycle {
            return Err(ModelMutationError::ShuttingDown);
        }
        let previous = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Running(previous) => Some(previous.clone()),
            _ => None,
        };
        if let Some(previous) = previous {
            let previous = previous.await.map(Some);
            owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(previous.clone()));
            previous?;
        }
        if let ModelMutationOwnerLifecycle::Settled(result) = &owner.lifecycle {
            result.as_ref().clone()?;
        }

        let state = Arc::clone(self);
        #[cfg(test)]
        let abort_for_test = matches!(&request, ModelMutationRequest::AbortSettlementForTest);
        let task = tokio::spawn(async move {
            #[cfg(test)]
            if matches!(&request, ModelMutationRequest::AbortSettlementForTest) {
                return std::future::pending::<Result<ModelMutationReceipt, ModelMutationError>>()
                    .await;
            }
            state.apply_model_mutation_inner(request).await
        });
        #[cfg(test)]
        if abort_for_test {
            task.abort();
        }
        let settlement = async move {
            task.await
                .map_err(|error| ModelMutationError::Settlement(error.to_string()))?
        }
        .boxed()
        .shared();
        owner.lifecycle = ModelMutationOwnerLifecycle::Running(settlement.clone());
        let result = settlement.await;
        owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(result.clone().map(Some)));
        result
    }

    async fn apply_model_mutation_inner(
        &self,
        request: ModelMutationRequest,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        let current = self.config.app_config.read().await.clone();
        let active_model_id = self.config.active_model_id.read().await.clone();
        let mutation = prepare_model_mutation(&current, &active_model_id, request)?;
        let next_active_runtime = if mutation.deactivated {
            None
        } else if mutation.activated {
            Some(mutation.runtime.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its runtime candidate".to_string(),
                )
            })?)
        } else {
            resolve_active_model_runtime(&mutation.config, &active_model_id)?
        };
        let pool_session_config = match next_active_runtime.as_ref() {
            Some(runtime) => {
                crate::model_config::session_config_for_runtime(&mutation.config, runtime)
                    .map_err(ModelMutationError::Publication)?
            }
            None => mutation.config.clone(),
        };
        let _foreground = if mutation.activated || mutation.deactivated {
            Some(
                self.session
                    .foreground_turns
                    .suspend_admission_if_idle()
                    .map_err(|error| ModelMutationError::Publication(error.to_string()))?,
            )
        } else {
            None
        };
        let (runtime, prepared) = if mutation.activated {
            let runtime = mutation.runtime.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its runtime candidate".to_string(),
                )
            })?;
            let prepared = mutation.prepared.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its prepared client".to_string(),
                )
            })?;
            (Some(runtime), Some(prepared))
        } else {
            (None, None)
        };
        let pool_publication = match (
            self.connection.pool.as_ref(),
            runtime.as_ref(),
            prepared.as_ref(),
        ) {
            (Some(pool), Some(runtime), Some(prepared)) => Some(
                pool.prepare_model_publication(
                    pool_session_config.clone(),
                    runtime.clone(),
                    prepared.clone(),
                )
                .await
                .map_err(ModelMutationError::Publication)?,
            ),
            _ => None,
        };
        let pool_deactivation = if mutation.deactivated {
            match self.connection.pool.as_ref() {
                Some(pool) => Some(
                    pool.prepare_model_deactivation(pool_session_config.clone())
                        .await
                        .map_err(ModelMutationError::Publication)?,
                ),
                None => None,
            }
        } else {
            None
        };
        let primary_publication = match (runtime.as_ref(), prepared.as_ref()) {
            (Some(runtime), Some(prepared)) => {
                let consumers = self.connection.model_consumers.clone().ok_or_else(|| {
                    ModelMutationError::Publication(
                        "primary model consumers are unavailable".to_string(),
                    )
                })?;
                Some(
                    crate::infra::prepare_agent_model_publication(
                        &self.connection.agent,
                        consumers,
                        runtime,
                        prepared,
                        crate::infra::effective_token_limit(&mutation.config, Some(runtime)),
                    )
                    .await
                    .map_err(ModelMutationError::Publication)?,
                )
            }
            _ => None,
        };
        let primary_deactivation = if mutation.deactivated {
            let consumers = self.connection.model_consumers.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "primary model consumers are unavailable".to_string(),
                )
            })?;
            Some(
                crate::infra::prepare_agent_model_deactivation(&self.connection.agent, consumers)
                    .await,
            )
        } else {
            None
        };

        self.save_app_config(&mutation.config)
            .map_err(ModelMutationError::Persistence)?;
        *self.config.app_config.write().await = mutation.config.clone();

        if let Some(publication) = primary_publication {
            publication.commit().await;
        } else if let Some(deactivation) = primary_deactivation {
            deactivation.commit().await;
        }

        if let Some(publication) = pool_publication {
            publication.commit().await;
        } else if let Some(deactivation) = pool_deactivation {
            deactivation.commit().await;
        } else if let Some(pool) = self.connection.pool.as_ref() {
            pool.update_app_config(pool_session_config).await;
        }

        if let Some(runtime) = runtime.as_ref() {
            *self.config.active_model_id.write().await = runtime.id.clone();
            tracing::info!(
                model_id = %runtime.id,
                provider = %runtime.provider,
                model = %runtime.model,
                "active model mutation fully settled"
            );
        } else if mutation.deactivated {
            self.config.active_model_id.write().await.clear();
            tracing::info!("active model removed; agent requires model configuration");
        }
        Ok(ModelMutationReceipt {
            config: mutation.config,
            model_id: mutation.model_id,
            runtime: mutation.runtime,
            activated: mutation.activated,
            deleted: mutation.deleted,
        })
    }

    /// Close model mutation admission and await an accepted settlement whose
    /// caller was dropped before application shutdown.
    pub async fn shutdown_model_mutations(&self) -> Result<(), ModelMutationError> {
        let mut owner = self.config.model_mutations.lock().await;
        if let ModelMutationOwnerLifecycle::Closed(result) = &owner.lifecycle {
            return result.clone();
        }
        let settlement = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Running(settlement) => Some(settlement.clone()),
            _ => None,
        };
        if let Some(settlement) = settlement {
            let result = settlement.await.map(Some);
            owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(result));
        }
        let result = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Settled(result) => result.as_ref().clone().map(|_| ()),
            ModelMutationOwnerLifecycle::Closed(result) => result.clone(),
            ModelMutationOwnerLifecycle::Running(_) => Err(ModelMutationError::Settlement(
                "model mutation owner did not reach a terminal state".to_string(),
            )),
        };
        owner.lifecycle = ModelMutationOwnerLifecycle::Closed(result.clone());
        result
    }

    /// Attach the shared review integration created during runtime bootstrap.
    pub fn with_review_integration(
        mut self,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    ) -> Self {
        self.review_integration = review_integration;
        self
    }

    /// Attach the prompt-module report captured during runtime bootstrap.
    pub fn with_prompt_assembly(
        mut self,
        prompt_assembly: crate::project::prompt::PromptAssembly,
    ) -> Self {
        *self.observability.prompt_assembly.get_mut() = Some(prompt_assembly);
        self
    }

    /// Attach the shared plugin runtime (P0-4).
    ///
    /// Built once bootstrap has created the primary agent (the service derives
    /// its `project_root` from the agent's `working_dir`). Call before wrapping
    /// in `Arc`.
    pub fn with_plugin_runtime(
        mut self,
        plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    ) -> Self {
        self.plugin_runtime = plugin_runtime;
        self
    }

    pub fn with_config_watcher(
        mut self,
        config_watcher: Option<Arc<crate::config_watcher::ConfigWatcherHandle>>,
    ) -> Self {
        self.config_watcher = config_watcher;
        self
    }

    /// Share one foreground admission authority across concurrently active
    /// headless surfaces such as CLI and channels.
    pub fn with_foreground_turns(
        mut self,
        foreground_turns: crate::foreground_turn::ForegroundTurnControl,
    ) -> Self {
        self.session.foreground_turns = foreground_turns;
        self
    }

    /// Return the conversation store from the currently published workspace binding.
    pub async fn conversation_store(&self) -> Option<Arc<dyn ConversationStore>> {
        self.storage.conversation.read().await.store.clone()
    }

    /// Create a conversation under the same identity lock used by aggregate deletion.
    pub async fn create_conversation_owned(
        &self,
        conversation: NewConversation,
    ) -> std::result::Result<Conversation, crate::conversation_deletion::ConversationDeletionError>
    {
        let _workspace = self.workspace.transition.lock().await;
        let binding = self.storage.conversation.read().await;
        let store = binding
            .store
            .as_ref()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        binding
            .deletions
            .create_conversation(store.as_ref(), conversation)
            .await
    }

    /// Ensure a conversation under the same identity lock used by aggregate deletion.
    pub async fn ensure_conversation_owned(
        &self,
        conversation: NewConversation,
    ) -> std::result::Result<Conversation, crate::conversation_deletion::ConversationDeletionError>
    {
        let _workspace = self.workspace.transition.lock().await;
        let binding = self.storage.conversation.read().await;
        let store = binding
            .store
            .as_ref()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        binding
            .deletions
            .ensure_conversation(store.as_ref(), conversation)
            .await
    }

    /// Begin a real user turn through the durable conversation admission boundary.
    pub async fn begin_conversation_turn_owned(
        &self,
        surface: crate::foreground_turn::ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: impl Into<String>,
    ) -> std::result::Result<
        crate::foreground_turn::ForegroundTurnLease,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let binding = self.storage.conversation.read().await;
        binding
            .deletions
            .begin_foreground_turn(
                &self.session.foreground_turns,
                surface,
                conversation_id,
                turn_id,
            )
            .await
    }

    /// Delete every application-owned projection before retiring transcript authority.
    pub async fn delete_conversation_owned(
        &self,
        conversation_id: &str,
    ) -> std::result::Result<
        crate::conversation_deletion::ConversationDeletionReceipt,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let _workspace = self.workspace.transition.lock().await;
        let binding = self.storage.conversation.read().await;
        let artifact_config = self
            .connection
            .agent
            .read(|agent| agent.tool_output_artifacts())
            .await;
        binding
            .deletions
            .delete(
                conversation_id,
                binding.store.clone(),
                self.connection.pool.clone(),
                self.tasks.runtime.clone(),
                self.storage.tool_executions.clone(),
                self.storage.chat_events.clone(),
                binding.runtime_state.clone(),
                &self.session.foreground_turns,
                artifact_config,
            )
            .await
    }

    /// Resume finalizer cleanup that crossed the transcript commit boundary.
    pub async fn recover_committed_conversation_deletions(
        &self,
    ) -> std::result::Result<
        Vec<crate::conversation_deletion::ConversationDeletionReceipt>,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let _workspace = self.workspace.transition.lock().await;
        let binding = self.storage.conversation.read().await;
        let store = binding
            .store
            .as_ref()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        binding
            .deletions
            .recover_committed_deletions(store.as_ref())
            .await
    }

    /// Set the agent pool for multi-conversation parallel execution.
    ///
    /// Call this **before** wrapping in `Arc`.
    pub fn set_pool(&mut self, pool: Arc<crate::agent_pool::AgentPool>) {
        self.connection.pool = Some(pool);
    }

    /// 启动定时任务调度器（仅在 Web 或双模式下调用）
    ///
    /// Call this **before** wrapping in `Arc`.
    pub async fn start_scheduler(&mut self) -> echo_agent::error::Result<()> {
        self.start_scheduler_with_store(None).await
    }

    /// 启动定时任务调度器，可选 Store 后端
    pub async fn start_scheduler_with_store(
        &mut self,
        backend: Option<Arc<dyn echo_agent::memory::Store>>,
    ) -> echo_agent::error::Result<()> {
        if self.scheduler.runner.is_some() {
            return Ok(());
        }
        let store = match backend {
            Some(b) => crate::scheduler::CronTaskStore::with_store(b).await?,
            None => crate::scheduler::CronTaskStore::new(),
        };
        // Phase C: pass the agent pool so each cron run acquires its OWN
        // per-run agent (worktree working_dir binding is per-run, fixing the
        // latent override bug where overlapping cron runs clobbered the shared
        // agent's working_dir). Falls back to the shared primary agent when no
        // pool is configured.
        let runner = crate::scheduler::new_scheduler_runner(
            store,
            self.scheduler.cancel_token.clone(),
            self.connection.agent.clone(),
            self.tasks.runtime.clone(),
            self.connection.pool.clone(),
            // Share the AppState's webhook emitter so cron runs emit
            // CronTaskCompleted on the same endpoint set as chat. `emit`
            // cheaply no-ops when no endpoints are registered.
            Some(self.webhook.emitter.clone()),
            self.review_integration.clone(),
        )
        .await?;
        let runner = Arc::new(runner);
        let handle = runner.clone().spawn();
        *self.scheduler.handle.get_mut() = Some(handle);
        self.scheduler.runner = Some(runner);
        tracing::info!("Scheduler runner started");
        Ok(())
    }

    /// Cancel the scheduler loop and await any in-flight cron fire.
    ///
    /// Repeated calls are harmless. The framework handle is process-scoped;
    /// workspace changes rebind the shared TaskRuntime store instead of
    /// starting another scheduler.
    pub async fn shutdown_scheduler(&self) -> echo_agent::error::Result<()> {
        self.scheduler.shutdown().await
    }

    /// Start the fallible scheduler before admitting background TaskRun
    /// recovery, then start the pool monitor only after both owners exist.
    pub async fn start_scheduler_and_task_service(
        &mut self,
        backend: Option<Arc<dyn echo_agent::memory::Store>>,
    ) -> echo_agent::error::Result<()> {
        self.start_scheduler_with_store(backend).await?;
        self.start_task_service().await;
        if let Some(pool) = self.connection.pool.as_ref() {
            pool.spawn_cleanup_monitor().await;
        }
        Ok(())
    }

    /// 启动后台任务服务（所有模式都应调用）
    ///
    /// When an agent pool is active, background tasks run on a dedicated
    /// pool agent instead of the primary agent, enabling parallel execution
    /// with foreground conversations.
    ///
    /// Call this **before** wrapping in `Arc`.
    pub async fn start_task_service(&mut self) {
        if self.tasks.service.is_some() {
            return;
        }

        let service_result = if let Some(ref pool) = self.connection.pool {
            crate::tasks::BackgroundTaskService::with_pool(
                pool.clone(),
                self.tasks.cancel_token.clone(),
                self.tasks.runtime.clone(),
            )
            .await
        } else {
            crate::tasks::BackgroundTaskService::new(
                self.connection.agent.clone(),
                self.tasks.cancel_token.clone(),
                self.tasks.runtime.clone(),
            )
            .await
        };

        match service_result {
            Ok(service) => {
                let service =
                    Arc::new(service.with_review_integration(self.review_integration.clone()));
                service.clone().spawn();
                self.tasks.service = Some(service);
                tracing::info!("BackgroundTaskService started");
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "BackgroundTaskService failed to initialize — background tasks will be unavailable"
                );
            }
        }
    }

    /// 获取工具列表信息
    pub async fn get_tool_infos(&self, handle: &AgentHandle) -> Vec<crate::types::ToolInfo> {
        let tool_states = self.session.tool_states.read().await;

        handle
            .read(|agent| agent.tool_definitions())
            .await
            .into_iter()
            .map(|def| {
                let state = tool_states
                    .get(&def.function.name)
                    .cloned()
                    .unwrap_or(ToolState {
                        enabled: true,
                        need_approval: false,
                    });

                crate::types::ToolInfo {
                    name: def.function.name,
                    description: def.function.description,
                    parameters: def.function.parameters,
                    enabled: state.enabled,
                    need_approval: state.need_approval,
                    source: crate::types::ToolSource::Builtin,
                }
            })
            .collect()
    }

    /// 运行一次 MCP 健康检查，更新 `mcp_health` 状态
    pub async fn run_mcp_health_check(&self) {
        let server_names: Vec<String> = self
            .connection
            .agent
            .read(|agent| {
                agent
                    .list_mcp_servers()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .await;

        let now = chrono::Utc::now();
        let mut new_health: HashMap<String, McpHealthStatus> = HashMap::new();

        for name in &server_names {
            let healthy = self
                .connection
                .agent
                .read(|agent| {
                    agent
                        .mcp_client(name)
                        .map(|client| !client.tools().is_empty())
                        .unwrap_or(false)
                })
                .await;
            let error = if healthy {
                None
            } else {
                Some("MCP server unresponsive or returned empty tools".to_string())
            };
            new_health.insert(
                name.clone(),
                McpHealthStatus {
                    name: name.clone(),
                    healthy,
                    last_check: Some(now),
                    error,
                },
            );
        }

        // 写入健康状态
        let mut health_map = self.plugins.mcp_health.write().await;
        *health_map = new_health;
    }

    /// 添加审计日志条目（FIFO 淘汰，防止内存无限增长）
    pub async fn add_audit_entry(&self, entry: AuditLogEntry) {
        let mut logs = self.history.audit_logs.write().await;
        logs.push(entry);
        // Trim oldest entries if over the limit
        if logs.len() > max_audit_log_entries() {
            let excess = logs.len() - max_audit_log_entries();
            logs.drain(0..excess);
        }
    }

    /// 获取审计日志的只读快照
    pub async fn get_audit_logs(&self) -> Vec<AuditLogEntry> {
        self.history.audit_logs.read().await.clone()
    }

    /// 获取审计日志分页
    pub async fn get_audit_logs_paged(&self, offset: usize, limit: usize) -> Vec<AuditLogEntry> {
        let logs = self.history.audit_logs.read().await;
        logs.iter().skip(offset).take(limit).cloned().collect()
    }

    /// 获取审计日志总数
    pub async fn audit_log_count(&self) -> usize {
        self.history.audit_logs.read().await.len()
    }

    /// 清空审计日志，返回清除的条目数
    pub async fn clear_audit_entries(&self) -> usize {
        let mut logs = self.history.audit_logs.write().await;
        let count = logs.len();
        logs.clear();
        count
    }

    // ── 工作区管理 ──

    /// 获取当前活跃工作区（None 表示使用全局默认路径）。
    pub async fn current_workspace(&self) -> Option<Workspace> {
        self.workspace.current.read().await.clone()
    }

    /// 切换到指定工作区。
    ///
    /// 这会重新初始化 persistence 和 session manager 以使用工作区路径。
    pub async fn switch_workspace(
        self: &Arc<Self>,
        workspace: Workspace,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        self.run_owned_workspace_transition(WorkspaceTransitionRequest::Switch(workspace))
            .await
    }

    pub async fn exit_workspace(self: &Arc<Self>) -> anyhow::Result<WorkspaceTransitionReceipt> {
        self.run_owned_workspace_transition(WorkspaceTransitionRequest::Exit)
            .await
    }

    async fn run_owned_workspace_transition(
        self: &Arc<Self>,
        request: WorkspaceTransitionRequest,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let mut settlement = self.workspace.settlement.lock().await;
        if let Some(previous) = settlement.as_mut() {
            if let Err(error) = await_workspace_settlement(previous).await {
                tracing::warn!(
                    %error,
                    "previous detached workspace transition settled with an error"
                );
            }
            settlement.take();
        }

        let state = Arc::clone(self);
        *settlement = Some(tokio::spawn(async move {
            match request {
                WorkspaceTransitionRequest::Switch(workspace) => {
                    state.switch_workspace_inner(workspace).await
                }
                WorkspaceTransitionRequest::Exit => state.exit_workspace_inner().await,
            }
        }));
        let result = match settlement.as_mut() {
            Some(handle) => await_workspace_settlement(handle).await,
            None => Err(anyhow::anyhow!(
                "workspace settlement owner lost the accepted transition"
            )),
        };
        settlement.take();
        result
    }

    /// Await a detached workspace settlement before tearing down plugin,
    /// scheduler, watcher, MCP, or browser owners.
    pub async fn shutdown_workspace_transition(&self) -> anyhow::Result<()> {
        let mut settlement = self.workspace.settlement.lock().await;
        let result = match settlement.as_mut() {
            Some(handle) => await_workspace_settlement(handle).await.map(|_| ()),
            None => Ok(()),
        };
        settlement.take();
        result
    }

    async fn switch_workspace_inner(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let _transition = self.workspace.transition.lock().await;
        let foreground_transition = self.session.foreground_turns.suspend_admission_if_idle()?;
        let task_transition = match self.tasks.runtime.as_deref() {
            Some(runtime) => Some(runtime.begin_workspace_transition().await?),
            None => None,
        };
        ensure_no_running_task_runs(task_transition.as_ref())?;
        let root = validated_workspace_root(&workspace.root)?;
        let previous_workspace = self.workspace.current.read().await.clone();
        let previous_workspace_id = previous_workspace
            .as_ref()
            .map(|workspace| workspace.id.to_string());
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref() {
            plugin_runtime.preflight_workspace(root.clone()).await?;
        }
        if let Some(config_watcher) = self.config_watcher.as_ref() {
            config_watcher.preflight_workspace(&root)?;
        }
        let state_dir = crate::workspace::layout::WorkspaceLayout::state_dir(&root);
        let sessions_dir = crate::workspace::layout::WorkspaceLayout::sessions(&root);
        let tasks_dir = crate::workspace::layout::WorkspaceLayout::tasks(&root);
        std::fs::create_dir_all(&state_dir)?;
        std::fs::create_dir_all(&sessions_dir)?;
        std::fs::create_dir_all(&tasks_dir)?;
        let conversation_store: Arc<dyn echo_agent::memory::ConversationStore> = Arc::new(
            echo_agent::memory::FileConversationStore::new(&state_dir).map_err(|error| {
                anyhow::anyhow!("Failed to prepare workspace conversation store: {error}")
            })?,
        );
        let runtime_store = crate::infra::create_runtime_state_store_in(&sessions_dir)
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare workspace runtime state store"))?;
        let deletion_service = Arc::new(
            crate::conversation_deletion::ConversationDeletionService::new(
                state_dir.join("conversation-deletions"),
            ),
        );
        if let Err(error) = deletion_service
            .recover_committed_deletions(conversation_store.as_ref())
            .await
        {
            tracing::warn!(%error, "workspace conversation deletion recovery remains pending");
        }
        let memory_store = crate::infra::create_memory_store_for_workspace(&root)
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare workspace memory store"))?;
        // Reserve the memory/evolution generation before the process cwd or
        // any runtime projection crosses the workspace commit boundary. A
        // running review/Dreaming pass returns a deterministic Busy error.
        let mut memory_rebind_permit = match self.review_integration.as_ref() {
            Some(integration) => {
                if integration.has_pending_rule_projection() {
                    integration.initialize_rule_promotions().await?;
                }
                Some(
                    integration
                        .prepare_rebind(state_dir.clone(), memory_store.clone())?
                        .reconcile_rule_promotions()
                        .await?,
                )
            }
            None => None,
        };
        let mut pool_transition = match self.connection.pool.as_ref() {
            Some(pool) => Some(pool.preflight_workspace_transition().await?),
            None => None,
        };
        let mut workspace = workspace;
        workspace.root = root;

        // 切换进程工作目录到工作区根目录。
        // 这样所有工具（shell、文件读写、搜索等）都会自动在工作区目录下执行。
        std::env::set_current_dir(&workspace.root).map_err(|error| {
            anyhow::anyhow!(
                "Failed to switch process directory to {}: {error}",
                workspace.root.display()
            )
        })?;
        // The process cwd is the canonical commit boundary. Memory binding
        // publication is infallible after this point; incomplete old-root
        // trigger settlement is carried into the workspace receipt.
        let memory_rebind_receipt = memory_rebind_permit
            .as_mut()
            .map(crate::evolution::ReconciledReviewRebindPermit::commit);
        if let Some(pool_transition) = pool_transition.as_mut() {
            pool_transition.commit().await;
        }

        // 更新 agent 的 working_dir 配置（影响 project rules 注入等）
        let new_wd = Some(workspace.root.clone());
        let primary_root = workspace.root.clone();
        let artifact_config = crate::infra::tool_output_artifact_config(Some(&primary_root));
        self.storage
            .tool_executions
            .register_artifact_config(artifact_config.clone());
        self.connection
            .agent
            .write_async(|agent| {
                let artifact_config = artifact_config.clone();
                Box::pin(async move {
                    agent.set_working_dir(Some(primary_root.clone()));
                    agent.set_tool_output_artifacts(Some(artifact_config));
                    crate::infra::refresh_dynamic_context(agent, Some(&primary_root)).await;
                })
            })
            .await;
        // Propagate to all pooled agents so background tasks run in the new
        // workspace (P1-7).
        if let Some(ref pool) = self.connection.pool {
            pool.apply_working_dir(new_wd).await;
        }

        self.connection
            .agent
            .write(|agent| {
                agent.set_conversation_store(conversation_store.clone());
                agent.set_state_store(runtime_store.clone());
            })
            .await;
        if let Some(pool) = &self.connection.pool {
            pool.apply_conversation_store(conversation_store.clone())
                .await;
            pool.apply_state_store(runtime_store.clone()).await;
        }
        {
            let mut binding = self.storage.conversation.write().await;
            *binding = ConversationStorageBinding {
                store: Some(conversation_store),
                runtime_state: Some(runtime_store),
                deletions: deletion_service,
            };
        }

        // 重新初始化 memory store 到工作区的存储目录（物理隔离：动态记忆
        // 跟 workspace 走，不再共享全局 ~/.eko/store.json）。
        // hot 层 MEMORY.md 的 echo_agent_dir 与 warm 层 store.json 同根，
        // 都落在 {workspace.root}/.eko/，保证两层一致。
        let mem_root = workspace.root.clone();
        {
            let store = memory_store;
            let echo_agent_dir = crate::workspace::layout::WorkspaceLayout::state_dir(&mem_root); // {root}/.eko
            if let Some(receipt) = memory_rebind_receipt.as_ref() {
                let generation = receipt.generation;
                if receipt.is_degraded() {
                    tracing::warn!(
                        pending_old = receipt.pending_old,
                        delivery_error = ?receipt.delivery_error,
                        "Workspace memory generation published with pending old-root triggers"
                    );
                }
                tracing::info!(
                    generation,
                    "Published workspace memory evolution generation"
                );
            }
            // (a) 主 agent：替换 warm 层 store（重新注册 remember/recall/search_memory/
            //     forget 工具）+ 重建 hot 层 MemoryLayerManager。
            let store_for_mgr = store.clone();
            let layer_manager = self
                .review_integration
                .as_ref()
                .map(|integration| integration.create_layer_manager())
                .unwrap_or_else(|| {
                    echo_agent::evolution::MemoryRuntimeIntegrationBuilder::new(
                        echo_agent_dir.clone(),
                        store_for_mgr.clone(),
                    )
                    .build_layer_manager()
                })?;
            self.connection
                .agent
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_store(store_for_mgr.clone()).await;
                        a.install_memory_layer_manager(std::sync::Arc::new(layer_manager));
                    })
                })
                .await;
            // (b) ReviewIntegration：rebind 到新 dir/store（后续 /memory-review、
            //     dreaming、session-end 都用新 workspace 的记忆）。
            if let Some(ref ri) = self.review_integration {
                let curator = ri.curator();
                self.connection
                    .agent
                    .write_async(|agent| {
                        Box::pin(async move {
                            agent.set_skill_curator(Some(curator));
                            agent.reconcile_skill_load_policy().await;
                        })
                    })
                    .await;
                let workspace_skills = echo_agent_dir.join("skills");
                if workspace_skills.is_dir() {
                    self.connection
                        .agent
                        .write_async(|agent| {
                            Box::pin(async move {
                                if let Err(error) =
                                    agent.load_skills_from_dir(workspace_skills).await
                                {
                                    tracing::warn!(
                                        %error,
                                        "Failed to reload workspace-curated skills"
                                    );
                                }
                            })
                        })
                        .await;
                }
            }
            tracing::info!(
                workspace = %workspace.id,
                dir = %echo_agent_dir.display(),
                "Switched memory store to workspace"
            );
            // (c) 池 agent 同步重载（仿 apply_working_dir pattern）。
            if let Some(ref pool) = self.connection.pool {
                pool.apply_memory_store(&mem_root).await;
            }
        }
        let rule_projection_error =
            if let Some(review_integration) = self.review_integration.as_ref() {
                review_integration
                    .settle_rebind_rule_promotions(pool_transition.as_ref())
                    .await
                    .err()
            } else {
                None
            };

        tracing::info!(
            workspace = %workspace.id,
            root = %workspace.root.display(),
            "Switched to workspace"
        );

        // 根据工作区类型配置 Agent（自动激活 Skills 和注入系统提示词）
        self.apply_workspace_routing(&workspace).await;

        let mut degraded_subsystems = Vec::new();
        if let Some(receipt) = memory_rebind_receipt.as_ref()
            && receipt.is_degraded()
        {
            degraded_subsystems.push(memory_rebind_degradation(state_dir.clone(), receipt));
        }
        if let Some(error) = rule_projection_error {
            degraded_subsystems.push(memory_rule_projection_degradation(state_dir.clone(), error));
        }
        if let Some(task_transition) = task_transition.as_ref()
            && let Err(error) =
                task_transition.rebind_shadow_root(tasks_dir.clone(), workspace.id.to_string())
        {
            let settled_root = task_transition.active_shadow_root();
            degraded_subsystems.push(WorkspaceSubsystemTransition {
                subsystem: "task_runtime".to_string(),
                target_root: tasks_dir.clone(),
                stale_roots: (settled_root != tasks_dir)
                    .then_some(settled_root)
                    .into_iter()
                    .collect(),
                error: error.to_string(),
            });
        }
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = plugin_runtime
                .rebind_workspace(workspace.root.clone())
                .await
        {
            let settled_root = plugin_runtime.workspace_root().await;
            let mut stale_roots = plugin_runtime.cleanup_debt_roots().await;
            if settled_root != workspace.root {
                stale_roots.push(settled_root);
            }
            stale_roots.sort();
            stale_roots.dedup();
            degraded_subsystems.push(WorkspaceSubsystemTransition {
                subsystem: "plugin_runtime".to_string(),
                target_root: workspace.root.clone(),
                stale_roots,
                error: error.to_string(),
            });
        }
        if let Some(config_watcher) = self.config_watcher.as_ref() {
            match config_watcher
                .rebind_workspace(workspace.root.clone())
                .await
            {
                Ok(watcher_receipt) if !watcher_receipt.errors.is_empty() => {
                    degraded_subsystems.push(WorkspaceSubsystemTransition {
                        subsystem: "config_watcher".to_string(),
                        target_root: watcher_receipt.settled_root,
                        stale_roots: watcher_receipt.stale_watch_roots,
                        error: watcher_receipt.errors.join("; "),
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    let settled_root = config_watcher.settled_root().await;
                    degraded_subsystems.push(WorkspaceSubsystemTransition {
                        subsystem: "config_watcher".to_string(),
                        target_root: workspace.root.clone(),
                        stale_roots: (settled_root != workspace.root)
                            .then_some(settled_root)
                            .into_iter()
                            .collect(),
                        error: error.to_string(),
                    });
                }
            }
        }

        {
            let mut current = self.workspace.current.write().await;
            *current = Some(workspace.clone());
        }
        let receipt = WorkspaceTransitionReceipt::committed(
            previous_workspace_id,
            Some(workspace.id.to_string()),
            workspace.root.clone(),
            degraded_subsystems,
        );
        *self.workspace.last_transition.write().await = Some(receipt.clone());
        drop(pool_transition);
        drop(memory_rebind_permit);
        drop(task_transition);
        drop(foreground_transition);

        Ok(receipt)
    }

    /// 应用工作区路由配置（根据 WorkspaceKind 激活 Skills 和注入系统提示词）
    async fn apply_workspace_routing(&self, workspace: &Workspace) {
        let kind = workspace.kind.clone();
        let primary_kind = kind.clone();
        self.connection
            .agent
            .write_async(|agent| {
                Box::pin(async move {
                    crate::workspace_routing::configure_agent_for_workspace(agent, &primary_kind)
                        .await;
                })
            })
            .await;
        if let Some(ref pool) = self.connection.pool {
            pool.apply_workspace_routing(kind).await;
        }
    }

    /// 退出工作区（回到全局默认路径）。
    async fn exit_workspace_inner(&self) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let _transition = self.workspace.transition.lock().await;
        let foreground_transition = self.session.foreground_turns.suspend_admission_if_idle()?;
        let task_transition = match self.tasks.runtime.as_deref() {
            Some(runtime) => Some(runtime.begin_workspace_transition().await?),
            None => None,
        };
        ensure_no_running_task_runs(task_transition.as_ref())?;
        let global_cwd = self.workspace.global_cwd.canonicalize().map_err(|error| {
            anyhow::anyhow!("Failed to resolve the global working directory: {error}")
        })?;
        let previous_workspace = self.workspace.current.read().await.clone();
        let previous_workspace_id = previous_workspace
            .as_ref()
            .map(|workspace| workspace.id.to_string());
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref() {
            plugin_runtime
                .preflight_workspace(global_cwd.clone())
                .await?;
        }
        if let Some(config_watcher) = self.config_watcher.as_ref() {
            config_watcher.preflight_workspace(&global_cwd)?;
        }
        let conversation_store = crate::infra::create_conversation_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global conversation store"))?;
        let runtime_store = crate::infra::create_runtime_state_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global runtime state store"))?;
        let deletion_service =
            Arc::new(crate::conversation_deletion::ConversationDeletionService::at_default_root());
        if let Err(error) = deletion_service
            .recover_committed_deletions(conversation_store.as_ref())
            .await
        {
            tracing::warn!(%error, "global conversation deletion recovery remains pending");
        }
        let memory_store = crate::infra::create_global_memory_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global memory store"))?;
        let (_, prepared_global_echo_dir) = crate::infra::global_memory_paths();
        let mut memory_rebind_permit = match self.review_integration.as_ref() {
            Some(integration) => {
                if integration.has_pending_rule_projection() {
                    integration.initialize_rule_promotions().await?;
                }
                Some(
                    integration
                        .prepare_rebind(prepared_global_echo_dir.clone(), memory_store.clone())?
                        .reconcile_rule_promotions()
                        .await?,
                )
            }
            None => None,
        };
        let mut pool_transition = match self.connection.pool.as_ref() {
            Some(pool) => Some(pool.preflight_workspace_transition().await?),
            None => None,
        };
        let global_tasks_dir =
            crate::tasks::task_runtime::file_shadow::FileTaskShadow::default_root();
        std::fs::create_dir_all(&global_tasks_dir)?;
        std::env::set_current_dir(&global_cwd).map_err(|error| {
            anyhow::anyhow!("Failed to restore process directory after workspace exit: {error}")
        })?;
        let memory_rebind_receipt = memory_rebind_permit
            .as_mut()
            .map(crate::evolution::ReconciledReviewRebindPermit::commit);
        if let Some(pool_transition) = pool_transition.as_mut() {
            pool_transition.commit().await;
        }

        let artifact_config = crate::infra::tool_output_artifact_config(None);
        self.storage
            .tool_executions
            .register_artifact_config(artifact_config.clone());
        self.connection
            .agent
            .write_async(|agent| {
                let artifact_config = artifact_config.clone();
                Box::pin(async move {
                    agent.set_working_dir(None);
                    agent.set_tool_output_artifacts(Some(artifact_config));
                    crate::infra::refresh_dynamic_context(agent, None).await;
                })
            })
            .await;

        self.connection
            .agent
            .write(|agent| {
                agent.set_conversation_store(conversation_store.clone());
                agent.set_state_store(runtime_store.clone());
            })
            .await;
        if let Some(pool) = &self.connection.pool {
            pool.apply_conversation_store(conversation_store.clone())
                .await;
            pool.apply_state_store(runtime_store.clone()).await;
        }
        {
            let mut binding = self.storage.conversation.write().await;
            *binding = ConversationStorageBinding {
                store: Some(conversation_store),
                runtime_state: Some(runtime_store),
                deletions: deletion_service,
            };
        }

        // 重置 memory store 到全局默认路径（~/.eko/store.json）。
        // 与 switch_workspace 的 memory 重载对称：exit 后动态记忆回到全局 store，
        // 不再读已退出 workspace 的 .eko/memory/。
        {
            let store = memory_store;
            let (global_store_path, global_echo_dir) = crate::infra::global_memory_paths();
            if let Some(receipt) = memory_rebind_receipt.as_ref() {
                let generation = receipt.generation;
                if receipt.is_degraded() {
                    tracing::warn!(
                        pending_old = receipt.pending_old,
                        delivery_error = ?receipt.delivery_error,
                        "Global memory generation published with pending workspace triggers"
                    );
                }
                tracing::info!(generation, "Published global memory evolution generation");
            }
            // 主 agent：替换 store + 重建 layer manager。
            let store_for_mgr = store.clone();
            let layer_manager = self
                .review_integration
                .as_ref()
                .map(|integration| integration.create_layer_manager())
                .unwrap_or_else(|| {
                    echo_agent::evolution::MemoryRuntimeIntegrationBuilder::new(
                        global_echo_dir.clone(),
                        store_for_mgr.clone(),
                    )
                    .build_layer_manager()
                })?;
            self.connection
                .agent
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_store(store_for_mgr.clone()).await;
                        a.install_memory_layer_manager(std::sync::Arc::new(layer_manager));
                    })
                })
                .await;
            if let Some(ref ri) = self.review_integration {
                let curator = ri.curator();
                self.connection
                    .agent
                    .write_async(|agent| {
                        Box::pin(async move {
                            agent.set_skill_curator(Some(curator));
                            agent.reconcile_skill_load_policy().await;
                        })
                    })
                    .await;
                let global_skills = global_echo_dir.join("skills");
                if global_skills.is_dir() {
                    self.connection
                        .agent
                        .write_async(|agent| {
                            Box::pin(async move {
                                if let Err(error) = agent.load_skills_from_dir(global_skills).await
                                {
                                    tracing::warn!(
                                        %error,
                                        "Failed to reload global curated skills"
                                    );
                                }
                            })
                        })
                        .await;
                }
            }
            tracing::info!(
                path = %global_store_path.display(),
                "Memory store reset to global"
            );
            if let Some(ref pool) = self.connection.pool {
                pool.apply_memory_store_global().await;
            }
        }

        // Reset pooled agents' working_dir so background tasks don't keep
        // running in the exited workspace (P1 — exit_workspace pool reset).
        if let Some(ref pool) = self.connection.pool {
            pool.apply_working_dir(None).await;
        }
        let rule_projection_error =
            if let Some(review_integration) = self.review_integration.as_ref() {
                review_integration
                    .settle_rebind_rule_promotions(pool_transition.as_ref())
                    .await
                    .err()
            } else {
                None
            };

        let general = crate::workspace::WorkspaceKind::General;
        self.connection
            .agent
            .write_async(|agent| {
                Box::pin(async move {
                    crate::workspace_routing::configure_agent_for_workspace(agent, &general).await;
                })
            })
            .await;
        if let Some(ref pool) = self.connection.pool {
            pool.apply_workspace_routing(crate::workspace::WorkspaceKind::General)
                .await;
        }

        let mut degraded_subsystems = Vec::new();
        if let Some(receipt) = memory_rebind_receipt.as_ref()
            && receipt.is_degraded()
        {
            degraded_subsystems.push(memory_rebind_degradation(
                prepared_global_echo_dir.clone(),
                receipt,
            ));
        }
        if let Some(error) = rule_projection_error {
            degraded_subsystems.push(memory_rule_projection_degradation(
                prepared_global_echo_dir,
                error,
            ));
        }
        if let Some(task_transition) = task_transition.as_ref()
            && let Err(error) =
                task_transition.rebind_shadow_root(global_tasks_dir.clone(), "global")
        {
            let settled_root = task_transition.active_shadow_root();
            degraded_subsystems.push(WorkspaceSubsystemTransition {
                subsystem: "task_runtime".to_string(),
                target_root: global_tasks_dir.clone(),
                stale_roots: (settled_root != global_tasks_dir)
                    .then_some(settled_root)
                    .into_iter()
                    .collect(),
                error: error.to_string(),
            });
        }
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = plugin_runtime.rebind_workspace(global_cwd.clone()).await
        {
            let settled_root = plugin_runtime.workspace_root().await;
            let mut stale_roots = plugin_runtime.cleanup_debt_roots().await;
            if settled_root != global_cwd {
                stale_roots.push(settled_root);
            }
            stale_roots.sort();
            stale_roots.dedup();
            degraded_subsystems.push(WorkspaceSubsystemTransition {
                subsystem: "plugin_runtime".to_string(),
                target_root: global_cwd.clone(),
                stale_roots,
                error: error.to_string(),
            });
        }
        if let Some(config_watcher) = self.config_watcher.as_ref() {
            match config_watcher.rebind_workspace(global_cwd.clone()).await {
                Ok(watcher_receipt) if !watcher_receipt.errors.is_empty() => {
                    degraded_subsystems.push(WorkspaceSubsystemTransition {
                        subsystem: "config_watcher".to_string(),
                        target_root: watcher_receipt.settled_root,
                        stale_roots: watcher_receipt.stale_watch_roots,
                        error: watcher_receipt.errors.join("; "),
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    let settled_root = config_watcher.settled_root().await;
                    degraded_subsystems.push(WorkspaceSubsystemTransition {
                        subsystem: "config_watcher".to_string(),
                        target_root: global_cwd.clone(),
                        stale_roots: (settled_root != global_cwd)
                            .then_some(settled_root)
                            .into_iter()
                            .collect(),
                        error: error.to_string(),
                    });
                }
            }
        }

        {
            let mut current = self.workspace.current.write().await;
            *current = None;
        }
        let receipt = WorkspaceTransitionReceipt::committed(
            previous_workspace_id,
            None,
            global_cwd,
            degraded_subsystems,
        );
        *self.workspace.last_transition.write().await = Some(receipt.clone());
        drop(pool_transition);
        drop(memory_rebind_permit);
        drop(task_transition);
        drop(foreground_transition);

        tracing::info!("Exited workspace, using global default paths");
        Ok(receipt)
    }
}

fn prepare_model_mutation(
    current: &echo_agent::config::AppConfig,
    active_model_id: &str,
    request: ModelMutationRequest,
) -> Result<PreparedModelMutation, ModelMutationError> {
    match request {
        ModelMutationRequest::UpsertModel(mutation) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let previous_default = current.model.default_model_id.clone();
            let model_id =
                crate::model_config::upsert_configured_model(&mut config, mutation.model)
                    .map_err(ModelMutationError::Validation)?;
            let became_first_default = previous_default.is_none()
                && config.model.default_model_id.as_deref() == Some(model_id.as_str());
            let updates_persisted_default = mutation.set_default
                || previous_default.as_deref() == Some(model_id.as_str())
                || became_first_default;
            if updates_persisted_default {
                crate::model_config::set_default_model(&mut config, &model_id)
                    .map_err(ModelMutationError::Validation)?;
            }
            let updates_active_model = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.id == model_id);
            let activates_upserted_model =
                mutation.set_default || updates_active_model || became_first_default;
            let runtime = crate::model_config::resolve_runtime_model(&config, Some(&model_id));
            let prepared = crate::infra::prepare_runtime_llm(&runtime)
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id,
                runtime: Some(runtime),
                prepared: Some(prepared),
                activated: activates_upserted_model,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::UpsertProvider(mutation) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let mut provider = mutation.provider;
            if mutation.preserve_auth_token && provider.auth_token.is_none() {
                provider.auth_token = current
                    .model_providers
                    .get(&mutation.id)
                    .and_then(|current| current.auth_token.clone());
            }
            let provider_id =
                crate::model_config::upsert_model_provider(&mut config, &mutation.id, provider)
                    .map_err(ModelMutationError::Validation)?;
            let activated = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.provider == provider_id);
            let runtime = if activated {
                let active_id = active_before
                    .as_ref()
                    .map(|runtime| runtime.id.as_str())
                    .unwrap_or_default();
                resolve_active_model_runtime(&config, active_id)?
            } else {
                None
            };
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: provider_id,
                runtime,
                prepared,
                activated,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::SetDefault(selector) => {
            let selected =
                crate::model_config::resolve_runtime_model_selector(current, Some(&selector))
                    .map_err(|error| ModelMutationError::Validation(error.to_string()))?;
            let mut config = current.clone();
            let runtime = crate::model_config::set_default_model(&mut config, &selected.id)
                .map_err(ModelMutationError::Validation)?;
            let prepared = crate::infra::prepare_runtime_llm(&runtime)
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: runtime.id.clone(),
                runtime: Some(runtime),
                prepared: Some(prepared),
                activated: true,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::DeleteModel(model_id) => {
            let mut config = current.clone();
            match crate::model_config::delete_configured_model(&mut config, &model_id)
                .map_err(ModelMutationError::Validation)?
            {
                crate::model_config::DeleteConfiguredModelOutcome::RemovedNonDefault => {
                    if active_model_id == model_id {
                        let runtime = resolve_active_model_runtime(&config, active_model_id)?
                            .ok_or_else(|| {
                                ModelMutationError::Validation(
                                    "Deleted active model has no enabled successor".to_string(),
                                )
                            })?;
                        let prepared = crate::infra::prepare_runtime_llm(&runtime)
                            .map_err(ModelMutationError::Validation)?;
                        Ok(PreparedModelMutation {
                            config,
                            model_id,
                            runtime: Some(runtime),
                            prepared: Some(prepared),
                            activated: true,
                            deactivated: false,
                            deleted: true,
                        })
                    } else {
                        Ok(PreparedModelMutation {
                            config,
                            model_id,
                            runtime: None,
                            prepared: None,
                            activated: false,
                            deactivated: false,
                            deleted: true,
                        })
                    }
                }
                crate::model_config::DeleteConfiguredModelOutcome::ActivatedSuccessor(runtime) => {
                    let prepared = crate::infra::prepare_runtime_llm(&runtime)
                        .map_err(ModelMutationError::Validation)?;
                    Ok(PreparedModelMutation {
                        config,
                        model_id,
                        runtime: Some(*runtime),
                        prepared: Some(prepared),
                        activated: true,
                        deactivated: false,
                        deleted: true,
                    })
                }
                crate::model_config::DeleteConfiguredModelOutcome::Deactivated => {
                    Ok(PreparedModelMutation {
                        config,
                        model_id,
                        runtime: None,
                        prepared: None,
                        activated: false,
                        deactivated: true,
                        deleted: true,
                    })
                }
            }
        }
        ModelMutationRequest::DeleteProvider(provider_id) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let removes_active_model = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.provider == provider_id);
            crate::model_config::delete_model_provider(&mut config, &provider_id)
                .map_err(ModelMutationError::Validation)?;
            let runtime = if removes_active_model {
                resolve_active_model_runtime(&config, active_model_id)?
            } else {
                None
            };
            if removes_active_model && runtime.is_none() {
                crate::model_config::clear_selected_model(&mut config);
            }
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: provider_id,
                activated: runtime.is_some(),
                deactivated: removes_active_model && runtime.is_none(),
                runtime,
                prepared,
                deleted: true,
            })
        }
        ModelMutationRequest::UpdateConfig {
            update,
            reapply_active_model,
        } => {
            let mut config = current.clone();
            update(&mut config).map_err(ModelMutationError::Validation)?;
            let runtime = if reapply_active_model {
                resolve_active_model_runtime(&config, active_model_id)?
            } else {
                None
            };
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            let model_id = runtime
                .as_ref()
                .map(|runtime| runtime.id.clone())
                .or_else(|| config.model.default_model_id.clone())
                .unwrap_or_default();
            Ok(PreparedModelMutation {
                config,
                model_id,
                runtime,
                prepared,
                activated: reapply_active_model,
                deactivated: false,
                deleted: false,
            })
        }
        #[cfg(test)]
        ModelMutationRequest::AbortSettlementForTest => Err(ModelMutationError::Settlement(
            "test-only aborted settlement reached mutation preparation".to_string(),
        )),
    }
}

fn resolve_active_model_runtime(
    config: &echo_agent::config::AppConfig,
    active_model_id: &str,
) -> Result<Option<crate::model_config::ModelRuntimeConfig>, ModelMutationError> {
    if !config.configured_models.iter().any(|model| model.enabled) {
        return Ok(None);
    }
    let active_is_available = config
        .configured_models
        .iter()
        .any(|model| model.id == active_model_id && model.enabled);
    let selector = if active_is_available || config.configured_models.is_empty() {
        Some(active_model_id)
    } else {
        config.model.default_model_id.as_deref()
    };
    crate::model_config::resolve_runtime_model_selector(config, selector)
        .map(Some)
        .map_err(|error| ModelMutationError::Validation(error.to_string()))
}

#[cfg(test)]
mod model_mutation_tests {
    use super::*;
    use echo_agent::config::{ConfiguredModel, ModelProviderConfig};
    use echo_agent::llm::LlmApiProtocol;

    const MODEL_A: &str = "model-a";
    const MODEL_B: &str = "model-b";
    const ENDPOINT_A: &str = "http://127.0.0.1:11434/v1/chat/completions";
    const ENDPOINT_B: &str = "http://127.0.0.1:11435/v1/chat/completions";
    const ENDPOINT_C: &str = "http://127.0.0.1:11436/v1/chat/completions";
    const RESPONSES_ENDPOINT: &str = "http://127.0.0.1:11435/v1/responses";
    const WINDOW_A: usize = 120_000;
    const WINDOW_B: usize = 240_000;

    #[test]
    fn first_configured_model_becomes_the_active_generation() -> Result<(), String> {
        let mut config = echo_agent::config::AppConfig::default();
        config.model_providers.insert(
            "local".to_string(),
            ModelProviderConfig {
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        let mutation = prepare_model_mutation(
            &config,
            "",
            ModelMutationRequest::UpsertModel(ConfiguredModelMutation {
                model: model(MODEL_A, "local", "runtime-a", WINDOW_A as u32),
                set_default: false,
            }),
        )
        .map_err(|error| error.to_string())?;

        assert!(mutation.activated);
        assert_eq!(mutation.model_id, MODEL_A);
        assert_eq!(
            mutation.config.model.default_model_id.as_deref(),
            Some(MODEL_A)
        );
        assert_eq!(
            mutation.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        assert!(mutation.prepared.is_some());
        Ok(())
    }

    struct ModelMutationFixture {
        _temp: tempfile::TempDir,
        config_path: std::path::PathBuf,
        state: Arc<AppState>,
        pool: Arc<crate::agent_pool::AgentPool>,
        existing: AgentHandle,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AgentModelProjection {
        model: String,
        client_model: String,
        base_url: String,
        api_protocol: LlmApiProtocol,
        token_limit: usize,
    }

    fn model(id: &str, provider: &str, model: &str, context_window: u32) -> ConfiguredModel {
        ConfiguredModel {
            id: id.to_string(),
            display_name: id.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            enabled: true,
            context_window: Some(context_window),
            ..ConfiguredModel::default()
        }
    }

    fn provider_mutation(id: &str, base_url: &str) -> ModelProviderMutation {
        ModelProviderMutation {
            id: id.to_string(),
            provider: ModelProviderConfig {
                name: id.to_string(),
                base_url: Some(base_url.to_string()),
                default_api_protocol: Some(LlmApiProtocol::ChatCompletions),
                ..Default::default()
            },
            preserve_auth_token: false,
        }
    }

    fn valid_config() -> Result<echo_agent::config::AppConfig, String> {
        let mut config = echo_agent::config::AppConfig::default();
        config.model_providers.insert(
            "local-a".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        config.model_providers.insert(
            "local-b".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_B.to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            model(MODEL_A, "local-a", "runtime-a", WINDOW_A as u32),
            model(MODEL_B, "local-b", "runtime-b", WINDOW_B as u32),
        ];
        crate::model_config::set_default_model(&mut config, MODEL_A)?;
        Ok(config)
    }

    fn invalid_successor_config() -> Result<echo_agent::config::AppConfig, String> {
        let mut config = valid_config()?;
        let invalid = config
            .configured_models
            .iter_mut()
            .find(|model| model.id == MODEL_B)
            .ok_or_else(|| "missing invalid successor candidate".to_string())?;
        invalid.provider = "openai".to_string();
        invalid.api_protocol = LlmApiProtocol::Responses;
        config.model_providers.insert(
            "openai".to_string(),
            ModelProviderConfig {
                auth_token: Some("invalid\nheader".to_string()),
                base_url: Some("https://api.openai.com/v1/responses".to_string()),
                ..Default::default()
            },
        );
        Ok(config)
    }

    fn shared_provider_config() -> Result<echo_agent::config::AppConfig, String> {
        let mut config = echo_agent::config::AppConfig::default();
        config.model_providers.insert(
            "local-shared".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            model(MODEL_A, "local-shared", "runtime-a", WINDOW_A as u32),
            model(MODEL_B, "local-shared", "runtime-b", WINDOW_B as u32),
        ];
        crate::model_config::set_default_model(&mut config, MODEL_A)?;
        Ok(config)
    }

    async fn fixture(
        config: echo_agent::config::AppConfig,
        persistence_fails: bool,
    ) -> Result<ModelMutationFixture, String> {
        fixture_with_active(config, persistence_fails, MODEL_A).await
    }

    async fn fixture_with_active(
        config: echo_agent::config::AppConfig,
        persistence_fails: bool,
        active_model_id: &str,
    ) -> Result<ModelMutationFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = if persistence_fails {
            let path = temp.path().join("config-as-directory");
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            path
        } else {
            let path = temp.path().join("echo-agent.yaml");
            echo_agent::config::save_config_file(&path, &config)?;
            path
        };
        let created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                model: Some(active_model_id.to_string()),
                system_prompt: Some("model mutation test".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;
        let active_runtime = created
            .runtime_model
            .ok_or_else(|| "model mutation fixture did not resolve its active model".to_string())?;
        let primary_consumers = created.model_consumers;
        let primary = AgentHandle::new(created.agent);
        let session_config =
            crate::model_config::session_config_for_runtime(&config, &active_runtime)?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::for_model_mutation_test(&primary, session_config).await,
        );
        let existing_lease = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            primary,
            Some(primary_consumers),
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            config,
            mcp_runtime,
        )
        .with_active_model_id(active_runtime.id)
        .with_config_path(config_path.clone());
        state.set_pool(pool.clone());
        Ok(ModelMutationFixture {
            _temp: temp,
            config_path,
            state: Arc::new(state),
            pool,
            existing,
        })
    }

    async fn agent_projection(handle: &AgentHandle) -> Result<AgentModelProjection, String> {
        handle
            .read(|agent| {
                let llm = agent
                    .llm_config()
                    .ok_or_else(|| "agent has no LLM config".to_string())?;
                Ok(AgentModelProjection {
                    model: llm.model.clone(),
                    client_model: agent
                        .llm_client()
                        .map(|client| client.model_name().to_string())
                        .ok_or_else(|| "agent has no prepared LLM client".to_string())?,
                    base_url: llm.base_url.clone(),
                    api_protocol: llm.api_protocol,
                    token_limit: agent.config().get_token_limit(),
                })
            })
            .await
    }

    async fn assert_live_generation(
        fixture: &ModelMutationFixture,
        model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert_eq!(snapshot.model.default_model_id.as_deref(), Some(model_id));
        drop(snapshot);
        let expected = AgentModelProjection {
            model: runtime_model.to_string(),
            client_model: runtime_model.to_string(),
            base_url: endpoint.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            token_limit: context_window,
        };
        assert_eq!(
            agent_projection(&fixture.state.connection.agent).await?,
            expected
        );
        assert_eq!(agent_projection(&fixture.existing).await?, expected);
        let new_lease = fixture
            .pool
            .acquire("new-after-mutation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(agent_projection(&new_lease.agent()).await?, expected);
        drop(new_lease);
        Ok(())
    }

    async fn assert_no_live_generation(fixture: &ModelMutationFixture) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert!(snapshot.model.default_model_id.is_none());
        assert!(snapshot.configured_models.is_empty());
        drop(snapshot);
        assert!(fixture.state.config.active_model_id.read().await.is_empty());

        for handle in [
            fixture.state.connection.agent.clone(),
            fixture.existing.clone(),
            inherited_handle(fixture)?,
        ] {
            let projection = handle
                .read(|agent| {
                    (
                        agent.model_name().to_string(),
                        agent.llm_config().is_none(),
                        agent.llm_client().is_none(),
                    )
                })
                .await;
            assert!(projection.0.is_empty());
            assert!(projection.1);
            assert!(projection.2);
        }

        let new_lease = fixture
            .pool
            .acquire("new-after-model-deactivation")
            .await
            .map_err(|error| error.to_string())?;
        let new_projection = new_lease
            .agent()
            .read(|agent| {
                (
                    agent.model_name().to_string(),
                    agent.llm_config().is_none(),
                    agent.llm_client().is_none(),
                )
            })
            .await;
        assert!(new_projection.0.is_empty());
        assert!(new_projection.1);
        assert!(new_projection.2);
        drop(new_lease);
        Ok(())
    }

    async fn assert_session_generation(
        fixture: &ModelMutationFixture,
        durable_default_id: &str,
        active_model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert_eq!(
            snapshot.model.default_model_id.as_deref(),
            Some(durable_default_id)
        );
        drop(snapshot);
        assert_eq!(
            fixture.state.config.active_model_id.read().await.as_str(),
            active_model_id
        );
        let expected = AgentModelProjection {
            model: runtime_model.to_string(),
            client_model: runtime_model.to_string(),
            base_url: endpoint.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            token_limit: context_window,
        };
        assert_eq!(
            agent_projection(&fixture.state.connection.agent).await?,
            expected
        );
        assert_eq!(agent_projection(&fixture.existing).await?, expected);
        let new_lease = fixture
            .pool
            .acquire("new-after-session-mutation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(agent_projection(&new_lease.agent()).await?, expected);
        drop(new_lease);
        Ok(())
    }

    async fn assert_full_generation(
        fixture: &ModelMutationFixture,
        model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(model_id));
        assert_live_generation(fixture, model_id, runtime_model, endpoint, context_window).await
    }

    async fn wait_for_pool_model_admission(
        pool: &crate::agent_pool::AgentPool,
    ) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if pool.transition_admission_closed_for_test() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "timed out waiting for pool model admission".to_string())?;
        Ok(())
    }

    async fn join_mutation(
        handle: tokio::task::JoinHandle<Result<ModelMutationReceipt, ModelMutationError>>,
    ) -> Result<ModelMutationReceipt, String> {
        let joined = tokio::time::timeout(std::time::Duration::from_secs(3), handle)
            .await
            .map_err(|_| "model mutation task timed out".to_string())?;
        let settled = joined.map_err(|error| error.to_string())?;
        settled.map_err(|error| error.to_string())
    }

    async fn invalidate_model_budget(handle: &AgentHandle) {
        let invalid_budget =
            echo_agent::workspace::core::budget::TokenBudgetConfig::enabled().with_total_window(0);
        handle
            .write(|agent| {
                let config = agent.config();
                let model = config.get_model_name().to_string();
                let name = config.get_agent_name().to_string();
                let prompt = config.get_system_prompt().to_string();
                let token_limit = config.get_token_limit();
                *agent.config_mut() = echo_agent::agent::AgentConfig::new(&model, &name, &prompt)
                    .token_limit(token_limit)
                    .token_budget(invalid_budget);
            })
            .await;
    }

    fn inherited_handle(fixture: &ModelMutationFixture) -> Result<AgentHandle, String> {
        fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())
    }

    #[tokio::test]
    async fn gui_and_tui_model_mutations_share_one_linearized_owner() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let primary = fixture.state.connection.agent.inner().clone();
        let barrier = primary.write().await;
        let gui_state = fixture.state.clone();
        let gui = tokio::spawn(async move { gui_state.set_default_model_owned(MODEL_B).await });
        wait_for_pool_model_admission(&fixture.pool).await?;

        let tui_state = fixture.state.clone();
        let tui = tokio::spawn(async move { tui_state.set_default_model_owned(MODEL_A).await });
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model
                .default_model_id
                .as_deref(),
            Some(MODEL_A)
        );
        drop(barrier);

        let gui_receipt = join_mutation(gui).await?;
        let tui_receipt = join_mutation(tui).await?;
        assert_eq!(gui_receipt.model_id, MODEL_B);
        assert_eq!(tui_receipt.model_id, MODEL_A);
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn aborted_model_mutation_waiter_does_not_cancel_accepted_settlement()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let primary = fixture.state.connection.agent.inner().clone();
        let barrier = primary.write().await;
        let caller_state = fixture.state.clone();
        let caller =
            tokio::spawn(async move { caller_state.set_default_model_owned(MODEL_B).await });
        wait_for_pool_model_admission(&fixture.pool).await?;
        caller.abort();
        assert!(caller.await.is_err());
        drop(barrier);

        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            fixture.state.shutdown_model_mutations(),
        )
        .await
        .map_err(|_| "model mutation shutdown timed out".to_string())?
        .map_err(|error| error.to_string())?;
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn invalid_default_successor_delete_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(invalid_successor_config()?, false).await?;

        let result = fixture.state.delete_configured_model_owned(MODEL_A).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.configured_models.len(), 2);
        assert!(
            persisted
                .configured_models
                .iter()
                .any(|model| model.id == MODEL_A)
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn valid_default_successor_delete_settles_all_layers() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_A)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(receipt.activated);
        assert!(
            receipt
                .config
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_A)
        );
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_A)
        );
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn omitted_subagent_model_tracks_parent_while_explicit_model_stays_fixed()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let registry = fixture
            .state
            .connection
            .agent
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let inherited_before = registry
            .get_agent("general-purpose")
            .await
            .ok_or_else(|| "inherit-parent subagent was not registered".to_string())?;
        let fixed_before = registry
            .get_agent("explorer")
            .await
            .ok_or_else(|| "explicit-model subagent was not registered".to_string())?;
        let inherited_handle = fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())?;
        let fixed_model = fixed_before.model_name().to_string();
        assert_eq!(inherited_before.model_name(), "runtime-a");

        fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        let inherited_after = registry
            .get_agent("general-purpose")
            .await
            .ok_or_else(|| "inherit-parent subagent was not refreshed".to_string())?;
        let fixed_after = registry
            .get_agent("explorer")
            .await
            .ok_or_else(|| "explicit-model subagent disappeared".to_string())?;
        assert_eq!(inherited_after.model_name(), "runtime-b");
        assert_eq!(fixed_after.model_name(), fixed_model);
        assert_eq!(
            agent_projection(&inherited_handle).await?,
            AgentModelProjection {
                model: "runtime-b".to_string(),
                client_model: "runtime-b".to_string(),
                base_url: ENDPOINT_B.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_B,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn model_mutation_preserves_primary_custom_critic() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let custom = Arc::new(echo_agent::agent::critic::StaticCritic::always_pass());
        fixture
            .state
            .connection
            .agent
            .write(|agent| agent.set_critic(custom.clone()))
            .await;

        fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(Arc::strong_count(&custom), 2);
        assert_eq!(
            fixture
                .state
                .connection
                .agent
                .read(|agent| agent.critic_owner().map(str::to_string))
                .await,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_last_default_deactivates_every_model_consumer() -> Result<(), String> {
        let mut config = valid_config()?;
        config.configured_models.retain(|model| model.id == MODEL_A);
        let fixture = fixture(config, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_A)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.runtime.is_none());
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert!(persisted.configured_models.is_empty());
        assert!(persisted.model.default_model_id.is_none());
        assert_no_live_generation(&fixture).await
    }

    #[tokio::test]
    async fn deleting_provider_cascades_its_models_and_deactivates_every_consumer()
    -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_model_provider_owned("local-shared")
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.config.model_providers.is_empty());
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert!(persisted.model_providers.is_empty());
        assert_no_live_generation(&fixture).await
    }

    #[tokio::test]
    async fn persistence_failure_rolls_back_snapshot_primary_and_pool() -> Result<(), String> {
        let fixture = fixture(valid_config()?, true).await?;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Persistence(_))));
        assert!(fixture.config_path.is_dir());
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn failed_settlement_is_stable_for_later_mutations_and_repeated_shutdown()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, true).await?;
        let first = fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .err()
            .ok_or_else(|| "persistence unexpectedly succeeded".to_string())?
            .to_string();
        let second = fixture
            .state
            .set_default_model_owned(MODEL_A)
            .await
            .err()
            .ok_or_else(|| "later mutation ignored failed settlement".to_string())?
            .to_string();
        assert_eq!(second, first);

        let first_shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "shutdown lost the settlement failure".to_string())?
            .to_string();
        let second_shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost the settlement failure".to_string())?
            .to_string();
        assert_eq!(first_shutdown, first);
        assert_eq!(second_shutdown, first);
        Ok(())
    }

    #[tokio::test]
    async fn join_error_is_stable_for_later_mutations_and_repeated_shutdown() -> Result<(), String>
    {
        let fixture = fixture(valid_config()?, false).await?;
        let first = fixture
            .state
            .run_owned_model_mutation(ModelMutationRequest::AbortSettlementForTest)
            .await
            .err()
            .ok_or_else(|| "aborted settlement unexpectedly succeeded".to_string())?;
        assert!(matches!(first, ModelMutationError::Settlement(_)));
        let first = first.to_string();

        let later = fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .err()
            .ok_or_else(|| "later mutation ignored JoinError".to_string())?
            .to_string();
        let shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "shutdown lost JoinError".to_string())?
            .to_string();
        let repeated = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost JoinError".to_string())?
            .to_string();
        assert_eq!(later, first);
        assert_eq!(shutdown, first);
        assert_eq!(repeated, first);
        Ok(())
    }

    #[tokio::test]
    async fn later_pool_agent_prepare_failure_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let failing_lease = fixture
            .pool
            .acquire("z-failing")
            .await
            .map_err(|error| error.to_string())?;
        let failing = failing_lease.agent();
        drop(failing_lease);
        invalidate_model_budget(&failing).await;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Publication(_))));
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            agent_projection(&failing).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn inherited_subagent_prepare_failure_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let inherited = fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())?;
        invalidate_model_budget(&inherited).await;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Publication(_))));
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn zero_context_window_is_rejected_before_persistence_or_publication()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let mutation = ConfiguredModelMutation {
            model: model(MODEL_A, "local-a", "runtime-a", 0),
            set_default: false,
        };

        let result = fixture.state.upsert_configured_model_owned(mutation).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn provider_upsert_refreshes_active_generation_when_provider_is_shared()
    -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-shared", ENDPOINT_B))
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert_eq!(receipt.model_id, "local-shared");
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            persisted
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_B)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_B)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_B, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_B.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn session_override_stays_active_when_its_shared_provider_changes() -> Result<(), String>
    {
        let fixture = fixture_with_active(shared_provider_config()?, false, MODEL_B).await?;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-shared", ENDPOINT_C))
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert_eq!(receipt.model_id, "local-shared");
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_B)
        );
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_session_generation(
            &fixture,
            MODEL_A,
            MODEL_B,
            "runtime-b",
            ENDPOINT_C,
            WINDOW_B,
        )
        .await
    }

    #[tokio::test]
    async fn deleting_session_override_reactivates_the_durable_default() -> Result<(), String> {
        let fixture = fixture_with_active(valid_config()?, false, MODEL_B).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert!(receipt.deleted);
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_B)
        );
        assert_session_generation(
            &fixture,
            MODEL_A,
            MODEL_A,
            "runtime-a",
            ENDPOINT_A,
            WINDOW_A,
        )
        .await
    }

    #[tokio::test]
    async fn invalid_shared_provider_upsert_rolls_back_every_layer() -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        let mut mutation = provider_mutation("local-shared", RESPONSES_ENDPOINT);
        mutation.provider.auth_token = Some("invalid\nheader".to_string());
        let result = fixture.state.upsert_model_provider_owned(mutation).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(
            persisted
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_A)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_A)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn unrelated_provider_upsert_remains_persistence_only() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        invalidate_model_budget(&fixture.state.connection.agent).await;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-b", ENDPOINT_C))
            .await
            .map_err(|error| error.to_string())?;

        assert!(!receipt.activated);
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            persisted
                .model_providers
                .get("local-b")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_C)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-b")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_C)
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_non_default_commits_without_reapplying_active_runtime() -> Result<(), String>
    {
        let fixture = fixture(valid_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.runtime.is_none());
        let persisted = echo_agent::config::load_config_file(&fixture.config_path)?;
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_B)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }
}

#[cfg(test)]
mod workspace_transition_tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    static CWD_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct RestoreCwd(std::path::PathBuf);

    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    fn workspace(name: &str, root: std::path::PathBuf) -> Workspace {
        Workspace {
            id: crate::workspace::WorkspaceId::from_name(name),
            name: name.to_string(),
            root,
            project_root: None,
            kind: crate::workspace::WorkspaceKind::General,
            metadata: crate::workspace::WorkspaceMetadata::default(),
            created_at: Utc::now(),
            last_active: Utc::now(),
        }
    }

    fn write_hook(root: &std::path::Path, content: &str) -> std::result::Result<(), String> {
        let directory = root.join(".eko");
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(directory.join("hooks.yaml"), content).map_err(|error| error.to_string())
    }

    #[test]
    fn workspace_transition_receipt_serializes_generated_typescript_contract()
    -> std::result::Result<(), String> {
        let receipt = WorkspaceTransitionReceipt::committed(
            Some("workspace-a".to_string()),
            Some("workspace-b".to_string()),
            std::path::PathBuf::from("/workspace-b"),
            vec![WorkspaceSubsystemTransition {
                subsystem: "config_watcher".to_string(),
                target_root: std::path::PathBuf::from("/workspace-b"),
                stale_roots: Vec::new(),
                error: "watch settled with degraded cleanup".to_string(),
            }],
        );
        let serialized = serde_json::to_value(receipt).map_err(|error| error.to_string())?;

        assert_eq!(
            serialized.get("status").and_then(serde_json::Value::as_str),
            Some("degraded")
        );
        assert_eq!(
            serialized
                .get("target_root")
                .and_then(serde_json::Value::as_str),
            Some("/workspace-b")
        );
        assert_eq!(
            serialized
                .pointer("/degraded_subsystems/0/stale_roots")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        Ok(())
    }

    #[test]
    fn memory_rebind_degradation_preserves_owned_stale_roots() {
        let target = std::path::PathBuf::from("/workspace-b/.eko");
        let stale = std::path::PathBuf::from("/workspace-a/.eko");
        let receipt = crate::evolution::MemoryRebindReceipt {
            generation: 2,
            pending_old: 1,
            pending_roots: vec![stale.clone()],
            delivery_error: Some("injected evidence write failure".to_string()),
        };

        let subsystem = memory_rebind_degradation(target.clone(), &receipt);

        assert_eq!(subsystem.subsystem, "memory_evolution");
        assert_eq!(subsystem.target_root, target);
        assert_eq!(subsystem.stale_roots, vec![stale]);
        assert!(
            subsystem
                .error
                .contains("1 trigger(s) remain owned for retry")
        );
        assert!(subsystem.error.contains("injected evidence write failure"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn app_state_workspace_transition_publishes_degraded_memory_receipt_and_binding()
    -> std::result::Result<(), String> {
        const CHILD_PROCESS: &str = "EKO_MEMORY_WORKSPACE_TRANSITION_TEST";
        if std::env::var_os(CHILD_PROCESS).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("app_state_workspace_transition_publishes_degraded_memory_receipt_and_binding")
            .arg("--test-threads=1")
            .env(CHILD_PROCESS, "1")
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated memory workspace transition test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let _serial = CWD_TEST_LOCK.lock().await;
        let original_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let _restore = RestoreCwd(original_cwd);
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_root_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_root_b = root_b.canonicalize().map_err(|error| error.to_string())?;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .model("test-model")
                .llm_client(Arc::new(MockLlmClient::new().with_model_name("test-model")))
                .system_prompt("memory workspace transition test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let initial_store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration = Arc::new(crate::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join("initial/.eko"),
            initial_store.clone(),
        ));
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                agent.clone(),
                Some(integration.clone()),
                Some(initial_store),
                2,
                false,
            )
            .await,
        );
        integration.bind_rule_projection_primary(agent.clone());
        integration
            .bind_rule_projection_pool(&pool)
            .await
            .map_err(|error| error.to_string())?;
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            agent,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
        )
        .with_review_integration(Some(integration.clone()));
        state.tasks.runtime = None;
        state.set_pool(pool.clone());
        let state = Arc::new(state);

        state
            .switch_workspace(workspace("a", canonical_root_a.clone()))
            .await
            .map_err(|error| error.to_string())?;

        // Acquire the parking execution while the generation is healthy. The
        // trigger and projection fault below arrive after rebind preparation.
        let pool_execution = pool
            .acquire("memory-transition-parking-run")
            .await
            .map_err(|error| error.to_string())?;
        let blocked_evolution = canonical_root_a.join(".eko/evolution");
        if blocked_evolution.is_dir() {
            std::fs::remove_dir_all(&blocked_evolution).map_err(|error| error.to_string())?;
        }
        std::fs::write(&blocked_evolution, b"blocks evidence directory creation")
            .map_err(|error| error.to_string())?;

        // The pool lease holds transition settlement after memory prepare, so
        // the trigger deterministically arrives inside the rebind admission.
        let switch_state = state.clone();
        let switch_root = canonical_root_b.clone();
        let switch = tokio::spawn(async move {
            switch_state
                .switch_workspace(workspace("b", switch_root))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match integration.lease_generation() {
                    Err(crate::evolution::ReviewGenerationError::Busy {
                        rebind_in_progress: true,
                        ..
                    }) => return Ok(()),
                    Ok(lease) => drop(lease),
                    Err(error) => return Err(error.to_string()),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "memory transition did not enter rebind admission".to_string())??;

        integration.inject_pool_projection_fault_for_test();

        let trigger = echo_agent::evolution::TriggerMatch {
            content: "workspace A uses a pinned memory root".to_string(),
            memory_type: echo_agent::memory::MemoryType::ProjectFact,
            source: echo_agent::memory::MemorySource::ExplicitSave,
            confidence: 1.0,
            topic: "workspace-memory".to_string(),
            trust_level: echo_agent::evolution::InputTrustLevel::Trusted,
            suggested_key: "workspace-a-memory-root".to_string(),
            evidence: vec![echo_agent::evolution::TriggerEvidence {
                source_role: "user".to_string(),
                quote: "workspace A uses a pinned memory root".to_string(),
            }],
        };
        let disposition =
            echo_agent::evolution::MemoryTriggerSink::on_trigger(integration.as_ref(), &trigger)
                .await?;
        assert_eq!(
            disposition,
            echo_agent::evolution::MemoryTriggerDisposition::Captured
        );
        drop(pool_execution);

        let receipt = switch
            .await
            .map_err(|error| format!("workspace transition failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, WorkspaceTransitionStatus::Degraded);
        let memory = receipt
            .degraded_subsystems
            .iter()
            .find(|subsystem| subsystem.subsystem == "memory_evolution")
            .ok_or_else(|| "missing memory_evolution degradation".to_string())?;
        let target_state_dir = canonical_root_b.join(".eko");
        let stale_state_dir = canonical_root_a.join(".eko");
        assert_eq!(memory.target_root, target_state_dir);
        assert_eq!(memory.stale_roots, vec![stale_state_dir.clone()]);
        assert!(memory.error.contains("1 trigger(s) remain owned for retry"));
        let projection = receipt
            .degraded_subsystems
            .iter()
            .find(|subsystem| subsystem.subsystem == "memory_rule_projection")
            .ok_or_else(|| "missing memory_rule_projection degradation".to_string())?;
        assert_eq!(projection.target_root, target_state_dir);
        assert!(projection.stale_roots.is_empty());
        assert!(projection.error.contains("injected pool"));
        assert_eq!(integration.trigger_delivery_status().pending, 1);
        let published = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(published.echo_agent_dir(), target_state_dir);
        drop(published);

        std::fs::remove_file(&blocked_evolution).map_err(|error| error.to_string())?;
        drop(
            integration
                .lease_generation()
                .map_err(|error| error.to_string())?,
        );
        let retry_store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        assert!(matches!(
            integration.prepare_rebind(canonical_root_a.join("retry/.eko"), retry_store),
            Err(crate::evolution::ReviewGenerationError::ProjectionSettlement { .. })
        ));
        integration
            .initialize_rule_promotions()
            .await
            .map_err(|error| error.to_string())?;
        let retried = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(integration.trigger_delivery_status().pending, 0);
        assert_eq!(
            crate::evolution::EvidenceStore::new(stale_state_dir)
                .list()?
                .len(),
            1
        );
        drop(retried);
        integration.shutdown_background_reviews().await?;
        pool.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_boundary_abort_settles_owned_transition_and_preserves_preboundary_atomicity()
    -> std::result::Result<(), String> {
        const CHILD_PROCESS: &str = "EKO_WORKSPACE_TRANSITION_CWD_TEST";
        if std::env::var_os(CHILD_PROCESS).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("post_boundary_abort_settles_owned_transition_and_preserves_preboundary_atomicity")
            .arg("--test-threads=1")
            .env(CHILD_PROCESS, "1")
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated workspace transition test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let _serial = CWD_TEST_LOCK.lock().await;
        let original_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let _restore = RestoreCwd(original_cwd.clone());
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_root_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_root_b = root_b.canonicalize().map_err(|error| error.to_string())?;
        write_hook(&root_a, "{}\n")?;
        write_hook(&root_b, "SessionStart: [not-a-hook-rule]\n")?;

        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace transition test")
            .build()
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(agent);
        let cancel = tokio_util::sync::CancellationToken::new();
        let watcher = Arc::new(crate::config_watcher::spawn_config_watcher(
            None,
            agent.clone(),
            None,
            cancel,
        ));
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            agent.clone(),
            original_cwd.clone(),
            temp.path().join("plugin-registry.json"),
            temp.path().join("plugin-data"),
        )
        .await;
        let mut state = AppState::from_shared(
            agent,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
        )
        .with_config_watcher(Some(watcher.clone()))
        .with_plugin_runtime(Some(plugin_runtime.clone()));
        let initial_shadow = temp.path().join("initial-tasks");
        state.tasks.runtime = Some(Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                &initial_shadow,
            )
            .map_err(|error| error.to_string())?,
        ));
        let state = Arc::new(state);

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            state.switch_workspace(workspace("a", root_a.clone())),
        )
        .await
        .map_err(|_| "initial workspace transition timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let before_boundary = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            state.switch_workspace(workspace("b", root_b.clone())),
        )
        .await
        .map_err(|_| "pre-boundary workspace transition timed out".to_string())?;
        assert!(before_boundary.is_err());
        assert_eq!(std::env::current_dir().ok(), Some(canonical_root_a.clone()));
        assert_eq!(
            state.current_workspace().await.map(|current| current.root),
            Some(canonical_root_a.clone())
        );
        assert_eq!(
            state
                .tasks
                .runtime
                .as_ref()
                .map(|runtime| runtime.active_shadow_root()),
            Some(crate::workspace::layout::WorkspaceLayout::tasks(
                &canonical_root_a
            ))
        );
        assert_eq!(watcher.settled_root().await, canonical_root_a.clone());
        assert_eq!(
            plugin_runtime.workspace_root().await,
            canonical_root_a.clone()
        );
        let reopened = state
            .session
            .foreground_turns
            .begin(
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                "after-preboundary",
                "turn",
            )
            .map_err(|error| error.to_string())?;
        reopened.settle(crate::chat_driver::TurnOutcome::Completed);
        let task_runtime = state
            .tasks
            .runtime
            .as_ref()
            .ok_or_else(|| "missing task runtime".to_string())?;
        task_runtime
            .create_run_for_active_workspace(
                "after-preboundary",
                "conversation-a",
                "message-a",
                crate::tasks::task_runtime::DomainProfile::General,
                "pre-boundary admission reopened",
                "task",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            crate::workspace::layout::WorkspaceLayout::tasks(&root_a)
                .join("after-preboundary/events.jsonl")
                .is_file()
        );

        write_hook(&root_b, "{}\n")?;
        watcher
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        let conversation_binding_write = state.storage.conversation.write().await;
        let detached_state = Arc::clone(&state);
        let detached_root = root_b.clone();
        let caller = tokio::spawn(async move {
            detached_state
                .switch_workspace(workspace("b", detached_root))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if std::env::current_dir().ok().as_ref() == Some(&canonical_root_b) {
                    return Ok(());
                }
                if caller.is_finished() {
                    return Err(
                        "transition finished before the post-boundary abort point".to_string()
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "workspace transition did not reach its commit boundary".to_string())??;
        caller.abort();
        let caller_result = caller.await;
        assert!(caller_result.is_err_and(|error| error.is_cancelled()));
        drop(conversation_binding_write);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            state.shutdown_workspace_transition(),
        )
        .await
        .map_err(|_| "detached workspace settlement timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let committed = state
            .workspace
            .last_transition
            .read()
            .await
            .clone()
            .ok_or_else(|| "detached transition did not publish a receipt".to_string())?;
        assert_eq!(committed.status, WorkspaceTransitionStatus::Degraded);
        assert_eq!(std::env::current_dir().ok(), Some(canonical_root_b.clone()));
        assert_eq!(
            state.current_workspace().await.map(|current| current.root),
            Some(canonical_root_b.clone())
        );
        assert_eq!(
            state
                .tasks
                .runtime
                .as_ref()
                .map(|runtime| runtime.active_shadow_root()),
            Some(crate::workspace::layout::WorkspaceLayout::tasks(
                &canonical_root_b
            ))
        );
        assert_eq!(
            plugin_runtime.workspace_root().await,
            canonical_root_b.clone()
        );
        let watcher_failure = committed
            .degraded_subsystems
            .iter()
            .find(|subsystem| subsystem.subsystem == "config_watcher")
            .ok_or_else(|| "missing config watcher degradation".to_string())?;
        assert_eq!(watcher_failure.target_root, canonical_root_b);
        assert_eq!(watcher_failure.stale_roots, vec![canonical_root_a.clone()]);

        let exited =
            tokio::time::timeout(std::time::Duration::from_secs(5), state.exit_workspace())
                .await
                .map_err(|_| "workspace exit settlement timed out".to_string())?
                .map_err(|error| error.to_string())?;
        assert_eq!(exited.status, WorkspaceTransitionStatus::Degraded);
        assert_eq!(std::env::current_dir().ok(), Some(original_cwd.clone()));
        assert!(state.current_workspace().await.is_none());
        assert_eq!(plugin_runtime.workspace_root().await, original_cwd.clone());
        let exit_watcher_failure = exited
            .degraded_subsystems
            .iter()
            .find(|subsystem| subsystem.subsystem == "config_watcher")
            .ok_or_else(|| "missing exit config watcher degradation".to_string())?;
        assert_eq!(exit_watcher_failure.target_root, original_cwd);
        assert_eq!(exit_watcher_failure.stale_roots, vec![canonical_root_a]);
        Ok(())
    }
}

#[cfg(test)]
mod service_bootstrap_tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::memory::{Store, StoreItem};
    use echo_agent::testing::MockLlmClient;
    use futures::future::BoxFuture;

    struct SchedulerInitFailureStore;

    fn scheduler_store_failure<T>() -> echo_agent::error::Result<T> {
        Err(echo_agent::error::ReactError::Other(
            "injected scheduler store failure".to_string(),
        ))
    }

    impl Store for SchedulerInitFailureStore {
        fn put<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
            _value: serde_json::Value,
        ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn get<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<Option<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn search<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _query: &'a str,
            _limit: usize,
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn delete<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<bool>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn list_namespaces<'a>(
            &'a self,
            _prefix: Option<&'a [&'a str]>,
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<Vec<String>>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn list<'a>(
            &'a self,
            _namespace: &'a [&'a str],
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }
    }

    #[tokio::test]
    async fn scheduler_init_failure_does_not_start_task_service_or_run_driver()
    -> std::result::Result<(), String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("service bootstrap test")
            .build()
            .map_err(|error| error.to_string())?;
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            std::env::temp_dir().join(format!("eko-mcp-{}.json", uuid::Uuid::new_v4())),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            AgentHandle::new(agent),
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
        );
        let runtime_store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        state.tasks.runtime = Some(runtime_store.clone());

        let result = state
            .start_scheduler_and_task_service(Some(Arc::new(SchedulerInitFailureStore)))
            .await;

        assert!(result.is_err());
        assert!(state.scheduler.runner.is_none());
        assert!(state.tasks.service.is_none());
        assert_eq!(runtime_store.active_run_driver_count()?, 0);
        Ok(())
    }
}

async fn await_workspace_settlement(
    handle: &mut WorkspaceSettlementHandle,
) -> anyhow::Result<WorkspaceTransitionReceipt> {
    handle
        .await
        .map_err(|error| anyhow::anyhow!("workspace settlement task failed: {error}"))?
}

fn validated_workspace_root(root: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
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

fn ensure_no_running_task_runs(
    transition: Option<&crate::tasks::task_runtime::store::TaskRuntimeWorkspaceTransition<'_>>,
) -> anyhow::Result<()> {
    let Some(transition) = transition else {
        return Ok(());
    };
    let running = transition
        .list_runs_in(&[crate::tasks::task_runtime::TaskRunStatus::Running])
        .map_err(|error| anyhow::anyhow!("Failed to inspect active task runs: {error}"))?;
    if running.is_empty() {
        return Ok(());
    }
    let run_ids = running
        .iter()
        .take(5)
        .map(|run| run.run_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!("Cannot change workspace while TaskRun is running: {run_ids}")
}

#[cfg(test)]
mod permission_rule_tests {
    use super::*;
    use echo_agent::tools::permission::{RuleBehavior, RuleMatcher, RuleSource, ToolPermission};

    #[tokio::test]
    async fn scheduler_shutdown_joins_owned_handle_and_is_idempotent()
    -> std::result::Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = crate::scheduler::CronTaskStore::new()
            .with_path(temp.path().join("scheduler-tasks.json"));
        let cancel_token = echo_agent::agent::CancellationToken::new();
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_task| Box::pin(async { Ok("done".to_string()) }));
        let runner = Arc::new(
            crate::scheduler::SchedulerRunner::new(store, cancel_token.clone(), fire_fn)
                .await
                .map_err(|error| error.to_string())?,
        );
        let handle = runner.clone().spawn();
        let scheduler = SchedulerState {
            runner: Some(runner),
            cancel_token,
            handle: Mutex::new(Some(handle)),
        };

        scheduler
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        scheduler
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        assert!(scheduler.handle.lock().await.is_none());
        Ok(())
    }

    #[test]
    fn application_permission_rule_converts_without_losing_semantics()
    -> std::result::Result<(), String> {
        let config = PermissionRuleConfig {
            matcher: "permission:write".to_string(),
            behavior: PermissionBehavior::Deny,
            source: "projectSettings".to_string(),
        };

        let rule = config.to_framework_rule()?;
        assert!(matches!(
            rule.matcher,
            RuleMatcher::Permission {
                permission: ToolPermission::Write
            }
        ));
        assert!(matches!(rule.behavior, RuleBehavior::Deny { .. }));
        assert_eq!(rule.source, RuleSource::ProjectSettings);
        Ok(())
    }

    #[test]
    fn workspace_preflight_rejects_missing_and_non_directory_roots()
    -> std::result::Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let file = temp.path().join("workspace-file");
        std::fs::write(&file, "not a directory").map_err(|error| error.to_string())?;

        assert!(validated_workspace_root(&temp.path().join("missing")).is_err());
        assert!(validated_workspace_root(&file).is_err());
        assert_eq!(
            validated_workspace_root(temp.path()).map_err(|error| error.to_string())?,
            temp.path()
                .canonicalize()
                .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[tokio::test]
    async fn workspace_preflight_rejects_running_task_runs() -> std::result::Result<(), String> {
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

        let runtime = crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
            .map_err(|error| error.to_string())?;
        runtime
            .create_run(
                "workspace-transition-run",
                "workspace-a",
                "conversation-a",
                "message-a",
                DomainProfile::General,
                "verify workspace transition",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        {
            let transition = runtime
                .begin_workspace_transition()
                .await
                .map_err(|error| error.to_string())?;
            assert!(ensure_no_running_task_runs(Some(&transition)).is_ok());
        }

        runtime
            .transition_run("workspace-transition-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let transition = runtime
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        let error = match ensure_no_running_task_runs(Some(&transition)) {
            Ok(()) => return Err("a running TaskRun did not block workspace change".to_string()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("workspace-transition-run"));
        Ok(())
    }
}
