//! Route kinds persisted on a run.
//!
//! Phase B4 removed the LLM route pre-judgment + the entire routing pipeline
//! (`route_message` / `route_message_with_feedback` / the `TaskRouteDecision`
//! struct / `classify.rs` / the route-feedback learning subsystem): chat now
//! routes through `drive_chat` and the agent decides complexity itself via
//! `create_complex_task`. What remains here is the *value type* still used as
//! the persisted `run.route` string and as the approval-gate policy token:
//! `execute_plan_tool` reads `run.route == ComplexRuntime` to pause for
//! approval. The variants whose only consumer was the removed router
//! (`NormalChat` / `PlanOnly` / `BackgroundTask` / `DirectEdit`) were never
//! persisted by any live caller and are now removed (cosmetic prune). The two
//! kept variants are the only ones persisted (`ParallelReadonlyDelegation` by
//! submit_run/cron, `complex_runtime` by the legacy file_shadow/event_rebuild
//! approval-gate path). `from_str` falls through to `None` for any legacy
//! persisted value, which `execute_plan_tool` unwraps to
//! `ParallelReadonlyDelegation` — the safe no-approval default.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Runtime path persisted on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRouteKind")]
pub enum TaskRouteKind {
    /// Generate a TaskRuntime plan and wait for explicit approval.
    ComplexRuntime,
    /// Generate a read-only parallel plan and auto-launch workers.
    ParallelReadonlyDelegation,
}

impl TaskRouteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ComplexRuntime => "complex_runtime",
            Self::ParallelReadonlyDelegation => "parallel_readonly_delegation",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "complex_runtime" => Self::ComplexRuntime,
            "parallel_readonly_delegation" => Self::ParallelReadonlyDelegation,
            _ => return None,
        })
    }

    pub fn should_create_runtime_run(&self) -> bool {
        matches!(
            self,
            Self::ComplexRuntime | Self::ParallelReadonlyDelegation
        )
    }

    pub fn should_auto_execute(&self) -> bool {
        matches!(self, Self::ParallelReadonlyDelegation)
    }
}
