pub mod chrome_native_host;
pub mod cli;
pub mod logging;

#[cfg(feature = "gui")]
pub mod tauri;

#[cfg(feature = "tui")]
pub mod tui;

// Re-export from app-core
pub use echo_agent_app_core::{
    AppState, agent_handle, config, config_watcher, error, infra, output, persistence, profiles,
    project, scheduler, sessions, skills_hub, state, tasks, types, webhook,
};
