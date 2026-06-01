//! TUI/GUI 产品入口与工具子命令。
//!
//! 默认用户入口是全屏 TUI；GUI 使用 Tauri 入口。旧 REPL/Web 运行模式保留为内部兼容实现。

pub mod args;
pub mod cmd_impls;
pub mod command;
pub mod commands;
pub mod completion;
pub mod editor;
pub mod eval;
pub mod export;
pub mod git_ops;
pub mod handlers;
pub mod keybindings;
pub mod modes;
pub mod onboard;
pub mod repl;
pub mod router;

pub use args::{Args, Commands, ProfileAction, SessionAction};
pub use handlers::{
    handle_completions_command, handle_profile_action, handle_run_command, handle_session_action,
    handle_subcommand,
};
#[cfg(feature = "channels")]
pub use modes::run_channels_mode;
pub use modes::{run_both_modes, run_cli_mode, run_headless_mode, run_web_mode};
pub use repl::{ReplConfig, run_repl};
pub use router::build_router;
