//! 工具列表命令

use super::super::state::TauriState;
use tauri::State;

#[tauri::command]
pub async fn list_tools(state: State<'_, TauriState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.agent.inner().read().await;
    let tools: Vec<serde_json::Value> = guard
        .tool_names()
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name.to_string(),
                "enabled": true,
            })
        })
        .collect();
    Ok(tools)
}
