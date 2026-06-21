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
pub mod classify;
pub mod delegation;
pub mod executor;
pub mod hitrisk;
pub mod ledger;
pub mod memory_bridge;
pub mod planner;
pub mod profiles;
pub mod review;
pub mod router;
pub mod signals;
pub mod store;
pub mod types;

pub use classify::{Classification, Complexity, ComplexityLabel, HeuristicClassifier};
pub use delegation::{DelegationPlanner, DelegationRequest, WorkerSpec};
pub use executor::{ConcurrencyLimits, ExecError, RunOutcome, WorkerTraceSink, execute_run};
pub use hitrisk::{HighRiskMatch, check as check_high_risk, requires_fresh_approval};
pub use ledger::{export_path, render_progress, write_progress};
pub use memory_bridge::{MemoryEvent, write_memory_candidate};
pub use planner::{GeneratedPlan, PlanError, generate_parallel_readonly_plan, generate_plan};
pub use profiles::ProfileTemplate;
pub use review::{
    BreakerAction, ReviewError, build_fix_task, circuit_breaker_action, requires_review,
    review_task,
};
pub use router::{TaskRouteDecision, TaskRouteKind, route_message};
pub use store::{StoreError, TaskRuntimeStore};
pub use types::*;
