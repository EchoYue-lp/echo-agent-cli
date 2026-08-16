pub mod cli;
pub mod logging;
pub mod task_run_control;

#[cfg(feature = "gui")]
// `#[tauri::command]` expands to dispatch glue containing `unreachable!()`.
// Keep the exception at the generated IPC boundary; application/core code
// remains covered by the workspace's strict panic lint gate.
#[allow(clippy::unreachable)]
pub mod tauri;

#[cfg(feature = "tui")]
pub mod tui;

// Re-export from app-core
pub use echo_agent_app_core::{
    AppState, agent_handle, config_watcher, error, infra, output, persistence, profiles, project,
    scheduler, sessions, skills_hub, state, tasks, types, webhook,
};
