//! MCP 服务端管理 API

use axum::{
    Json, debug_handler,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;
use crate::types::McpTransportConfig;
use crate::types::{ConnectMcpRequest, McpConnectionStatus, McpServerInfo, McpToolInfo};
use echo_agent::mcp::McpConfigFile;

/// 将 MCP 工具转换为响应格式
fn mcp_tool_to_info(tool: &echo_agent::mcp::McpTool) -> McpToolInfo {
    McpToolInfo {
        name: tool.name.clone(),
        description: tool.description.clone().unwrap_or_default(),
        input_schema: tool.input_schema.clone(),
    }
}

/// 从 MCP 服务器条目推断传输类型字符串
fn infer_transport(entry: &echo_agent::mcp::McpServerEntry) -> &'static str {
    if let Some(_url) = &entry.url {
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

/// 将 MCP 传输配置转换为 MCP 服务器条目
fn transport_to_entry(transport: &McpTransportConfig) -> echo_agent::mcp::McpServerEntry {
    use echo_agent::mcp::McpServerEntry;
    use std::collections::HashMap;

    match transport {
        McpTransportConfig::Stdio { command, args, env } => McpServerEntry {
            command: Some(command.clone()),
            args: args.clone(),
            env: env.clone(),
            url: None,
            headers: HashMap::new(),
            transport: None,
            disabled: false,
        },
        McpTransportConfig::Http { url, headers } => McpServerEntry {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url.clone()),
            headers: headers.clone(),
            transport: None, // 默认HTTP传输
            disabled: false,
        },
        McpTransportConfig::Sse { url, headers } => McpServerEntry {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url.clone()),
            headers: headers.clone(),
            transport: Some("sse".to_string()),
            disabled: false,
        },
    }
}

/// GET /api/mcp - 列出所有 MCP 服务端状态
#[debug_handler]
#[allow(clippy::unnecessary_filter_map)]
pub async fn list_mcp_servers(State(state): State<Arc<AppState>>) -> Response {
    let mcp_config = state.plugins.mcp_config.read().await;
    let mcp_health = state.plugins.mcp_health.read().await;

    let servers: Vec<McpServerInfo> = state
        .connection
        .agent
        .read(|agent| {
            agent
                .list_mcp_servers()
                .into_iter()
                .filter_map(|name| {
                    // 获取实际的工具信息
                    let tools: Vec<McpToolInfo> = agent
                        .mcp_client(name)
                        .map(|client| client.tools().iter().map(mcp_tool_to_info).collect())
                        .unwrap_or_default();

                    // 从配置中推断传输类型
                    let transport = mcp_config
                        .mcp_servers
                        .get(name)
                        .map(|entry| infer_transport(entry))
                        .unwrap_or("stdio");

                    // 从健康检查获取状态
                    let (status, error) = mcp_health
                        .get(name)
                        .map(|hs| {
                            if hs.healthy {
                                (McpConnectionStatus::Connected, hs.error.clone())
                            } else {
                                (
                                    McpConnectionStatus::Error(
                                        hs.error
                                            .clone()
                                            .unwrap_or_else(|| "Unknown error".to_string()),
                                    ),
                                    hs.error.clone(),
                                )
                            }
                        })
                        .unwrap_or((McpConnectionStatus::Connected, None));

                    Some(McpServerInfo {
                        name: name.to_string(),
                        status,
                        transport: transport.to_string(),
                        tool_count: tools.len(),
                        tools,
                        connected_at: None,
                        error,
                    })
                })
                .collect()
        })
        .await;

    Json(servers).into_response()
}

/// 验证 MCP stdio 命令和参数的安全性
///
/// 对命令名和参数进行严格的注入字符检测，防止命令注入和路径遍历。
fn validate_mcp_stdio_command(command: &str, args: &[String]) -> Result<(), WebError> {
    // ── 命令名校验 ──────────────────────────────────────────────────────────
    // 白名单：仅允许受信任的命令解释器/运行时
    const ALLOWED_COMMANDS: &[&str] = &[
        "npx", "node", "python3", "python", "uvx", "uv", "bun", "deno", "go", "java",
    ];

    // 提取命令的基本文件名（防止路径绕过）
    let cmd_base = std::path::Path::new(command)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(command);

    // 检查命令名是否在白名单中
    if !ALLOWED_COMMANDS.contains(&cmd_base) {
        return Err(WebError::Validation(format!(
            "Command '{}' is not allowed. Allowed: {}",
            command,
            ALLOWED_COMMANDS.join(", ")
        )));
    }

    // 检查命令名本身是否包含路径遍历或注入字符
    // （虽然通过 file_name() 做了提取，但需要防御底层绕过）
    const CMD_DISALLOWED: &[char] = &['/', '.', '~', '$', '`', '\\', '|', '&', ';', '!', '*', '?'];
    for ch in CMD_DISALLOWED {
        if cmd_base.contains(*ch) {
            return Err(WebError::Validation(format!(
                "Command name contains disallowed character: '{}'",
                ch
            )));
        }
    }

    // ── 参数校验 ──────────────────────────────────────────────────────────
    if args.len() > 50 {
        return Err(WebError::Validation(
            "Too many arguments (max 50)".to_string(),
        ));
    }

    // 参数禁止的注入/遍历字符
    const ARG_DISALLOWED: &[char] = &[
        '|', '&', ';', '`', '$', '!', '~', '\\', '*',
        '?',
        // 路径遍历检测
        // 注意：".." 需要作为子串检测（见下方循环）
    ];

    for arg in args {
        // 检查单个字符
        for ch in ARG_DISALLOWED {
            if arg.contains(*ch) {
                return Err(WebError::Validation(format!(
                    "Argument contains disallowed character '{}': '{}'",
                    ch, arg
                )));
            }
        }
        // 检查路径遍历序列
        if arg.contains("..") {
            return Err(WebError::Validation(format!(
                "Argument contains path traversal '..': '{}'",
                arg
            )));
        }
    }

    Ok(())
}

/// POST /api/mcp/connect - 连接新的 MCP 服务端
#[debug_handler]
pub async fn connect_mcp_server(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ConnectMcpRequest>,
) -> Response {
    use echo_agent::mcp::McpServerConfig;

    // Validate name
    if req.name.trim().is_empty() {
        return WebError::Validation("MCP server name cannot be empty".to_string()).into_response();
    }

    let config = match &req.transport {
        McpTransportConfig::Stdio { command, args, env } => {
            if let Err(e) = validate_mcp_stdio_command(command, args) {
                return e.into_response();
            }

            // 记录脱敏信息，避免泄露敏感参数
            tracing::info!("MCP 连接: command={}, args_count={}", command, args.len());
            let env_vec: Vec<(String, String)> =
                env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let args_clone = args.clone();
            McpServerConfig::stdio_with_env(
                req.name.as_str(),
                command.as_str(),
                args_clone,
                env_vec,
            )
        }
        McpTransportConfig::Http { url, headers } => {
            let headers_clone = headers.clone();
            McpServerConfig::http_with_headers(req.name.as_str(), url.as_str(), headers_clone)
        }
        McpTransportConfig::Sse { url, headers } => {
            let headers_clone = headers.clone();
            McpServerConfig::sse_with_headers(req.name.as_str(), url.as_str(), headers_clone)
        }
    };

    // 使用公开的 MCP 连接方法
    let server_name = req.name.clone();
    let transport = req.transport.clone();
    match state
        .connection
        .agent
        .write_async(|agent| Box::pin(async move { agent.connect_mcp_from_config(config).await }))
        .await
    {
        Ok(_client) => {
            // 获取实际的工具信息
            let tools: Vec<McpToolInfo> = state
                .connection
                .agent
                .read(|agent| {
                    agent
                        .mcp_client(&server_name)
                        .map(|client| client.tools().iter().map(mcp_tool_to_info).collect())
                        .unwrap_or_default()
                })
                .await;

            // 更新 MCP 配置文件
            {
                let entry = transport_to_entry(&transport);
                let mut mcp_config = state.plugins.mcp_config.write().await;
                mcp_config.mcp_servers.insert(server_name.clone(), entry);
            }

            let transport_str = match &transport {
                McpTransportConfig::Stdio { .. } => "stdio",
                McpTransportConfig::Http { .. } => "http",
                McpTransportConfig::Sse { .. } => "sse",
            };

            Json(McpServerInfo {
                name: server_name,
                status: McpConnectionStatus::Connected,
                transport: transport_str.to_string(),
                tool_count: tools.len(),
                tools,
                connected_at: Some(chrono::Utc::now()),
                error: None,
            })
            .into_response()
        }
        Err(e) => WebError::Internal(format!("MCP 连接失败: {}", e)).into_response(),
    }
}

/// GET /api/mcp/{name} - 获取指定 MCP 服务端详情
#[debug_handler]
pub async fn get_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    // 获取工具信息（需要 agent read lock）
    let tools: Vec<McpToolInfo> = match state
        .connection
        .agent
        .read(|agent| {
            agent
                .mcp_client(&name)
                .map(|client| client.tools().iter().map(mcp_tool_to_info).collect())
        })
        .await
    {
        Some(t) => t,
        None => return WebError::McpServerNotFound(name).into_response(),
    };

    // 从配置中推断传输类型
    let transport = state
        .plugins
        .mcp_config
        .read()
        .await
        .mcp_servers
        .get(&name)
        .map(|entry| infer_transport(entry))
        .unwrap_or("stdio");

    // 从健康检查获取状态
    let (status, error) = state
        .plugins
        .mcp_health
        .read()
        .await
        .get(&name)
        .map(|hs| {
            if hs.healthy {
                (McpConnectionStatus::Connected, hs.error.clone())
            } else {
                (
                    McpConnectionStatus::Error(
                        hs.error
                            .clone()
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    ),
                    hs.error.clone(),
                )
            }
        })
        .unwrap_or((McpConnectionStatus::Connected, None));

    Json(McpServerInfo {
        name,
        status,
        transport: transport.to_string(),
        tool_count: tools.len(),
        tools,
        connected_at: None,
        error,
    })
    .into_response()
}

/// POST /api/mcp/{name}/disconnect - 断开 MCP 服务端
#[debug_handler]
pub async fn disconnect_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    // 检查服务端是否存在
    let exists = state
        .connection
        .agent
        .read(|agent| agent.mcp_client(&name).is_some())
        .await;
    if !exists {
        return WebError::McpServerNotFound(name).into_response();
    }

    // 断开连接
    let name_for_disconnect = name.clone();
    let disconnected = state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move { agent.disconnect_mcp(&name_for_disconnect).await })
        })
        .await;

    if disconnected {
        // 更新健康状态为已断开
        let mut health = state.plugins.mcp_health.write().await;
        health.insert(
            name.clone(),
            crate::state::McpHealthStatus {
                name: name.clone(),
                healthy: false,
                last_check: Some(chrono::Utc::now()),
                error: Some("Disconnected by user".to_string()),
            },
        );

        // 从 MCP 配置文件中移除
        {
            let mut mcp_config = state.plugins.mcp_config.write().await;
            mcp_config.mcp_servers.remove(&name);
        }
        Json(serde_json::json!({
            "success": true,
            "message": format!("MCP 服务端 '{}' 已断开", name)
        }))
        .into_response()
    } else {
        Json(serde_json::json!({
            "success": false,
            "message": format!("MCP 服务端 '{}' 断开失败", name)
        }))
        .into_response()
    }
}

/// GET /api/mcp/health - 获取所有 MCP 服务端健康状态
#[debug_handler]
pub async fn get_mcp_health(State(state): State<Arc<AppState>>) -> Response {
    let health = state.plugins.mcp_health.read().await;
    let health_list: Vec<&crate::state::McpHealthStatus> = health.values().collect();
    Json(health_list).into_response()
}

/// GET /api/mcp/config - 获取完整的 MCP 配置文件
#[debug_handler]
pub async fn get_mcp_config(State(state): State<Arc<AppState>>) -> Response {
    let config = state.plugins.mcp_config.read().await;
    Json(config.clone()).into_response()
}

/// PUT /api/mcp/config - 更新完整的 MCP 配置文件
#[debug_handler]
pub async fn update_mcp_config(
    State(state): State<Arc<AppState>>,
    Json(config): Json<McpConfigFile>,
) -> Response {
    // 验证配置
    match config.to_server_configs() {
        Ok(server_configs) => {
            // 断开所有现有连接并连接新服务器
            let state_for_config = state.clone();
            let (errors, connected_servers) = state
                .connection
                .agent
                .write_async(|agent| {
                    Box::pin(async move {
                        // 断开所有现有 MCP 连接
                        let existing_servers: Vec<String> = agent
                            .list_mcp_servers()
                            .into_iter()
                            .map(|s| s.to_string())
                            .collect();
                        for name in existing_servers {
                            let _ = agent.disconnect_mcp(&name).await;
                        }

                        // 连接新的服务器配置
                        let mut errors = Vec::new();
                        let mut connected_servers = Vec::new();

                        for server_config in server_configs {
                            let name = server_config.name.clone();
                            match agent.connect_mcp_from_config(server_config).await {
                                Ok(_client) => {
                                    connected_servers.push(name);
                                }
                                Err(e) => {
                                    errors.push(format!(
                                        "Failed to connect server '{}': {}",
                                        name, e
                                    ));
                                }
                            }
                        }

                        (errors, connected_servers)
                    })
                })
                .await;

            // 更新存储的配置
            {
                let mut stored_config = state_for_config.plugins.mcp_config.write().await;
                *stored_config = config;
            }

            if errors.is_empty() {
                Json(serde_json::json!({
                    "success": true,
                    "message": format!("MCP 配置已更新，成功连接 {} 个服务器", connected_servers.len()),
                    "connected_servers": connected_servers
                })).into_response()
            } else {
                Json(serde_json::json!({
                    "success": false,
                    "message": format!("MCP 配置部分成功，{} 个服务器连接失败", errors.len()),
                    "connected_servers": connected_servers,
                    "errors": errors
                }))
                .into_response()
            }
        }
        Err(e) => WebError::Validation(format!("无效的 MCP 配置: {}", e)).into_response(),
    }
}
