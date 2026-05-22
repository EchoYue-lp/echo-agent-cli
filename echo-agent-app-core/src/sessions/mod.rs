//! 增强会话管理模块
//!
//! 提供会话的创建、分支、差异对比、自动保存和导出功能。
//! 数据存储在 `~/.echo-agent/sessions_v2/`。

pub mod export;
pub mod manager;
pub mod search;
pub mod types;

pub use export::SessionExporter;
pub use manager::SessionManager;
pub use search::{SearchResult, SessionSearchEngine};
pub use types::{DiffHunk, DiffLine, Session, SessionDiff, SessionMessage, SessionSummary};
