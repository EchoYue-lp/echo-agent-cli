//! EKO application task system.
//!
//! Product task persistence and policy are owned by `task_runtime`; dependency
//! traversal, revision safe points, Subagent waves, cancellation, and stall
//! detection use the shared `echo-agent` runtime DAG executor.

pub mod background;
pub mod service;
pub mod task_runtime;

// Re-export key types for convenience
pub use background::{BackgroundTaskKind, ResearchOutputFormat};
pub use service::{BackgroundTaskService, UnifiedTaskInfo};
pub use task_runtime::TaskRuntimeStore;
