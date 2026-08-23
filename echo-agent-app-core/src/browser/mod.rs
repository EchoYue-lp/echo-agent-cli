pub mod config;
pub mod error;
pub mod event;
pub mod risk;
pub mod session;
pub mod sidecar;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use base64::Engine;
use echo_agent::human_loop::{
    HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use echo_agent::mcp::{McpClient, McpContent, McpToolCallResult};
use echo_agent::prelude::{
    Tool, ToolFailure, ToolFailureCategory, ToolParameters, ToolResult, ToolRiskLevel,
    ToolSideEffect,
};
use echo_agent::tools::{ToolContext, ToolResultContent, ToolResultKind};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub use config::BrowserConfig;
pub use error::{BrowserError, BrowserResult};
pub use event::{BrowserEvent, BrowserFrame};
pub use risk::BrowserActionRisk;
pub use session::{
    BrowserBackend, BrowserObservation, BrowserSession, BrowserSessionAddress,
    BrowserSessionManager, BrowserSessionStatus, BrowserTab, MAIN_TAB_OWNER,
};
use sidecar::BrowserSidecar;

pub type BrowserApprovalAddress = BrowserSessionAddress;

#[derive(Clone)]
pub struct BrowserRuntime {
    inner: Arc<BrowserRuntimeInner>,
}

struct BrowserRuntimeInner {
    config: BrowserConfig,
    sessions: BrowserSessionManager,
    managed_client: RwLock<Option<Arc<McpClient>>>,
    extension_client: RwLock<Option<Arc<McpClient>>>,
    managed_connect_lock: Mutex<()>,
    extension_connect_lock: Mutex<()>,
    extension_startup_error: RwLock<Option<String>>,
    locator_failures: Mutex<HashMap<String, u8>>,
    default_approval_provider: RwLock<Option<Arc<dyn HumanLoopProvider>>>,
    approval_providers:
        RwLock<HashMap<BrowserApprovalAddress, BrowserApprovalProviderRegistration>>,
    workspace_roots: RwLock<HashMap<PathBuf, String>>,
    shutdown: CancellationToken,
    prewarm: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct BrowserApprovalProviderRegistration {
    registration_id: uuid::Uuid,
    provider: Arc<dyn HumanLoopProvider>,
}

#[must_use = "the browser approval registration must be retained for the owning turn"]
pub struct BrowserApprovalRegistration {
    runtime: Weak<BrowserRuntimeInner>,
    address: BrowserApprovalAddress,
    registration_id: uuid::Uuid,
    closed: bool,
}

impl BrowserApprovalRegistration {
    pub async fn close(mut self) {
        self.remove_if_current().await;
        self.closed = true;
    }

    async fn remove_if_current(&self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let mut providers = runtime.approval_providers.write().await;
        if providers
            .get(&self.address)
            .is_some_and(|registration| registration.registration_id == self.registration_id)
        {
            providers.remove(&self.address);
        }
    }
}

impl Drop for BrowserApprovalRegistration {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let runtime = self.runtime.clone();
        let address = self.address.clone();
        let registration_id = self.registration_id;
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            let mut providers = runtime.approval_providers.write().await;
            if providers
                .get(&address)
                .is_some_and(|registration| registration.registration_id == registration_id)
            {
                providers.remove(&address);
            }
        });
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExtensionStatus {
    pub enabled: bool,
    pub connected: bool,
    pub token_configured: bool,
    pub package: String,
    pub startup_error: Option<String>,
}

impl BrowserRuntime {
    pub async fn start(config: BrowserConfig) -> Arc<Self> {
        let sessions = BrowserSessionManager::new(config.session_dir.clone(), 12_000);
        sessions.restore_metadata().await;
        let runtime = Arc::new(Self {
            inner: Arc::new(BrowserRuntimeInner {
                config,
                sessions,
                managed_client: RwLock::new(None),
                extension_client: RwLock::new(None),
                managed_connect_lock: Mutex::new(()),
                extension_connect_lock: Mutex::new(()),
                extension_startup_error: RwLock::new(None),
                locator_failures: Mutex::new(HashMap::new()),
                default_approval_provider: RwLock::new(None),
                approval_providers: RwLock::new(HashMap::new()),
                workspace_roots: RwLock::new(HashMap::new()),
                shutdown: CancellationToken::new(),
                prewarm: Mutex::new(None),
            }),
        });
        if !runtime.inner.config.enabled {
            tracing::info!("managed Playwright MCP runtime disabled by configuration");
            return runtime;
        }
        let prewarm = runtime.clone();
        let prewarm_cancel = runtime.inner.shutdown.clone();
        let handle = tokio::spawn(async move {
            let result = tokio::select! {
                _ = prewarm_cancel.cancelled() => return,
                result = prewarm.ensure_client(BrowserBackend::Managed) => result,
            };
            match result {
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
        *runtime.inner.prewarm.lock().await = Some(handle);
        runtime
    }

    pub fn install_tools(&self, agent: &mut echo_agent::agent::ReactAgent) {
        self.install_tools_for(agent, BrowserActor::Main);
    }

    pub fn install_subagent_tools(&self, agent: &mut echo_agent::agent::ReactAgent) {
        self.install_tools_for(agent, BrowserActor::Subagent);
    }

    fn install_tools_for(&self, agent: &mut echo_agent::agent::ReactAgent, actor: BrowserActor) {
        if self.inner.config.enabled || self.inner.config.extension_enabled {
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

    pub async fn set_default_approval_provider(&self, provider: Arc<dyn HumanLoopProvider>) {
        *self.inner.default_approval_provider.write().await = Some(provider);
    }

    pub async fn register_approval_provider(
        &self,
        address: BrowserApprovalAddress,
        workspace_root: PathBuf,
        provider: Arc<dyn HumanLoopProvider>,
    ) -> BrowserApprovalRegistration {
        let registration_id = uuid::Uuid::new_v4();
        self.inner.approval_providers.write().await.insert(
            address.clone(),
            BrowserApprovalProviderRegistration {
                registration_id,
                provider,
            },
        );
        self.register_workspace_root(address.workspace_id.clone(), workspace_root)
            .await;
        BrowserApprovalRegistration {
            runtime: Arc::downgrade(&self.inner),
            address,
            registration_id,
            closed: false,
        }
    }

    pub async fn execute_main(
        &self,
        workspace_id: String,
        workspace_root: PathBuf,
        conversation_id: String,
        action: BrowserAction,
        params: ToolParameters,
        cancel: Option<Arc<CancellationToken>>,
    ) -> BrowserResult<ToolResult> {
        self.register_workspace_root(workspace_id, workspace_root.clone())
            .await;
        let context = ToolContext {
            working_dir: Some(workspace_root),
            conversation_id: Some(conversation_id),
            cancel,
            ..ToolContext::default()
        };
        self.call(action, params, &context, BrowserActor::Main)
            .await
    }

    pub async fn register_workspace_root(
        &self,
        workspace_id: impl Into<String>,
        workspace_root: PathBuf,
    ) {
        self.inner
            .workspace_roots
            .write()
            .await
            .insert(workspace_root, workspace_id.into());
    }

    pub async fn remove_workspace(&self, workspace_id: &str) {
        self.inner
            .approval_providers
            .write()
            .await
            .retain(|address, _| address.workspace_id != workspace_id);
        self.inner
            .workspace_roots
            .write()
            .await
            .retain(|_, candidate| candidate != workspace_id);
        self.inner.sessions.remove_workspace(workspace_id).await;
    }

    pub async fn interrupt(&self) {
        for backend in [BrowserBackend::Managed, BrowserBackend::Chrome] {
            let client = self.client_slot(backend).write().await.take();
            if let Some(client) = client {
                client.close().await;
            }
        }
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        if let Some(handle) = self.inner.prewarm.lock().await.take()
            && let Err(error) = handle.await
        {
            tracing::warn!(%error, "browser prewarm task failed during shutdown");
        }
        self.inner.sessions.close_all().await;
        for backend in [BrowserBackend::Managed, BrowserBackend::Chrome] {
            let client = self.client_slot(backend).write().await.take();
            if let Some(client) = client {
                client.close().await;
            }
        }
        tracing::info!("Playwright MCP browser runtimes stopped");
    }

    pub async fn extension_status(&self) -> BrowserExtensionStatus {
        let startup_error = self.inner.extension_startup_error.read().await.clone();
        BrowserExtensionStatus {
            enabled: self.inner.config.extension_enabled,
            connected: self.inner.extension_client.read().await.is_some()
                && startup_error.is_none(),
            token_configured: self.inner.config.extension_token.is_some(),
            package: self.inner.config.package.clone(),
            startup_error,
        }
    }

    fn client_slot(&self, backend: BrowserBackend) -> &RwLock<Option<Arc<McpClient>>> {
        match backend {
            BrowserBackend::Managed => &self.inner.managed_client,
            BrowserBackend::Chrome => &self.inner.extension_client,
        }
    }

    fn connect_lock(&self, backend: BrowserBackend) -> &Mutex<()> {
        match backend {
            BrowserBackend::Managed => &self.inner.managed_connect_lock,
            BrowserBackend::Chrome => &self.inner.extension_connect_lock,
        }
    }

    async fn ensure_client(&self, backend: BrowserBackend) -> BrowserResult<Arc<McpClient>> {
        if self.inner.shutdown.is_cancelled() {
            return Err(BrowserError::Connection(
                "browser runtime is shutting down".to_string(),
            ));
        }
        let enabled = match backend {
            BrowserBackend::Managed => self.inner.config.enabled,
            BrowserBackend::Chrome => self.inner.config.extension_enabled,
        };
        if !enabled {
            return Err(BrowserError::Disabled);
        }
        if let Some(client) = self.client_slot(backend).read().await.clone() {
            return Ok(client);
        }

        let _guard = self.connect_lock(backend).lock().await;
        if let Some(client) = self.client_slot(backend).read().await.clone() {
            return Ok(client);
        }

        BrowserSidecar::prepare(&self.inner.config).await?;
        let timeout = Duration::from_secs(self.inner.config.startup_timeout_secs);
        let client = tokio::time::timeout(
            timeout,
            McpClient::new(BrowserSidecar::server_config(&self.inner.config, backend)),
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
        *self.client_slot(backend).write().await = Some(client.clone());
        Ok(client)
    }

    async fn invalidate_client(&self, backend: BrowserBackend, failed: &Arc<McpClient>) {
        let mut current = self.client_slot(backend).write().await;
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
        let address = self.resolve_address(context, conversation_id).await;
        let navigation_url = match action {
            BrowserAction::Navigate => {
                Some(params.get("url").and_then(Value::as_str).ok_or_else(|| {
                    BrowserError::Tool {
                        tool: action.name().to_string(),
                        message: "url must be a string".to_string(),
                    }
                })?)
            }
            BrowserAction::Tabs if params.get("action").and_then(Value::as_str) == Some("new") => {
                params.get("url").and_then(Value::as_str)
            }
            _ => None,
        };
        if let Some(url) = navigation_url
            && !self.inner.config.allows_url(url)
        {
            return Err(BrowserError::Tool {
                tool: action.name().to_string(),
                message: "navigation blocked by browser domain configuration".to_string(),
            });
        }
        let owner_id = match actor {
            BrowserActor::Main => MAIN_TAB_OWNER.to_string(),
            BrowserActor::Subagent => context
                .execution_id
                .clone()
                .or_else(|| context.run_id.clone())
                .or_else(|| context.turn_id.clone())
                .unwrap_or_else(|| format!("subagent-{}", uuid::Uuid::new_v4())),
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
                .open_tab(&address, &owner_id, context.run_id.as_deref())
                .await
                .ok_or_else(|| {
                    BrowserError::Connection("failed to allocate browser tab".to_string())
                })?,
            Some("select") => {
                let index = requested_index.ok_or_else(|| BrowserError::Tool {
                    tool: action.name().to_string(),
                    message: "select requires a valid non-negative tab index".to_string(),
                })?;
                self.inner
                    .sessions
                    .select_tab(&address, &owner_id, index)
                    .await
                    .ok_or_else(|| BrowserError::Tool {
                        tool: action.name().to_string(),
                        message: format!("browser tab index {index} does not exist"),
                    })?
            }
            _ => {
                self.inner
                    .sessions
                    .lease_tab(&address, &owner_id, context.run_id.as_deref())
                    .await
            }
        };
        let run_id = context.run_id.clone();
        let turn_id = context.turn_id.clone();
        let execution_id = context.execution_id.clone();
        let action_name = action.name().to_string();
        if action == BrowserAction::Backend {
            let backend = params
                .get("backend")
                .and_then(Value::as_str)
                .ok_or_else(|| BrowserError::Tool {
                    tool: action_name.clone(),
                    message: "backend must be managed or chrome".to_string(),
                })?;
            self.inner.sessions.emit(BrowserEvent::ActionStarted {
                session_id: lease.session_id.clone(),
                tab_id: lease.tab_id.clone(),
                action: action_name.clone(),
                run_id: run_id.clone(),
                turn_id: turn_id.clone(),
                execution_id: execution_id.clone(),
            });
            let result = match backend {
                "managed" => {
                    let session = self
                        .inner
                        .sessions
                        .switch_backend(&address, BrowserBackend::Managed)
                        .await
                        .ok_or_else(|| {
                            BrowserError::Connection("browser session missing".to_string())
                        })?;
                    self.inner
                        .sessions
                        .emit(BrowserEvent::SessionUpdated { session });
                    json!({ "backend": "managed" })
                }
                "chrome" => {
                    let connection = self
                        .call_mcp(
                            BrowserBackend::Chrome,
                            "browser_tabs",
                            json!({ "action": "list" }),
                            context.cancel.as_deref(),
                        )
                        .await;
                    if let Err(error) = connection {
                        *self.inner.extension_startup_error.write().await = Some(error.to_string());
                        self.inner.sessions.emit(BrowserEvent::ActionFailed {
                            session_id: lease.session_id,
                            tab_id: lease.tab_id,
                            action: action_name,
                            run_id,
                            turn_id,
                            execution_id,
                            error: error.to_string(),
                        });
                        return Err(error);
                    }
                    *self.inner.extension_startup_error.write().await = None;
                    let session = self
                        .inner
                        .sessions
                        .switch_backend(&address, BrowserBackend::Chrome)
                        .await
                        .ok_or_else(|| {
                            BrowserError::Connection("browser session missing".to_string())
                        })?;
                    self.inner
                        .sessions
                        .emit(BrowserEvent::SessionUpdated { session });
                    json!({ "backend": "chrome", "driver": "playwright_extension" })
                }
                _ => {
                    return Err(BrowserError::Tool {
                        tool: action_name,
                        message: "backend must be managed or chrome".to_string(),
                    });
                }
            };
            let completion_lease = self
                .inner
                .sessions
                .lease_tab(&address, &owner_id, context.run_id.as_deref())
                .await;
            let selected_backend = self.inner.sessions.backend(&address).await;
            self.inner.sessions.emit(BrowserEvent::BackendChanged {
                session_id: completion_lease.session_id.clone(),
                backend: selected_backend,
            });
            self.inner.sessions.emit(BrowserEvent::ActionCompleted {
                session_id: completion_lease.session_id,
                tab_id: completion_lease.tab_id,
                action: action_name,
                run_id,
                turn_id,
                execution_id,
            });
            return Ok(ToolResult::success_json(result));
        }
        let action_risk = BrowserActionRisk::classify(action, &params)?;
        if action_risk.requires_confirmation() {
            let confirmation_args = action_risk.confirmation_args(action, &params);
            let summary = confirmation_args
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or(action_risk.label())
                .to_string();
            self.inner
                .sessions
                .set_status(&address, BrowserSessionStatus::WaitingConfirmation)
                .await;
            self.inner
                .sessions
                .emit(BrowserEvent::ConfirmationRequested {
                    session_id: lease.session_id.clone(),
                    tab_id: lease.tab_id.clone(),
                    risk: action_risk.label().to_string(),
                    summary,
                });
            let approved = match self
                .confirm_action(&address, action, action_risk, &params)
                .await
            {
                Ok(approved) => approved,
                Err(error) => {
                    self.inner
                        .sessions
                        .emit(BrowserEvent::ConfirmationResolved {
                            session_id: lease.session_id.clone(),
                            tab_id: lease.tab_id.clone(),
                            approved: false,
                        });
                    self.inner
                        .sessions
                        .set_status(&address, BrowserSessionStatus::Ready)
                        .await;
                    return Err(error);
                }
            };
            self.inner
                .sessions
                .emit(BrowserEvent::ConfirmationResolved {
                    session_id: lease.session_id.clone(),
                    tab_id: lease.tab_id.clone(),
                    approved,
                });
            if !approved {
                self.inner
                    .sessions
                    .set_status(&address, BrowserSessionStatus::Ready)
                    .await;
                return Err(BrowserError::Tool {
                    tool: action_name,
                    message: "browser action was not approved".to_string(),
                });
            }
        }
        if action == BrowserAction::DeveloperMode {
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| BrowserError::Tool {
                    tool: action_name.clone(),
                    message: "enabled must be a boolean".to_string(),
                })?;
            self.inner.sessions.emit(BrowserEvent::ActionStarted {
                session_id: lease.session_id.clone(),
                tab_id: lease.tab_id.clone(),
                action: action_name.clone(),
                run_id: run_id.clone(),
                turn_id: turn_id.clone(),
                execution_id: execution_id.clone(),
            });
            self.inner
                .sessions
                .set_developer_mode(&address, enabled)
                .await;
            self.inner.sessions.emit(BrowserEvent::ActionCompleted {
                session_id: lease.session_id,
                tab_id: lease.tab_id,
                action: action_name,
                run_id,
                turn_id,
                execution_id,
            });
            return Ok(ToolResult::success(if enabled {
                "Browser Developer Mode enabled for this conversation."
            } else {
                "Browser Developer Mode disabled for this conversation."
            }));
        }
        if action == BrowserAction::PerformanceTrace
            && !self.inner.sessions.developer_mode(&address).await
        {
            return Err(BrowserError::Tool {
                tool: action_name,
                message: "enable browser_developer_mode for this conversation before tracing"
                    .to_string(),
            });
        }
        let backend = self.inner.sessions.backend(&address).await;
        if backend == BrowserBackend::Chrome
            && tabs_command.as_deref() == Some("close")
            && requested_index.unwrap_or(lease.tab_index) == 0
        {
            return Err(BrowserError::Tool {
                tool: action_name,
                message: "the Chrome tab selected through the Playwright extension remains user-owned; close it in Chrome"
                    .to_string(),
            });
        }
        let locator_failure_key = locator_failure_key(&lease, action, &params);
        if let Some(key) = locator_failure_key.as_ref()
            && self
                .inner
                .locator_failures
                .lock()
                .await
                .get(key)
                .copied()
                .unwrap_or(0)
                >= 2
        {
            return Err(BrowserError::Tool {
                tool: action_name,
                message: "the same locator failed twice; inspect a fresh DOM fragment or use coordinate control"
                    .to_string(),
            });
        }
        let _operation = self.inner.sessions.lock_operation().await;
        if tabs_command.as_deref() == Some("new") || tabs_command.as_deref() == Some("select") {
            // The explicit browser_tabs call below performs the matching MCP operation.
        } else if lease.opened {
            self.call_mcp(
                backend,
                "browser_tabs",
                json!({ "action": "new", "url": "about:blank" }),
                context.cancel.as_deref(),
            )
            .await?;
        } else {
            self.call_mcp(
                backend,
                "browser_tabs",
                json!({ "action": "select", "index": lease.tab_index }),
                context.cancel.as_deref(),
            )
            .await?;
        }

        let type_at_text = if action == BrowserAction::TypeAt {
            Some(
                params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BrowserError::Tool {
                        tool: action_name.clone(),
                        message: "text must be a string".to_string(),
                    })?
                    .chars()
                    .take(500)
                    .collect::<String>(),
            )
        } else {
            None
        };
        let (tool, mut arguments) = action.translate(params)?;
        if action == BrowserAction::TypeAt
            && let Value::Object(values) = &mut arguments
        {
            values.remove("text");
        }
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
                .set_status(&address, BrowserSessionStatus::Navigating)
                .await;
        } else {
            self.inner
                .sessions
                .set_status(&address, BrowserSessionStatus::Acting)
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

        let highlight_arguments = if matches!(action, BrowserAction::Click | BrowserAction::Fill) {
            arguments
                .get("target")
                .and_then(Value::as_str)
                .map(|target| {
                    json!({
                        "target": target,
                        "element": arguments.get("element").and_then(Value::as_str),
                        "style": "outline: 2px solid #2563eb; outline-offset: 2px"
                    })
                })
        } else {
            None
        };
        if let Some(highlight) = highlight_arguments.as_ref() {
            let _ = self
                .call_mcp(
                    backend,
                    "browser_highlight",
                    highlight.clone(),
                    context.cancel.as_deref(),
                )
                .await;
        }

        let raw_call = if let Some(text) = type_at_text {
            self.type_at(backend, arguments.clone(), &text, context.cancel.as_deref())
                .await
        } else {
            self.call_mcp(backend, tool, arguments.clone(), context.cancel.as_deref())
                .await
        };
        match raw_call {
            Ok(raw_result) => {
                if let Some(key) = locator_failure_key.as_ref() {
                    self.inner.locator_failures.lock().await.remove(key);
                }
                let (mut result, result_frame) = tool_result_with_frame(raw_result);
                if matches!(action, BrowserAction::Console | BrowserAction::Network) {
                    result.output = redact_browser_diagnostics(&result.output);
                    result.data = None;
                    result.kind = ToolResultKind::Text;
                }
                let (page_url, page_title) = attach_browser_page_metadata(&mut result);
                self.inner
                    .sessions
                    .update_page_metadata(
                        &address,
                        &lease.tab_id,
                        page_url.as_deref(),
                        page_title.as_deref(),
                    )
                    .await;
                self.inner
                    .sessions
                    .set_status(&address, BrowserSessionStatus::Ready)
                    .await;
                if action == BrowserAction::Navigate {
                    let url = arguments
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if page_url.is_none() {
                        self.inner
                            .sessions
                            .update_url(&address, &lease.tab_id, url)
                            .await;
                    }
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
                    BrowserAction::DomInspect => {
                        self.inner.sessions.emit(BrowserEvent::Diagnostic {
                            category: "dom".to_string(),
                            observation,
                        })
                    }
                    BrowserAction::Console => self.inner.sessions.emit(BrowserEvent::Diagnostic {
                        category: "console".to_string(),
                        observation,
                    }),
                    BrowserAction::Network => self.inner.sessions.emit(BrowserEvent::Diagnostic {
                        category: "network".to_string(),
                        observation,
                    }),
                    BrowserAction::PerformanceTrace => {
                        self.inner.sessions.emit(BrowserEvent::Diagnostic {
                            category: "performance".to_string(),
                            observation,
                        })
                    }
                    BrowserAction::Screenshot => {
                        self.inner.sessions.emit(BrowserEvent::Screenshot {
                            observation,
                            frame: result_frame,
                        });
                    }
                    BrowserAction::Navigate
                    | BrowserAction::Click
                    | BrowserAction::Fill
                    | BrowserAction::ClickAt
                    | BrowserAction::TypeAt
                    | BrowserAction::Scroll
                    | BrowserAction::Back
                    | BrowserAction::Reload => {
                        if let Ok(raw_frame) = self
                            .call_mcp(
                                backend,
                                "browser_take_screenshot",
                                json!({ "type": "png" }),
                                context.cancel.as_deref(),
                            )
                            .await
                        {
                            let (frame_result, frame) = tool_result_with_frame(raw_frame);
                            let frame_observation = self.inner.sessions.observation(
                                &lease,
                                "browser_screenshot",
                                &frame_result,
                            );
                            self.inner.sessions.emit(BrowserEvent::Screenshot {
                                observation: frame_observation,
                                frame,
                            });
                        }
                    }
                    BrowserAction::Backend | BrowserAction::Tabs | BrowserAction::DeveloperMode => {
                    }
                }
                if tabs_command.as_deref() == Some("close") {
                    let index = requested_index.unwrap_or(lease.tab_index);
                    self.inner.sessions.close_tab(&address, index).await;
                }
                if let Some(highlight) = highlight_arguments {
                    let _ = self
                        .call_mcp(
                            backend,
                            "browser_hide_highlight",
                            hide_highlight_arguments(highlight),
                            context.cancel.as_deref(),
                        )
                        .await;
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
                if let Some(highlight) = highlight_arguments {
                    let _ = self
                        .call_mcp(
                            backend,
                            "browser_hide_highlight",
                            hide_highlight_arguments(highlight),
                            context.cancel.as_deref(),
                        )
                        .await;
                }
                if let Some(key) = locator_failure_key {
                    let mut failures = self.inner.locator_failures.lock().await;
                    let count = failures.entry(key).or_insert(0);
                    *count = count.saturating_add(1);
                }
                self.inner
                    .sessions
                    .set_status(&address, BrowserSessionStatus::Failed)
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
        backend: BrowserBackend,
        tool: &'static str,
        arguments: Value,
        cancel: Option<&CancellationToken>,
    ) -> BrowserResult<McpToolCallResult> {
        let first = self.ensure_client(backend).await?;
        let first_call = first.call_tool(tool, arguments.clone());
        let first_result = if let Some(cancel) = cancel {
            tokio::select! {
                result = first_call => result,
                _ = cancel.cancelled() => {
                    self.invalidate_client(backend, &first).await;
                    first.close().await;
                    return Err(BrowserError::Cancelled);
                }
            }
        } else {
            first_call.await
        };
        match first_result {
            Ok(result) => mcp_tool_result(tool, result),
            Err(first_error) if !browser_mcp_retry_safe(tool, &arguments) => {
                tracing::warn!(
                    tool,
                    ?backend,
                    error = %first_error,
                    "Playwright MCP call failed after a possibly consequential action; not replaying"
                );
                self.invalidate_client(backend, &first).await;
                first.close().await;
                Err(BrowserError::Connection(first_error.to_string()))
            }
            Err(first_error) => {
                tracing::warn!(
                    tool,
                    ?backend,
                    error = %first_error,
                    "Playwright MCP call failed; restarting browser sidecar"
                );
                self.invalidate_client(backend, &first).await;
                first.close().await;
                let restarted = self.ensure_client(backend).await?;
                let retry = restarted.call_tool(tool, arguments);
                let result = if let Some(cancel) = cancel {
                    tokio::select! {
                        result = retry => result,
                        _ = cancel.cancelled() => {
                            self.invalidate_client(backend, &restarted).await;
                            restarted.close().await;
                            return Err(BrowserError::Cancelled);
                        }
                    }
                } else {
                    retry.await
                };
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        self.invalidate_client(backend, &restarted).await;
                        restarted.close().await;
                        return Err(BrowserError::Tool {
                            tool: tool.to_string(),
                            message: error.to_string(),
                        });
                    }
                };
                mcp_tool_result(tool, result)
            }
        }
    }

    async fn type_at(
        &self,
        backend: BrowserBackend,
        click_arguments: Value,
        text: &str,
        cancel: Option<&CancellationToken>,
    ) -> BrowserResult<McpToolCallResult> {
        self.call_mcp(backend, "browser_mouse_click_xy", click_arguments, cancel)
            .await?;
        for character in text.chars() {
            let key = match character {
                '\n' => "Enter".to_string(),
                '\t' => "Tab".to_string(),
                value => value.to_string(),
            };
            self.call_mcp(backend, "browser_press_key", json!({ "key": key }), cancel)
                .await?;
        }
        Ok(McpToolCallResult {
            content: vec![McpContent::Text {
                text: format!(
                    "Typed {} characters at the requested coordinates.",
                    text.chars().count()
                ),
            }],
            is_error: false,
            structured_content: None,
            extra: serde_json::Map::new(),
        })
    }

    async fn confirm_action(
        &self,
        address: &BrowserSessionAddress,
        action: BrowserAction,
        risk: BrowserActionRisk,
        params: &ToolParameters,
    ) -> BrowserResult<bool> {
        let registration = self
            .inner
            .approval_providers
            .read()
            .await
            .get(address)
            .map(|registration| registration.provider.clone());
        let provider = match registration.as_ref() {
            Some(provider) => Some(provider.clone()),
            None => self.inner.default_approval_provider.read().await.clone(),
        }
        .ok_or_else(|| BrowserError::Tool {
            tool: action.name().to_string(),
            message: "no HITL provider is available for consequential browser action".to_string(),
        })?;
        let request = HumanLoopRequest {
            request_id: None,
            session_id: Some(
                registration
                    .as_ref()
                    .map(|_| address.session_id())
                    .unwrap_or_else(|| address.conversation_id.clone()),
            ),
            agent_name: None,
            kind: HumanLoopKind::Approval,
            prompt: risk.prompt(params),
            tool_name: Some(action.name().to_string()),
            args: Some(risk.confirmation_args(action, params)),
            risk_level: Some(risk.risk_level()),
            approval_context: None,
            suggestions: Vec::new(),
            timeout: Some(Duration::from_secs(5 * 60)),
            task_id: None,
            options: None,
            context: None,
            phase: None,
        };
        let response = provider
            .request(request)
            .await
            .map_err(|error| BrowserError::Tool {
                tool: action.name().to_string(),
                message: error.to_string(),
            })?;
        Ok(matches!(
            response,
            HumanLoopResponse::Approved
                | HumanLoopResponse::ApprovedWithScope { .. }
                | HumanLoopResponse::ModifiedArgs { .. }
        ))
    }

    async fn resolve_address(
        &self,
        context: &ToolContext,
        conversation_id: &str,
    ) -> BrowserSessionAddress {
        let roots = self.inner.workspace_roots.read().await;
        let mut candidates = roots
            .iter()
            .filter(|(root, _)| {
                context
                    .working_dir
                    .as_deref()
                    .is_some_and(|working_dir| working_dir.starts_with(root))
            })
            .map(|(root, workspace_id)| (root.components().count(), workspace_id.clone()))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        let registered_address = if candidates.is_empty() {
            let providers = self.inner.approval_providers.read().await;
            let mut addresses = providers
                .keys()
                .filter(|address| address.conversation_id == conversation_id)
                .cloned();
            let first = addresses.next();
            if addresses.next().is_none() {
                first
            } else {
                None
            }
        } else {
            None
        };
        if let Some(address) = registered_address {
            return address;
        }
        BrowserSessionAddress::new(
            candidates
                .first()
                .map(|(_, workspace_id)| workspace_id.clone())
                .unwrap_or_else(|| "global".to_string()),
            conversation_id,
        )
    }
}

fn mcp_tool_result(tool: &str, result: McpToolCallResult) -> BrowserResult<McpToolCallResult> {
    if result.is_error {
        return Err(BrowserError::Tool {
            tool: tool.to_string(),
            message: McpClient::content_to_text(&result.content),
        });
    }
    Ok(result)
}

fn browser_mcp_retry_safe(tool: &str, arguments: &Value) -> bool {
    match tool {
        "browser_get_config"
        | "browser_snapshot"
        | "browser_take_screenshot"
        | "browser_console_messages"
        | "browser_network_requests"
        | "browser_find" => true,
        "browser_tabs" => arguments
            .get("action")
            .and_then(Value::as_str)
            .is_none_or(|action| action == "list"),
        _ => false,
    }
}

fn browser_action_retry_safe(action: BrowserAction, parameters: &ToolParameters) -> bool {
    if action == BrowserAction::Tabs {
        return parameters
            .get("action")
            .and_then(Value::as_str)
            .is_none_or(|action| action == "list");
    }
    action.risk() == ToolRiskLevel::ReadOnly
}

fn browser_failure(retry_safe: bool, error: &BrowserError, context: &ToolContext) -> ToolFailure {
    match error {
        BrowserError::Cancelled => ToolFailure::new(ToolFailureCategory::Cancelled),
        BrowserError::Disabled | BrowserError::Prerequisite(_) => {
            ToolFailure::new(ToolFailureCategory::Unavailable)
        }
        BrowserError::Io(_) | BrowserError::Connection(_) if retry_safe => {
            ToolFailure::new(ToolFailureCategory::Unavailable).retryable()
        }
        BrowserError::Io(_) | BrowserError::Connection(_) => {
            let failure = ToolFailure::new(ToolFailureCategory::PartialSideEffect)
                .with_side_effect(ToolSideEffect::Possible)
                .with_postcondition(
                    "take a fresh browser snapshot and verify page/tab state before retrying",
                );
            match context.call_id.as_ref() {
                Some(call_id) => failure.with_idempotency_key(call_id.clone()),
                None => failure,
            }
        }
        BrowserError::Tool { .. } if retry_safe => {
            ToolFailure::new(ToolFailureCategory::InvalidArguments)
        }
        BrowserError::Tool { .. } => ToolFailure::new(ToolFailureCategory::PartialSideEffect)
            .with_side_effect(ToolSideEffect::Possible)
            .with_postcondition(
                "take a fresh browser snapshot and verify whether the requested action completed",
            ),
    }
}

fn locator_failure_key(
    lease: &session::BrowserLease,
    action: BrowserAction,
    params: &ToolParameters,
) -> Option<String> {
    if !matches!(action, BrowserAction::Click | BrowserAction::Fill) {
        return None;
    }
    let target = params.get("target")?.as_str()?;
    Some(format!(
        "{}:{}:{}:{}",
        lease.session_id,
        lease.tab_id,
        action.name(),
        target
    ))
}

fn hide_highlight_arguments(mut arguments: Value) -> Value {
    if let Value::Object(values) = &mut arguments {
        values.remove("style");
    }
    arguments
}

fn redact_browser_diagnostics(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "authorization",
                "proxy-authorization",
                "cookie:",
                "set-cookie",
                "api_key",
                "api-key",
                "access_token",
                "refresh_token",
                "password",
                "passwd",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                "[REDACTED sensitive browser diagnostic]".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Backend,
    Navigate,
    Snapshot,
    Click,
    Fill,
    Screenshot,
    Back,
    Reload,
    Tabs,
    ClickAt,
    TypeAt,
    Scroll,
    Console,
    Network,
    DomInspect,
    PerformanceTrace,
    DeveloperMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserActor {
    Main,
    Subagent,
}

impl BrowserAction {
    const ALL: [Self; 17] = [
        Self::Backend,
        Self::Navigate,
        Self::Snapshot,
        Self::Click,
        Self::Fill,
        Self::Screenshot,
        Self::Back,
        Self::Reload,
        Self::Tabs,
        Self::ClickAt,
        Self::TypeAt,
        Self::Scroll,
        Self::Console,
        Self::Network,
        Self::DomInspect,
        Self::PerformanceTrace,
        Self::DeveloperMode,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Backend => "browser_backend",
            Self::Navigate => "browser_navigate",
            Self::Snapshot => "browser_snapshot",
            Self::Click => "browser_click",
            Self::Fill => "browser_fill",
            Self::Screenshot => "browser_screenshot",
            Self::Back => "browser_back",
            Self::Reload => "browser_reload",
            Self::Tabs => "browser_tabs",
            Self::ClickAt => "browser_click_at",
            Self::TypeAt => "browser_type_at",
            Self::Scroll => "browser_scroll",
            Self::Console => "browser_console",
            Self::Network => "browser_network",
            Self::DomInspect => "browser_dom_inspect",
            Self::PerformanceTrace => "browser_performance_trace",
            Self::DeveloperMode => "browser_developer_mode",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Backend => {
                "Select managed Chromium or Chrome through the official Playwright Extension for this conversation."
            }
            Self::Navigate => "Navigate the current browser backend to a URL.",
            Self::Snapshot => "Read a structured accessibility snapshot of the current page.",
            Self::Click => "Click an element identified by a Playwright snapshot target.",
            Self::Fill => "Fill or type text into an editable element.",
            Self::Screenshot => "Capture the current page or a selected element.",
            Self::Back => "Navigate the current tab back one history entry.",
            Self::Reload => "Reload the current page.",
            Self::Tabs => "List, create, close, or select browser tabs.",
            Self::ClickAt => {
                "Click viewport coordinates when semantic DOM targeting is unavailable."
            }
            Self::TypeAt => {
                "Focus viewport coordinates and type text when semantic DOM targeting is unavailable."
            }
            Self::Scroll => "Scroll the current viewport by pixel deltas.",
            Self::Console => "Read bounded browser console diagnostics.",
            Self::Network => "Read bounded browser network request diagnostics.",
            Self::DomInspect => {
                "Inspect a bounded DOM/accessibility fragment near a target or matching text."
            }
            Self::PerformanceTrace => {
                "Start or stop a Playwright performance trace in Developer Mode."
            }
            Self::DeveloperMode => {
                "Enable or disable session-scoped browser developer diagnostics."
            }
        }
    }

    fn parameters(self) -> Value {
        match self {
            Self::Backend => object_schema(
                json!({
                    "backend": { "type": "string", "enum": ["managed", "chrome"] }
                }),
                &["backend"],
            ),
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
                    "doubleClick": { "type": "boolean" },
                    "effect": browser_effect_schema(),
                    "confirmationSummary": confirmation_summary_schema(),
                    "destination": confirmation_destination_schema(),
                    "dataCategories": data_categories_schema()
                }),
                &["target", "effect"],
            ),
            Self::Fill => object_schema(
                json!({
                    "target": { "type": "string", "description": "Exact target reference from browser_snapshot" },
                    "text": { "type": "string", "description": "Text to enter" },
                    "element": { "type": "string", "description": "Human-readable field description" },
                    "submit": { "type": "boolean" },
                    "slowly": { "type": "boolean" },
                    "effect": browser_effect_schema(),
                    "confirmationSummary": confirmation_summary_schema(),
                    "destination": confirmation_destination_schema(),
                    "dataCategories": data_categories_schema()
                }),
                &["target", "text", "effect"],
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
            Self::ClickAt => object_schema(
                json!({
                    "x": { "type": "number", "minimum": 0 },
                    "y": { "type": "number", "minimum": 0 },
                    "button": { "type": "string", "enum": ["left", "right", "middle"] },
                    "clickCount": { "type": "integer", "minimum": 1, "maximum": 3 },
                    "effect": browser_effect_schema(),
                    "confirmationSummary": confirmation_summary_schema(),
                    "destination": confirmation_destination_schema(),
                    "dataCategories": data_categories_schema()
                }),
                &["x", "y", "effect"],
            ),
            Self::TypeAt => object_schema(
                json!({
                    "x": { "type": "number", "minimum": 0 },
                    "y": { "type": "number", "minimum": 0 },
                    "text": { "type": "string", "maxLength": 500 },
                    "effect": browser_effect_schema(),
                    "confirmationSummary": confirmation_summary_schema(),
                    "destination": confirmation_destination_schema(),
                    "dataCategories": data_categories_schema()
                }),
                &["x", "y", "text", "effect"],
            ),
            Self::Scroll => object_schema(
                json!({
                    "deltaX": { "type": "number" },
                    "deltaY": { "type": "number" }
                }),
                &["deltaY"],
            ),
            Self::Console => object_schema(
                json!({
                    "level": { "type": "string", "enum": ["error", "warning", "info", "debug"] },
                    "all": { "type": "boolean" }
                }),
                &[],
            ),
            Self::Network => object_schema(
                json!({
                    "includeStatic": { "type": "boolean" },
                    "filter": { "type": "string" }
                }),
                &[],
            ),
            Self::DomInspect => object_schema(
                json!({
                    "target": { "type": "string" },
                    "text": { "type": "string" },
                    "regex": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 12 },
                    "boxes": { "type": "boolean" }
                }),
                &[],
            ),
            Self::PerformanceTrace => object_schema(
                json!({ "action": { "type": "string", "enum": ["start", "stop"] } }),
                &["action"],
            ),
            Self::DeveloperMode => {
                object_schema(json!({ "enabled": { "type": "boolean" } }), &["enabled"])
            }
        }
    }

    fn translate(self, params: ToolParameters) -> BrowserResult<(&'static str, Value)> {
        let mut arguments = serde_json::Map::from_iter(params);
        for key in [
            "effect",
            "confirmationSummary",
            "destination",
            "dataCategories",
        ] {
            arguments.remove(key);
        }
        match self {
            Self::Backend => Ok(("browser_get_config", Value::Object(arguments))),
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
            Self::ClickAt => Ok(("browser_mouse_click_xy", Value::Object(arguments))),
            Self::TypeAt => Ok(("browser_mouse_click_xy", Value::Object(arguments))),
            Self::Scroll => {
                arguments
                    .entry("deltaX".to_string())
                    .or_insert_with(|| Value::Number(0.into()));
                Ok(("browser_mouse_wheel", Value::Object(arguments)))
            }
            Self::Console => {
                arguments
                    .entry("level".to_string())
                    .or_insert_with(|| Value::String("warning".to_string()));
                Ok(("browser_console_messages", Value::Object(arguments)))
            }
            Self::Network => {
                if let Some(value) = arguments.remove("includeStatic") {
                    arguments.insert("static".to_string(), value);
                }
                Ok(("browser_network_requests", Value::Object(arguments)))
            }
            Self::DomInspect => {
                if arguments.contains_key("text") || arguments.contains_key("regex") {
                    arguments.remove("target");
                    arguments.remove("depth");
                    arguments.remove("boxes");
                    Ok(("browser_find", Value::Object(arguments)))
                } else {
                    Ok(("browser_snapshot", Value::Object(arguments)))
                }
            }
            Self::PerformanceTrace => {
                let command = arguments
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("start");
                if command == "stop" {
                    Ok(("browser_stop_tracing", json!({})))
                } else {
                    Ok(("browser_start_tracing", json!({})))
                }
            }
            Self::DeveloperMode => Ok(("browser_get_config", json!({}))),
        }
    }

    fn risk(self) -> ToolRiskLevel {
        match self {
            Self::Backend
            | Self::Snapshot
            | Self::Screenshot
            | Self::Console
            | Self::Network
            | Self::DomInspect
            | Self::PerformanceTrace => ToolRiskLevel::ReadOnly,
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
            let retry_safe = browser_action_retry_safe(self.action, &parameters);
            match self
                .runtime
                .call(self.action, parameters, &ToolContext::default(), self.actor)
                .await
            {
                Ok(result) => Ok(result),
                Err(error) => Ok(ToolResult::error(error.to_string())
                    .with_failure(browser_failure(retry_safe, &error, &ToolContext::default()))),
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let retry_safe = browser_action_retry_safe(self.action, &parameters);
            match self
                .runtime
                .call(self.action, parameters, context, self.actor)
                .await
            {
                Ok(result) => Ok(result),
                Err(error) => Ok(ToolResult::error(error.to_string())
                    .with_failure(browser_failure(retry_safe, &error, context))),
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

fn browser_effect_schema() -> Value {
    json!({
        "type": "string",
        "description": "Declare the real external effect. Use none for navigation or unsubmitted input.",
        "enum": [
            "none",
            "sensitive_submit",
            "purchase",
            "publish",
            "send_message",
            "account_change",
            "permission_change",
            "cloud_delete"
        ]
    })
}

fn confirmation_summary_schema() -> Value {
    json!({
        "type": "string",
        "maxLength": 300,
        "description": "Short user-facing description of the consequence. Never include passwords, tokens, cookies, payment numbers, or entered field values."
    })
}

fn confirmation_destination_schema() -> Value {
    json!({
        "type": "string",
        "maxLength": 300,
        "description": "Human-readable destination such as the site, recipient, account, or resource. Never include secret values."
    })
}

fn data_categories_schema() -> Value {
    json!({
        "type": "array",
        "description": "Names of data categories being submitted, without their values.",
        "items": { "type": "string", "maxLength": 100 },
        "maxItems": 12
    })
}

fn tool_result_with_frame(result: McpToolCallResult) -> (ToolResult, Option<BrowserFrame>) {
    let text = McpClient::content_to_text(&result.content);
    if result.is_error {
        let mut tool_result = ToolResult::error(text);
        if let Some(structured) = result.structured_content {
            tool_result = tool_result.with_data(structured);
            tool_result.kind = ToolResultKind::StructuredError {
                error_code: "playwright_mcp_error".to_string(),
            };
        }
        return (tool_result, None);
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
    let frame = image.and_then(|(bytes, mime_type)| browser_frame(&bytes, &mime_type));
    if let Some(frame) = frame.as_ref() {
        tool_result.kind = ToolResultKind::Image {
            mime_type: frame.mime_type.clone(),
        };
        tool_result.mime_type = Some(frame.mime_type.clone());
        tool_result = tool_result.with_model_content(ToolResultContent::ImageUrl {
            url: frame.data_url.clone(),
            detail: Some("high".to_string()),
        });
    }
    if !result.extra.is_empty()
        && let Ok(extra) = serde_json::to_string_pretty(&result.extra)
    {
        tool_result.output.push_str("\n\nAdditional fields:\n");
        tool_result.output.push_str(&extra);
    }
    (tool_result, frame)
}

fn browser_frame(bytes: &[u8], mime_type: &str) -> Option<BrowserFrame> {
    if bytes.len() > 8 * 1024 * 1024 {
        tracing::warn!(
            bytes = bytes.len(),
            "browser screenshot omitted from GUI event"
        );
        return None;
    }
    Some(BrowserFrame {
        data_url: format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        mime_type: mime_type.to_string(),
    })
}

fn attach_browser_page_metadata(result: &mut ToolResult) -> (Option<String>, Option<String>) {
    let mut url = None;
    let mut title = None;
    for line in result.output.lines() {
        let line = line.trim();
        if url.is_none() {
            url = line
                .strip_prefix("- Page URL:")
                .or_else(|| line.strip_prefix("Page URL:"))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if title.is_none() {
            title = line
                .strip_prefix("- Page Title:")
                .or_else(|| line.strip_prefix("Page Title:"))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }
    if let Some(url) = &url {
        result
            .metadata
            .insert("browser_url".to_string(), url.clone());
    }
    if let Some(title) = &title {
        result
            .metadata
            .insert("browser_title".to_string(), title.clone());
    }
    (url, title)
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
                "browser_backend",
                "browser_navigate",
                "browser_snapshot",
                "browser_click",
                "browser_fill",
                "browser_screenshot",
                "browser_back",
                "browser_reload",
                "browser_tabs",
                "browser_click_at",
                "browser_type_at",
                "browser_scroll",
                "browser_console",
                "browser_network",
                "browser_dom_inspect",
                "browser_performance_trace",
                "browser_developer_mode",
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
        let (click_at, _) = BrowserAction::ClickAt
            .translate(ToolParameters::new())
            .unwrap_or(("", Value::Null));
        let (console, _) = BrowserAction::Console
            .translate(ToolParameters::new())
            .unwrap_or(("", Value::Null));
        assert_eq!(fill, "browser_type");
        assert_eq!(screenshot, "browser_take_screenshot");
        assert_eq!(back, "browser_navigate_back");
        assert_eq!(click_at, "browser_mouse_click_xy");
        assert_eq!(console, "browser_console_messages");
    }

    #[test]
    fn application_confirmation_metadata_is_not_sent_to_playwright() {
        let translated = BrowserAction::Click.translate(ToolParameters::from([
            ("target".to_string(), Value::String("submit".to_string())),
            ("effect".to_string(), Value::String("purchase".to_string())),
            (
                "confirmationSummary".to_string(),
                Value::String("Place order".to_string()),
            ),
            (
                "destination".to_string(),
                Value::String("example.com".to_string()),
            ),
            ("dataCategories".to_string(), json!(["shipping address"])),
        ]));

        assert!(translated.is_ok());
        let (_, arguments) = translated.unwrap_or(("", Value::Null));
        assert_eq!(
            arguments.get("target").and_then(Value::as_str),
            Some("submit")
        );
        assert!(arguments.get("effect").is_none());
        assert!(arguments.get("confirmationSummary").is_none());
        assert!(arguments.get("destination").is_none());
        assert!(arguments.get("dataCategories").is_none());
    }

    #[tokio::test]
    async fn domain_policy_covers_navigation_and_new_tabs() -> Result<(), String> {
        let runtime = BrowserRuntime::start(BrowserConfig {
            enabled: false,
            blocked_domains: vec!["blocked.example".to_string()],
            ..BrowserConfig::default()
        })
        .await;
        let context = ToolContext::default();
        let navigate = runtime
            .call(
                BrowserAction::Navigate,
                ToolParameters::from([(
                    "url".to_string(),
                    Value::String("https://blocked.example/page".to_string()),
                )]),
                &context,
                BrowserActor::Main,
            )
            .await;
        let new_tab = runtime
            .call(
                BrowserAction::Tabs,
                ToolParameters::from([
                    ("action".to_string(), Value::String("new".to_string())),
                    (
                        "url".to_string(),
                        Value::String("https://blocked.example/page".to_string()),
                    ),
                ]),
                &context,
                BrowserActor::Main,
            )
            .await;

        assert!(navigate.is_err());
        assert!(new_tab.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn approval_registration_uses_full_address_and_owned_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = BrowserRuntime::start(BrowserConfig {
            enabled: false,
            extension_enabled: false,
            session_dir: temp.path().join("sessions"),
            ..BrowserConfig::default()
        })
        .await;
        let provider: Arc<dyn HumanLoopProvider> = Arc::new(crate::hitl::HitlDispatcher::new());
        let address_a = BrowserApprovalAddress::new("workspace-a", "conversation-1");
        let address_b = BrowserApprovalAddress::new("workspace-b", "conversation-1");
        let receipt_a = runtime
            .register_approval_provider(
                address_a.clone(),
                temp.path().join("workspace-a"),
                provider.clone(),
            )
            .await;
        let receipt_b = runtime
            .register_approval_provider(
                address_b.clone(),
                temp.path().join("workspace-b"),
                provider.clone(),
            )
            .await;
        let resolved = runtime
            .resolve_address(
                &ToolContext {
                    working_dir: Some(temp.path().join("workspace-b/worktree")),
                    conversation_id: Some("conversation-1".to_string()),
                    ..ToolContext::default()
                },
                "conversation-1",
            )
            .await;
        assert_eq!(resolved, address_b);

        let replacement = runtime
            .register_approval_provider(
                address_a.clone(),
                temp.path().join("workspace-a"),
                provider,
            )
            .await;
        receipt_a.close().await;
        assert!(
            runtime
                .inner
                .approval_providers
                .read()
                .await
                .contains_key(&address_a)
        );

        replacement.close().await;
        receipt_b.close().await;
        assert!(runtime.inner.approval_providers.read().await.is_empty());
        Ok(())
    }

    #[test]
    fn dom_and_network_diagnostics_choose_bounded_tools() {
        let (find, find_args) = BrowserAction::DomInspect
            .translate(ToolParameters::from([(
                "text".to_string(),
                Value::String("checkout".to_string()),
            )]))
            .unwrap_or(("", Value::Null));
        let (requests, request_args) = BrowserAction::Network
            .translate(ToolParameters::from([(
                "includeStatic".to_string(),
                Value::Bool(false),
            )]))
            .unwrap_or(("", Value::Null));

        assert_eq!(find, "browser_find");
        assert_eq!(
            find_args.get("text").and_then(Value::as_str),
            Some("checkout")
        );
        assert_eq!(requests, "browser_network_requests");
        assert_eq!(
            request_args.get("static").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn mcp_application_errors_are_not_reported_as_success() {
        let result = mcp_tool_result(
            "browser_click",
            McpToolCallResult {
                content: vec![McpContent::Text {
                    text: "locator not found".to_string(),
                }],
                is_error: true,
                structured_content: None,
                extra: serde_json::Map::new(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn browser_retry_contract_distinguishes_reads_from_consequential_actions() {
        assert!(browser_mcp_retry_safe("browser_snapshot", &json!({})));
        assert!(browser_mcp_retry_safe(
            "browser_tabs",
            &json!({"action": "list"})
        ));
        assert!(!browser_mcp_retry_safe(
            "browser_click",
            &json!({"target": "submit"})
        ));

        let context = ToolContext {
            call_id: Some("call-browser-1".to_string()),
            ..Default::default()
        };
        let read_failure = browser_failure(
            browser_action_retry_safe(BrowserAction::Snapshot, &ToolParameters::new()),
            &BrowserError::Connection("closed".to_string()),
            &context,
        );
        let click_failure = browser_failure(
            browser_action_retry_safe(
                BrowserAction::Click,
                &ToolParameters::from([("target".to_string(), json!("submit"))]),
            ),
            &BrowserError::Connection("closed".to_string()),
            &context,
        );

        assert_eq!(read_failure.category, ToolFailureCategory::Unavailable);
        assert!(read_failure.allows_automatic_retry());
        assert_eq!(
            click_failure.category,
            ToolFailureCategory::PartialSideEffect
        );
        assert!(!click_failure.allows_automatic_retry());
        assert_eq!(
            click_failure.idempotency_key.as_deref(),
            Some("call-browser-1")
        );
    }

    #[test]
    fn browser_diagnostics_redact_sensitive_lines() {
        let output = "GET /api 500\nAuthorization: Bearer secret\nCookie: sid=secret\nplain error";
        let redacted = redact_browser_diagnostics(output);
        assert!(redacted.contains("GET /api 500"));
        assert!(redacted.contains("plain error"));
        assert!(!redacted.contains("Bearer secret"));
        assert!(!redacted.contains("sid=secret"));
    }

    #[test]
    fn playwright_page_state_becomes_renderer_metadata() {
        let mut result = ToolResult::success(
            "### Page state\n- Page URL: https://example.com/docs\n- Page Title: Example Docs",
        );
        let (url, title) = attach_browser_page_metadata(&mut result);
        assert_eq!(url.as_deref(), Some("https://example.com/docs"));
        assert_eq!(title.as_deref(), Some("Example Docs"));
        assert_eq!(
            result.metadata.get("browser_title").map(String::as_str),
            Some("Example Docs")
        );
    }

    #[test]
    fn locator_failure_key_is_scoped_to_tab_action_and_target() {
        let lease = session::BrowserLease {
            session_id: "session".to_string(),
            tab_id: "tab".to_string(),
            tab_index: 0,
            opened: false,
        };
        let params = ToolParameters::from([(
            "target".to_string(),
            Value::String("button-submit".to_string()),
        )]);

        assert_eq!(
            locator_failure_key(&lease, BrowserAction::Click, &params).as_deref(),
            Some("session:tab:browser_click:button-submit")
        );
        assert!(locator_failure_key(&lease, BrowserAction::ClickAt, &params).is_none());
    }

    #[test]
    fn screenshot_result_preserves_image_bytes() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]);
        let (result, frame) = tool_result_with_frame(McpToolCallResult {
            content: vec![McpContent::Image {
                data: encoded,
                mime_type: "image/png".to_string(),
            }],
            is_error: false,
            structured_content: None,
            extra: serde_json::Map::new(),
        });

        assert_eq!(result.mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            frame.as_ref().map(|value| value.data_url.as_str()),
            Some("data:image/png;base64,AQID")
        );
        assert_eq!(
            result.kind,
            ToolResultKind::Image {
                mime_type: "image/png".to_string()
            }
        );
        assert!(matches!(
            result.model_content.first(),
            Some(ToolResultContent::ImageUrl { url, detail })
                if url == "data:image/png;base64,AQID"
                    && detail.as_deref() == Some("high")
        ));
    }

    #[test]
    fn screenshot_frame_is_serializable_for_tauri_events() {
        let frame = browser_frame(&[1_u8, 2, 3], "image/png");
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
        let mut subagent = ReactAgentBuilder::new()
            .model("test-model")
            .name("subagent")
            .system_prompt("test")
            .build()?;

        runtime.install_tools(&mut primary);
        runtime.install_tools(&mut subagent);

        for action in BrowserAction::ALL {
            assert!(primary.list_tools().contains(&action.name().to_string()));
            assert!(subagent.list_tools().contains(&action.name().to_string()));
        }
        runtime.shutdown().await;
        Ok(())
    }
}
