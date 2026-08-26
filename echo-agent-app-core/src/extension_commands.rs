//! Surface-neutral Extension management commands.
//!
//! Parsing and dispatch live in app-core so every product surface submits the
//! same typed request to the application-owned Extension control authority.
//! Specialist runtimes remain responsible for execution; this module only
//! converts command arguments and projects bounded wire receipts.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::extension_control::{
    ExtensionMcpServer, ExtensionSkillEntry, HookReloadReceipt, HookSourceSnapshot,
    PluginMutationReceipt, SkillArtifactSyncReceipt, SkillInstallSettlementReceipt,
    SkillSettlementStatus, SkillSyncReceipt, SkillUninstallSettlementReceipt,
};
use crate::state::{AppState, ScopedChatRuntime};

const MAX_EXTENSION_ITEMS: usize = 256;
const MAX_EXTENSION_TOOLS: usize = 128;
const MAX_EXTENSION_ERRORS: usize = 64;
const MAX_EXTENSION_TEXT_CHARS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionCommandIdentity {
    pub request_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionRequestScope {
    pub workspace_id: String,
    pub workspace_generation: String,
    pub sender_id: Option<String>,
    pub sender_incarnation: Option<String>,
}

impl ExtensionRequestScope {
    pub fn new(
        workspace_id: impl Into<String>,
        workspace_generation: impl Into<String>,
        sender_id: Option<String>,
        sender_incarnation: Option<String>,
    ) -> Result<Self, ExtensionCommandParseError> {
        let scope = Self {
            workspace_id: workspace_id.into(),
            workspace_generation: workspace_generation.into(),
            sender_id,
            sender_incarnation,
        };
        scope.validate()?;
        Ok(scope)
    }

    fn validate(&self) -> Result<(), ExtensionCommandParseError> {
        if self.workspace_id.trim().is_empty() {
            return Err(ExtensionCommandParseError::new(
                None,
                "workspace_id must not be empty",
            ));
        }
        if self.workspace_generation.trim().is_empty() {
            return Err(ExtensionCommandParseError::new(
                None,
                "workspace_generation must not be empty",
            ));
        }
        match (&self.sender_id, &self.sender_incarnation) {
            (Some(sender), Some(incarnation))
                if !sender.trim().is_empty() && !incarnation.trim().is_empty() =>
            {
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err(ExtensionCommandParseError::new(
                None,
                "sender_id and sender_incarnation must be supplied together",
            )),
        }
    }
}

impl ExtensionCommandIdentity {
    pub fn new(
        request_id: impl Into<String>,
        operation_id: impl Into<String>,
    ) -> Result<Self, ExtensionCommandParseError> {
        let identity = Self {
            request_id: request_id.into(),
            operation_id: operation_id.into(),
        };
        if identity.request_id.trim().is_empty() {
            return Err(ExtensionCommandParseError::new(
                None,
                "request_id must not be empty",
            ));
        }
        if identity.operation_id.trim().is_empty() {
            return Err(ExtensionCommandParseError::new(
                None,
                "operation_id must not be empty",
            ));
        }
        Ok(identity)
    }

    pub fn random() -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            operation_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ExtensionKind {
    Skills,
    Plugins,
    Mcp,
    Hooks,
    Lsp,
    Browser,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionCommandRequest {
    pub request_id: String,
    pub operation_id: String,
    #[serde(default)]
    pub scope: Option<ExtensionRequestScope>,
    #[serde(flatten)]
    pub command: ExtensionCommand,
}

impl ExtensionCommandRequest {
    pub fn identity(&self) -> ExtensionCommandIdentity {
        ExtensionCommandIdentity {
            request_id: self.request_id.clone(),
            operation_id: self.operation_id.clone(),
        }
    }

    pub fn kind(&self) -> ExtensionKind {
        self.command.kind()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "extension", content = "command", rename_all = "snake_case")]
#[ts(export)]
pub enum ExtensionCommand {
    Skills(SkillCommand),
    Plugins(PluginCommand),
    Mcp(McpCommand),
    Hooks(HookCommand),
    Lsp(LspCommand),
    Browser(BrowserCommand),
}

impl ExtensionCommand {
    fn kind(&self) -> ExtensionKind {
        match self {
            Self::Skills(_) => ExtensionKind::Skills,
            Self::Plugins(_) => ExtensionKind::Plugins,
            Self::Mcp(_) => ExtensionKind::Mcp,
            Self::Hooks(_) => ExtensionKind::Hooks,
            Self::Lsp(_) => ExtensionKind::Lsp,
            Self::Browser(_) => ExtensionKind::Browser,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum SkillCommand {
    List,
    Search { query: String },
    Info { name: String },
    Install { source: String },
    Uninstall { name: String },
    Enable { name: String },
    Disable { name: String },
    Refresh,
    CheckUpdates { target: Option<String> },
    Sync { target: Option<String>, force: bool },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PluginInstallScope {
    User,
    Project,
    Local,
}

impl From<PluginInstallScope> for echo_agent::plugin::PluginScope {
    fn from(value: PluginInstallScope) -> Self {
        match value {
            PluginInstallScope::User => Self::User,
            PluginInstallScope::Project => Self::Project,
            PluginInstallScope::Local => Self::Local,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum PluginCommand {
    List,
    Info {
        name: String,
    },
    Reload,
    Install {
        source: String,
        scope: PluginInstallScope,
    },
    Uninstall {
        name: String,
        keep_data: bool,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    Themes,
    Theme {
        name: Option<String>,
    },
    Styles,
    Style {
        name: Option<String>,
    },
    Configure {
        name: String,
        values: HashMap<String, serde_json::Value>,
    },
    Scaffold {
        directory: String,
        name: String,
    },
    Validate {
        directory: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum McpCommand {
    List,
    Connect {
        name: String,
    },
    Disconnect {
        name: String,
    },
    Upsert {
        name: String,
        server: McpServerConfig,
    },
    Remove {
        name: String,
    },
    SetEnabled {
        name: String,
        enabled: bool,
    },
    Import {
        config: McpConfigDocument,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct McpConfigDocument {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct McpServerConfig {
    #[serde(rename = "type")]
    pub server_type: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub headers: HashMap<String, String>,
    pub transport: Option<String>,
    pub disabled: bool,
}

impl From<McpServerConfig> for echo_agent::mcp::McpServerEntry {
    fn from(value: McpServerConfig) -> Self {
        Self {
            server_type: value.server_type,
            command: value.command,
            args: value.args,
            env: value.env,
            cwd: value.cwd,
            url: value.url,
            headers: value.headers,
            transport: value.transport,
            disabled: value.disabled,
        }
    }
}

impl From<McpConfigDocument> for echo_agent::mcp::McpConfigFile {
    fn from(value: McpConfigDocument) -> Self {
        Self {
            schema: value.schema,
            mcp_servers: value
                .mcp_servers
                .into_iter()
                .map(|(name, server)| (name, server.into()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum HookCommand {
    List,
    Reload,
    Test { event: String, matcher: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum LspCommand {
    List,
    Status,
    Start { language: String },
    Stop { language: String },
    Restart { language: String },
}

impl LspCommand {
    fn specialist_args(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::List => ("list", None),
            Self::Status => ("status", None),
            Self::Start { language } => ("start", Some(language)),
            Self::Stop { language } => ("stop", Some(language)),
            Self::Restart { language } => ("restart", Some(language)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum BrowserCommand {
    Status,
    Managed,
    Chrome,
    Navigate {
        url: String,
    },
    Snapshot {
        filename: Option<String>,
    },
    ClickTarget {
        target: String,
        element: Option<String>,
        button: Option<String>,
        double_click: bool,
        effect: String,
    },
    Fill {
        target: String,
        text: String,
        element: Option<String>,
        submit: bool,
        slowly: bool,
        effect: String,
    },
    Back,
    Reload,
    Screenshot,
    Click {
        x: f64,
        y: f64,
    },
    TypeAt {
        x: f64,
        y: f64,
        text: String,
        submit: bool,
        slowly: bool,
        effect: String,
    },
    Scroll {
        delta_x: f64,
        delta_y: f64,
    },
    Tabs {
        tab_action: String,
        #[ts(type = "number | null")]
        index: Option<u64>,
        url: Option<String>,
    },
    Console {
        level: Option<String>,
        contains: Option<String>,
    },
    Network {
        method: Option<String>,
        status: Option<u16>,
        contains: Option<String>,
    },
    DomInspect {
        target: Option<String>,
        text: Option<String>,
        max_depth: Option<u16>,
    },
    PerformanceTrace {
        trace_action: String,
        path: Option<String>,
    },
    DeveloperMode {
        enabled: bool,
    },
    Stop,
}

impl BrowserCommand {
    fn action_name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Managed => "managed",
            Self::Chrome => "chrome",
            Self::Navigate { .. } => "navigate",
            Self::Snapshot { .. } => "snapshot",
            Self::ClickTarget { .. } => "click_target",
            Self::Fill { .. } => "fill",
            Self::Back => "back",
            Self::Reload => "reload",
            Self::Screenshot => "screenshot",
            Self::Click { .. } => "click",
            Self::TypeAt { .. } => "type_at",
            Self::Scroll { .. } => "scroll",
            Self::Tabs { .. } => "tabs",
            Self::Console { .. } => "console",
            Self::Network { .. } => "network",
            Self::DomInspect { .. } => "dom_inspect",
            Self::PerformanceTrace { .. } => "performance_trace",
            Self::DeveloperMode { .. } => "developer_mode",
            Self::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ExtensionCommandParseError {
    pub extension: Option<ExtensionKind>,
    pub message: String,
}

impl ExtensionCommandParseError {
    fn new(extension: Option<ExtensionKind>, message: impl Into<String>) -> Self {
        Self {
            extension,
            message: message.into(),
        }
    }
}

/// Parse only the six explicit Extension management command families.
/// Unrelated prompts and slash commands return `Ok(None)` and continue to the
/// ordinary model path. Invalid commands under a supported root return a typed
/// parse error so a surface cannot accidentally submit them to the model.
pub fn parse_extension_command(
    input: &str,
    identity: ExtensionCommandIdentity,
) -> Result<Option<ExtensionCommandRequest>, ExtensionCommandParseError> {
    if identity.request_id.trim().is_empty() || identity.operation_id.trim().is_empty() {
        return Err(ExtensionCommandParseError::new(
            None,
            "Extension command identity must be populated",
        ));
    }
    let Some((root, arguments)) = split_head(input) else {
        return Ok(None);
    };
    let command = match root {
        "/skills" => parse_skill_command(arguments).map(ExtensionCommand::Skills),
        "/plugins" => parse_plugin_command(arguments).map(ExtensionCommand::Plugins),
        "/mcp" => parse_mcp_command(arguments).map(ExtensionCommand::Mcp),
        "/hooks" => parse_hook_command(arguments).map(ExtensionCommand::Hooks),
        "/lsp" => parse_lsp_command(arguments).map(ExtensionCommand::Lsp),
        "/browser" => parse_browser_command(arguments).map(ExtensionCommand::Browser),
        _ => return Ok(None),
    }?;
    Ok(Some(ExtensionCommandRequest {
        request_id: identity.request_id,
        operation_id: identity.operation_id,
        scope: None,
        command,
    }))
}

fn split_head(input: &str) -> Option<(&str, &str)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let boundary = input.find(char::is_whitespace).unwrap_or(input.len());
    let head = input.get(..boundary)?;
    let rest = input.get(boundary..)?.trim();
    Some((head, rest))
}

fn required_argument<'a>(
    extension: ExtensionKind,
    value: &'a str,
    usage: &str,
) -> Result<&'a str, ExtensionCommandParseError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ExtensionCommandParseError::new(Some(extension), usage))
    } else {
        Ok(value)
    }
}

fn parse_skill_command(input: &str) -> Result<SkillCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("list", ""));
    let error = |usage: &str| ExtensionCommandParseError::new(Some(ExtensionKind::Skills), usage);
    match action {
        "" | "list" | "ls" => Ok(SkillCommand::List),
        "search" | "find" => Ok(SkillCommand::Search {
            query: required_argument(ExtensionKind::Skills, rest, "/skills search <query>")?
                .to_string(),
        }),
        "info" => Ok(SkillCommand::Info {
            name: required_argument(ExtensionKind::Skills, rest, "/skills info <name>")?
                .to_string(),
        }),
        "install" => Ok(SkillCommand::Install {
            source: required_argument(ExtensionKind::Skills, rest, "/skills install <source>")?
                .to_string(),
        }),
        "uninstall" | "remove" | "rm" => Ok(SkillCommand::Uninstall {
            name: required_argument(ExtensionKind::Skills, rest, "/skills uninstall <name>")?
                .to_string(),
        }),
        "enable" => Ok(SkillCommand::Enable {
            name: required_argument(ExtensionKind::Skills, rest, "/skills enable <name>")?
                .to_string(),
        }),
        "disable" => Ok(SkillCommand::Disable {
            name: required_argument(ExtensionKind::Skills, rest, "/skills disable <name>")?
                .to_string(),
        }),
        "refresh" => Ok(SkillCommand::Refresh),
        "check-updates" | "check" => Ok(SkillCommand::CheckUpdates {
            target: (!rest.is_empty() && !matches!(rest, "all" | "*")).then(|| rest.to_string()),
        }),
        "sync" => {
            let mut target = None;
            let mut force = false;
            for value in rest.split_whitespace() {
                if value == "--force" {
                    force = true;
                } else if target.is_none() {
                    target = (!matches!(value, "all" | "*")).then(|| value.to_string());
                } else {
                    return Err(error("/skills sync [name|all] [--force]"));
                }
            }
            Ok(SkillCommand::Sync { target, force })
        }
        _ => Err(error(
            "/skills [list|search|info|install|uninstall|enable|disable|refresh|check-updates|sync]",
        )),
    }
}

fn parse_plugin_command(input: &str) -> Result<PluginCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("list", ""));
    let error = |usage: &str| ExtensionCommandParseError::new(Some(ExtensionKind::Plugins), usage);
    match action {
        "" | "list" | "ls" => Ok(PluginCommand::List),
        "info" | "details" => Ok(PluginCommand::Info {
            name: required_argument(ExtensionKind::Plugins, rest, "/plugins info <name>")?
                .to_string(),
        }),
        "reload" => Ok(PluginCommand::Reload),
        "install" => {
            let mut values = rest.split_whitespace();
            let source = values
                .next()
                .ok_or_else(|| error("/plugins install <source> [--scope user|project|local]"))?
                .to_string();
            let mut scope = PluginInstallScope::User;
            while let Some(value) = values.next() {
                if value != "--scope" {
                    return Err(error(
                        "/plugins install <source> [--scope user|project|local]",
                    ));
                }
                let scope_value = values.next().ok_or_else(|| {
                    error("/plugins install <source> [--scope user|project|local]")
                })?;
                scope = match scope_value {
                    "user" | "u" => PluginInstallScope::User,
                    "project" | "p" => PluginInstallScope::Project,
                    "local" | "l" => PluginInstallScope::Local,
                    _ => {
                        return Err(error(
                            "/plugins install <source> [--scope user|project|local]",
                        ));
                    }
                };
            }
            Ok(PluginCommand::Install { source, scope })
        }
        "uninstall" | "remove" => {
            let mut values = rest.split_whitespace();
            let name = values
                .next()
                .ok_or_else(|| error("/plugins uninstall <name> [--keep-data]"))?
                .to_string();
            let mut keep_data = false;
            for value in values {
                if value == "--keep-data" {
                    keep_data = true;
                } else {
                    return Err(error("/plugins uninstall <name> [--keep-data]"));
                }
            }
            Ok(PluginCommand::Uninstall { name, keep_data })
        }
        "enable" => Ok(PluginCommand::Enable {
            name: required_argument(ExtensionKind::Plugins, rest, "/plugins enable <name>")?
                .to_string(),
        }),
        "disable" => Ok(PluginCommand::Disable {
            name: required_argument(ExtensionKind::Plugins, rest, "/plugins disable <name>")?
                .to_string(),
        }),
        "themes" => Ok(PluginCommand::Themes),
        "theme" => Ok(PluginCommand::Theme {
            name: parse_optional_preference(rest, "/plugins theme <name|default>")?,
        }),
        "styles" => Ok(PluginCommand::Styles),
        "style" => Ok(PluginCommand::Style {
            name: parse_optional_preference(rest, "/plugins style <name|default>")?,
        }),
        "config" | "configure" => {
            let (name, json) =
                split_head(rest).ok_or_else(|| error("/plugins config <name> <json-object>"))?;
            let values = serde_json::from_str::<HashMap<String, serde_json::Value>>(json)
                .map_err(|parse| error(&format!("Plugin config JSON is invalid: {parse}")))?;
            Ok(PluginCommand::Configure {
                name: name.to_string(),
                values,
            })
        }
        "init" | "scaffold" => {
            let (directory, name) =
                split_head(rest).ok_or_else(|| error("/plugins init <directory> <name>"))?;
            Ok(PluginCommand::Scaffold {
                directory: directory.to_string(),
                name: required_argument(
                    ExtensionKind::Plugins,
                    name,
                    "/plugins init <directory> <name>",
                )?
                .to_string(),
            })
        }
        "validate" => Ok(PluginCommand::Validate {
            directory: required_argument(
                ExtensionKind::Plugins,
                rest,
                "/plugins validate <directory>",
            )?
            .to_string(),
        }),
        _ => Err(error(
            "/plugins [list|info|reload|install|uninstall|enable|disable|themes|theme|styles|style|config|init|validate]",
        )),
    }
}

fn parse_optional_preference(
    input: &str,
    usage: &str,
) -> Result<Option<String>, ExtensionCommandParseError> {
    let value = required_argument(ExtensionKind::Plugins, input, usage)?;
    Ok((!matches!(value, "default" | "off" | "none")).then(|| value.to_string()))
}

fn parse_mcp_command(input: &str) -> Result<McpCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("list", ""));
    match action {
        "" | "list" | "ls" => Ok(McpCommand::List),
        "connect" => Ok(McpCommand::Connect {
            name: required_argument(ExtensionKind::Mcp, rest, "/mcp connect <name>")?.to_string(),
        }),
        "disconnect" => Ok(McpCommand::Disconnect {
            name: required_argument(ExtensionKind::Mcp, rest, "/mcp disconnect <name>")?
                .to_string(),
        }),
        "remove" => Ok(McpCommand::Remove {
            name: required_argument(ExtensionKind::Mcp, rest, "/mcp remove <name>")?.to_string(),
        }),
        "enable" | "disable" => Ok(McpCommand::SetEnabled {
            name: required_argument(ExtensionKind::Mcp, rest, "/mcp <enable|disable> <name>")?
                .to_string(),
            enabled: action == "enable",
        }),
        "upsert" => {
            let (name, json) = split_head(rest).ok_or_else(|| {
                ExtensionCommandParseError::new(
                    Some(ExtensionKind::Mcp),
                    "/mcp upsert <name> <server-json>",
                )
            })?;
            let server = serde_json::from_str(json).map_err(|error| {
                ExtensionCommandParseError::new(
                    Some(ExtensionKind::Mcp),
                    format!("MCP server JSON is invalid: {error}"),
                )
            })?;
            Ok(McpCommand::Upsert {
                name: name.to_string(),
                server,
            })
        }
        "import" => Ok(McpCommand::Import {
            config: serde_json::from_str(required_argument(
                ExtensionKind::Mcp,
                rest,
                "/mcp import <config-json>",
            )?)
            .map_err(|error| {
                ExtensionCommandParseError::new(
                    Some(ExtensionKind::Mcp),
                    format!("MCP config JSON is invalid: {error}"),
                )
            })?,
        }),
        _ => Err(ExtensionCommandParseError::new(
            Some(ExtensionKind::Mcp),
            "/mcp [list|connect|disconnect|remove|enable|disable|upsert|import]",
        )),
    }
}

fn parse_hook_command(input: &str) -> Result<HookCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("list", ""));
    match action {
        "" | "list" | "ls" => Ok(HookCommand::List),
        "reload" => Ok(HookCommand::Reload),
        "test" => {
            let (event, matcher) = split_head(rest).ok_or_else(|| {
                ExtensionCommandParseError::new(
                    Some(ExtensionKind::Hooks),
                    "/hooks test <event> [matcher]",
                )
            })?;
            Ok(HookCommand::Test {
                event: event.to_string(),
                matcher: if matcher.is_empty() { "*" } else { matcher }.to_string(),
            })
        }
        _ => Err(ExtensionCommandParseError::new(
            Some(ExtensionKind::Hooks),
            "/hooks [list|reload|test]",
        )),
    }
}

fn parse_lsp_command(input: &str) -> Result<LspCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("status", ""));
    let language =
        |usage: &str| required_argument(ExtensionKind::Lsp, rest, usage).map(str::to_string);
    match action {
        "" | "status" => Ok(LspCommand::Status),
        "list" | "ls" => Ok(LspCommand::List),
        "start" => Ok(LspCommand::Start {
            language: language("/lsp start <language>")?,
        }),
        "stop" => Ok(LspCommand::Stop {
            language: language("/lsp stop <language>")?,
        }),
        "restart" => Ok(LspCommand::Restart {
            language: language("/lsp restart <language>")?,
        }),
        _ => Err(ExtensionCommandParseError::new(
            Some(ExtensionKind::Lsp),
            "/lsp [list|status|start|stop|restart] [language]",
        )),
    }
}

fn parse_browser_command(input: &str) -> Result<BrowserCommand, ExtensionCommandParseError> {
    let (action, rest) = split_head(input).unwrap_or(("status", ""));
    let error = |usage: &str| ExtensionCommandParseError::new(Some(ExtensionKind::Browser), usage);
    match action {
        "" | "status" => Ok(BrowserCommand::Status),
        "managed" => Ok(BrowserCommand::Managed),
        "chrome" => Ok(BrowserCommand::Chrome),
        "navigate" => Ok(BrowserCommand::Navigate {
            url: required_argument(ExtensionKind::Browser, rest, "/browser navigate <url>")?
                .to_string(),
        }),
        "snapshot" => Ok(BrowserCommand::Snapshot {
            filename: (!rest.is_empty()).then(|| rest.to_string()),
        }),
        "click-target" => {
            parse_browser_json_action("click_target", rest, "/browser click-target <json>")
        }
        "fill" => parse_browser_json_action("fill", rest, "/browser fill <json>"),
        "back" => Ok(BrowserCommand::Back),
        "reload" => Ok(BrowserCommand::Reload),
        "screenshot" => Ok(BrowserCommand::Screenshot),
        "click" => {
            let (x, y) = parse_browser_pair(rest, "/browser click <x> <y>")?;
            Ok(BrowserCommand::Click { x, y })
        }
        "scroll" => {
            let (delta_x, delta_y) =
                parse_browser_pair(rest, "/browser scroll <delta-x> <delta-y>")?;
            Ok(BrowserCommand::Scroll { delta_x, delta_y })
        }
        "type-at" => parse_browser_json_action("type_at", rest, "/browser type-at <json>"),
        "tabs" => parse_browser_tabs(rest),
        "console" => parse_browser_json_action("console", rest, "/browser console <json>"),
        "network" => parse_browser_json_action("network", rest, "/browser network <json>"),
        "dom-inspect" => {
            parse_browser_json_action("dom_inspect", rest, "/browser dom-inspect <json>")
        }
        "performance-trace" => parse_browser_json_action(
            "performance_trace",
            rest,
            "/browser performance-trace <json>",
        ),
        "developer-mode" => {
            parse_browser_json_action("developer_mode", rest, "/browser developer-mode <json>")
        }
        "stop" => Ok(BrowserCommand::Stop),
        _ => Err(error(
            "/browser [status|managed|chrome|navigate|snapshot|click-target|fill|back|reload|screenshot|click|type-at|scroll|tabs|console|network|dom-inspect|performance-trace|developer-mode|stop]",
        )),
    }
}

fn parse_browser_json_action(
    action: &str,
    input: &str,
    usage: &str,
) -> Result<BrowserCommand, ExtensionCommandParseError> {
    let mut value = serde_json::from_str::<serde_json::Value>(required_argument(
        ExtensionKind::Browser,
        input,
        usage,
    )?)
    .map_err(|error| {
        ExtensionCommandParseError::new(
            Some(ExtensionKind::Browser),
            format!("{usage}: invalid JSON: {error}"),
        )
    })?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ExtensionCommandParseError::new(Some(ExtensionKind::Browser), usage))?;
    object.insert(
        "action".to_string(),
        serde_json::Value::String(action.to_string()),
    );
    serde_json::from_value(value).map_err(|error| {
        ExtensionCommandParseError::new(Some(ExtensionKind::Browser), format!("{usage}: {error}"))
    })
}

fn parse_browser_pair(input: &str, usage: &str) -> Result<(f64, f64), ExtensionCommandParseError> {
    let mut values = input.split_whitespace();
    let first = values
        .next()
        .ok_or_else(|| ExtensionCommandParseError::new(Some(ExtensionKind::Browser), usage))?;
    let second = values
        .next()
        .ok_or_else(|| ExtensionCommandParseError::new(Some(ExtensionKind::Browser), usage))?;
    if values.next().is_some() {
        return Err(ExtensionCommandParseError::new(
            Some(ExtensionKind::Browser),
            usage,
        ));
    }
    let first = first.parse::<f64>().map_err(|error| {
        ExtensionCommandParseError::new(
            Some(ExtensionKind::Browser),
            format!("{usage}: invalid first coordinate: {error}"),
        )
    })?;
    let second = second.parse::<f64>().map_err(|error| {
        ExtensionCommandParseError::new(
            Some(ExtensionKind::Browser),
            format!("{usage}: invalid second coordinate: {error}"),
        )
    })?;
    Ok((first, second))
}

fn parse_browser_tabs(input: &str) -> Result<BrowserCommand, ExtensionCommandParseError> {
    let (action, value) = split_head(input).unwrap_or(("list", ""));
    let usage = "/browser tabs <list|select|new|close> [index|url]";
    let error = || ExtensionCommandParseError::new(Some(ExtensionKind::Browser), usage);
    match action {
        "list" => {
            if !value.is_empty() {
                return Err(error());
            }
            Ok(BrowserCommand::Tabs {
                tab_action: action.to_string(),
                index: None,
                url: None,
            })
        }
        "select" | "close" => {
            let index = value.parse::<u64>().map_err(|_| error())?;
            Ok(BrowserCommand::Tabs {
                tab_action: action.to_string(),
                index: Some(index),
                url: None,
            })
        }
        "new" => Ok(BrowserCommand::Tabs {
            tab_action: action.to_string(),
            index: None,
            url: (!value.is_empty()).then(|| value.to_string()),
        }),
        _ => Err(error()),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ExtensionCommandStatus {
    Committed,
    Settled,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionReceiptMeta {
    pub request_id: String,
    pub operation_id: String,
    pub authority_scope: String,
    pub workspace_generation: String,
    pub sender_id: Option<String>,
    pub sender_incarnation: Option<String>,
    pub status: ExtensionCommandStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "extension", rename_all = "snake_case")]
#[ts(export)]
pub enum ExtensionCommandReceipt {
    Skills {
        meta: ExtensionReceiptMeta,
        receipt: Option<SkillCommandReceipt>,
    },
    Plugins {
        meta: ExtensionReceiptMeta,
        receipt: Option<PluginCommandReceipt>,
    },
    Mcp {
        meta: ExtensionReceiptMeta,
        receipt: Option<McpCommandReceipt>,
    },
    Hooks {
        meta: ExtensionReceiptMeta,
        receipt: Option<HookCommandReceipt>,
    },
    Lsp {
        meta: ExtensionReceiptMeta,
        receipt: Option<ExtensionMessageReceipt>,
    },
    Browser {
        meta: ExtensionReceiptMeta,
        receipt: Option<ExtensionMessageReceipt>,
    },
}

impl ExtensionCommandReceipt {
    pub fn status(&self) -> ExtensionCommandStatus {
        self.meta().status
    }

    pub fn meta(&self) -> &ExtensionReceiptMeta {
        match self {
            Self::Skills { meta, .. }
            | Self::Plugins { meta, .. }
            | Self::Mcp { meta, .. }
            | Self::Hooks { meta, .. }
            | Self::Lsp { meta, .. }
            | Self::Browser { meta, .. } => meta,
        }
    }

    pub fn failed(
        kind: ExtensionKind,
        identity: ExtensionCommandIdentity,
        authority_scope: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        let authority_scope = authority_scope.into();
        Self::failed_scoped(
            kind,
            identity,
            ExtensionRequestScope {
                workspace_generation: if authority_scope == "global" {
                    "global".to_string()
                } else {
                    "unresolved".to_string()
                },
                workspace_id: authority_scope,
                sender_id: None,
                sender_incarnation: None,
            },
            error,
        )
    }

    pub fn failed_scoped(
        kind: ExtensionKind,
        identity: ExtensionCommandIdentity,
        scope: ExtensionRequestScope,
        error: impl Into<String>,
    ) -> Self {
        let meta = ExtensionReceiptMeta {
            request_id: identity.request_id,
            operation_id: identity.operation_id,
            authority_scope: scope.workspace_id,
            workspace_generation: scope.workspace_generation,
            sender_id: scope.sender_id,
            sender_incarnation: scope.sender_incarnation,
            status: ExtensionCommandStatus::Failed,
            error: Some(bounded_text(error.into())),
        };
        match kind {
            ExtensionKind::Skills => Self::Skills {
                meta,
                receipt: None,
            },
            ExtensionKind::Plugins => Self::Plugins {
                meta,
                receipt: None,
            },
            ExtensionKind::Mcp => Self::Mcp {
                meta,
                receipt: None,
            },
            ExtensionKind::Hooks => Self::Hooks {
                meta,
                receipt: None,
            },
            ExtensionKind::Lsp => Self::Lsp {
                meta,
                receipt: None,
            },
            ExtensionKind::Browser => Self::Browser {
                meta,
                receipt: None,
            },
        }
    }

    /// Deterministic text projection shared by text surfaces. The renderer is
    /// deliberately typed: it never round-trips a receipt through JSON and it
    /// exposes degraded/failed settlement before any action detail.
    pub fn display_message(&self) -> String {
        let meta = self.meta();
        let mut lines = vec![format!(
            "[{}] Extension scope={} workspace_generation={} request_id={} operation_id={}",
            extension_status_label(meta.status),
            meta.authority_scope,
            meta.workspace_generation,
            meta.request_id,
            meta.operation_id,
        )];
        if let Some(error) = meta.error.as_deref() {
            lines.push(format!("error={error}"));
        }
        match self {
            Self::Skills { receipt, .. } => render_skill_command(receipt.as_ref(), &mut lines),
            Self::Plugins { receipt, .. } => render_plugin_command(receipt.as_ref(), &mut lines),
            Self::Mcp { receipt, .. } => render_mcp_command(receipt.as_ref(), &mut lines),
            Self::Hooks { receipt, .. } => render_hook_command(receipt.as_ref(), &mut lines),
            Self::Lsp { receipt, .. } => {
                render_message_command("LSP", receipt.as_ref(), &mut lines)
            }
            Self::Browser { receipt, .. } => {
                render_message_command("Browser", receipt.as_ref(), &mut lines);
            }
        }
        lines.join("\n")
    }
}

fn extension_status_label(status: ExtensionCommandStatus) -> &'static str {
    match status {
        ExtensionCommandStatus::Committed => "COMMITTED",
        ExtensionCommandStatus::Settled => "SETTLED",
        ExtensionCommandStatus::Degraded => "DEGRADED",
        ExtensionCommandStatus::Failed => "FAILED",
    }
}

fn render_skill_command(receipt: Option<&SkillCommandReceipt>, lines: &mut Vec<String>) {
    match receipt {
        Some(SkillCommandReceipt::Listed { skills }) => {
            lines.push(format!(
                "Skills listed: {} item(s), {} omitted",
                skills.len(),
                skills.omitted
            ));
            lines.extend(skills.iter().map(render_skill_entry));
        }
        Some(SkillCommandReceipt::Searched { query, skills }) => {
            lines.push(format!(
                "Skill search '{query}': {} result(s), {} omitted",
                skills.len(),
                skills.omitted
            ));
            lines.extend(skills.iter().map(render_skill_entry));
        }
        Some(SkillCommandReceipt::Info { skill }) => match skill {
            Some(skill) => lines.push(render_skill_entry(skill)),
            None => lines.push("Skill not found.".to_string()),
        },
        Some(SkillCommandReceipt::Installed { settlement }) => {
            lines.push(format!(
                "Installed skill '{}' from {} at {} revision={}",
                settlement.name,
                settlement.source,
                settlement.path.display(),
                settlement.revision.as_deref().unwrap_or("none"),
            ));
            render_skill_settlement(&settlement.settlement, lines);
        }
        Some(SkillCommandReceipt::Uninstalled { settlement }) => {
            lines.push(format!(
                "Uninstalled skill '{}' artifact_removed={} artifact_error={}",
                settlement.name,
                settlement.artifact_removed,
                settlement.artifact_error.as_deref().unwrap_or("none"),
            ));
            render_skill_settlement(&settlement.settlement, lines);
        }
        Some(SkillCommandReceipt::Enabled { settlement }) => {
            lines.push("Skill enablement submitted.".to_string());
            render_skill_settlement(settlement, lines);
        }
        Some(SkillCommandReceipt::Disabled { settlement }) => {
            lines.push("Skill disablement submitted.".to_string());
            render_skill_settlement(settlement, lines);
        }
        Some(SkillCommandReceipt::Refreshed { settlement }) => {
            lines.push("Skill policy refreshed.".to_string());
            render_skill_settlement(settlement, lines);
        }
        Some(SkillCommandReceipt::UpdatesChecked { updates }) => {
            lines.push(format!(
                "Skill updates checked: {} item(s), {} omitted",
                updates.len(),
                updates.omitted
            ));
            lines.extend(updates.iter().map(|update| {
                format!("  [{}] {} - {}", update.state, update.name, update.message)
            }));
        }
        Some(SkillCommandReceipt::Synced { settlement }) => {
            lines.push(format!(
                "Skill artifacts synced: {} result(s)",
                settlement.results.len()
            ));
            lines.extend(settlement.results.iter().map(|result| {
                format!(
                    "  skill={} success={} updated={} revision={} message={}",
                    result.name,
                    result.success,
                    result.updated,
                    result.revision.as_deref().unwrap_or("none"),
                    result.message,
                )
            }));
            render_skill_settlement(&settlement.settlement, lines);
        }
        None => {}
    }
}

fn render_skill_entry(entry: &ExtensionSkillEntry) -> String {
    format!(
        "  [{}] {} - {} ({})",
        if entry.loaded { "loaded" } else { "available" },
        entry.catalog.name,
        entry.catalog.description,
        entry.catalog.path.display(),
    )
}

fn render_skill_settlement(receipt: &SkillSyncReceipt, lines: &mut Vec<String>) {
    lines.push(format!(
        "  settlement={:?} operation_id={} committed_file_path={} content_identity={} generation={}/{} durable_committed={} idempotent={}",
        receipt.status,
        receipt.operation_id,
        receipt.committed_file_path.display(),
        receipt.content_identity,
        receipt.settled_generation,
        receipt.desired_generation,
        receipt.durable_committed,
        receipt.idempotent,
    ));
    lines.extend(receipt.target_receipts.iter().map(|target| {
        format!(
            "  target={} status={:?} workspace_generation={} specialist_generation={} changed_entries={} error={}",
            target.target,
            target.status,
            target.workspace_generation,
            target.specialist_generation,
            if target.changed_entries.is_empty() {
                "none".to_string()
            } else {
                target.changed_entries.join(",")
            },
            target.error.as_deref().unwrap_or("none"),
        )
    }));
    if let Some(debt) = receipt.repair_debt.as_ref() {
        lines.push(format!(
            "  repair_debt generation={} attempts={} content_identity={}",
            debt.generation, debt.attempts, debt.content_identity,
        ));
        lines.extend(
            debt.target_failures
                .iter()
                .map(|failure| {
                    format!(
                        "  repair_target target={} component={} expected_generation={} observed_generation={} retryable={} reason={}",
                        failure.target,
                        failure.component,
                        failure.expected_generation,
                        failure
                            .observed_generation
                            .map_or_else(|| "none".to_string(), |generation| generation.to_string()),
                        failure.retryable,
                        failure.reason,
                    )
                }),
        );
        lines.extend(
            debt.artifact_removals
                .iter()
                .map(|name| format!("  repair_artifact_removal={name}")),
        );
        lines.extend(debt.artifact_syncs.iter().map(|pending| {
            format!(
                "  repair_artifact_sync={} force={}",
                pending.name, pending.force
            )
        }));
        lines.extend(
            debt.artifact_enablements
                .iter()
                .map(|name| format!("  repair_artifact_enablement={name}")),
        );
    }
}

fn render_plugin_command(receipt: Option<&PluginCommandReceipt>, lines: &mut Vec<String>) {
    match receipt {
        Some(PluginCommandReceipt::Listed { plugins }) => {
            lines.push(format!(
                "Plugins listed: {} item(s), {} omitted",
                plugins.items.len(),
                plugins.omitted
            ));
            lines.extend(plugins.items.iter().map(render_plugin_entry));
        }
        Some(PluginCommandReceipt::Info { plugin }) => match plugin {
            Some(plugin) => lines.push(render_plugin_entry(plugin)),
            None => lines.push("Plugin not found.".to_string()),
        },
        Some(PluginCommandReceipt::Mutation { projection }) => {
            lines.push(format!(
                "Plugin mutation plugin_id={} total={} enabled={} skills={} hooks={} mcp={} agents={} lsp={} monitors={} themes={} output_styles={}",
                projection.plugin_id.as_deref().unwrap_or("none"),
                projection.summary.total,
                projection.summary.enabled,
                projection.summary.skills_loaded,
                projection.summary.hooks_registered,
                projection.summary.mcp_connected,
                projection.summary.agents_loaded,
                projection.summary.lsp_languages_loaded,
                projection.summary.monitors_loaded,
                projection.summary.themes_loaded,
                projection.summary.output_styles_loaded,
            ));
            if let Some(plugin) = projection.plugin.as_ref() {
                lines.push(render_plugin_entry(plugin));
            }
            render_bounded_errors(&projection.summary.errors, lines);
            lines.push(format!(
                "active_theme={} themes={} omitted_themes={} active_output_style={} output_styles={} omitted_output_styles={}",
                projection.active_theme.as_deref().unwrap_or("default"),
                projection.themes.items.len(),
                projection.themes.omitted,
                projection.active_output_style.as_deref().unwrap_or("default"),
                projection.output_styles.items.len(),
                projection.output_styles.omitted,
            ));
            lines.extend(projection.themes.items.iter().map(render_plugin_theme));
            lines.extend(
                projection
                    .output_styles
                    .items
                    .iter()
                    .map(render_plugin_style),
            );
        }
        Some(PluginCommandReceipt::Themes { active, themes }) => {
            lines.push(format!(
                "Plugin themes active={} count={} omitted={}",
                active.as_deref().unwrap_or("default"),
                themes.items.len(),
                themes.omitted,
            ));
            lines.extend(themes.items.iter().map(render_plugin_theme));
        }
        Some(PluginCommandReceipt::Theme { active, theme }) => {
            lines.push(format!(
                "Plugin theme active={}",
                active.as_deref().unwrap_or("default")
            ));
            if let Some(theme) = theme {
                lines.push(render_plugin_theme(theme));
            }
        }
        Some(PluginCommandReceipt::Styles { active, styles }) => {
            lines.push(format!(
                "Plugin output styles active={} count={} omitted={}",
                active.as_deref().unwrap_or("default"),
                styles.items.len(),
                styles.omitted,
            ));
            lines.extend(styles.items.iter().map(render_plugin_style));
        }
        Some(PluginCommandReceipt::Style { active }) => lines.push(format!(
            "Plugin output style active={}",
            active.as_deref().unwrap_or("default")
        )),
        Some(PluginCommandReceipt::Scaffolded { scaffold }) => lines.push(format!(
            "Plugin '{}' scaffolded at {}.",
            scaffold.name, scaffold.path
        )),
        Some(PluginCommandReceipt::Validated { validation }) => {
            lines.push(format!(
                "Plugin validation valid={} name={} components={} omitted_components={}",
                validation.valid,
                validation.name.as_deref().unwrap_or("unknown"),
                validation.components.items.join(","),
                validation.components.omitted,
            ));
            render_bounded_errors(&validation.errors, lines);
        }
        None => {}
    }
}

fn render_plugin_entry(entry: &PluginEntryProjection) -> String {
    format!(
        "  plugin={} version={} enabled={} scope={} path={} capabilities={} description={}",
        entry.name,
        entry.version,
        entry.enabled,
        entry.scope,
        entry.root,
        if entry.capabilities.is_empty() {
            "none".to_string()
        } else {
            entry.capabilities.join(",")
        },
        entry.description,
    )
}

fn render_plugin_theme(theme: &PluginThemeProjection) -> String {
    format!(
        "  theme={} display_name={} dark={} plugin={}",
        theme.name,
        theme.display_name.as_deref().unwrap_or(&theme.name),
        theme.dark,
        theme.plugin,
    )
}

fn render_plugin_style(style: &PluginOutputStyleProjection) -> String {
    format!(
        "  output_style={} plugin={} description={}",
        style.name, style.plugin, style.description,
    )
}

fn render_bounded_errors(errors: &BoundedItems<String>, lines: &mut Vec<String>) {
    lines.extend(
        errors
            .items
            .iter()
            .map(|error| format!("  plugin_error={error}")),
    );
    if errors.omitted > 0 {
        lines.push(format!("  plugin_errors_omitted={}", errors.omitted));
    }
}

fn render_mcp_command(receipt: Option<&McpCommandReceipt>, lines: &mut Vec<String>) {
    match receipt {
        Some(McpCommandReceipt::Listed { servers }) => {
            lines.push(format!(
                "MCP servers listed: {} item(s), {} omitted",
                servers.items.len(),
                servers.omitted,
            ));
            for server in &servers.items {
                lines.push(format!(
                    "  server={} status={} transport={} enabled={} tool_count={} connected_at={} error={} omitted_tools={}",
                    server.name,
                    server.status,
                    server.transport,
                    server.enabled,
                    server.tool_count,
                    server.connected_at.as_deref().unwrap_or("none"),
                    server.error.as_deref().unwrap_or("none"),
                    server.tools.omitted,
                ));
                lines.extend(server.tools.items.iter().map(|tool| {
                    format!("    tool={} description={}", tool.name, tool.description)
                }));
            }
        }
        Some(McpCommandReceipt::Reconciled {
            name,
            enabled,
            generation,
        }) => lines.push(format!(
            "MCP server '{}' enabled={} generation={generation}",
            name, enabled,
        )),
        Some(McpCommandReceipt::Configured { name, generation }) => lines.push(format!(
            "MCP configuration committed name={} generation={generation}",
            name.as_deref().unwrap_or("all"),
        )),
        None => {}
    }
}

fn render_hook_command(receipt: Option<&HookCommandReceipt>, lines: &mut Vec<String>) {
    match receipt {
        Some(HookCommandReceipt::Listed { sources }) => {
            lines.push(format!(
                "Hook sources listed: {} item(s), {} omitted",
                sources.items.len(),
                sources.omitted,
            ));
            lines.extend(
                sources
                    .items
                    .iter()
                    .map(|source| format!("  source={} rules={}", source.source, source.rules)),
            );
        }
        Some(HookCommandReceipt::Reloaded {
            loaded_from,
            rule_count,
        }) => {
            lines.push(format!(
                "Hooks reloaded: rules={} sources={} omitted={}",
                rule_count,
                loaded_from.items.len(),
                loaded_from.omitted,
            ));
            lines.extend(
                loaded_from
                    .items
                    .iter()
                    .map(|source| format!("  loaded_from={source}")),
            );
        }
        Some(HookCommandReceipt::Tested {
            event,
            matcher,
            matches,
        }) => {
            lines.push(format!(
                "Hook test event={event} matcher={matcher}: {} match(es), {} omitted",
                matches.len(),
                matches.omitted
            ));
            lines.extend(matches.iter().map(|item| {
                format!(
                    "  source={} matcher={} action={}",
                    item.source, item.matcher, item.action
                )
            }));
        }
        None => {}
    }
}

fn render_message_command(
    label: &str,
    receipt: Option<&ExtensionMessageReceipt>,
    lines: &mut Vec<String>,
) {
    if let Some(receipt) = receipt {
        lines.push(format!(
            "{label} action={}: {}",
            receipt.action, receipt.message
        ));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum SkillCommandReceipt {
    Listed {
        skills: BoundedItems<ExtensionSkillEntry>,
    },
    Searched {
        query: String,
        skills: BoundedItems<ExtensionSkillEntry>,
    },
    Info {
        skill: Option<ExtensionSkillEntry>,
    },
    Installed {
        settlement: SkillInstallSettlementReceipt,
    },
    Uninstalled {
        settlement: SkillUninstallSettlementReceipt,
    },
    Enabled {
        settlement: SkillSyncReceipt,
    },
    Disabled {
        settlement: SkillSyncReceipt,
    },
    Refreshed {
        settlement: SkillSyncReceipt,
    },
    UpdatesChecked {
        updates: BoundedItems<SkillUpdateProjection>,
    },
    Synced {
        settlement: SkillArtifactSyncReceipt,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct BoundedItems<T> {
    pub items: Vec<T>,
    pub omitted: usize,
}

impl<T> BoundedItems<T> {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillUpdateProjection {
    pub name: String,
    pub state: String,
    pub current_revision: Option<String>,
    pub remote_revision: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginEntryProjection {
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub author: Option<PluginAuthorProjection>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub scope: String,
    pub enabled: bool,
    pub root: String,
    pub capabilities: Vec<String>,
    pub keywords: Vec<String>,
    pub dependencies: Vec<PluginDependencyProjection>,
    pub config: HashMap<String, PluginConfigEntryProjection>,
    pub config_values: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginAuthorProjection {
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginDependencyProjection {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginConfigEntryProjection {
    #[serde(rename = "type")]
    pub value_type: String,
    pub title: String,
    pub description: String,
    pub sensitive: bool,
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub multiple: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginReloadProjection {
    pub total: usize,
    pub enabled: usize,
    pub skills_loaded: usize,
    pub hooks_registered: usize,
    pub mcp_connected: usize,
    pub agents_loaded: usize,
    pub lsp_languages_loaded: usize,
    pub monitors_loaded: usize,
    pub themes_loaded: usize,
    pub output_styles_loaded: usize,
    pub errors: BoundedItems<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginThemeProjection {
    pub name: String,
    pub display_name: Option<String>,
    pub dark: bool,
    pub colors: HashMap<String, String>,
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginOutputStyleProjection {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginScaffoldProjection {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginValidationProjection {
    pub valid: bool,
    pub name: Option<String>,
    pub components: BoundedItems<String>,
    pub errors: BoundedItems<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
#[allow(clippy::large_enum_variant)]
pub enum PluginCommandReceipt {
    Listed {
        plugins: BoundedItems<PluginEntryProjection>,
    },
    Info {
        plugin: Option<PluginEntryProjection>,
    },
    Mutation {
        projection: Box<PluginMutationProjection>,
    },
    Themes {
        active: Option<String>,
        themes: BoundedItems<PluginThemeProjection>,
    },
    Theme {
        active: Option<String>,
        theme: Option<PluginThemeProjection>,
    },
    Styles {
        active: Option<String>,
        styles: BoundedItems<PluginOutputStyleProjection>,
    },
    Style {
        active: Option<String>,
    },
    Scaffolded {
        scaffold: PluginScaffoldProjection,
    },
    Validated {
        validation: PluginValidationProjection,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PluginMutationProjection {
    pub plugin_id: Option<String>,
    pub plugin: Option<PluginEntryProjection>,
    pub summary: PluginReloadProjection,
    pub active_theme: Option<String>,
    pub themes: BoundedItems<PluginThemeProjection>,
    pub active_output_style: Option<String>,
    pub output_styles: BoundedItems<PluginOutputStyleProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct McpToolProjection {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct McpServerProjection {
    pub name: String,
    pub status: String,
    pub transport: String,
    pub tool_count: usize,
    pub tools: BoundedItems<McpToolProjection>,
    pub connected_at: Option<String>,
    pub error: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum McpCommandReceipt {
    Listed {
        servers: BoundedItems<McpServerProjection>,
    },
    Reconciled {
        name: String,
        enabled: bool,
        generation: String,
    },
    Configured {
        name: Option<String>,
        generation: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HookSourceProjection {
    pub source: String,
    pub rules: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HookTestMatchProjection {
    pub source: String,
    pub matcher: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum HookCommandReceipt {
    Listed {
        sources: BoundedItems<HookSourceProjection>,
    },
    Reloaded {
        loaded_from: BoundedItems<String>,
        rule_count: usize,
    },
    Tested {
        event: String,
        matcher: String,
        matches: BoundedItems<HookTestMatchProjection>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionMessageReceipt {
    pub action: String,
    pub message: String,
}

#[derive(Clone)]
pub struct ExtensionCommandDispatcher {
    state: Arc<AppState>,
}

impl ExtensionCommandDispatcher {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Admit one command into an owned task. Once this future has been polled
    /// and the task is spawned, dropping the surface caller does not cancel an
    /// accepted specialist settlement.
    pub async fn dispatch(
        &self,
        request: ExtensionCommandRequest,
        runtime: Option<ScopedChatRuntime>,
        conversation_id: String,
    ) -> ExtensionCommandReceipt {
        let state = Arc::clone(&self.state);
        let fallback_identity = request.identity();
        let fallback_kind = request.kind();
        let fallback_scope = match (request.scope.clone(), runtime.as_ref()) {
            (Some(scope), _) => scope,
            (None, Some(runtime)) => state
                .product_data_for_runtime(runtime)
                .await
                .map(|product_data| ExtensionRequestScope {
                    workspace_id: product_data.workspace_id().to_string(),
                    workspace_generation: product_data.generation(),
                    sender_id: None,
                    sender_incarnation: None,
                })
                .unwrap_or_else(|_| ExtensionRequestScope {
                    workspace_id: runtime.execution_scope().workspace_id().to_string(),
                    workspace_generation: "unresolved".to_string(),
                    sender_id: None,
                    sender_incarnation: None,
                }),
            (None, None) => ExtensionRequestScope {
                workspace_id: "unresolved".to_string(),
                workspace_generation: "unresolved".to_string(),
                sender_id: None,
                sender_incarnation: None,
            },
        };
        match tokio::spawn(async move {
            dispatch_owned(state, request, runtime.as_ref(), &conversation_id).await
        })
        .await
        {
            Ok(receipt) => receipt,
            Err(error) => ExtensionCommandReceipt::failed_scoped(
                fallback_kind,
                fallback_identity,
                fallback_scope,
                format!("Extension settlement task failed: {error}"),
            ),
        }
    }

    /// Resolve and pin the exact workspace requested by a structured surface.
    /// Focus changes after admission cannot retarget the accepted command.
    pub async fn dispatch_for_scope(
        &self,
        scope: ExtensionRequestScope,
        mut request: ExtensionCommandRequest,
        conversation_id: String,
    ) -> ExtensionCommandReceipt {
        let identity = request.identity();
        let kind = request.kind();
        if let Err(error) = scope.validate() {
            return ExtensionCommandReceipt::failed_scoped(
                kind,
                identity,
                scope,
                error.to_string(),
            );
        }
        let control = match self
            .state
            .workspace_control_for_scope(&scope.workspace_id)
            .await
        {
            Ok(control) => control,
            Err(error) => {
                return ExtensionCommandReceipt::failed_scoped(
                    kind,
                    identity,
                    scope,
                    error.to_string(),
                );
            }
        };
        if let Err(error) = control.validate_generation(&scope.workspace_generation) {
            return ExtensionCommandReceipt::failed_scoped(
                kind,
                identity,
                scope,
                error.to_string(),
            );
        }
        let runtime = control.runtime().clone();
        request.scope = Some(scope);
        self.dispatch(request, Some(runtime), conversation_id).await
    }
}

async fn dispatch_owned(
    state: Arc<AppState>,
    request: ExtensionCommandRequest,
    runtime: Option<&ScopedChatRuntime>,
    conversation_id: &str,
) -> ExtensionCommandReceipt {
    let identity = request.identity();
    let kind = request.kind();
    let (scope, runtime) =
        match resolve_request_scope(&state, request.scope.as_ref(), runtime).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return ExtensionCommandReceipt::failed_scoped(
                    kind,
                    identity,
                    request.scope.unwrap_or_else(|| ExtensionRequestScope {
                        workspace_id: "unresolved".to_string(),
                        workspace_generation: "unresolved".to_string(),
                        sender_id: None,
                        sender_incarnation: None,
                    }),
                    error.to_string(),
                );
            }
        };
    match request.command {
        ExtensionCommand::Skills(command) => {
            dispatch_skill(&state, Some(&runtime), identity, scope, command).await
        }
        ExtensionCommand::Plugins(command) => {
            dispatch_plugin(&state, Some(&runtime), identity, scope, command).await
        }
        ExtensionCommand::Mcp(command) => {
            dispatch_mcp(&state, Some(&runtime), identity, scope, command).await
        }
        ExtensionCommand::Hooks(command) => {
            dispatch_hook(&state, Some(&runtime), identity, scope, command).await
        }
        ExtensionCommand::Lsp(command) => {
            dispatch_lsp(&state, Some(&runtime), identity, scope, command).await
        }
        ExtensionCommand::Browser(command) => {
            dispatch_browser(
                &state,
                Some(&runtime),
                identity,
                scope,
                conversation_id,
                command,
            )
            .await
        }
    }
}

async fn resolve_request_scope(
    state: &AppState,
    requested: Option<&ExtensionRequestScope>,
    runtime: Option<&ScopedChatRuntime>,
) -> anyhow::Result<(ExtensionRequestScope, ScopedChatRuntime)> {
    if let Some(requested) = requested {
        requested.validate().map_err(anyhow::Error::new)?;
    }
    let product_data = match runtime {
        Some(runtime) => state.product_data_for_runtime(runtime).await?,
        None => match requested {
            Some(scope) => {
                return state
                    .workspace_control_for_scope(&scope.workspace_id)
                    .await
                    .and_then(|control| {
                        control
                            .validate_generation(&scope.workspace_generation)
                            .map_err(anyhow::Error::new)?;
                        Ok((scope.clone(), control.runtime().clone()))
                    });
            }
            None => state
                .current_product_data()
                .await
                .map_err(anyhow::Error::new)?,
        },
    };
    let actual = ExtensionRequestScope {
        workspace_id: product_data.workspace_id().to_string(),
        workspace_generation: product_data.generation(),
        sender_id: requested.and_then(|scope| scope.sender_id.clone()),
        sender_incarnation: requested.and_then(|scope| scope.sender_incarnation.clone()),
    };
    if let Some(requested) = requested
        && requested != &actual
    {
        anyhow::bail!(
            "Extension request scope is stale: expected workspace '{}' generation '{}'",
            requested.workspace_id,
            requested.workspace_generation
        );
    }
    Ok((actual, product_data.runtime().clone()))
}

async fn dispatch_skill(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    command: SkillCommand,
) -> ExtensionCommandReceipt {
    let result = match command {
        SkillCommand::List => state
            .extension_control
            .list_skills_scoped(state, runtime)
            .await
            .map(|skills| SkillCommandReceipt::Listed {
                skills: bounded_items(skills, MAX_EXTENSION_ITEMS),
            }),
        SkillCommand::Search { query } => {
            let folded = query.to_lowercase();
            state
                .extension_control
                .list_skills_scoped(state, runtime)
                .await
                .map(|skills| SkillCommandReceipt::Searched {
                    query,
                    skills: bounded_items(
                        skills.into_iter().filter(|entry| {
                            entry.catalog.name.to_lowercase().contains(&folded)
                                || entry.catalog.description.to_lowercase().contains(&folded)
                                || entry
                                    .catalog
                                    .tags
                                    .iter()
                                    .any(|tag| tag.to_lowercase().contains(&folded))
                        }),
                        MAX_EXTENSION_ITEMS,
                    ),
                })
        }
        SkillCommand::Info { name } => state
            .extension_control
            .list_skills_scoped(state, runtime)
            .await
            .map(|skills| SkillCommandReceipt::Info {
                skill: skills.into_iter().find(|entry| entry.catalog.name == name),
            }),
        SkillCommand::Install { source } => state
            .extension_control
            .install_skill_with_operation(state, &identity.operation_id, &source)
            .await
            .map(|settlement| SkillCommandReceipt::Installed { settlement })
            .map_err(anyhow::Error::new),
        SkillCommand::Uninstall { name } => state
            .extension_control
            .uninstall_skill_with_operation(state, &identity.operation_id, &name)
            .await
            .map(|settlement| SkillCommandReceipt::Uninstalled { settlement })
            .map_err(anyhow::Error::new),
        SkillCommand::Enable { name } => state
            .extension_control
            .set_skill_enabled_with_operation(state, &identity.operation_id, &name, true)
            .await
            .map(|settlement| SkillCommandReceipt::Enabled { settlement })
            .map_err(anyhow::Error::new),
        SkillCommand::Disable { name } => state
            .extension_control
            .set_skill_enabled_with_operation(state, &identity.operation_id, &name, false)
            .await
            .map(|settlement| SkillCommandReceipt::Disabled { settlement })
            .map_err(anyhow::Error::new),
        SkillCommand::Refresh => state
            .extension_control
            .refresh_enabled_skills_with_operation(state, &identity.operation_id)
            .await
            .map(|settlement| SkillCommandReceipt::Refreshed { settlement })
            .map_err(anyhow::Error::new),
        SkillCommand::CheckUpdates { target } => {
            let root = state.skills_hub.read().await.root().to_path_buf();
            let hub = crate::skills_hub::SkillsHub::with_root(root);
            crate::skills_hub::check_updates(&hub, target.as_deref())
                .await
                .map(|updates| SkillCommandReceipt::UpdatesChecked {
                    updates: bounded_items(
                        updates.into_iter().map(project_skill_update),
                        MAX_EXTENSION_ITEMS,
                    ),
                })
                .map_err(anyhow::Error::msg)
        }
        SkillCommand::Sync { target, force } => state
            .extension_control
            .sync_skills_with_operation(state, &identity.operation_id, target.as_deref(), force)
            .await
            .map(|settlement| SkillCommandReceipt::Synced { settlement }),
    };
    match result {
        Ok(receipt) => {
            let status = skill_receipt_status(&receipt);
            ExtensionCommandReceipt::Skills {
                meta: success_meta(identity, scope, status),
                receipt: Some(receipt),
            }
        }
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Skills,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

fn skill_receipt_status(receipt: &SkillCommandReceipt) -> ExtensionCommandStatus {
    let settlement = match receipt {
        SkillCommandReceipt::Installed { settlement } => Some(&settlement.settlement),
        SkillCommandReceipt::Uninstalled { settlement } => Some(&settlement.settlement),
        SkillCommandReceipt::Enabled { settlement }
        | SkillCommandReceipt::Disabled { settlement }
        | SkillCommandReceipt::Refreshed { settlement } => Some(settlement),
        SkillCommandReceipt::Synced { settlement } => Some(&settlement.settlement),
        SkillCommandReceipt::Listed { .. }
        | SkillCommandReceipt::Searched { .. }
        | SkillCommandReceipt::Info { .. }
        | SkillCommandReceipt::UpdatesChecked { .. } => None,
    };
    match settlement.map(|settlement| &settlement.status) {
        Some(SkillSettlementStatus::Settled) | None => ExtensionCommandStatus::Settled,
        Some(SkillSettlementStatus::Committed) => ExtensionCommandStatus::Committed,
        Some(SkillSettlementStatus::Degraded) => ExtensionCommandStatus::Degraded,
    }
}

async fn dispatch_plugin(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    command: PluginCommand,
) -> ExtensionCommandReceipt {
    let result = match command {
        PluginCommand::List => state
            .extension_control
            .plugin_catalog_scoped(state, runtime)
            .await
            .map(|snapshot| PluginCommandReceipt::Listed {
                plugins: bounded_items(
                    snapshot.plugins.into_iter().map(project_plugin_entry),
                    MAX_EXTENSION_ITEMS,
                ),
            }),
        PluginCommand::Info { name } => state
            .extension_control
            .plugin_entry_scoped(state, runtime, &name)
            .await
            .map(|(_, entry)| PluginCommandReceipt::Info {
                plugin: entry.map(project_plugin_entry),
            }),
        PluginCommand::Reload => state
            .extension_control
            .reload_plugins_scoped(state, runtime)
            .await
            .map(project_plugin_mutation),
        PluginCommand::Install { source, scope } => {
            let source = echo_agent::plugin::InstallSource::parse(&source);
            state
                .extension_control
                .install_plugin_scoped(state, runtime, &source, scope.into())
                .await
                .map(project_plugin_mutation)
        }
        PluginCommand::Uninstall { name, keep_data } => state
            .extension_control
            .uninstall_plugin_scoped(state, runtime, &name, keep_data)
            .await
            .map(project_plugin_mutation),
        PluginCommand::Enable { name } => state
            .extension_control
            .set_plugin_enabled_scoped(state, runtime, &name, true)
            .await
            .map(project_plugin_mutation),
        PluginCommand::Disable { name } => state
            .extension_control
            .set_plugin_enabled_scoped(state, runtime, &name, false)
            .await
            .map(project_plugin_mutation),
        PluginCommand::Themes => state
            .extension_control
            .plugin_themes_scoped(state, runtime)
            .await
            .map(|snapshot| PluginCommandReceipt::Themes {
                active: snapshot.active.map(bounded_text),
                themes: bounded_items(
                    snapshot.themes.into_iter().map(project_plugin_theme),
                    MAX_EXTENSION_ITEMS,
                ),
            }),
        PluginCommand::Theme { name } => state
            .extension_control
            .activate_theme_scoped(state, runtime, name.as_deref())
            .await
            .map(|receipt| PluginCommandReceipt::Theme {
                active: receipt.active.map(bounded_text),
                theme: receipt.value.map(project_plugin_theme),
            }),
        PluginCommand::Styles => state
            .extension_control
            .plugin_output_styles_scoped(state, runtime)
            .await
            .map(|snapshot| PluginCommandReceipt::Styles {
                active: snapshot.active.map(bounded_text),
                styles: bounded_items(
                    snapshot.styles.into_iter().map(project_plugin_style),
                    MAX_EXTENSION_ITEMS,
                ),
            }),
        PluginCommand::Style { name } => state
            .extension_control
            .activate_output_style_scoped(state, runtime, name.as_deref())
            .await
            .map(|receipt| PluginCommandReceipt::Style {
                active: receipt.active.map(bounded_text),
            }),
        PluginCommand::Configure { name, values } => state
            .extension_control
            .configure_plugin_scoped(state, runtime, &name, values)
            .await
            .map(project_plugin_mutation),
        PluginCommand::Scaffold { directory, name } => state
            .extension_control
            .scaffold_plugin(state, directory, name)
            .await
            .map(|result| PluginCommandReceipt::Scaffolded {
                scaffold: PluginScaffoldProjection {
                    path: bounded_text(result.path.to_string_lossy().to_string()),
                    name: bounded_text(result.name),
                },
            }),
        PluginCommand::Validate { directory } => state
            .extension_control
            .validate_plugin(state, directory)
            .await
            .map(|report| PluginCommandReceipt::Validated {
                validation: PluginValidationProjection {
                    valid: report.valid,
                    name: report.name.map(bounded_text),
                    components: bounded_items(
                        report.components.into_iter().map(bounded_text),
                        MAX_EXTENSION_ITEMS,
                    ),
                    errors: bounded_items(
                        report.errors.into_iter().map(bounded_text),
                        MAX_EXTENSION_ERRORS,
                    ),
                },
            }),
    };
    match result {
        Ok(receipt) => {
            let status = match &receipt {
                PluginCommandReceipt::Mutation { projection }
                    if !projection.summary.errors.items.is_empty() =>
                {
                    ExtensionCommandStatus::Degraded
                }
                _ => ExtensionCommandStatus::Settled,
            };
            ExtensionCommandReceipt::Plugins {
                meta: success_meta(identity, scope, status),
                receipt: Some(receipt),
            }
        }
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Plugins,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

async fn dispatch_mcp(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    command: McpCommand,
) -> ExtensionCommandReceipt {
    let result = match command {
        McpCommand::List => state
            .extension_control
            .list_mcp_servers_scoped(state, runtime)
            .await
            .map(|servers| McpCommandReceipt::Listed {
                servers: bounded_items(
                    servers.into_iter().map(project_mcp_server),
                    MAX_EXTENSION_ITEMS,
                ),
            }),
        McpCommand::Connect { name } => state
            .extension_control
            .connect_mcp_server(state, &name)
            .await
            .map(|generation| McpCommandReceipt::Reconciled {
                name,
                enabled: true,
                generation: generation.to_string(),
            }),
        McpCommand::Disconnect { name } => state
            .extension_control
            .disconnect_mcp_server(state, &name)
            .await
            .map(|generation| McpCommandReceipt::Reconciled {
                name,
                enabled: false,
                generation: generation.to_string(),
            }),
        McpCommand::Upsert { name, server } => state
            .extension_control
            .upsert_mcp_server(state, name.clone(), server.into())
            .await
            .map_err(anyhow::Error::new)
            .map(|generation| McpCommandReceipt::Configured {
                name: Some(name),
                generation: generation.to_string(),
            }),
        McpCommand::Remove { name } => state
            .extension_control
            .remove_mcp_server(state, &name)
            .await
            .map_err(anyhow::Error::new)
            .map(|generation| McpCommandReceipt::Configured {
                name: Some(name),
                generation: generation.to_string(),
            }),
        McpCommand::SetEnabled { name, enabled } => {
            let result = if enabled {
                state
                    .extension_control
                    .connect_mcp_server(state, &name)
                    .await
            } else {
                state
                    .extension_control
                    .disconnect_mcp_server(state, &name)
                    .await
            };
            result.map(|generation| McpCommandReceipt::Reconciled {
                name,
                enabled,
                generation: generation.to_string(),
            })
        }
        McpCommand::Import { config } => state
            .extension_control
            .replace_mcp_config(state, config.into())
            .await
            .map_err(anyhow::Error::new)
            .map(|generation| McpCommandReceipt::Configured {
                name: None,
                generation: generation.to_string(),
            }),
    };
    match result {
        Ok(receipt) => {
            let status = match &receipt {
                McpCommandReceipt::Listed { .. } => ExtensionCommandStatus::Settled,
                McpCommandReceipt::Reconciled { .. } | McpCommandReceipt::Configured { .. } => {
                    ExtensionCommandStatus::Committed
                }
            };
            ExtensionCommandReceipt::Mcp {
                meta: success_meta(identity, scope, status),
                receipt: Some(receipt),
            }
        }
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Mcp,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

async fn dispatch_hook(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    command: HookCommand,
) -> ExtensionCommandReceipt {
    let result = match command {
        HookCommand::List => state
            .extension_control
            .list_hooks_scoped(state, runtime)
            .await
            .map(project_hook_sources),
        HookCommand::Reload => state
            .extension_control
            .reload_hooks_scoped(state, runtime)
            .await
            .map(project_hook_reload),
        HookCommand::Test { event, matcher } => {
            let hook_event = echo_agent::skills::hooks::HookEvent::from_name(&event)
                .ok_or_else(|| anyhow::anyhow!("Unknown hook event: {event}"));
            match (runtime, hook_event) {
                (Some(runtime), Ok(hook_event)) => {
                    let control = state.extension_control_for_runtime(runtime).await;
                    match control {
                        Ok(control) => {
                            let context = echo_agent::skills::hooks::HookContext::for_dry_run(
                                hook_event, &matcher,
                            );
                            let dry_run = control
                                .runtime()
                                .primary_agent()
                                .read_async(|agent| {
                                    Box::pin(async move {
                                        agent.hook_registry().read().await.dry_run(&context)
                                    })
                                })
                                .await;
                            Ok(HookCommandReceipt::Tested {
                                event,
                                matcher,
                                matches: bounded_items(
                                    dry_run.matches.into_iter().map(|item| {
                                        HookTestMatchProjection {
                                            source: bounded_text(item.source),
                                            matcher: bounded_text(item.matcher),
                                            action: bounded_text(item.action),
                                        }
                                    }),
                                    MAX_EXTENSION_ITEMS,
                                ),
                            })
                        }
                        Err(error) => Err(anyhow::Error::new(error)),
                    }
                }
                (None, _) => Err(anyhow::anyhow!("Hook test requires a scoped runtime")),
                (_, Err(error)) => Err(error),
            }
        }
    };
    match result {
        Ok(receipt) => ExtensionCommandReceipt::Hooks {
            meta: success_meta(identity, scope, ExtensionCommandStatus::Settled),
            receipt: Some(receipt),
        },
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Hooks,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

async fn dispatch_lsp(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    command: LspCommand,
) -> ExtensionCommandReceipt {
    let (action, language) = command.specialist_args();
    match state
        .extension_control
        .lsp_command_scoped(state, runtime, action, language)
        .await
    {
        Ok(message) => ExtensionCommandReceipt::Lsp {
            meta: success_meta(identity, scope, ExtensionCommandStatus::Settled),
            receipt: Some(ExtensionMessageReceipt {
                action: action.to_string(),
                message: bounded_text(message),
            }),
        },
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Lsp,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

async fn dispatch_browser(
    state: &Arc<AppState>,
    runtime: Option<&ScopedChatRuntime>,
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    conversation_id: &str,
    command: BrowserCommand,
) -> ExtensionCommandReceipt {
    let action = command.action_name();
    let result = match command {
        BrowserCommand::Status => state
            .extension_control
            .browser_status_scoped(state, runtime)
            .await
            .map(|status| {
                format!(
                    "Browser extension: {}; token: {}",
                    if status.connected {
                        "connected"
                    } else {
                        "disconnected"
                    },
                    if status.token_configured {
                        "configured"
                    } else {
                        "missing"
                    },
                )
            }),
        BrowserCommand::Stop => state
            .extension_control
            .browser_stop_scoped(state, runtime)
            .await
            .map(|()| "Browser stop completed.".to_string()),
        command => {
            let Some((browser_action, parameters)) = browser_command_action(command) else {
                return ExtensionCommandReceipt::failed_scoped(
                    ExtensionKind::Browser,
                    identity,
                    scope,
                    "Browser command has no specialist action",
                );
            };
            state
                .extension_control
                .execute_browser_action_scoped(
                    state,
                    runtime,
                    conversation_id,
                    browser_action,
                    parameters,
                )
                .await
                .map(|()| format!("Browser {action} completed."))
        }
    };
    match result {
        Ok(message) => ExtensionCommandReceipt::Browser {
            meta: success_meta(identity, scope, ExtensionCommandStatus::Settled),
            receipt: Some(ExtensionMessageReceipt {
                action: action.to_string(),
                message: bounded_text(message),
            }),
        },
        Err(error) => ExtensionCommandReceipt::failed_scoped(
            ExtensionKind::Browser,
            identity,
            scope,
            error.to_string(),
        ),
    }
}

fn browser_command_action(
    command: BrowserCommand,
) -> Option<(
    crate::browser::BrowserAction,
    echo_agent::prelude::ToolParameters,
)> {
    match command {
        BrowserCommand::Managed => Some((
            crate::browser::BrowserAction::Backend,
            HashMap::from([(
                "backend".to_string(),
                serde_json::Value::String("managed".to_string()),
            )]),
        )),
        BrowserCommand::Chrome => Some((
            crate::browser::BrowserAction::Backend,
            HashMap::from([(
                "backend".to_string(),
                serde_json::Value::String("chrome".to_string()),
            )]),
        )),
        BrowserCommand::Navigate { url } => Some((
            crate::browser::BrowserAction::Navigate,
            HashMap::from([("url".to_string(), serde_json::Value::String(url))]),
        )),
        BrowserCommand::Snapshot { filename } => {
            let mut parameters = HashMap::new();
            insert_optional_string(&mut parameters, "filename", filename);
            Some((crate::browser::BrowserAction::Snapshot, parameters))
        }
        BrowserCommand::ClickTarget {
            target,
            element,
            button,
            double_click,
            effect,
        } => {
            let mut parameters = HashMap::from([
                ("target".to_string(), serde_json::Value::String(target)),
                (
                    "doubleClick".to_string(),
                    serde_json::Value::Bool(double_click),
                ),
                ("effect".to_string(), serde_json::Value::String(effect)),
            ]);
            insert_optional_string(&mut parameters, "element", element);
            insert_optional_string(&mut parameters, "button", button);
            Some((crate::browser::BrowserAction::Click, parameters))
        }
        BrowserCommand::Fill {
            target,
            text,
            element,
            submit,
            slowly,
            effect,
        } => {
            let mut parameters = HashMap::from([
                ("target".to_string(), serde_json::Value::String(target)),
                ("text".to_string(), serde_json::Value::String(text)),
                ("submit".to_string(), serde_json::Value::Bool(submit)),
                ("slowly".to_string(), serde_json::Value::Bool(slowly)),
                ("effect".to_string(), serde_json::Value::String(effect)),
            ]);
            insert_optional_string(&mut parameters, "element", element);
            Some((crate::browser::BrowserAction::Fill, parameters))
        }
        BrowserCommand::Back => Some((crate::browser::BrowserAction::Back, HashMap::new())),
        BrowserCommand::Reload => Some((crate::browser::BrowserAction::Reload, HashMap::new())),
        BrowserCommand::Screenshot => {
            Some((crate::browser::BrowserAction::Screenshot, HashMap::new()))
        }
        BrowserCommand::Click { x, y } => Some((
            crate::browser::BrowserAction::ClickAt,
            HashMap::from([
                ("x".to_string(), serde_json::json!(x)),
                ("y".to_string(), serde_json::json!(y)),
                (
                    "effect".to_string(),
                    serde_json::Value::String("none".to_string()),
                ),
            ]),
        )),
        BrowserCommand::TypeAt {
            x,
            y,
            text,
            submit,
            slowly,
            effect,
        } => Some((
            crate::browser::BrowserAction::TypeAt,
            HashMap::from([
                ("x".to_string(), serde_json::json!(x)),
                ("y".to_string(), serde_json::json!(y)),
                ("text".to_string(), serde_json::Value::String(text)),
                ("submit".to_string(), serde_json::Value::Bool(submit)),
                ("slowly".to_string(), serde_json::Value::Bool(slowly)),
                ("effect".to_string(), serde_json::Value::String(effect)),
            ]),
        )),
        BrowserCommand::Scroll { delta_x, delta_y } => Some((
            crate::browser::BrowserAction::Scroll,
            HashMap::from([
                ("deltaX".to_string(), serde_json::json!(delta_x)),
                ("deltaY".to_string(), serde_json::json!(delta_y)),
            ]),
        )),
        BrowserCommand::Tabs {
            tab_action,
            index,
            url,
        } => {
            let mut parameters =
                HashMap::from([("action".to_string(), serde_json::Value::String(tab_action))]);
            if let Some(index) = index {
                parameters.insert("index".to_string(), serde_json::Value::Number(index.into()));
            }
            if let Some(url) = url {
                parameters.insert("url".to_string(), serde_json::Value::String(url));
            }
            Some((crate::browser::BrowserAction::Tabs, parameters))
        }
        BrowserCommand::Console { level, contains } => {
            let mut parameters = HashMap::new();
            insert_optional_string(&mut parameters, "level", level);
            insert_optional_string(&mut parameters, "contains", contains);
            Some((crate::browser::BrowserAction::Console, parameters))
        }
        BrowserCommand::Network {
            method,
            status,
            contains,
        } => {
            let mut parameters = HashMap::new();
            insert_optional_string(&mut parameters, "method", method);
            insert_optional_string(&mut parameters, "contains", contains);
            if let Some(status) = status {
                parameters.insert("status".to_string(), serde_json::json!(status));
            }
            Some((crate::browser::BrowserAction::Network, parameters))
        }
        BrowserCommand::DomInspect {
            target,
            text,
            max_depth,
        } => {
            let mut parameters = HashMap::new();
            insert_optional_string(&mut parameters, "target", target);
            insert_optional_string(&mut parameters, "text", text);
            if let Some(max_depth) = max_depth {
                parameters.insert("maxDepth".to_string(), serde_json::json!(max_depth));
            }
            Some((crate::browser::BrowserAction::DomInspect, parameters))
        }
        BrowserCommand::PerformanceTrace { trace_action, path } => {
            let mut parameters = HashMap::from([(
                "action".to_string(),
                serde_json::Value::String(trace_action),
            )]);
            insert_optional_string(&mut parameters, "path", path);
            Some((crate::browser::BrowserAction::PerformanceTrace, parameters))
        }
        BrowserCommand::DeveloperMode { enabled } => Some((
            crate::browser::BrowserAction::DeveloperMode,
            HashMap::from([("enabled".to_string(), serde_json::Value::Bool(enabled))]),
        )),
        BrowserCommand::Status | BrowserCommand::Stop => None,
    }
}

fn insert_optional_string(
    parameters: &mut echo_agent::prelude::ToolParameters,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        parameters.insert(key.to_string(), serde_json::Value::String(value));
    }
}

fn success_meta(
    identity: ExtensionCommandIdentity,
    scope: ExtensionRequestScope,
    status: ExtensionCommandStatus,
) -> ExtensionReceiptMeta {
    ExtensionReceiptMeta {
        request_id: identity.request_id,
        operation_id: identity.operation_id,
        authority_scope: scope.workspace_id,
        workspace_generation: scope.workspace_generation,
        sender_id: scope.sender_id,
        sender_incarnation: scope.sender_incarnation,
        status,
        error: None,
    }
}

fn project_skill_update(update: crate::skills_hub::SkillUpdateStatus) -> SkillUpdateProjection {
    let state = match update.state {
        crate::skills_hub::SkillUpdateState::UpToDate => "up_to_date",
        crate::skills_hub::SkillUpdateState::UpdateAvailable => "update_available",
        crate::skills_hub::SkillUpdateState::LocalChanges => "local_changes",
        crate::skills_hub::SkillUpdateState::Untracked => "untracked",
        crate::skills_hub::SkillUpdateState::Error => "error",
    };
    SkillUpdateProjection {
        name: bounded_text(update.name),
        state: state.to_string(),
        current_revision: update.current_revision.map(bounded_text),
        remote_revision: update.remote_revision.map(bounded_text),
        message: bounded_text(update.message),
    }
}

fn project_plugin_entry(entry: echo_agent::plugin::PluginEntry) -> PluginEntryProjection {
    let version = bounded_text(entry.manifest.version_label().to_string());
    let display_name = bounded_text(entry.manifest.display_name().to_string());
    let capabilities = crate::plugin_runtime::plugin_capabilities(&entry)
        .into_iter()
        .map(|capability| bounded_text(capability.display_name().to_string()))
        .collect();
    let author = entry
        .manifest
        .author
        .as_ref()
        .map(|author| PluginAuthorProjection {
            name: author.name.clone().map(bounded_text),
            email: author.email.clone().map(bounded_text),
            url: author.url.clone().map(bounded_text),
        });
    let dependencies = entry
        .manifest
        .dependencies
        .iter()
        .map(|dependency| PluginDependencyProjection {
            name: bounded_text(dependency.name().to_string()),
            version: dependency
                .version_constraint()
                .map(|value| bounded_text(value.to_string())),
        })
        .collect();
    let config = entry
        .manifest
        .config
        .iter()
        .map(|(name, config)| {
            let value_type = match config.value_type {
                echo_agent::plugin::PluginUserConfigType::String => "string",
                echo_agent::plugin::PluginUserConfigType::Number => "number",
                echo_agent::plugin::PluginUserConfigType::Boolean => "boolean",
                echo_agent::plugin::PluginUserConfigType::Directory => "directory",
                echo_agent::plugin::PluginUserConfigType::File => "file",
            };
            (
                bounded_text(name.clone()),
                PluginConfigEntryProjection {
                    value_type: value_type.to_string(),
                    title: bounded_text(config.title.clone()),
                    description: bounded_text(config.description.clone()),
                    sensitive: config.sensitive,
                    required: config.required,
                    default: config.default.clone(),
                    multiple: config.multiple,
                    min: config.min,
                    max: config.max,
                },
            )
        })
        .collect();
    PluginEntryProjection {
        name: bounded_text(entry.manifest.name),
        display_name,
        version,
        description: bounded_text(entry.manifest.description),
        author,
        homepage: entry.manifest.homepage.map(bounded_text),
        repository: entry.manifest.repository.map(bounded_text),
        license: entry.manifest.license.map(bounded_text),
        scope: entry.scope.to_string(),
        enabled: entry.enabled,
        root: bounded_text(entry.root.display().to_string()),
        capabilities,
        keywords: entry
            .manifest
            .keywords
            .into_iter()
            .map(bounded_text)
            .collect(),
        dependencies,
        config,
        config_values: entry.user_config,
    }
}

fn project_plugin_mutation(receipt: PluginMutationReceipt) -> PluginCommandReceipt {
    PluginCommandReceipt::Mutation {
        projection: Box::new(PluginMutationProjection {
            plugin_id: receipt.plugin_id.map(bounded_text),
            plugin: receipt.entry.map(project_plugin_entry),
            summary: PluginReloadProjection {
                total: receipt.summary.total,
                enabled: receipt.summary.enabled,
                skills_loaded: receipt.summary.skills_loaded,
                hooks_registered: receipt.summary.hooks_registered,
                mcp_connected: receipt.summary.mcp_connected,
                agents_loaded: receipt.summary.agents_loaded,
                lsp_languages_loaded: receipt.summary.lsp_languages_loaded,
                monitors_loaded: receipt.summary.monitors_loaded,
                themes_loaded: receipt.summary.themes_loaded,
                output_styles_loaded: receipt.summary.output_styles_loaded,
                errors: bounded_items(
                    receipt.summary.errors.into_iter().map(bounded_text),
                    MAX_EXTENSION_ERRORS,
                ),
            },
            active_theme: receipt.theme.active.map(bounded_text),
            themes: bounded_items(
                receipt.theme.themes.into_iter().map(project_plugin_theme),
                MAX_EXTENSION_ITEMS,
            ),
            active_output_style: receipt.output_style.active.map(bounded_text),
            output_styles: bounded_items(
                receipt
                    .output_style
                    .styles
                    .into_iter()
                    .map(project_plugin_style),
                MAX_EXTENSION_ITEMS,
            ),
        }),
    }
}

fn project_plugin_theme(
    theme: crate::plugin_runtime::PluginThemeDefinition,
) -> PluginThemeProjection {
    PluginThemeProjection {
        name: bounded_text(theme.name),
        display_name: theme.display_name.map(bounded_text),
        dark: theme.dark,
        colors: theme.colors,
        plugin: bounded_text(theme.plugin),
    }
}

fn project_plugin_style(
    style: crate::plugin_runtime::PluginOutputStyle,
) -> PluginOutputStyleProjection {
    PluginOutputStyleProjection {
        name: bounded_text(style.name),
        description: bounded_text(style.description),
        instructions: bounded_text(style.instructions),
        plugin: bounded_text(style.plugin),
    }
}

fn project_mcp_server(server: ExtensionMcpServer) -> McpServerProjection {
    McpServerProjection {
        name: bounded_text(server.name),
        status: bounded_text(server.status),
        transport: bounded_text(server.transport),
        tool_count: server.tool_count,
        tools: bounded_items(
            server.tools.into_iter().map(|tool| McpToolProjection {
                name: bounded_text(tool.name),
                description: bounded_text(tool.description),
            }),
            MAX_EXTENSION_TOOLS,
        ),
        connected_at: server.connected_at.map(bounded_text),
        error: server.error.map(bounded_text),
        enabled: server.enabled,
    }
}

fn project_hook_sources(sources: Vec<HookSourceSnapshot>) -> HookCommandReceipt {
    HookCommandReceipt::Listed {
        sources: bounded_items(
            sources.into_iter().map(|source| HookSourceProjection {
                source: bounded_text(source.source),
                rules: source.rules,
            }),
            MAX_EXTENSION_ITEMS,
        ),
    }
}

fn project_hook_reload(receipt: HookReloadReceipt) -> HookCommandReceipt {
    HookCommandReceipt::Reloaded {
        loaded_from: bounded_items(
            receipt
                .loaded_from
                .into_iter()
                .map(|path| bounded_text(path.display().to_string())),
            MAX_EXTENSION_ITEMS,
        ),
        rule_count: receipt.rule_count,
    }
}

fn bounded_items<T>(items: impl IntoIterator<Item = T>, limit: usize) -> BoundedItems<T> {
    let mut items = items.into_iter();
    let retained = items.by_ref().take(limit).collect::<Vec<_>>();
    let omitted = items.count();
    BoundedItems {
        items: retained,
        omitted,
    }
}

fn bounded_text(value: String) -> String {
    value.chars().take(MAX_EXTENSION_TEXT_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ExtensionCommandIdentity {
        ExtensionCommandIdentity {
            request_id: "request-1".to_string(),
            operation_id: "operation-1".to_string(),
        }
    }

    fn skill_sync_receipt(status: SkillSettlementStatus) -> SkillSyncReceipt {
        SkillSyncReceipt {
            operation_id: "operation-skill-status".to_string(),
            committed_file_path: std::path::PathBuf::from("enabled-skills.json"),
            content_identity: "content-skill-status".to_string(),
            desired_generation: 2,
            settled_generation: 1,
            durable_committed: true,
            idempotent: false,
            status,
            target_receipts: Vec::new(),
            repair_debt: None,
        }
    }

    #[test]
    fn skill_receipt_status_preserves_committed_settled_and_degraded() {
        let committed = SkillCommandReceipt::Enabled {
            settlement: skill_sync_receipt(SkillSettlementStatus::Committed),
        };
        let settled = SkillCommandReceipt::Enabled {
            settlement: skill_sync_receipt(SkillSettlementStatus::Settled),
        };
        let degraded = SkillCommandReceipt::Enabled {
            settlement: skill_sync_receipt(SkillSettlementStatus::Degraded),
        };

        assert_eq!(
            skill_receipt_status(&committed),
            ExtensionCommandStatus::Committed
        );
        assert_eq!(
            skill_receipt_status(&settled),
            ExtensionCommandStatus::Settled
        );
        assert_eq!(
            skill_receipt_status(&degraded),
            ExtensionCommandStatus::Degraded
        );
    }

    #[test]
    fn parses_every_extension_family_without_claiming_other_prompts() -> Result<(), String> {
        let cases = [
            ("/skills enable review", ExtensionKind::Skills),
            ("/plugins reload", ExtensionKind::Plugins),
            ("/mcp connect github", ExtensionKind::Mcp),
            ("/hooks reload", ExtensionKind::Hooks),
            ("/lsp restart rust", ExtensionKind::Lsp),
            (
                "/browser navigate https://example.com",
                ExtensionKind::Browser,
            ),
        ];
        for (input, expected) in cases {
            let parsed = parse_extension_command(input, identity())
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{input} was not recognized"))?;
            assert_eq!(parsed.kind(), expected);
        }
        assert!(
            parse_extension_command("explain /skills", identity())
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(
            parse_extension_command("/help", identity())
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn request_roundtrip_preserves_tag_and_identity() -> Result<(), String> {
        let request = parse_extension_command(
            "/plugins config formatter {\"line_width\":100,\"enabled\":true}",
            identity(),
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "plugin command was not recognized".to_string())?;
        let encoded = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        let decoded: ExtensionCommandRequest =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        assert_eq!(decoded.request_id, "request-1");
        assert_eq!(decoded.operation_id, "operation-1");
        assert_eq!(decoded.kind(), ExtensionKind::Plugins);
        let value: serde_json::Value =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("extension").and_then(serde_json::Value::as_str),
            Some("plugins")
        );
        Ok(())
    }

    #[test]
    fn parses_complete_browser_control_surface() -> Result<(), String> {
        let commands = [
            "/browser status",
            "/browser managed",
            "/browser chrome",
            "/browser navigate https://example.com",
            "/browser back",
            "/browser reload",
            "/browser screenshot",
            "/browser click 12.5 24",
            "/browser scroll 0 -320",
            "/browser tabs list",
            "/browser tabs select 2",
            "/browser tabs new https://example.com/new",
            "/browser tabs close 2",
            "/browser stop",
        ];
        for input in commands {
            let parsed = parse_extension_command(input, identity())
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{input} was not recognized"))?;
            assert!(matches!(parsed.command, ExtensionCommand::Browser(_)));
        }
        let tabs = parse_extension_command("/browser tabs select 7", identity())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "browser tabs command was not recognized".to_string())?;
        assert!(matches!(
            tabs.command,
            ExtensionCommand::Browser(BrowserCommand::Tabs {
                tab_action,
                index: Some(7),
                url: None,
            }) if tab_action == "select"
        ));
        Ok(())
    }

    #[test]
    fn invalid_supported_command_is_not_forwarded_as_model_input() {
        let error = parse_extension_command("/mcp connect", identity())
            .err()
            .unwrap_or_else(|| ExtensionCommandParseError::new(None, "missing error"));
        assert_eq!(error.extension, Some(ExtensionKind::Mcp));
        assert!(error.message.contains("/mcp connect"));
    }

    #[test]
    fn failed_receipt_roundtrip_is_typed_and_bounded() -> Result<(), String> {
        let receipt = ExtensionCommandReceipt::failed(
            ExtensionKind::Browser,
            identity(),
            "workspace-a",
            "failure".repeat(MAX_EXTENSION_TEXT_CHARS),
        );
        let encoded = serde_json::to_string(&receipt).map_err(|error| error.to_string())?;
        let decoded: ExtensionCommandReceipt =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        assert_eq!(decoded.status(), ExtensionCommandStatus::Failed);
        assert_eq!(decoded.meta().request_id, "request-1");
        assert_eq!(decoded.meta().authority_scope, "workspace-a");
        assert_eq!(
            decoded
                .meta()
                .error
                .as_deref()
                .map(str::chars)
                .map(Iterator::count),
            Some(MAX_EXTENSION_TEXT_CHARS)
        );
        let display = decoded.display_message();
        assert!(display.starts_with("[FAILED] Extension scope=workspace-a"));
        assert!(display.contains("request_id=request-1"));
        assert!(display.contains("operation_id=operation-1"));
        Ok(())
    }

    #[test]
    fn skill_debt_display_and_wire_contract_are_structured() -> Result<(), String> {
        let receipt = SkillSyncReceipt {
            operation_id: "operation-skill-1".to_string(),
            committed_file_path: std::path::PathBuf::from("/tmp/enabled-skills.json"),
            content_identity: "content-2".to_string(),
            desired_generation: 2,
            settled_generation: 1,
            durable_committed: true,
            idempotent: false,
            status: SkillSettlementStatus::Degraded,
            target_receipts: vec![crate::extension_control::SkillTargetSettlementReceipt {
                target: "workspace-a".to_string(),
                workspace_generation: "workspace-generation-a".to_string(),
                specialist_generation: 1,
                status: crate::extension_control::SkillTargetSettlementStatus::Degraded,
                changed_entries: Vec::new(),
                error: Some("fanout failed".to_string()),
            }],
            repair_debt: Some(crate::skills_hub::enabled_skills::SkillRepairDebt {
                generation: 2,
                content_identity: "content-2".to_string(),
                attempts: 1,
                target_failures: vec![crate::skills_hub::enabled_skills::SkillRepairTargetDebt {
                    target: "workspace-a".to_string(),
                    component: "runtime_fanout".to_string(),
                    expected_generation: 2,
                    observed_generation: Some(1),
                    reason: "fanout failed".to_string(),
                    retryable: true,
                }],
                artifact_removals: Vec::new(),
                artifact_syncs: Vec::new(),
                artifact_enablements: Vec::new(),
            }),
        };
        let mut lines = Vec::new();
        render_skill_settlement(&receipt, &mut lines);
        let display = lines.join("\n");
        assert!(display.contains("committed_file_path=/tmp/enabled-skills.json"));
        assert!(display.contains("component=runtime_fanout"));
        assert!(display.contains("expected_generation=2 observed_generation=1"));
        let value = serde_json::to_value(&receipt).map_err(|error| error.to_string())?;
        assert_eq!(
            value
                .pointer("/repair_debt/target_failures/0/expected_generation")
                .and_then(serde_json::Value::as_str),
            Some("2")
        );
        assert_eq!(
            value
                .pointer("/repair_debt/target_failures/0/observed_generation")
                .and_then(serde_json::Value::as_str),
            Some("1")
        );
        Ok(())
    }

    #[test]
    fn bounded_projection_reports_omitted_items() {
        let projection = bounded_items(0..5, 2);
        assert_eq!(projection.items, vec![0, 1]);
        assert_eq!(projection.omitted, 3);
    }
}
