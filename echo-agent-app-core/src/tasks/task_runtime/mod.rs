//! EKO product-layer TaskRuntime.
//!
//! Interactive, background, scheduled, and pipeline work share the same
//! file-backed run, plan, todo, event, artifact, review, and summary lifecycle.
//! Plan review remains an artifact/tool interaction; it is not encoded as a
//! separate run state.
//!
//! # Module layout
//!
//! - [`types`] — `DomainProfile`, `TaskRunStatus` state machine, the persisted
//!   `TaskRun`/`PlanRevision`/`RunStateSnapshot` projections, and the internal
//!   `TaskPlan`/`PlanTask` spec-execution join used by the runtime.
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
pub mod boot_reconciler;
pub mod command_cells;
pub mod compact_context;
pub mod completion_gate;
pub mod continuation;
pub mod event_rebuild;
pub mod execution_target;
pub mod executor;
pub(crate) mod file_shadow;
pub(crate) mod file_store;
mod history_projection;
pub mod hook_event_dispatcher;
pub mod ledger;
pub mod memory_bridge;
pub mod planner;
pub mod profiles;
pub mod register;
pub mod review;
pub mod revisioned_adapter;
mod root_authority;
mod run_authority;
pub mod store;
pub mod subagent_control;
pub mod task_execute_tool;
pub mod task_tools;
pub(crate) mod turn_lifecycle;
pub mod types;
pub mod worktree;

#[cfg(test)]
mod long_horizon_contracts;

pub use boot_reconciler::{TaskRunBootOutcome, TaskRunBootReconciler};
pub use command_cells::{AwaiterSurfaceProjection, project_awaiter_surface_event};
pub use completion_gate::requirements_for_plan;
pub use execution_target::TaskExecutionTargetResolver;
pub(crate) use executor::drive_unattended_run;
pub use executor::{
    EkoExecutionLimits, ExecError, ExecSink, PlannedRunResumeLaunch, PreflightRejection,
    ProcessExecutionResourceSnapshot, RunOutcome, TaskRuntimeBlockingAdapter, execute_run,
    launch_planned_run_resume, preflight_unattended_plan, preflight_unattended_task,
    process_execution_resource_snapshot,
};
pub use ledger::{export_path, render_progress, write_progress};
pub use memory_bridge::{
    MemoryEvent, MemoryPolicy, write_memory_candidate_dispatch, write_memory_candidate_settled,
};
pub use planner::{
    FileOverlapPair, OwnershipReport, analyze_file_ownership, has_writer_file_overlap,
};
pub use profiles::ProfileTemplate;
pub use register::{
    bind_task_execute_to_pool, register_task_tools_on_agent, task_revision_service_for_agent,
};
pub use review::{BreakerAction, ReviewError, build_fix_task, requires_review, review_task};
pub use revisioned_adapter::{
    apply_eko_task_update, build_eko_task_revision_service, commit_eko_task_plan,
};
pub use store::{
    AbandonedRunSettlement, BootAutoResumeBlocker, BootAutoResumeDecision, BootAutoResumeOutcome,
    ProviderRetryDisposition, RunDriverReceiptOwner, StoreError, TaskRetryPreparation,
    TaskRunDriverShutdownError, TaskRuntimeStore,
};
pub use subagent_control::SubagentControlService;
pub use task_execute_tool::ExecuteTaskTool;
pub use types::*;
