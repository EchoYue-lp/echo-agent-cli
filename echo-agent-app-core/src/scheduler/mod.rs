//! Cron/定时任务调度模块
//!
//! 提供基于 cron 表达式的定时 Agent 任务调度。
//! Types and store are re-exported from the framework (`echo_agent::scheduler`);
//! `runner` provides the CLI-specific `build_fire_fn` adapter.

pub mod runner;
pub mod task;

pub use echo_agent::scheduler::{CronTask, CronTaskStatus, CronTaskStore};
pub use runner::{SchedulerRunner, build_fire_fn, new_scheduler_runner};
// Backward-compatible alias for callers that still use `task::TaskStore`.
pub use task::TaskStore;
