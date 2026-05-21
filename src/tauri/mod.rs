//! Tauri 桌面应用 — 命令模块
//!
//! 前端为 React (Vite) SPA，位于 `web-frontend/` 目录。
//! Rust 后端通过 Tauri IPC commands 暴露 Agent API。

pub mod commands;
pub mod state;

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::persistence::Persistence;

/// 构建已配置好的 Tauri Builder（由 src-tauri/src/main.rs 调用）
pub fn build_tauri(
    agent_handle: AgentHandle,
    persistence: Persistence,
    app_config: AppConfig,
) -> tauri::Builder<tauri::Wry> {
    let state = state::TauriState::new(agent_handle, persistence, app_config);

    commands::register_commands(tauri::Builder::default())
        .manage(state)
        .plugin(tauri_plugin_shell::init())
}
