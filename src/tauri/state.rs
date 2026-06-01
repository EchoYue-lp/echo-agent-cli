//! Tauri shared state wrapper.

use echo_agent_app_core::AppState;
use std::sync::Arc;

use super::terminal::TerminalManager;

/// Shared state accessible from all Tauri IPC commands.
pub struct TauriState {
    pub app_state: Arc<AppState>,
    pub terminal_manager: Arc<TerminalManager>,
}

impl TauriState {
    pub fn new(app_state: Arc<AppState>) -> Self {
        Self {
            app_state,
            terminal_manager: Arc::new(TerminalManager::new()),
        }
    }
}
