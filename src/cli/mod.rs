//! 命令行交互模块
//!
//! 提供 REPL (Read-Eval-Print-Loop) 交互界面，支持：
//! - 多轮对话
//! - 斜杠命令（/help, /reset, /tools 等）
//! - 流式输出
//! - 富文本格式化

pub mod commands;
pub mod repl;

pub use repl::{run_repl, ReplConfig};