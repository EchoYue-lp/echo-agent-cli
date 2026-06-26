//! TUI/GUI 产品入口与工具子命令。
//!
//! 默认用户入口是全屏 TUI；GUI 使用 Tauri 入口。旧 REPL 运行模式保留为内部兼容实现。

pub mod args;
#[cfg(feature = "channels")]
pub mod channels;
pub mod cmd_impls;
pub mod command;
pub mod commands;
pub mod completion;
pub mod editor;
pub mod git_ops;
pub mod keybindings;
pub mod modes;
pub mod repl;

pub use args::Args;
#[cfg(feature = "channels")]
pub use modes::run_channels_mode;
pub use modes::run_cli_mode;
pub use repl::{ReplConfig, run_repl};
