//! Web CLI - 基于 echo-agent 框架的 Web 终端
//!
//! 提供阻塞式和流式对话接口，支持人工介入、工具调用可视化等功能。

pub mod error;
pub mod persistence;
pub mod routes;
pub mod state;
pub mod types;
pub mod ws;

pub use state::AppState;