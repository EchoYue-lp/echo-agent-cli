//! Tauri IPC commands for MCP server management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::types::McpTransportConfig;

#[tauri::command]
pub async fn list_mcp_servers(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mcp_config = state.app_state.plugins.mcp_config.read().await;
    let mcp_health = state.app_state.plugins.mcp_health.read().await;

    let servers: Vec<serde_json::Value> = state
        .app_state
        .connection
        .agent
        .read(|agent| {
            let connected_names: Vec<String> = agent
                .list_mcp_servers()
                .into_iter()
                .map(|s| s.to_string())
                .collect();

            let mut result: Vec<serde_json::Value> = Vec::new();

            // 1. Connected servers (from agent)
            for name in &connected_names {
                let health_entry = mcp_health.get(name.as_str());
                let status = if let Some(h) = health_entry.as_ref() {
                    if h.healthy { "connected" } else { "error" }
                } else {
                    "disconnected"
                };
                let transport = mcp_config
                    .mcp_servers
                    .get(name.as_str())
                    .map(|e| infer_transport(e))
                    .unwrap_or("stdio");
                let error = health_entry.as_ref().and_then(|h| h.error.clone());

                // Get actual tools from the MCP client
                let (tools, tool_count) = if let Some(client) = agent.mcp_client(name) {
                    let mcp_tools = client.tools();
                    let count = mcp_tools.len();
                    let tools: Vec<serde_json::Value> = mcp_tools
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "name": t.name,
                                "description": t.description.as_deref().unwrap_or(""),
                            })
                        })
                        .collect();
                    (tools, count)
                } else {
                    (vec![], 0)
                };

                result.push(serde_json::json!({
                    "name": name,
                    "status": status,
                    "transport": transport,
                    "tool_count": tool_count,
                    "tools": tools,
                    "connected_at": null,
                    "error": error,
                    "enabled": true,
                }));
            }

            // 2. Configured but not connected servers (disabled or not yet connected)
            for (config_name, entry) in &mcp_config.mcp_servers {
                if !connected_names.contains(config_name) {
                    let transport = infer_transport(entry);
                    result.push(serde_json::json!({
                        "name": config_name,
                        "status": if entry.disabled { "disabled" } else { "disconnected" },
                        "transport": transport,
                        "tool_count": 0,
                        "tools": [],
                        "connected_at": null,
                        "error": null,
                        "enabled": !entry.disabled,
                    }));
                }
            }

            result
        })
        .await;

    Ok(serde_json::json!(servers))
}

fn infer_transport(entry: &echo_agent::mcp::McpServerEntry) -> &'static str {
    if entry.url.is_some() {
        if entry.transport.as_deref() == Some("sse") {
            "sse"
        } else {
            "http"
        }
    } else if entry.command.is_some() {
        "stdio"
    } else {
        "unknown"
    }
}

#[tauri::command]
pub async fn connect_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
    transport: McpTransportConfig,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent::mcp::McpServerConfig;

    let config = match transport {
        McpTransportConfig::Stdio { command, args, env } => {
            let env_pairs: Vec<(String, String)> = env.into_iter().map(|(k, v)| (k, v)).collect();
            McpServerConfig::stdio_with_env(&name, &command, args, env_pairs)
        }
        McpTransportConfig::Http { url, headers } => {
            McpServerConfig::http_with_headers(&name, &url, headers)
        }
        McpTransportConfig::Sse { url, headers } => {
            McpServerConfig::sse_with_headers(&name, &url, headers)
        }
    };

    let connect_result = state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let config = config.clone();
            Box::pin(async move { agent.connect_mcp_from_config(config).await })
        })
        .await;

    match connect_result {
        Ok(_) => Ok(serde_json::json!({"success": true, "name": name})),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "error": format!("Failed to connect to MCP server '{}': {}", name, e),
        })),
    }
}

#[tauri::command]
pub async fn disconnect_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let name = name.clone();
            Box::pin(async move {
                agent.disconnect_mcp(&name).await;
            })
        })
        .await;

    {
        let mut health = state.app_state.plugins.mcp_health.write().await;
        health.remove(&name);
    }

    Ok(serde_json::json!({
        "success": true,
        "message": format!("Disconnected from MCP server '{}'", name),
    }))
}

/// Toggle MCP server enabled/disabled — takes effect immediately.
///
/// When `enabled = true`: connects the server (if configured in mcp_config).
/// When `enabled = false`: disconnects the server and marks it as disabled in config.
#[tauri::command]
pub async fn toggle_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
    enabled: bool,
) -> Result<serde_json::Value, IpcError> {
    // Update the disabled flag in config
    {
        let mut cfg = state.app_state.plugins.mcp_config.write().await;
        if let Some(entry) = cfg.mcp_servers.get_mut(&name) {
            entry.disabled = !enabled;
        } else if enabled {
            return Err(IpcError::NotFound(format!(
                "MCP server '{}' not found in config",
                name
            )));
        }
    }

    if enabled {
        // Connect: get the server config and connect via agent
        let server_config = {
            let cfg = state.app_state.plugins.mcp_config.read().await;
            cfg.mcp_servers
                .get(&name)
                .and_then(|e| e.to_server_config(&name).ok())
        };

        if let Some(config) = server_config {
            let result = state
                .app_state
                .connection
                .agent
                .write_async(|agent| {
                    Box::pin(async move { agent.connect_mcp_from_config(config).await })
                })
                .await;

            match result {
                Ok(_) => Ok(serde_json::json!({
                    "success": true,
                    "enabled": true,
                    "message": format!("MCP server '{}' enabled and connected", name),
                })),
                Err(e) => Ok(serde_json::json!({
                    "success": true,
                    "enabled": true,
                    "message": format!("MCP server '{}' enabled but connection failed: {}", name, e),
                })),
            }
        } else {
            Ok(serde_json::json!({
                "success": true,
                "enabled": true,
                "message": format!("MCP server '{}' enabled (no config to connect)", name),
            }))
        }
    } else {
        // Disconnect: remove from agent and health
        state
            .app_state
            .connection
            .agent
            .write_async(|agent| {
                let name = name.clone();
                Box::pin(async move {
                    agent.disconnect_mcp(&name).await;
                })
            })
            .await;

        {
            let mut health = state.app_state.plugins.mcp_health.write().await;
            health.remove(&name);
        }

        Ok(serde_json::json!({
            "success": true,
            "enabled": false,
            "message": format!("MCP server '{}' disabled and disconnected", name),
        }))
    }
}

#[tauri::command]
pub async fn get_mcp_config(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.plugins.mcp_config.read().await;
    serde_json::to_value(&*config).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn update_mcp_config(
    state: tauri::State<'_, TauriState>,
    config: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let new_config: echo_agent::mcp::McpConfigFile =
        serde_json::from_value(config).map_err(|e| IpcError::Validation(e.to_string()))?;

    {
        let mut cfg = state.app_state.plugins.mcp_config.write().await;
        *cfg = new_config.clone();
    }

    // Reconnect with new config
    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let cfg = new_config.clone();
            Box::pin(async move {
                // Disconnect all existing
                let names: Vec<String> = agent
                    .list_mcp_servers()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                for name in names {
                    agent.disconnect_mcp(&name).await;
                }
                // Reconnect from config
                for (name, entry) in &cfg.mcp_servers {
                    if !entry.disabled {
                        if let Ok(server_config) = entry.to_server_config(name) {
                            agent.connect_mcp_from_config(server_config).await.ok();
                        }
                    }
                }
            })
        })
        .await;

    Ok(serde_json::json!({
        "success": true,
        "message": "MCP config updated",
    }))
}
