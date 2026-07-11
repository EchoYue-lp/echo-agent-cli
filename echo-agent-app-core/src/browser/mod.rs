pub mod config;
pub mod error;
pub mod event;
pub mod session;
pub mod sidecar;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use echo_agent::mcp::{McpClient, McpContent, McpToolCallResult};
use echo_agent::prelude::{Tool, ToolParameters, ToolResult, ToolRiskLevel};
use echo_core::tools::{ToolContext, ToolResultKind};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub use config::BrowserConfig;
pub use error::{BrowserError, BrowserResult};
pub use event::{BrowserEvent, BrowserFrame};
pub use session::{
    BrowserObservation, BrowserSession, BrowserSessionManager, BrowserSessionStatus, BrowserTab,
    MAIN_TAB_OWNER,
};
use sidecar::BrowserSidecar;

#[derive(Clone)]
pub struct BrowserRuntime {
    inner: Arc<BrowserRuntimeInner>,
}

struct BrowserRuntimeInner {
    config: BrowserConfig,
    sessions: BrowserSessionManager,
    client: RwLock<Option<Arc<McpClient>>>,
    connect_lock: Mutex<()>,
    shutdown: CancellationToken,
}

impl BrowserRuntime {
    pub async fn start(config: BrowserConfig) -> Arc<Self> {
        let sessions = BrowserSessionManager::new(config.session_dir.clone(), 12_000);
        sessions.restore_metadata().await;
        let runtime = Arc::new(Self {
            inner: Arc::new(BrowserRuntimeInner {
                config,
                sessions,
                client: RwLock::new(None),
                connect_lock: Mutex::new(()),
                shutdown: CancellationToken::new(),
            }),
        });
        if !runtime.inner.config.enabled {
            tracing::info!("managed Playwright MCP runtime disabled by configuration");
            return runtime;
        }
        let prewarm = runtime.clone();
        tokio::spawn(async move {
            match prewarm.ensure_client().await {
                Ok(client) => tracing::info!(
                    tools = client.tools().len(),
                    "managed Playwright MCP runtime ready"
                ),
                Err(error) => tracing::warn!(
                    error = %error,
                    "managed Playwright MCP unavailable at startup; browser tools will retry lazily"
                ),
            }
        });
        runtime
    }

    pub fn install_tools(&self, agent: &mut echo_agent::agent::ReactAgent) {
        self.install_tools_for(agent, BrowserActor::Main);
    }

    pub fn install_worker_tools(&self, agent: &mut echo_agent::agent::ReactAgent) {
        self.install_tools_for(agent, BrowserActor::Worker);
    }

    fn install_tools_for(&self, agent: &mut echo_agent::agent::ReactAgent, actor: BrowserActor) {
        if self.inner.config.enabled {
            agent.add_tools(self.tools_for(actor));
        }
    }

    pub fn tools(&self) -> Vec<Box<dyn Tool>> {
        self.tools_for(BrowserActor::Main)
    }

    fn tools_for(&self, actor: BrowserActor) -> Vec<Box<dyn Tool>> {
        BrowserAction::ALL
            .iter()
            .copied()
            .map(|action| {
                Box::new(ManagedBrowserTool {
                    runtime: self.clone(),
                    action,
                    actor,
                }) as Box<dyn Tool>
            })
            .collect()
    }

    pub fn session_manager(&self) -> BrowserSessionManager {
        self.inner.sessions.clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BrowserEvent> {
        self.inner.sessions.subscribe()
    }

    pub async fn execute_main(
        &self,
        conversation_id: String,
        action: BrowserAction,
        params: ToolParameters,
        cancel: Option<Arc<CancellationToken>>,
    ) -> BrowserResult<ToolResult> {
        let context = ToolContext {
            conversation_id: Some(conversation_id),
            cancel,
            ..ToolContext::default()
        };
        self.call(action, params, &context, BrowserActor::Main)
            .await
    }

    pub async fn interrupt(&self) {
        let client = self.inner.client.write().await.take();
        if let Some(client) = client {
            client.close().await;
        }
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.sessions.close_all().await;
        let client = self.inner.client.write().await.take();
        if let Some(client) = client {
            client.close().await;
            tracing::info!("managed Playwright MCP runtime stopped");
        }
    }

    async fn ensure_client(&self) -> BrowserResult<Arc<McpClient>> {
        if self.inner.shutdown.is_cancelled() {
            return Err(BrowserError::Connection(
                "browser runtime is shutting down".to_string(),
            ));
        }
        if !self.inner.config.enabled {
            return Err(BrowserError::Disabled);
        }
        if let Some(client) = self.inner.client.read().await.clone() {
            return Ok(client);
        }

        let _guard = self.inner.connect_lock.lock().await;
        if let Some(client) = self.inner.client.read().await.clone() {
            return Ok(client);
        }

        BrowserSidecar::prepare(&self.inner.config).await?;
        let timeout = Duration::from_secs(self.inner.config.startup_timeout_secs);
        let client = tokio::time::timeout(
            timeout,
            McpClient::new(BrowserSidecar::server_config(&self.inner.config)),
        )
        .await
        .map_err(|_| BrowserError::Connection(format!("startup timed out after {timeout:?}")))?
        .map_err(|error| BrowserError::Connection(error.to_string()))?;
        if self.inner.shutdown.is_cancelled() {
            client.close().await;
            return Err(BrowserError::Connection(
                "browser runtime shut down during startup".to_string(),
            ));
        }
        *self.inner.client.write().await = Some(client.clone());
        Ok(client)
    }

    async fn invalidate_client(&self, failed: &Arc<McpClient>) {
        let mut current = self.inner.client.write().await;
        if current
            .as_ref()
            .is_some_and(|client| Arc::ptr_eq(client, failed))
        {
            *current = None;
        }
    }

    async fn call(
        &self,
        action: BrowserAction,
        params: ToolParameters,
        context: &ToolContext,
        actor: BrowserActor,
    ) -> BrowserResult<ToolResult> {
        let conversation_id = context
            .conversation_id
            .as_deref()
            .unwrap_or("browser-default");
        let owner_id = match actor {
            BrowserActor::Main => MAIN_TAB_OWNER.to_string(),
            BrowserActor::Worker => context
                .execution_id
                .clone()
                .or_else(|| context.run_id.clone())
                .or_else(|| context.turn_id.clone())
                .unwrap_or_else(|| format!("worker-{}", uuid::Uuid::new_v4())),
        };
        let tabs_command = if action == BrowserAction::Tabs {
            params
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let requested_index = params
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok());
        let lease = match tabs_command.as_deref() {
            Some("new") => self
                .inner
                .sessions
                .open_tab(conversation_id, &owner_id, context.run_id.as_deref())
                .await
                .ok_or_else(|| {
                    BrowserError::Connection("failed to allocate managed browser tab".to_string())
                })?,
            Some("select") => {
                let index = requested_index.ok_or_else(|| BrowserError::Tool {
                    tool: action.name().to_string(),
                    message: "select requires a valid non-negative tab index".to_string(),
                })?;
                self.inner
                    .sessions
                    .select_tab(conversation_id, &owner_id, index)
                    .await
                    .ok_or_else(|| BrowserError::Tool {
                        tool: action.name().to_string(),
                        message: format!("managed tab index {index} does not exist"),
                    })?
            }
            _ => {
                self.inner
                    .sessions
                    .lease_tab(conversation_id, &owner_id, context.run_id.as_deref())
                    .await
            }
        };
        let _operation = self.inner.sessions.lock_operation().await;
        if tabs_command.as_deref() == Some("new") || tabs_command.as_deref() == Some("select") {
            // The explicit browser_tabs call below performs the matching MCP operation.
        } else if lease.opened {
            self.call_mcp(
                "browser_tabs",
                json!({ "action": "new", "url": "about:blank" }),
                context.cancel.as_deref(),
            )
            .await?;
        } else {
            self.call_mcp(
                "browser_tabs",
                json!({ "action": "select", "index": lease.tab_index }),
                context.cancel.as_deref(),
            )
            .await?;
        }

        let (tool, arguments) = action.translate(params)?;
        let run_id = context.run_id.clone();
        let turn_id = context.turn_id.clone();
        let execution_id = context.execution_id.clone();
        let action_name = action.name().to_string();
        if action == BrowserAction::Navigate {
            let url = arguments
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.inner.sessions.emit(BrowserEvent::NavigationStarted {
                session_id: lease.session_id.clone(),
                tab_id: lease.tab_id.clone(),
                url,
            });
            self.inner
                .sessions
                .set_status(conversation_id, BrowserSessionStatus::Navigating)
                .await;
        } else {
            self.inner
                .sessions
                .set_status(conversation_id, BrowserSessionStatus::Acting)
                .await;
        }
        self.inner.sessions.emit(BrowserEvent::ActionStarted {
            session_id: lease.session_id.clone(),
            tab_id: lease.tab_id.clone(),
            action: action_name.clone(),
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
            execution_id: execution_id.clone(),
        });

        match self
            .call_mcp(tool, arguments.clone(), context.cancel.as_deref())
            .await
        {
            Ok(raw_result) => {
                let result = tool_result(raw_result);
                self.inner
                    .sessions
                    .set_status(conversation_id, BrowserSessionStatus::Ready)
                    .await;
                if action == BrowserAction::Navigate {
                    let url = arguments
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.inner
                        .sessions
                        .update_url(conversation_id, &lease.tab_id, url)
                        .await;
                    self.inner.sessions.emit(BrowserEvent::NavigationCompleted {
                        session_id: lease.session_id.clone(),
                        tab_id: lease.tab_id.clone(),
                        url: url.to_string(),
                    });
                }
                let observation = self
                    .inner
                    .sessions
                    .observation(&lease, &action_name, &result);
                match action {
                    BrowserAction::Snapshot => self
                        .inner
                        .sessions
                        .emit(BrowserEvent::Snapshot { observation }),
                    BrowserAction::Screenshot => {
                        let frame = browser_frame(&result);
                        self.inner
                            .sessions
                            .emit(BrowserEvent::Screenshot { observation, frame });
                    }
                    BrowserAction::Navigate
                    | BrowserAction::Click
                    | BrowserAction::Fill
                    | BrowserAction::Back
                    | BrowserAction::Reload => {
                        if let Ok(raw_frame) = self
                            .call_mcp(
                                "browser_take_screenshot",
                                json!({ "type": "png" }),
                                context.cancel.as_deref(),
                            )
                            .await
                        {
                            let frame_result = tool_result(raw_frame);
                            let frame_observation = self.inner.sessions.observation(
                                &lease,
                                "browser_screenshot",
                                &frame_result,
                            );
                            self.inner.sessions.emit(BrowserEvent::Screenshot {
                                observation: frame_observation,
                                frame: browser_frame(&frame_result),
                            });
                        }
                    }
                    BrowserAction::Tabs => {}
                }
                if tabs_command.as_deref() == Some("close") {
                    let index = requested_index.unwrap_or(lease.tab_index);
                    self.inner.sessions.close_tab(conversation_id, index).await;
                }
                self.inner.sessions.emit(BrowserEvent::ActionCompleted {
                    session_id: lease.session_id,
                    tab_id: lease.tab_id,
                    action: action_name,
                    run_id,
                    turn_id,
                    execution_id,
                });
                Ok(result)
            }
            Err(error) => {
                self.inner
                    .sessions
                    .set_status(conversation_id, BrowserSessionStatus::Failed)
                    .await;
                self.inner.sessions.emit(BrowserEvent::ActionFailed {
                    session_id: lease.session_id,
                    tab_id: lease.tab_id,
                    action: action_name,
                    run_id,
                    turn_id,
                    execution_id,
                    error: error.to_string(),
                });
                Err(error)
            }
        }
    }

    async fn call_mcp(
        &self,
        tool: &'static str,
        arguments: Value,
        cancel: Option<&CancellationToken>,
    ) -> BrowserResult<McpToolCallResult> {
        let first = self.ensure_client().await?;
        let first_call = first.call_tool(tool, arguments.clone());
        let first_result = if let Some(cancel) = cancel {
            tokio::select! {
                result = first_call => result,
                _ = cancel.cancelled() => {
                    self.invalidate_client(&first).await;
                    first.close().await;
                    return Err(BrowserError::Cancelled);
                }
            }
        } else {
            first_call.await
        };
        match first_result {
            Ok(result) => Ok(result),
            Err(first_error) => {
                tracing::warn!(
                    tool,
                    error = %first_error,
                    "Playwright MCP call failed; restarting managed sidecar"
                );
                self.invalidate_client(&first).await;
                first.close().await;
                let restarted = self.ensure_client().await?;
                let retry = restarted.call_tool(tool, arguments);
                let result = if let Some(cancel) = cancel {
                    tokio::select! {
                        result = retry => result,
                        _ = cancel.cancelled() => {
                            self.invalidate_client(&restarted).await;
                            restarted.close().await;
                            return Err(BrowserError::Cancelled);
                        }
                    }
                } else {
                    retry.await
                };
                result.map_err(|error| BrowserError::Tool {
                    tool: tool.to_string(),
                    message: error.to_string(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Navigate,
    Snapshot,
    Click,
    Fill,
    Screenshot,
    Back,
    Reload,
    Tabs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserActor {
    Main,
    Worker,
}

impl BrowserAction {
    const ALL: [Self; 8] = [
        Self::Navigate,
        Self::Snapshot,
        Self::Click,
        Self::Fill,
        Self::Screenshot,
        Self::Back,
        Self::Reload,
        Self::Tabs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Navigate => "browser_navigate",
            Self::Snapshot => "browser_snapshot",
            Self::Click => "browser_click",
            Self::Fill => "browser_fill",
            Self::Screenshot => "browser_screenshot",
            Self::Back => "browser_back",
            Self::Reload => "browser_reload",
            Self::Tabs => "browser_tabs",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Navigate => "Navigate the managed browser to a URL.",
            Self::Snapshot => "Read a structured accessibility snapshot of the current page.",
            Self::Click => "Click an element identified by a Playwright snapshot target.",
            Self::Fill => "Fill or type text into an editable element.",
            Self::Screenshot => "Capture the current page or a selected element.",
            Self::Back => "Navigate the current tab back one history entry.",
            Self::Reload => "Reload the current page.",
            Self::Tabs => "List, create, close, or select managed browser tabs.",
        }
    }

    fn parameters(self) -> Value {
        match self {
            Self::Navigate => object_schema(
                json!({
                    "url": { "type": "string", "description": "URL to open" }
                }),
                &["url"],
            ),
            Self::Snapshot => object_schema(
                json!({
                    "filename": { "type": "string", "description": "Optional relative markdown output file" }
                }),
                &[],
            ),
            Self::Click => object_schema(
                json!({
                    "target": { "type": "string", "description": "Exact target reference from browser_snapshot" },
                    "element": { "type": "string", "description": "Human-readable element description" },
                    "button": { "type": "string", "enum": ["left", "right", "middle"] },
                    "doubleClick": { "type": "boolean" }
                }),
                &["target"],
            ),
            Self::Fill => object_schema(
                json!({
                    "target": { "type": "string", "description": "Exact target reference from browser_snapshot" },
                    "text": { "type": "string", "description": "Text to enter" },
                    "element": { "type": "string", "description": "Human-readable field description" },
                    "submit": { "type": "boolean" },
                    "slowly": { "type": "boolean" }
                }),
                &["target", "text"],
            ),
            Self::Screenshot => object_schema(
                json!({
                    "filename": { "type": "string", "description": "Optional relative output file" },
                    "fullPage": { "type": "boolean" },
                    "target": { "type": "string", "description": "Optional element target" },
                    "element": { "type": "string", "description": "Human-readable element description" },
                    "type": { "type": "string", "enum": ["png", "jpeg"] }
                }),
                &[],
            ),
            Self::Back | Self::Reload => object_schema(json!({}), &[]),
            Self::Tabs => object_schema(
                json!({
                    "action": { "type": "string", "enum": ["list", "new", "close", "select"] },
                    "index": { "type": "integer" },
                    "url": { "type": "string" }
                }),
                &["action"],
            ),
        }
    }

    fn translate(self, params: ToolParameters) -> BrowserResult<(&'static str, Value)> {
        let mut arguments = serde_json::Map::from_iter(params);
        match self {
            Self::Navigate => Ok(("browser_navigate", Value::Object(arguments))),
            Self::Snapshot => Ok(("browser_snapshot", Value::Object(arguments))),
            Self::Click => Ok(("browser_click", Value::Object(arguments))),
            Self::Fill => Ok(("browser_type", Value::Object(arguments))),
            Self::Screenshot => {
                arguments
                    .entry("type".to_string())
                    .or_insert_with(|| Value::String("png".to_string()));
                Ok(("browser_take_screenshot", Value::Object(arguments)))
            }
            Self::Back => Ok(("browser_navigate_back", json!({}))),
            Self::Reload => Ok((
                "browser_evaluate",
                json!({ "function": "() => window.location.reload()" }),
            )),
            Self::Tabs => Ok(("browser_tabs", Value::Object(arguments))),
        }
    }

    fn risk(self) -> ToolRiskLevel {
        match self {
            Self::Snapshot | Self::Screenshot => ToolRiskLevel::ReadOnly,
            _ => ToolRiskLevel::Standard,
        }
    }
}

struct ManagedBrowserTool {
    runtime: BrowserRuntime,
    action: BrowserAction,
    actor: BrowserActor,
}

impl Tool for ManagedBrowserTool {
    fn name(&self) -> &str {
        self.action.name()
    }

    fn description(&self) -> &str {
        self.action.description()
    }

    fn parameters(&self) -> Value {
        self.action.parameters()
    }

    fn risk_level(&self) -> ToolRiskLevel {
        self.action.risk()
    }

    fn execute<'a>(
        &'a self,
        parameters: ToolParameters,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            match self
                .runtime
                .call(self.action, parameters, &ToolContext::default(), self.actor)
                .await
            {
                Ok(result) => Ok(result),
                Err(error) => Ok(ToolResult::error(error.to_string())),
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            match self
                .runtime
                .call(self.action, parameters, context, self.actor)
                .await
            {
                Ok(result) => Ok(result),
                Err(error) => Ok(ToolResult::error(error.to_string())),
            }
        })
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn tool_result(result: McpToolCallResult) -> ToolResult {
    let text = McpClient::content_to_text(&result.content);
    if result.is_error {
        let mut tool_result = ToolResult::error(text);
        if let Some(structured) = result.structured_content {
            tool_result = tool_result.with_data(structured);
            tool_result.kind = ToolResultKind::StructuredError {
                error_code: "playwright_mcp_error".to_string(),
            };
        }
        return tool_result;
    }

    let image = result.content.iter().find_map(|content| match content {
        McpContent::Image { data, mime_type } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()
            .map(|bytes| (bytes, mime_type.clone())),
        _ => None,
    });
    let mut tool_result = if let Some(structured) = result.structured_content {
        let mut structured_result = ToolResult::success_json(structured);
        structured_result.output = text;
        structured_result
    } else {
        ToolResult::success(text)
    };
    if let Some((bytes, mime_type)) = image {
        tool_result.kind = ToolResultKind::Image {
            mime_type: mime_type.clone(),
        };
        tool_result.bytes = Some(bytes);
        tool_result.mime_type = Some(mime_type);
    }
    if !result.extra.is_empty()
        && let Ok(extra) = serde_json::to_string_pretty(&result.extra)
    {
        tool_result.output.push_str("\n\nAdditional fields:\n");
        tool_result.output.push_str(&extra);
    }
    tool_result
}

fn browser_frame(result: &ToolResult) -> Option<BrowserFrame> {
    let bytes = result.bytes.as_ref()?;
    if bytes.len() > 8 * 1024 * 1024 {
        tracing::warn!(
            bytes = bytes.len(),
            "browser screenshot omitted from GUI event"
        );
        return None;
    }
    let mime_type = result.mime_type.as_deref().unwrap_or("image/png");
    Some(BrowserFrame {
        data_url: format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        mime_type: mime_type.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;

    #[test]
    fn exposes_phase_one_tool_contract() {
        let names: Vec<&str> = BrowserAction::ALL
            .iter()
            .map(|action| action.name())
            .collect();
        assert_eq!(
            names,
            vec![
                "browser_navigate",
                "browser_snapshot",
                "browser_click",
                "browser_fill",
                "browser_screenshot",
                "browser_back",
                "browser_reload",
                "browser_tabs",
            ]
        );
    }

    #[test]
    fn aliases_translate_to_playwright_mcp_tools() {
        let (fill, _) = BrowserAction::Fill
            .translate(ToolParameters::new())
            .unwrap_or(("", Value::Null));
        let (screenshot, _) = BrowserAction::Screenshot
            .translate(ToolParameters::new())
            .unwrap_or(("", Value::Null));
        let (back, _) = BrowserAction::Back
            .translate(ToolParameters::new())
            .unwrap_or(("", Value::Null));
        assert_eq!(fill, "browser_type");
        assert_eq!(screenshot, "browser_take_screenshot");
        assert_eq!(back, "browser_navigate_back");
    }

    #[test]
    fn screenshot_result_preserves_image_bytes() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]);
        let result = tool_result(McpToolCallResult {
            content: vec![McpContent::Image {
                data: encoded,
                mime_type: "image/png".to_string(),
            }],
            is_error: false,
            structured_content: None,
            extra: serde_json::Map::new(),
        });

        assert_eq!(result.bytes.as_deref(), Some([1_u8, 2, 3].as_slice()));
        assert_eq!(result.mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            result.kind,
            ToolResultKind::Image {
                mime_type: "image/png".to_string()
            }
        );
    }

    #[test]
    fn screenshot_frame_is_serializable_for_tauri_events() {
        let mut result = ToolResult::success("captured");
        result.bytes = Some(vec![1_u8, 2, 3]);
        result.mime_type = Some("image/png".to_string());
        let frame = browser_frame(&result);
        assert_eq!(
            frame.as_ref().map(|value| value.data_url.as_str()),
            Some("data:image/png;base64,AQID")
        );
    }

    #[tokio::test]
    async fn one_runtime_installs_the_same_tools_on_multiple_agents()
    -> Result<(), Box<dyn std::error::Error>> {
        let runtime = BrowserRuntime::start(BrowserConfig {
            node_command: "__eko_missing_node_for_test__".to_string(),
            ..BrowserConfig::default()
        })
        .await;
        let mut primary = ReactAgentBuilder::new()
            .model("test-model")
            .name("primary")
            .system_prompt("test")
            .build()?;
        let mut worker = ReactAgentBuilder::new()
            .model("test-model")
            .name("worker")
            .system_prompt("test")
            .build()?;

        runtime.install_tools(&mut primary);
        runtime.install_tools(&mut worker);

        for action in BrowserAction::ALL {
            assert!(primary.list_tools().contains(&action.name().to_string()));
            assert!(worker.list_tools().contains(&action.name().to_string()));
        }
        runtime.shutdown().await;
        Ok(())
    }
}
