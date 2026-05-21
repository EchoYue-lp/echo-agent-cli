//! Echo Agent CLI
//!
//! 提供 Web、CLI (REPL/TUI)、IM Channel 三种交互模式。

pub mod agent_handle;
pub mod cli;
pub mod config;
pub mod config_watcher;
pub mod error;
pub mod infra;
pub mod logging;
pub mod metrics;
pub mod output;
pub mod persistence;
pub mod profiles;
pub mod project;
pub mod scheduler;
pub mod sessions;
pub mod skills_hub;
pub mod shell;
pub mod tui;
pub mod routes;
pub mod security;
pub mod state;
pub mod types;
pub mod webhook;
pub mod ws;

pub mod tauri;

pub use state::AppState;
