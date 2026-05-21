//! MCP 服务器管理命令

use tauri::State;
use super::super::state::TauriState;

#[tauri::command]
pub async fn list_mcp_servers(state: State<'_, TauriState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.agent.inner().read().await;
    let servers: Vec<serde_json::Value> = guard.mcp_server_names().iter().map(|name| {
        serde_json::json!({
            "name": name.to_string(),
            "connected": true,
        })
    }).collect();
    Ok(servers)
}

#[tauri::command]
pub async fn connect_mcp_server(
    _state: State<'_, TauriState>,
    _config_json: String,
) -> Result<String, String> {
    // MCP connect not available via direct agent API on this branch.
    // Use the CLI's infra::load_mcp_config or Web API instead.
    Err("MCP connect via IPC not yet available — use Web API or echo-agent.yaml config".into())
}

#[tauri::command]
pub async fn disconnect_mcp_server(
    _state: State<'_, TauriState>,
    _name: String,
) -> Result<(), String> {
    // MCP disconnect not available via direct agent API on this branch.
    Err("MCP disconnect via IPC not yet available".into())
}
