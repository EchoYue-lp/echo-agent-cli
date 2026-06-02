//! Cron task types and storage — re-exported from the framework.
//!
//! `TaskStore` is an alias for [`CronTaskStore`] to preserve backward
//! compatibility with callers that use the old name.

pub use echo_agent::scheduler::{CronTask, CronTaskStatus, CronTaskStore as TaskStore};
