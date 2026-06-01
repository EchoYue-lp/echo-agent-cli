//! Agent 执行可观测性模块
//!
//! 提供执行轨迹的收集、分析和实时推送功能。

pub mod collector;
pub mod types;

pub use collector::TraceCollector;
pub use types::{TraceEvent, TraceKind, TraceSummary};
