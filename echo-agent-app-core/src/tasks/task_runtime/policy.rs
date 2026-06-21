//! Execution policy for Chat/Task/Auto routing, approvals, and worker fanout.
//!
//! This is the single product-layer place that explains how a user message is
//! handled. UI snapshots and Tauri chat routing should both read from this
//! type instead of maintaining parallel mode/approval heuristics.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::router::TaskRouteKind;
use super::types::InteractionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, rename = "PermissionMode")]
pub enum PermissionMode {
    Default,
    AutoEdit,
    FullAuto,
    Strict,
}

impl PermissionMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "auto-edit" | "autoedit" | "accept-edits" | "auto-approve" => Self::AutoEdit,
            "full-auto" | "fullauto" | "bypass" => Self::FullAuto,
            "strict" | "strict-confirm" | "strict-confirmation" => Self::Strict,
            _ => Self::Default,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
            Self::Strict => "strict",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Default => "默认审批",
            Self::AutoEdit => "自动编辑",
            Self::FullAuto => "全自动",
            Self::Strict => "严格确认",
        }
    }

    pub fn approval_behavior(&self) -> &'static str {
        match self {
            Self::FullAuto => "尽量不打断执行；工具操作默认通过，框架级硬保护仍保留。",
            Self::AutoEdit => "读取和编辑类操作自动通过；命令、网络和敏感操作按风险询问。",
            Self::Strict => "写入、命令、网络和敏感操作都需要确认。",
            Self::Default => "高风险操作会询问；普通读取和低风险动作尽量不中断。",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    pub interaction_mode: InteractionMode,
    pub permission_mode: PermissionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ExecutionPolicySnapshot")]
pub struct ExecutionPolicySnapshot {
    pub interaction_mode: String,
    pub interaction_mode_id: u8,
    pub interaction_mode_label: String,
    pub permission_mode: String,
    pub permission_mode_label: String,
    pub router_behavior: String,
    pub approval_behavior: String,
    pub parallel_behavior: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeLaunchPolicy {
    pub auto_execute: bool,
    pub approval_policy: String,
}

impl ExecutionPolicy {
    pub fn new(interaction_mode: InteractionMode, permission_mode: impl AsRef<str>) -> Self {
        Self {
            interaction_mode,
            permission_mode: PermissionMode::from_str(permission_mode.as_ref()),
        }
    }

    pub fn from_raw(interaction_mode: u8, permission_mode: impl AsRef<str>) -> Self {
        Self::new(InteractionMode::from_u8(interaction_mode), permission_mode)
    }

    pub fn should_route_runtime(&self) -> bool {
        self.interaction_mode != InteractionMode::Chat
    }

    pub fn snapshot(&self) -> ExecutionPolicySnapshot {
        ExecutionPolicySnapshot {
            interaction_mode: self.interaction_mode.as_str().to_string(),
            interaction_mode_id: self.interaction_mode.as_u8(),
            interaction_mode_label: self.interaction_mode.label().to_string(),
            permission_mode: self.permission_mode.as_str().to_string(),
            permission_mode_label: self.permission_mode.label().to_string(),
            router_behavior: self.router_behavior().to_string(),
            approval_behavior: self.permission_mode.approval_behavior().to_string(),
            parallel_behavior: self.parallel_behavior().to_string(),
        }
    }

    pub fn runtime_launch_policy(
        &self,
        route: TaskRouteKind,
        all_plan_tasks_read_only: bool,
    ) -> RuntimeLaunchPolicy {
        let route_auto_execute = route.should_auto_execute();
        let auto_execute = route_auto_execute && all_plan_tasks_read_only;
        let approval_policy = if auto_execute {
            "只读并行任务已自动执行；后续工具审批仍由当前审批模式控制"
        } else if route_auto_execute {
            "已识别为只读并行路径，但计划包含需确认步骤，等待用户确认"
        } else {
            match self.permission_mode {
                PermissionMode::FullAuto => "工具操作默认自动通过，高风险保护仍会拦截",
                PermissionMode::AutoEdit => "读取和编辑类操作自动通过，高风险操作会询问",
                PermissionMode::Strict => "写入、命令、网络等敏感操作都会询问",
                PermissionMode::Default => "高风险操作会询问，计划执行前需要用户确认",
            }
        };
        RuntimeLaunchPolicy {
            auto_execute,
            approval_policy: approval_policy.to_string(),
        }
    }

    fn router_behavior(&self) -> &'static str {
        match self.interaction_mode {
            InteractionMode::Chat => "强制普通对话，不进入 TaskRuntime。",
            InteractionMode::Task => "强制进入 TaskRuntime；只读大任务会优先并行 worker。",
            InteractionMode::Auto => {
                "由语义路由、确定性信号和历史反馈共同决定 Chat 或 TaskRuntime。"
            }
        }
    }

    fn parallel_behavior(&self) -> &'static str {
        match self.interaction_mode {
            InteractionMode::Chat => "Chat 模式不会自动派生 worker。",
            InteractionMode::Task => "Task 模式会为可并行只读任务生成 runtime-owned worker。",
            InteractionMode::Auto => {
                "Auto 模式会在项目分析、代码审查、研究综述、数据画像等只读大任务上自动并行。"
            }
        }
    }
}
