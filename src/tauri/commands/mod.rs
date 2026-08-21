//! Tauri IPC command modules.
//!
//! Each module provides `#[tauri::command]` functions that:
//! - Deserialize parameters
//! - Call into `echo-agent-app-core` via `AppState`
//! - Convert errors to `IpcError`
//! - Return DTOs

pub mod agent_router;
pub mod analysis;
pub mod browser;
pub mod chat;
pub mod config;
pub mod conversations;
pub mod files;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod panels;
pub mod plugins;
pub mod providers;
pub mod research;
pub mod scheduler;
pub mod session;
pub mod task_runtime;
pub mod tasks;
pub mod tool_executions;
pub mod tools;
pub mod workspace;
