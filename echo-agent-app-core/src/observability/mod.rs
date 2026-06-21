//! Agent 执行可观测性模块
//!
//! 提供执行轨迹的收集、分析和实时推送功能。

pub mod collector;
pub mod diagnostics;
pub mod types;

pub use collector::TraceCollector;
pub use diagnostics::{
    CacheDiagnostics, CacheFingerprintChanges, CacheIssue, CacheIssueKind,
    compute_cache_diagnostics,
};
pub use types::{ContentFingerprint, TraceEvent, TraceKind, TraceSummary};
