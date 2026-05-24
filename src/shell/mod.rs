//! Shell 集成模块
//!
//! 提供 Shell 补全脚本生成和 Unix 管道模式支持。

pub mod completions;
pub mod pipe;

pub use completions::{ShellType, generate_all, generate_completion, print_install_hint};
pub use pipe::{PipeConfig, run_pipe, stdin_has_data};
