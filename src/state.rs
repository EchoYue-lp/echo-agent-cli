//! 应用状态管理
//!
//! 支持两种运行模式的状态共享：
//! - 单模式（Web 或 CLI）：独立的 Agent 实例
//! - 双模式（Web + CLI）：共享的 Agent 实例

use echo_agent::memory::conversation::ConversationStore;
use echo_agent::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::persistence::Persistence;

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
    pub max_iterations: usize,
    pub token_limit: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            model: "qwen-plus".to_string(),
            system_prompt: "你是一个智能助手。".to_string(),
            max_iterations: 10,
            token_limit: 8000,
        }
    }
}

// ── 审计日志 ──

/// 审计日志条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub decision: String,  // allow / deny / ask
    pub reason: String,
    pub source: String,
    pub duration_us: u64,
    pub elapsed_ms: u64,
    pub timestamp: String,
}

// ── 权限管理 ──

/// 权限规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub matcher: String,
    pub behavior: String,  // allow / deny / ask
    pub source: String,
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
    pub step_type: String,  // prompt / tool / condition
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<serde_json::Value>,
}

/// 工作流定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

// ── 沙箱配置 ──

/// 沙箱运行时配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfigData {
    pub security_level: String,
    pub max_memory_mb: u32,
    pub max_cpu_seconds: u32,
    pub network_enabled: bool,
}

impl Default for SandboxConfigData {
    fn default() -> Self {
        Self {
            security_level: "medium".to_string(),
            max_memory_mb: 512,
            max_cpu_seconds: 30,
            network_enabled: false,
        }
    }
}

/// 全局应用状态
///
/// 使用 `Arc<Mutex<ReactAgent>>` 以支持 Web 和 CLI 模式共享同一个 Agent。
pub struct AppState {
    /// Agent 实例（共享）
    pub agent: Arc<Mutex<ReactAgent>>,

    /// Web 配置
    pub config: std::sync::RwLock<WebConfig>,

    /// 工具状态映射
    pub tool_states: std::sync::RwLock<HashMap<String, ToolState>>,

    /// 审计日志
    pub audit_logs: std::sync::RwLock<Vec<AuditLogEntry>>,

    /// 权限模式: "default" / "auto-approve" / "strict"
    pub permission_mode: std::sync::RwLock<String>,

    /// 权限规则列表
    pub permission_rules: std::sync::RwLock<Vec<PermissionRule>>,

    /// 工作流存储
    pub workflows: std::sync::RwLock<HashMap<String, StoredWorkflow>>,

    /// 沙箱配置
    pub sandbox_config: std::sync::RwLock<SandboxConfigData>,

    /// 持久化管理器（用于 Markdown 导出）
    pub persistence: Persistence,

    /// 对话持久化 Store（SQLite）
    pub conversation_store: Arc<dyn ConversationStore>,
}

impl AppState {
    /// 从 Agent 实例创建状态
    pub fn new(agent: ReactAgent, conversation_store: Arc<dyn ConversationStore>) -> Self {
        let config = WebConfig {
            model: agent.model_name().to_string(),
            system_prompt: agent.system_prompt().to_string(),
            max_iterations: 10,
            token_limit: 8000,
        };

        Self {
            agent: Arc::new(Mutex::new(agent)),
            config: std::sync::RwLock::new(config),
            tool_states: std::sync::RwLock::new(HashMap::new()),
            audit_logs: std::sync::RwLock::new(Vec::new()),
            permission_mode: std::sync::RwLock::new("default".to_string()),
            permission_rules: std::sync::RwLock::new(Vec::new()),
            workflows: std::sync::RwLock::new(HashMap::new()),
            sandbox_config: std::sync::RwLock::new(SandboxConfigData::default()),
            persistence: Persistence::new(),
            conversation_store,
        }
    }

    /// 从共享的 Agent 创建状态（用于双模式）
    pub fn from_shared(
        agent: Arc<Mutex<ReactAgent>>,
        conversation_store: Arc<dyn ConversationStore>,
    ) -> Self {
        let config = agent
            .try_lock()
            .map(|guard| WebConfig {
                model: guard.model_name().to_string(),
                system_prompt: guard.system_prompt().to_string(),
                max_iterations: 10,
                token_limit: 8000,
            })
            .unwrap_or_default();

        Self {
            agent,
            config: std::sync::RwLock::new(config),
            tool_states: std::sync::RwLock::new(HashMap::new()),
            audit_logs: std::sync::RwLock::new(Vec::new()),
            permission_mode: std::sync::RwLock::new("default".to_string()),
            permission_rules: std::sync::RwLock::new(Vec::new()),
            workflows: std::sync::RwLock::new(HashMap::new()),
            sandbox_config: std::sync::RwLock::new(SandboxConfigData::default()),
            persistence: Persistence::new(),
            conversation_store,
        }
    }

    /// 获取共享的 Agent 引用（用于 CLI 模式）
    pub fn shared_agent(&self) -> Arc<Mutex<ReactAgent>> {
        self.agent.clone()
    }

    /// 获取工具列表信息
    pub fn get_tool_infos(&self, agent: &ReactAgent) -> Vec<crate::types::ToolInfo> {
        let tool_states = self.tool_states.read().unwrap();

        agent
            .tool_definitions()
            .into_iter()
            .map(|def| {
                let state = tool_states.get(&def.function.name).cloned().unwrap_or(ToolState {
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
}
