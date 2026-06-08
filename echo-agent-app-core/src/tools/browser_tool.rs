//! Browser Tool - MCP Playwright Integration
//!
//! Provides browser automation capabilities through MCP Playwright server.
//! Requires `@anthropic-ai/mcp-server-playwright` to be installed and configured
//! in your MCP config file (e.g., `~/.echo-agent/mcp.yaml`).
//!
//! ## Status
//!
//! This is a **stub tool** that returns an informative error directing users
//! to configure the MCP Playwright server. It is registered by default so
//! the agent knows browser capabilities exist, but actual browser operations
//! require the MCP server to be running.

use echo_agent::prelude::*;

/// Browser automation tool stub — directs users to configure MCP Playwright.
pub struct BrowserTool;

impl BrowserTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

const SETUP_MESSAGE: &str = "\
BrowserTool requires the MCP Playwright server to be configured.

To enable browser automation, add the following to your MCP config
(~/.echo-agent/mcp.yaml or --mcp-config <path>):

  servers:
    playwright:
      command: npx
      args:
        - \"@anthropic-ai/mcp-server-playwright\"

Then restart echo-agent. Browser actions will be routed through the MCP server.";

impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Browser automation tool (navigate, screenshot, click, fill, extract, scroll). \
         Requires MCP Playwright server to be configured."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "screenshot", "click", "fill", "extract", "scroll"],
                    "description": "The browser action to perform"
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (for navigate action)"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the target element"
                },
                "text": {
                    "type": "string",
                    "description": "Text to fill into an input (for fill action)"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum length of extracted content (for extract action)"
                }
            },
            "required": ["action"]
        })
    }

    fn execute<'a>(&'a self, parameters: ToolParameters) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let action = parameters.get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            tracing::warn!(
                action = action,
                "BrowserTool stub called — MCP Playwright not configured"
            );

            Ok(ToolResult::error(format!(
                "Browser action '{}' cannot be executed: {}",
                action, SETUP_MESSAGE
            )))
        })
    }

    fn permissions(&self) -> Vec<echo_agent::tools::permission::ToolPermission> {
        vec![echo_agent::tools::permission::ToolPermission::Network]
    }

    fn risk_level(&self) -> echo_agent::tools::ToolRiskLevel {
        echo_agent::tools::ToolRiskLevel::Standard
    }

    fn capability_description(&self) -> &str {
        "Browser automation via Playwright MCP (requires MCP server configuration)"
    }
}
