//! 日志与调试模块
//!
//! 提供详细模式下的 LLM 请求/响应检查功能。
//!
//! # 使用
//!
//! ```rust,ignore
//! let inspector = LlmInspector::new();
//! inspector.set_enabled(true);
//! // ... Agent 交互后
//! println!("{:?}", inspector.stats());
//! ```

pub mod inspector;

pub use inspector::{InspectorStats, LlmCallRecord, LlmInspector, create_record};
