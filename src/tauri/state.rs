//! Tauri shared state wrapper.

use echo_agent_app_core::{AppState, browser::BrowserRuntime};
use std::sync::Arc;

use super::terminal::TerminalManager;

/// Shared state accessible from all Tauri IPC commands.
pub struct TauriState {
    pub app_state: Arc<AppState>,
    pub browser_runtime: Arc<BrowserRuntime>,
    pub terminal_manager: Arc<TerminalManager>,
}

impl TauriState {
    pub fn new(app_state: Arc<AppState>, browser_runtime: Arc<BrowserRuntime>) -> Self {
        Self {
            app_state,
            browser_runtime,
            terminal_manager: Arc::new(TerminalManager::new()),
        }
    }
}
