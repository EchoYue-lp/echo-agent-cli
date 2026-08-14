//! 应用状态管理
//!
//! 支持两种运行模式的状态共享：
//! - 单模式（Web 或 CLI）：独立的 Agent 实例
//! - 双模式（Web + CLI）：共享的 Agent 实例

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use echo_agent::agent::CancellationToken;
use echo_agent::mcp::McpConfigFile;
use echo_agent::memory::ConversationStore;
use echo_agent::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::hitl::HitlDispatcher;
use tokio::sync::{Mutex, RwLock};

use crate::agent_handle::AgentHandle;
use crate::persistence::Persistence;
use crate::workspace::Workspace;
use crate::workspace::registry::WorkspaceRegistry;

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

// ── 工作流 ──

/// 存储的工作流定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWorkflow {
    pub id: String,
    pub name: String,
    pub definition: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// 工作流步骤（简单线性工作流）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String, // prompt / tool / condition
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<serde_json::Value>,
}

/// CLI workflow definition — a simple linear sequence of steps stored/edited
/// via the REST API. The framework also provides a full DAG-based
/// [`WorkflowDefinition`] (`echo_agent::workflow::WorkflowDefinition`) with
/// nodes, edges, entry/exit points and concurrent execution for advanced use
/// cases. This type covers the common "prompt → tool → prompt" pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
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
    /// HITL dispatcher — 多 Provider 协作（repl, ws, webhook 等）
    /// WS handler 注册到 dispatcher 而非替换 agent 的 provider，
    /// 确保多模式下 HITL 请求能路由到正确的 Provider。
    pub hitl_dispatcher: Arc<crate::hitl::HitlDispatcher>,
    /// Agent pool for multi-conversation parallel execution.
    /// When `Some`, `agent_for()` routes to pool agents by conversation_id.
    /// When `None`, all requests use the single `agent` (backward compatible).
    pub pool: Option<Arc<crate::agent_pool::AgentPool>>,
}

impl ConnectionState {
    /// Get the agent for a given conversation ID.
    ///
    /// If a pool is active, acquires (or reuses) a pool agent for the ID.
    /// Falls back to the primary `agent` if pool is disabled or acquire fails.
    pub async fn agent_for(&self, conversation_id: &str) -> AgentHandle {
        if let Some(ref pool) = self.pool {
            pool.acquire(conversation_id).await.unwrap_or_else(|e| {
                tracing::warn!(
                    conv_id = %conversation_id,
                    error = %e,
                    "AgentPool::acquire failed, falling back to primary agent"
                );
                self.agent.clone()
            })
        } else {
            self.agent.clone()
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
    /// Immutable startup source used for every application-side config commit.
    pub config_path: std::path::PathBuf,
    pub web_config: RwLock<WebConfig>,
    pub sandbox_config: RwLock<SandboxConfigData>,
    pub permission_mode: RwLock<String>,
    pub permission_rules: RwLock<Vec<PermissionRuleConfig>>,
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
    pub mcp_config: RwLock<McpConfigFile>,
    pub mcp_health: RwLock<HashMap<String, McpHealthStatus>>,
}

/// 持久化存储状态
pub struct StorageState {
    pub conversation_store: RwLock<Option<Arc<dyn ConversationStore>>>,
    pub persistence: RwLock<Persistence>,
    pub search_engine: crate::sessions::SessionSearchEngine,
    pub tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
}

/// 历史记录状态：审计日志 + 工作流
pub struct HistoryState {
    pub audit_logs: RwLock<Vec<AuditLogEntry>>,
    pub workflows: RwLock<HashMap<String, StoredWorkflow>>,
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
}

impl AppState {
    /// 从共享的 Agent 和 HITL Dispatcher 创建状态（用于双模式）
    pub fn from_shared(
        agent: AgentHandle,
        hitl_dispatcher: Arc<crate::hitl::HitlDispatcher>,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        app_config: echo_agent::config::AppConfig,
    ) -> Self {
        let config = agent
            .try_write(|guard| WebConfig {
                model: guard.model_name().to_string(),
                system_prompt: guard.system_prompt().to_string(),
                token_limit: 8000,
                ..Default::default()
            })
            .unwrap_or_default();

        let webhook_emitter = Arc::new(crate::webhook::WebhookEmitter::from_config(&app_config));

        Self {
            connection: ConnectionState {
                agent,
                hitl_dispatcher,
                pool: None,
            },
            config: ConfigState {
                app_config: RwLock::new(app_config),
                config_path: crate::config_watcher::resolve_config_save_path(None),
                web_config: RwLock::new(config),
                sandbox_config: RwLock::new(SandboxConfigData::default()),
                permission_mode: RwLock::new("default".to_string()),
                permission_rules: RwLock::new(Vec::new()),
            },
            session: SessionState {
                tool_states: RwLock::new(HashMap::new()),
                operation_cancel_tokens: Arc::new(DashMap::new()),
                foreground_turns: crate::foreground_turn::ForegroundTurnControl::default(),
            },
            plugins: PluginState {
                mcp_config: RwLock::new(McpConfigFile::default()),
                mcp_health: RwLock::new(HashMap::new()),
            },
            storage: StorageState {
                conversation_store: RwLock::new(conversation_store),
                persistence: RwLock::new(Persistence::new()),
                search_engine: {
                    // U1c: in-memory substring engine (no SQLite/FTS5). Reindex
                    // from session files on start; failures just mean an empty
                    // index (search returns nothing until content is re-added).
                    let engine = crate::sessions::SessionSearchEngine::new();
                    match engine.reindex_all() {
                        Ok(n) => tracing::info!("Session search index rebuilt: {n} sessions"),
                        Err(e) => tracing::warn!("Session search reindex failed: {e}"),
                    }
                    engine
                },
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
                    Arc::new(repository)
                },
            },
            history: HistoryState {
                audit_logs: RwLock::new(Vec::new()),
                workflows: RwLock::new(HashMap::new()),
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
                        let recovered = store.recover_incomplete();
                        if recovered > 0 {
                            tracing::info!(
                                count = recovered,
                                "Recovered interrupted task-runtime runs at boot"
                            );
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
        }
    }

    /// Bind config persistence to the source selected during bootstrap.
    pub fn with_config_path(mut self, path: std::path::PathBuf) -> Self {
        self.config.config_path = path;
        self
    }

    /// Persist one complete config snapshot to the immutable bootstrap source.
    pub fn save_app_config(
        &self,
        config: &echo_agent::config::AppConfig,
    ) -> std::result::Result<(), String> {
        echo_agent::config::save_config_file(&self.config.config_path, config)
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
                let service = Arc::new(service);
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
    pub async fn switch_workspace(&self, workspace: Workspace) -> anyhow::Result<()> {
        let _transition = self.workspace.transition.lock().await;
        if self.session.foreground_turns.has_active_turns() {
            anyhow::bail!("Cannot switch workspace while a foreground chat turn is running");
        }
        ensure_no_running_task_runs(self.tasks.runtime.as_deref())?;
        let root = validated_workspace_root(&workspace.root)?;
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
        let memory_store = crate::infra::create_memory_store_for_workspace(&root)
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare workspace memory store"))?;
        if let Some(pool) = &self.connection.pool {
            pool.reset_for_workspace_transition().await?;
        }
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

        // 更新 agent 的 working_dir 配置（影响 project rules 注入等）
        let new_wd = Some(workspace.root.clone());
        let primary_root = workspace.root.clone();
        self.connection
            .agent
            .write_async(|agent| {
                Box::pin(async move {
                    agent.set_working_dir(Some(primary_root.clone()));
                    agent.set_tool_output_artifacts(Some(
                        crate::infra::tool_output_artifact_config(Some(&primary_root)),
                    ));
                    crate::infra::refresh_dynamic_context(agent, Some(&primary_root)).await;
                })
            })
            .await;
        // Propagate to all pooled agents so background tasks run in the new
        // workspace (P1-7).
        if let Some(ref pool) = self.connection.pool {
            pool.apply_working_dir(new_wd).await;
        }

        // 重新初始化 persistence 以使用工作区路径
        let new_persistence = Persistence::with_base_dir(sessions_dir.clone());
        {
            let mut persistence = self.storage.persistence.write().await;
            *persistence = new_persistence;
        }

        // 重新初始化 conversation_store 到工作区的存储目录（U1c：文件后端）
        {
            let mut guard = self.storage.conversation_store.write().await;
            *guard = Some(conversation_store.clone());
        }
        self.connection
            .agent
            .write(|agent| {
                agent.set_conversation_store(conversation_store.clone());
                agent.set_state_store(runtime_store.clone());
            })
            .await;
        if let Some(pool) = &self.connection.pool {
            pool.apply_conversation_store(conversation_store).await;
        }

        // 重新初始化 memory store 到工作区的存储目录（物理隔离：动态记忆
        // 跟 workspace 走，不再共享全局 ~/.eko/store.json）。
        // hot 层 MEMORY.md 的 echo_agent_dir 与 warm 层 store.json 同根，
        // 都落在 {workspace.root}/.eko/，保证两层一致。
        let mem_root = workspace.root.clone();
        {
            let store = memory_store;
            let echo_agent_dir = crate::workspace::layout::WorkspaceLayout::state_dir(&mem_root); // {root}/.eko
            if let Some(ref ri) = self.review_integration {
                ri.rebind(echo_agent_dir.clone(), store.clone());
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
                });
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

        tracing::info!(
            workspace = %workspace.id,
            root = %workspace.root.display(),
            "Switched to workspace"
        );

        // 根据工作区类型配置 Agent（自动激活 Skills 和注入系统提示词）
        self.apply_workspace_routing(&workspace).await;

        if let Some(runtime) = self.tasks.runtime.as_deref() {
            runtime.rebind_shadow_root(tasks_dir, workspace.id.to_string())?;
        }

        {
            let mut current = self.workspace.current.write().await;
            *current = Some(workspace.clone());
        }
        crate::config_watcher::notify_config_watcher_workspace(workspace.root.clone());

        Ok(())
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
    pub async fn exit_workspace(&self) -> anyhow::Result<()> {
        let _transition = self.workspace.transition.lock().await;
        if self.session.foreground_turns.has_active_turns() {
            anyhow::bail!("Cannot exit workspace while a foreground chat turn is running");
        }
        ensure_no_running_task_runs(self.tasks.runtime.as_deref())?;
        let global_cwd = self.workspace.global_cwd.canonicalize().map_err(|error| {
            anyhow::anyhow!("Failed to resolve the global working directory: {error}")
        })?;
        let conversation_store = crate::infra::create_conversation_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global conversation store"))?;
        let runtime_store = crate::infra::create_runtime_state_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global runtime state store"))?;
        let memory_store = crate::infra::create_global_memory_store()
            .ok_or_else(|| anyhow::anyhow!("Failed to prepare global memory store"))?;
        let global_tasks_dir =
            crate::tasks::task_runtime::file_shadow::FileTaskShadow::default_root();
        std::fs::create_dir_all(&global_tasks_dir)?;
        if let Some(pool) = &self.connection.pool {
            pool.reset_for_workspace_transition().await?;
        }
        std::env::set_current_dir(&global_cwd).map_err(|error| {
            anyhow::anyhow!("Failed to restore process directory after workspace exit: {error}")
        })?;

        // 重置 persistence 到全局默认路径
        let global_persistence = Persistence::new();
        {
            let mut persistence = self.storage.persistence.write().await;
            *persistence = global_persistence;
        }

        self.connection
            .agent
            .write_async(|agent| {
                Box::pin(async move {
                    agent.set_working_dir(None);
                    agent.set_tool_output_artifacts(Some(
                        crate::infra::tool_output_artifact_config(None),
                    ));
                    crate::infra::refresh_dynamic_context(agent, None).await;
                })
            })
            .await;

        // 重置 conversation_store 到全局默认路径（U1c：文件后端）
        {
            let store = conversation_store;
            let mut guard = self.storage.conversation_store.write().await;
            *guard = Some(store.clone());
            drop(guard);
            self.connection
                .agent
                .write(|agent| agent.set_conversation_store(store.clone()))
                .await;
            if let Some(pool) = &self.connection.pool {
                pool.apply_conversation_store(store).await;
            }
        }

        // 重置 runtime_state_store 到全局默认路径
        self.connection
            .agent
            .try_write(|a| a.set_state_store(runtime_store));

        // 重置 memory store 到全局默认路径（~/.eko/store.json）。
        // 与 switch_workspace 的 memory 重载对称：exit 后动态记忆回到全局 store，
        // 不再读已退出 workspace 的 .eko/memory/。
        {
            let store = memory_store;
            let (global_store_path, global_echo_dir) = crate::infra::global_memory_paths();
            if let Some(ref ri) = self.review_integration {
                ri.rebind(global_echo_dir.clone(), store.clone());
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
                });
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

        if let Some(runtime) = self.tasks.runtime.as_deref() {
            runtime.rebind_shadow_root(global_tasks_dir, "global")?;
        }

        {
            let mut current = self.workspace.current.write().await;
            *current = None;
        }
        crate::config_watcher::notify_config_watcher_workspace(global_cwd);

        tracing::info!("Exited workspace, using global default paths");
        Ok(())
    }

    /// 获取工作区感知的 sessions 目录。
    pub async fn sessions_dir(&self) -> std::path::PathBuf {
        if let Some(ref ws) = *self.workspace.current.read().await {
            crate::workspace::layout::WorkspaceLayout::sessions(&ws.root)
        } else {
            Persistence::base_dir()
        }
    }
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
    runtime: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> anyhow::Result<()> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    let running = runtime
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

    #[test]
    fn workspace_preflight_rejects_running_task_runs() -> std::result::Result<(), String> {
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
        assert!(ensure_no_running_task_runs(Some(&runtime)).is_ok());

        runtime
            .transition_run("workspace-transition-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let error = match ensure_no_running_task_runs(Some(&runtime)) {
            Ok(()) => return Err("a running TaskRun did not block workspace change".to_string()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("workspace-transition-run"));
        Ok(())
    }
}
