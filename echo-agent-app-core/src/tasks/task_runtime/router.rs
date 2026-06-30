//! Route kinds persisted on a run.
//!
//! Phase B4 removed the LLM route pre-judgment + the entire routing pipeline
//! (`route_message` / `route_message_with_feedback` / the `TaskRouteDecision`
//! struct / `classify.rs` / the route-feedback learning subsystem): chat now
//! routes through `drive_chat` and the agent decides complexity itself via
//! `create_complex_task`. What remains here is the *value type* still used as
//! the persisted `run.route` string (e.g. `ParallelReadonlyDelegation.as_str()`
//! or `"agent_autonomous"`) and as the approval-gate policy token:
//! `execute_plan_tool` reads `run.route == ComplexRuntime` to pause for
//! approval. Variants whose only consumer was the removed router are kept for
//! now to preserve `from_str` parsing of any persisted value (removing them is
//! a separate cosmetic cleanup).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Runtime path persisted on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRouteKind")]
pub enum TaskRouteKind {
    /// Normal streaming chat; no first-class runtime run is created.
    NormalChat,
    /// Generate a plan and stop for user review.
    PlanOnly,
    /// Generate a TaskRuntime plan and wait for explicit approval.
    ComplexRuntime,
    /// Generate a read-only parallel plan and auto-launch workers.
    ParallelReadonlyDelegation,
    /// Reserved for long-running detached agents.
    BackgroundTask,
    /// Reserved for direct small edits on the main agent path.
    DirectEdit,
}

impl TaskRouteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NormalChat => "normal_chat",
            Self::PlanOnly => "plan_only",
            Self::ComplexRuntime => "complex_runtime",
            Self::ParallelReadonlyDelegation => "parallel_readonly_delegation",
            Self::BackgroundTask => "background_task",
            Self::DirectEdit => "direct_edit",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "normal_chat" => Self::NormalChat,
            "plan_only" => Self::PlanOnly,
            "complex_runtime" => Self::ComplexRuntime,
            "parallel_readonly_delegation" => Self::ParallelReadonlyDelegation,
            "background_task" => Self::BackgroundTask,
            "direct_edit" => Self::DirectEdit,
            _ => return None,
        })
    }

    pub fn should_create_runtime_run(&self) -> bool {
        matches!(
            self,
            Self::PlanOnly | Self::ComplexRuntime | Self::ParallelReadonlyDelegation
        )
    }

    pub fn should_auto_execute(&self) -> bool {
        matches!(self, Self::ParallelReadonlyDelegation)
    }
}
