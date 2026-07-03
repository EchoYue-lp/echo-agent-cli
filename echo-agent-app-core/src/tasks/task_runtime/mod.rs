//! Eko TaskRuntime — complex-task execution runtime.
//!
//! This is the new product-layer core that turns complex user requests into a
//! first-class *run → plan → approve → execute → review → synthesize*
//! lifecycle, replacing the ad-hoc "chat agent + scattered task modules"
//! shape.
//!
//! PR 1 ships only the **data model + canonical SQLite store**. Planning,
//! DAG execution, review gates, HITL/memory integration, and GUI
//! productization arrive in subsequent PRs (see
//! `docs/superpowers/plans/eko-taskruntime-2-plan.md`).
//!
//! # Module layout
//!
//! - [`types`] — `DomainProfile`, `TaskRunStatus` state machine, and all
//!   persisted structs (`TaskRun`, `TaskPlan`, `PlanTask`, `TodoItem`,
//!   `RuntimeTaskEvent`, `Artifact`, `ReviewResult`, `TaskExecutionSummary`).
//! - [`store`] — `TaskRuntimeStore`, the SQLite-backed canonical store. Every
//!   state mutation appends a `RuntimeTaskEvent` inside the same transaction.
//! - [`profiles`] — per-domain plan templates (worker roles, prompt suffix,
//!   review checklist).
//! - [`classify`] — input classifier (simple vs complex) + domain inference.
//! - [`planner`] — structured plan generation via a JSON-mode LLM call, with
//!   plan-quality validation.
//! - [`executor`] — DAG scheduler that runs an approved plan on pooled
//!   workers with concurrency limits, write serialization, and cancellation.
//! - [`review`] — review gates (spec + code quality) + retry circuit breaker.
//! - [`ledger`] — progress.md export, derived from the canonical SQLite state.
//! - [`hitrisk`] — high-risk argument re-checker (forces fresh approval even
//!   under a session-level grant for destructive patterns).
//! - [`memory_bridge`] — sinks run/task lifecycle events into long-term
//!   memory via the single `MemoryLayerManager::write_memory` chokepoint.
//!
//! # Naming
//!
//! The framework already re-exports a `TaskEvent` from `echo_agent::tasks`.
//! To avoid shadowing, this module's event type is named `RuntimeTaskEvent`
//! and its event-kind enum is `RuntimeEventKind`.
pub mod event_rebuild;
pub mod execute_plan_tool;
pub mod executor;
pub mod file_shadow;
pub mod file_store;
pub mod hitrisk;
pub mod ledger;
pub mod memory_bridge;
pub mod planner;
pub mod policy;
pub mod profiles;
pub mod register;
pub mod review;
pub mod router;
pub mod store;
pub mod task_tools;
pub mod types;
pub mod worktree;

pub use execute_plan_tool::ExecutePlanTool;
pub use executor::{
    ConcurrencyLimits, ExecError, ExecSink, PreflightRejection, RunOutcome, drive_unattended_run,
    execute_run, launch_cron_run, launch_unattended_run, preflight_unattended_plan,
    preflight_unattended_task,
};
pub use hitrisk::{HighRiskMatch, check as check_high_risk, requires_fresh_approval};
pub use ledger::{export_path, render_progress, write_progress};
pub use memory_bridge::{
    MemoryEvent, MemoryPolicy, write_memory_candidate, write_memory_candidate_blocking,
    write_memory_candidate_dispatch,
};
pub use planner::{
    FileOverlapPair, OwnershipReport, analyze_file_ownership, has_writer_file_overlap,
    validate_plan_deps,
};
pub use policy::{ExecutionPolicy, ExecutionPolicySnapshot, PermissionMode, RuntimeLaunchPolicy};
pub use profiles::ProfileTemplate;
pub use register::register_task_tools_on_agent;
pub use review::{
    BreakerAction, ReviewError, build_fix_task, circuit_breaker_action, requires_review,
    review_task,
};
pub use router::TaskRouteKind;
pub use store::{StoreError, TaskRuntimeStore};
pub use types::*;
