//! Tauri 托管状态
//!
//! 在 Tauri 应用中共享 AgentHandle、Persistence 和 AppConfig。

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::persistence::Persistence;

/// Tauri 应用的全局托管状态
pub struct TauriState {
    pub agent: AgentHandle,
    pub persistence: Persistence,
    pub app_config: AppConfig,
}

impl TauriState {
    pub fn new(agent: AgentHandle, persistence: Persistence, app_config: AppConfig) -> Self {
        Self {
            agent,
            persistence,
            app_config,
        }
    }
}
