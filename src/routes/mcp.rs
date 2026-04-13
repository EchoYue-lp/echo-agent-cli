//! MCP 服务端管理 API

use axum::{
    debug_handler,
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;
use crate::types::{
    ConnectMcpRequest, McpConnectionStatus, McpServerInfo, McpToolInfo,
};
use crate::types::McpTransportConfig;

/// 将 MCP 工具转换为响应格式
fn mcp_tool_to_info(tool: &echo_agent::mcp::McpTool) -> McpToolInfo {
    McpToolInfo {
        name: tool.name.clone(),
        description: tool.description.clone().unwrap_or_default(),
        input_schema: tool.input_schema.clone(),
    }
}

/// GET /api/mcp - 列出所有 MCP 服务端状态
#[debug_handler]
pub async fn list_mcp_servers(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;

    let servers: Vec<McpServerInfo> = agent
        .list_mcp_servers()
        .into_iter()
        .filter_map(|name| {
            // 获取实际的工具信息
            let tools: Vec<McpToolInfo> = agent
                .mcp_client(name)
                .map(|client| {
                    client.tools().iter().map(mcp_tool_to_info).collect()
                })
                .unwrap_or_default();

            Some(McpServerInfo {
                name: name.to_string(),
                status: McpConnectionStatus::Connected,
                transport: "stdio".to_string(),
                tool_count: tools.len(),
                tools,
                connected_at: None,
                error: None,
            })
        })
        .collect();

    Json(servers).into_response()
}

/// POST /api/mcp/connect - 连接新的 MCP 服务端
#[debug_handler]
pub async fn connect_mcp_server(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ConnectMcpRequest>,
) -> Response {
    use echo_agent::mcp::McpServerConfig;

    let config = match req.transport {
        McpTransportConfig::Stdio { command, args, env } => {
            tracing::info!("MCP 连接: command={}, args={:?}", command, args);
            let env_vec: Vec<(String, String)> = env.into_iter().collect();
            McpServerConfig::stdio_with_env(&req.name, &command, args, env_vec)
        }
        McpTransportConfig::Http { url, headers } => {
            McpServerConfig::http_with_headers(&req.name, &url, headers)
        }
        McpTransportConfig::Sse { url, headers } => {
            McpServerConfig::sse_with_headers(&req.name, &url, headers)
        }
    };

    let mut agent = state.agent.lock().await;

    // 使用公开的 MCP 连接方法
    match agent.connect_mcp_from_config(config).await {
        Ok(_client) => {
            // 获取实际的工具信息
            let tools: Vec<McpToolInfo> = agent
                .mcp_client(&req.name)
                .map(|client| {
                    client.tools().iter().map(mcp_tool_to_info).collect()
                })
                .unwrap_or_default();

            Json(McpServerInfo {
                name: req.name,
                status: McpConnectionStatus::Connected,
                transport: "stdio".to_string(),
                tool_count: tools.len(),
                tools,
                connected_at: Some(chrono::Utc::now()),
                error: None,
            }).into_response()
        }
        Err(e) => {
            WebError::Internal(format!("MCP 连接失败: {}", e)).into_response()
        }
    }
}

/// GET /api/mcp/{name} - 获取指定 MCP 服务端详情
#[debug_handler]
pub async fn get_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let agent = state.agent.lock().await;

    let client = match agent.mcp_client(&name) {
        Some(c) => c,
        None => return WebError::McpServerNotFound(name).into_response(),
    };

    let tools: Vec<McpToolInfo> = client.tools().iter().map(mcp_tool_to_info).collect();

    Json(McpServerInfo {
        name,
        status: McpConnectionStatus::Connected,
        transport: "stdio".to_string(),
        tool_count: tools.len(),
        tools,
        connected_at: None,
        error: None,
    }).into_response()
}

/// POST /api/mcp/{name}/disconnect - 断开 MCP 服务端
#[debug_handler]
pub async fn disconnect_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let mut agent = state.agent.lock().await;

    // 检查服务端是否存在
    if agent.mcp_client(&name).is_none() {
        return WebError::McpServerNotFound(name).into_response();
    }

    // 断开连接
    let disconnected = agent.disconnect_mcp(&name).await;

    if disconnected {
        Json(serde_json::json!({
            "success": true,
            "message": format!("MCP 服务端 '{}' 已断开", name)
        })).into_response()
    } else {
        Json(serde_json::json!({
            "success": false,
            "message": format!("MCP 服务端 '{}' 断开失败", name)
        })).into_response()
    }
}