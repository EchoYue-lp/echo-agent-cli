//! Real-time progress reporting — re-exported from the framework.
//!
//! The authoritative types live in `echo_orchestration::tasks::progress`
//! (exposed via `echo_agent::tasks::progress`). This module re-exports
//! them for use throughout this crate.

pub use echo_agent::tasks::progress::{ProgressReporter, TaskProgress};
