//! EKO product-layer TaskRuntime.
//!
//! Interactive, background, scheduled, and pipeline work share the same
//! file-backed run, plan, todo, event, artifact, review, and summary lifecycle.
//! Plan review remains an artifact/tool interaction; it is not encoded as a
//! separate run state.
//!
//! # Module layout
//!
//! - [`types`] — `DomainProfile`, `TaskRunStatus` state machine, and all
//!   persisted structs (`TaskRun`, `TaskPlan`, `PlanTask`, `TodoItem`,
//!   `RuntimeTaskEvent`, `Artifact`, `ReviewResult`, `TaskExecutionSummary`).
//! - [`store`] — file-backed `TaskRuntimeStore`; every state mutation appends a
//!   `RuntimeTaskEvent` before rebuilding the projections.
//! - [`profiles`] — per-domain plan templates (subagent roles, prompt suffix,
//!   review checklist).
//! - [`planner`] — structured plan generation via a JSON-mode LLM call, with
//!   plan-quality validation.
//! - [`executor`] — EKO controller/dispatcher adapter for the framework DAG
//!   executor, including review, resource limits, worktrees, and event mapping.
//! - [`review`] — review gates (spec + code quality) + retry circuit breaker.
//! - [`ledger`] — progress.md export derived from canonical run files.
//! - [`memory_bridge`] — sinks run/task lifecycle events into long-term
//!   memory via the single `MemoryLayerManager::write_memory` chokepoint.
//!
//! # Naming
//!
//! The framework already re-exports a `TaskEvent` from `echo_agent::tasks`.
//! To avoid shadowing, this module's event type is named `RuntimeTaskEvent`
//! and its event-kind enum is `RuntimeEventKind`.
pub mod compact_context;
pub mod event_rebuild;
pub mod executor;
pub mod file_shadow;
pub mod file_store;
pub mod ledger;
pub mod memory_bridge;
pub mod planner;
pub mod profiles;
pub mod register;
pub mod review;
pub mod revisioned_adapter;
pub mod store;
pub mod task_execute_tool;
pub mod task_tools;
pub mod types;
pub mod worktree;

pub use executor::{
    EkoExecutionLimits, ExecError, ExecSink, PreflightRejection, RunOutcome, drive_unattended_run,
    execute_run, launch_cron_run, launch_unattended_run, preflight_unattended_plan,
    preflight_unattended_task,
};
pub use ledger::{export_path, render_progress, write_progress};
pub use memory_bridge::{
    MemoryEvent, MemoryPolicy, write_memory_candidate, write_memory_candidate_blocking,
    write_memory_candidate_dispatch,
};
pub use planner::{
    FileOverlapPair, OwnershipReport, analyze_file_ownership, has_writer_file_overlap,
};
pub use profiles::ProfileTemplate;
pub use register::{register_task_tools_on_agent, task_revision_service_for_agent};
pub use review::{
    BreakerAction, ReviewError, build_fix_task, circuit_breaker_action, requires_review,
    review_task,
};
pub use revisioned_adapter::{
    apply_eko_task_update, build_eko_task_revision_service, commit_eko_task_plan,
};
pub use store::{StoreError, TaskRuntimeStore};
pub use task_execute_tool::ExecuteTaskTool;
pub use types::*;
