//! EKO application task system.
//!
//! Product task lifecycle is owned by `task_runtime`. The framework's generic
//! task APIs remain in `echo-agent` and are not re-exported as an EKO lifecycle.

pub mod background;
pub mod service;
pub mod task_runtime;

// Re-export key types for convenience
pub use background::{BackgroundTaskKind, ResearchOutputFormat};
pub use service::{BackgroundTaskService, UnifiedTaskInfo};
pub use task_runtime::TaskRuntimeStore;
