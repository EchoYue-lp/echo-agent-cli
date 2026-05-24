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
use governor::{Quota, RateLimiter, clock, state::keyed::DashMapStateStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agent_handle::AgentHandle;
use crate::persistence::Persistence;
use crate::security::SecurityConfig;

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
            model: "qwen-plus".to_string(),
            system_prompt: "你是一个智能助手。".to_string(),
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
    /// Check whether this rule applies to a given tool by name.
    ///
    /// Supported matcher patterns:
    /// - `tool:<name>` — exact tool name match
    /// - `perm:<flag>` — matches if the tool declares the given permission
    ///   (checked via the caller, not here — this method returns `true`
    ///    for all `perm:` matchers, leaving permission-checking to the caller)
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub last_check: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

// ── 子状态拆分 ──

/// 连接管理状态：Agent 句柄
pub struct ConnectionState {
    pub agent: AgentHandle,
    /// 对话串行化锁：确保同一时间只有一个 chat 在运行。
    ///
    /// 对于单用户本地应用，此锁防止多 WS 连接并发修改
    /// human-loop provider 等全局 agent 状态。
    /// 多用户部署需要通过 per-session agent 实例来隔离。
    pub chat_serializer: tokio::sync::Mutex<()>,
}

/// 配置状态：应用 / Web / 安全 / 沙箱 / 权限
pub struct ConfigState {
    pub app_config: RwLock<crate::config::AppConfig>,
    pub web_config: RwLock<WebConfig>,
    pub security_config: RwLock<SecurityConfig>,
    pub jwt_manager: RwLock<Option<crate::security::JwtManager>>,
    pub sandbox_config: RwLock<SandboxConfigData>,
    pub permission_mode: RwLock<String>,
    pub permission_rules: RwLock<Vec<PermissionRuleConfig>>,
}

/// 会话状态：工具状态 + 取消令牌 + 速率限制
pub struct SessionState {
    pub tool_states: RwLock<HashMap<String, ToolState>>,
    pub cancel_token: DashMap<String, CancellationToken>,
    pub rate_limiter: Arc<RateLimiter<String, DashMapStateStore<String>, clock::DefaultClock>>,
}

/// 插件状态：MCP 服务管理
pub struct PluginState {
    pub mcp_config: RwLock<McpConfigFile>,
    pub mcp_health: RwLock<HashMap<String, McpHealthStatus>>,
}

/// 持久化存储状态
pub struct StorageState {
    pub conversation_store: Option<Arc<dyn ConversationStore>>,
    pub persistence: Persistence,
    pub search_engine: crate::sessions::SessionSearchEngine,
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
}

/// Webhook 状态
pub struct WebhookState {
    pub emitter: crate::webhook::WebhookEmitter,
}

/// 全局应用状态
///
/// 按功能域拆分为 8 个子状态，通过 `Arc<AppState>` 共享。
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
    /// Webhook 事件回调
    pub webhook: WebhookState,
    /// Skills Hub（本地技能市场）
    pub skills_hub: Arc<RwLock<crate::skills_hub::SkillsHub>>,
}

impl AppState {
    /// 从共享的 Agent 创建状态（用于双模式）
    pub fn from_shared(
        agent: AgentHandle,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        app_config: crate::config::AppConfig,
    ) -> Self {
        let config = agent
            .try_write(|guard| WebConfig {
                model: guard.model_name().to_string(),
                system_prompt: guard.system_prompt().to_string(),
                token_limit: 8000,
                ..Default::default()
            })
            .unwrap_or_default();

        // 加载安全配置（环境变量覆盖默认值）
        let security_config = SecurityConfig::from_env();

        // 验证安全配置
        if let Err(e) = security_config.validate() {
            tracing::warn!("安全配置验证失败: {}", e);
        }

        // 创建速率限制器
        let rate_limit_per_minute = security_config.rate_limit_requests_per_minute;
        let rate_limiter = if rate_limit_per_minute > 0 {
            let quota = Quota::per_minute(
                std::num::NonZeroU32::new(rate_limit_per_minute)
                    .unwrap_or(std::num::NonZeroU32::new(1).unwrap()),
            );
            Arc::new(RateLimiter::keyed(quota))
        } else {
            // 如果速率为0，创建一个允许所有请求的限制器
            let quota = Quota::per_minute(std::num::NonZeroU32::new(u32::MAX).unwrap());
            Arc::new(RateLimiter::keyed(quota))
        };

        let security_config = RwLock::new(security_config);

        // Extract webhook endpoints before moving app_config
        let webhook_endpoints: Vec<crate::webhook::emitter::WebhookEndpoint> = app_config
            .webhooks
            .endpoints
            .iter()
            .map(|e| crate::webhook::emitter::WebhookEndpoint {
                url: e.url.clone(),
                events: e.events.clone(),
                secret: e.secret.clone(),
            })
            .collect();

        Self {
            connection: ConnectionState {
                agent,
                chat_serializer: tokio::sync::Mutex::new(()),
            },
            config: ConfigState {
                app_config: RwLock::new(app_config),
                web_config: RwLock::new(config),
                security_config,
                jwt_manager: RwLock::new(None),
                sandbox_config: RwLock::new(SandboxConfigData::default()),
                permission_mode: RwLock::new("default".to_string()),
                permission_rules: RwLock::new(Vec::new()),
            },
            session: SessionState {
                tool_states: RwLock::new(HashMap::new()),
                cancel_token: DashMap::new(),
                rate_limiter,
            },
            plugins: PluginState {
                mcp_config: RwLock::new(McpConfigFile::default()),
                mcp_health: RwLock::new(HashMap::new()),
            },
            storage: StorageState {
                conversation_store,
                persistence: Persistence::new(),
                search_engine: crate::sessions::SessionSearchEngine::new().unwrap_or_else(|e| {
                    tracing::warn!("Failed to init search engine: {e}, creating empty");
                    // Fallback: create an in-memory engine that won't persist
                    crate::sessions::SessionSearchEngine::new_in_memory()
                        .expect("in-memory FTS5 engine should always init")
                }),
            },
            history: HistoryState {
                audit_logs: RwLock::new(Vec::new()),
                workflows: RwLock::new(HashMap::new()),
            },
            scheduler: SchedulerState {
                runner: None,
                cancel_token: echo_agent::agent::CancellationToken::new(),
            },
            webhook: WebhookState {
                emitter: crate::webhook::WebhookEmitter::with_endpoints(webhook_endpoints),
            },
            skills_hub: Arc::new(RwLock::new(crate::skills_hub::SkillsHub::new())),
        }
    }

    /// 启动定时任务调度器（仅在 Web 或双模式下调用）
    ///
    /// Call this **before** wrapping in `Arc`.
    pub fn start_scheduler(&mut self) {
        if self.scheduler.runner.is_some() {
            return;
        }
        let runner = Arc::new(crate::scheduler::SchedulerRunner::new(
            self.connection.agent.clone(),
            self.scheduler.cancel_token.clone(),
        ));
        runner.clone().spawn();
        self.scheduler.runner = Some(runner);
        tracing::info!("Scheduler runner started");
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

    /// 重新加载安全配置（热更新，无需重启）
    pub async fn reload_security_config(&self) -> std::result::Result<(), String> {
        let new_config = SecurityConfig::from_env();
        new_config.validate()?;

        let mut guard = self.config.security_config.write().await;
        *guard = new_config;
        tracing::info!("安全配置已热更新生效");
        Ok(())
    }

    /// 获取或创建缓存的 JWT 管理器（避免每个请求重新构造密钥）
    pub async fn get_or_create_jwt_manager(&self, secret: &str) -> crate::security::JwtManager {
        // Fast path: already cached
        {
            let guard = self.config.jwt_manager.read().await;
            if let Some(ref mgr) = *guard {
                return mgr.clone();
            }
        }
        // Slow path: create and cache
        let mgr = crate::security::JwtManager::new(secret);
        let mut guard = self.config.jwt_manager.write().await;
        // Double-check: another thread may have created it while we waited
        if let Some(ref existing) = *guard {
            return existing.clone();
        }
        *guard = Some(mgr.clone());
        mgr
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

    /// 清空审计日志，返回清除的条目数
    pub async fn clear_audit_entries(&self) -> usize {
        let mut logs = self.history.audit_logs.write().await;
        let count = logs.len();
        logs.clear();
        count
    }
}
