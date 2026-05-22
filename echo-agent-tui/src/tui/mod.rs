//! 终端 UI (TUI) 模式
//!
//! 基于 `ratatui` 的全屏终端界面，支持：
//! - 分屏布局 (对话 70% | 工具/上下文 30%)
//! - 流式 Token 渲染
//! - 工具调用实时展示
//! - 键盘快捷键

pub mod app;
pub mod panels;
pub mod status_bar;
pub mod theme;

pub use app::run_tui;
