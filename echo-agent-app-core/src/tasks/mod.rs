//! 统一任务系统 — 框架层 re-export + 应用层扩展
//!
//! 将 echo-agent 框架的 Task/TaskManager/TaskExecutor 接入应用层，
//! 通过 BackgroundTaskKind 分发不同类型的后台工作。

// 框架层类型 re-export
pub use echo_agent::tasks::{
    CheckpointStore, ExecutionCheckpoint, LoggingHooks, NoopHooks, SqliteCheckpointStore,
    SqliteTaskStore, Task, TaskContext, TaskEvent, TaskEventBus, TaskExecuteFn, TaskExecutor,
    TaskExecutorConfig, TaskExecutionResult, TaskHookContext, TaskHookRegistry, TaskHooks,
    TaskManager, TaskStatus, TaskStore,
};

pub mod background;
pub mod pipelines;
pub mod service;

// Re-export key types for convenience
pub use background::{BackgroundTaskKind, BackgroundTaskMeta, ResearchOutputFormat};
pub use pipelines::{DataPipelineConfig, ResearchConfig, ResearchToWritingConfig, WritingPipelineConfig};
pub use service::BackgroundTaskService;
