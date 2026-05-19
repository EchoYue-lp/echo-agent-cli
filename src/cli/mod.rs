//! 命令行交互模块
//!
//! 提供 REPL (Read-Eval-Print-Loop) 交互界面，支持：
//! - 多轮对话
//! - 斜杠命令（/help, /reset, /tools 等）
//! - 流式输出
//! - 富文本格式化
//!
//! 同时包含 CLI 参数解析、子命令处理、运行模式和路由构建。

pub mod args;
pub mod commands;
pub mod completion;
pub mod editor;
pub mod handlers;
pub mod keybindings;
pub mod modes;
pub mod onboard;
pub mod repl;
pub mod router;

pub use args::{Args, Commands, ProfileAction, SessionAction};
pub use handlers::{
    handle_completions_command, handle_profile_action, handle_run_command,
    handle_session_action, handle_subcommand,
};
pub use modes::{run_both_modes, run_cli_mode, run_web_mode};
#[cfg(feature = "channels")]
pub use modes::run_channels_mode;
pub use repl::{ReplConfig, run_repl};
pub use router::build_router;
