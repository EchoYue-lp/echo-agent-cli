//! 权限管理 Tauri 命令
//!
//! Note: Full permission management is available through the Web API.
//! These IPC commands provide read-only status for the Tauri frontend.

use super::super::state::TauriState;
use tauri::State;

#[tauri::command]
pub async fn get_permission_status(
    state: State<'_, TauriState>,
) -> Result<serde_json::Value, String> {
    let guard = state.agent.inner().read().await;
    let cfg = guard.config();
    Ok(serde_json::json!({
        "human_in_loop": cfg.is_human_in_loop_enabled(),
        "tool_enabled": cfg.is_tool_enabled(),
        "task_enabled": cfg.is_task_enabled(),
        "subagent_enabled": cfg.is_subagent_enabled(),
    }))
}
