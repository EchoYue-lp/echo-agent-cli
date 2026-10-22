//! Cron/定时任务调度模块
//!
//! 提供基于 cron 表达式的定时 Agent 任务调度。
//! Types and store are re-exported from the framework (`echo_agent::scheduler`);
//! `runner` provides the CLI-specific [`runner::build_occurrence_fire_fn`]
//! adapter。

pub mod runner;

pub use echo_agent::scheduler::{CronTask, CronTaskStatus, CronTaskStore};
pub use runner::{SchedulerRunner, build_occurrence_fire_fn, new_scheduler_runner};

/// Conversation-id prefix owned by the cron scheduler adapter.
///
/// Cron TaskRuns are addressed by this prefix so ordinary boot recovery can
/// tell them apart from chat/background runs: a cron slot is owned by its
/// durable occurrence admission, never by the ordinary recovery launcher.
pub(crate) const CRON_CONVERSATION_PREFIX: &str = "cron:";

/// Stable conversation id for one cron occurrence slot.
///
/// Derived from the durable occurrence id, not from the delivery attempt, so a
/// retried delivery reconstructs exactly the same immutable run identity and
/// `get_or_create_occurrence_run` reads the existing run instead of rejecting
/// the slot as retargeted.
pub(crate) fn cron_conversation_id(scheduler_task_id: &str, occurrence_id: &str) -> String {
    format!("{CRON_CONVERSATION_PREFIX}{scheduler_task_id}:{occurrence_id}")
}
