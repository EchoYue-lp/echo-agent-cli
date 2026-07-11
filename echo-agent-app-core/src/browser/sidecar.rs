use std::process::Output;
use std::time::Duration;

use echo_agent::mcp::McpServerConfig;
use tokio::process::Command;

use super::config::BrowserConfig;
use super::error::{BrowserError, BrowserResult};

pub const BROWSER_MCP_SERVER_NAME: &str = "eko-playwright";

pub struct BrowserSidecar;

impl BrowserSidecar {
    pub async fn prepare(config: &BrowserConfig) -> BrowserResult<()> {
        if !config.enabled {
            return Err(BrowserError::Disabled);
        }

        let node = command_version(&config.node_command).await?;
        require_node_18(&node)?;
        command_version(&config.npm_command).await?;
        command_version(&config.npx_command).await?;

        tokio::fs::create_dir_all(&config.user_data_dir)
            .await
            .map_err(|error| BrowserError::Io(error.to_string()))?;
        tokio::fs::create_dir_all(&config.output_dir)
            .await
            .map_err(|error| BrowserError::Io(error.to_string()))?;
        tokio::fs::create_dir_all(&config.session_dir)
            .await
            .map_err(|error| BrowserError::Io(error.to_string()))?;
        Ok(())
    }

    pub fn server_config(config: &BrowserConfig) -> McpServerConfig {
        McpServerConfig::stdio_with_env(
            BROWSER_MCP_SERVER_NAME,
            config.npx_command.clone(),
            config.sidecar_args(),
            vec![
                ("NO_COLOR".to_string(), "1".to_string()),
                (
                    "PLAYWRIGHT_MCP_OUTPUT_DIR".to_string(),
                    config.output_dir.to_string_lossy().into_owned(),
                ),
            ],
        )
    }
}

async fn command_version(command: &str) -> BrowserResult<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(command).arg("--version").output(),
    )
    .await
    .map_err(|_| BrowserError::Prerequisite(format!("'{command} --version' timed out")))?
    .map_err(|error| BrowserError::Prerequisite(format!("cannot execute '{command}': {error}")))?;
    output_text(command, output)
}

fn output_text(command: &str, output: Output) -> BrowserResult<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(BrowserError::Prerequisite(format!(
            "'{command} --version' failed: {stderr}"
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        return Err(BrowserError::Prerequisite(format!(
            "'{command} --version' returned no version"
        )));
    }
    Ok(version)
}

fn require_node_18(version: &str) -> BrowserResult<()> {
    let normalized = version.trim().trim_start_matches('v');
    let major = normalized
        .split('.')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            BrowserError::Prerequisite(format!("cannot parse Node.js version '{version}'"))
        })?;
    if major < 18 {
        return Err(BrowserError::Prerequisite(format!(
            "Node.js 18 or newer is required, found {version}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::mcp::TransportConfig;
    use std::path::PathBuf;

    #[test]
    fn accepts_supported_node_versions() {
        assert!(require_node_18("v18.20.0").is_ok());
        assert!(require_node_18("22.14.0").is_ok());
    }

    #[test]
    fn rejects_old_or_malformed_node_versions() {
        assert!(require_node_18("v16.20.0").is_err());
        assert!(require_node_18("unknown").is_err());
    }

    #[test]
    fn output_dir_uses_environment_instead_of_rejected_o_flag() -> Result<(), &'static str> {
        let config = BrowserConfig {
            output_dir: PathBuf::from("/tmp/eko-browser-output"),
            ..BrowserConfig::default()
        };
        let server = BrowserSidecar::server_config(&config);
        let TransportConfig::Stdio { args, env, .. } = server.transport else {
            return Err("managed Playwright MCP must use stdio transport");
        };

        assert!(!args.iter().any(|arg| arg.starts_with("-o")));
        assert!(env.iter().any(|(name, value)| {
            name == "PLAYWRIGHT_MCP_OUTPUT_DIR" && value == "/tmp/eko-browser-output"
        }));
        Ok(())
    }
}
