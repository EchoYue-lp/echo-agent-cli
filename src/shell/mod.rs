//! Shell 集成模块
//!
//! 提供 Shell 补全脚本生成和 Unix 管道模式支持。

pub mod completions;
pub mod pipe;

pub use completions::{generate_completion, generate_all, print_install_hint, ShellType};
pub use pipe::{run_pipe, stdin_has_data, PipeConfig};
