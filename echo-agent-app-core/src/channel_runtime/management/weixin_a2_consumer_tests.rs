// Consumer scenarios share the real Scanner/Session/pump/Ready and sender.
// The constructor argument is only a test dependency binding: the original
// Framework owner supplies its loopback client, never a fake Ready or sender.
use super::*;
use crate::channel_runtime::{AppChannelMessageHandler, ChannelSessionCoordinator, input_pump};
use crate::conversation_input::{ConversationInputIdentity, ConversationInputProjection};
use echo_agent::channels::{
    ChannelHandle, ChannelManager, ChannelTransportEvidence, ChannelTransportPhase, SessionConfig,
    SessionHandler,
};
use echo_agent::memory::{FileStore, Store};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

type LocalChannel = fn(WeixinConfig, SocketAddr) -> echo_agent::error::Result<WeixinChannel>;
const BUSINESS_REPLY: &str = "已恢复🙂";
const LONG_REPLY_PREFIX: &str = "ORIGINAL_LONG_REPLY:";

const REPLY_OBSERVATION_TARGET: &str =
    "echo_agent_app_core::channel_runtime::weixin_reply_observation";

#[derive(Default, Clone)]
struct ReplyReceiptFields {
    values: std::collections::BTreeMap<String, String>,
    overflow: bool,
    explicit_root: bool,
}

impl ReplyReceiptFields {
    fn record(&mut self, field: &tracing::field::Field, value: &str) {
        if self.values.len() >= 8 || value.chars().count() > 256 {
            self.overflow = true;
            return;
        }
        self.values
            .insert(field.name().to_string(), value.to_string());
    }
}

impl tracing::field::Visit for ReplyReceiptFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record(field, &value.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record(field, value);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        // This target contains only a fixed message, SHA256, ordinal and enum label.
        self.record(field, &format!("{value:?}"));
    }
}

#[derive(Clone, Default)]
struct ReplyReceiptCapture {
    events: Arc<std::sync::Mutex<Vec<ReplyReceiptFields>>>,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

impl ReplyReceiptCapture {
    fn snapshot(&self) -> anyhow::Result<Vec<ReplyReceiptFields>> {
        anyhow::ensure!(
            !self.failed.load(Ordering::SeqCst),
            "reply receipt capture overflowed"
        );
        let events = self
            .events
            .lock()
            .map_err(|_| anyhow::anyhow!("reply capture unavailable"))?;
        anyhow::ensure!(
            events.iter().all(|event| !event.overflow),
            "reply fields exceeded retained bounds"
        );
        Ok(events.clone())
    }

    fn verify(&self, client_ids: &[&str]) -> anyhow::Result<()> {
        let events = self.snapshot()?;
        anyhow::ensure!(
            events.len() == client_ids.len(),
            "actual accepted observation count mismatch"
        );
        for (event, client_id) in events.iter().zip(client_ids) {
            anyhow::ensure!(
                event.explicit_root,
                "receipt observation inherited a caller span"
            );
            let keys: Vec<_> = event.values.keys().map(String::as_str).collect();
            anyhow::ensure!(
                keys == [
                    "local_client_id_sha256",
                    "message",
                    "outcome",
                    "reply_ordinal"
                ],
                "accepted observation leaked an extra field or lost correlation"
            );
            anyhow::ensure!(
                event.values.get("local_client_id_sha256")
                    == Some(&format!(
                        "{:x}",
                        <sha2::Sha256 as sha2::Digest>::digest(client_id.as_bytes())
                    )),
                "accepted observation did not retain the original receipt identity"
            );
            anyhow::ensure!(
                event.values.get("outcome").map(String::as_str) == Some("accepted"),
                "receipt was reclassified"
            );
            anyhow::ensure!(
                event.values.get("message").map(String::as_str)
                    == Some("Weixin original reply received its platform acknowledgement"),
                "accepted observation included non-canonical text"
            );
            let ordinal = client_id
                .rsplit(':')
                .next()
                .ok_or_else(|| anyhow::anyhow!("receipt ordinal missing"))?;
            anyhow::ensure!(
                event.values.get("reply_ordinal").map(String::as_str) == Some(ordinal),
                "receipt ordinal changed"
            );
        }
        Ok(())
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ReplyReceiptCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != REPLY_OBSERVATION_TARGET {
            return;
        }
        let mut fields = ReplyReceiptFields {
            explicit_root: event.is_root(),
            ..ReplyReceiptFields::default()
        };
        event.record(&mut fields);
        match self.events.lock() {
            Ok(mut events) if events.len() < 8 => events.push(fields),
            _ => self.failed.store(true, Ordering::SeqCst),
        }
    }
}

// 仅延迟本机provider的HTTP回执；不持有或释放App/Framework执行owner。
struct HeldSend {
    request: tokio::sync::watch::Sender<Option<Value>>,
    release: tokio::sync::Notify,
}

#[derive(Clone, Default)]
struct Observed {
    polls: usize,
    send_requests: usize,
    protocol_errors: usize,
    // Only requests accepted by the provider with ret:0 are recorded here.
    sends: Vec<serde_json::Value>,
    last_cursor: String,
}

impl Observed {
    // 生命周期通知另有真实 HTTP 发送记录；业务结果只按预置正文计数。
    // 配合子进程的 Completed、单 attempt 和分类/回答计数，不能把错误回复当成功。
    fn business_reply_count(&self) -> usize {
        self.sends
            .iter()
            .filter(|request| {
                request
                    .pointer("/msg/item_list/0/text_item/text")
                    .and_then(serde_json::Value::as_str)
                    == Some(BUSINESS_REPLY)
            })
            .count()
    }
}

#[derive(Clone)]
struct Provider {
    account: WeixinAccountConfig,
    // The first boot supplies a real inbound message. Recovery boots supply
    // empty authenticated polls: only original durable discovery may execute.
    input: Option<(String, String)>,
    pages: Option<Arc<Pages>>,
    held_send: Option<Arc<HeldSend>>,
    observed: tokio::sync::watch::Sender<Observed>,
}

struct Pages {
    admitted_pages: std::sync::atomic::AtomicUsize,
    messages: Vec<(String, String)>,
}

struct LocalProvider {
    address: SocketAddr,
    observed: tokio::sync::watch::Receiver<Observed>,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl LocalProvider {
    async fn start(
        account: WeixinAccountConfig,
        input: Option<(String, String)>,
    ) -> anyhow::Result<Self> {
        Self::start_with_pages(account, input, None).await
    }

    async fn start_with_pages(
        account: WeixinAccountConfig,
        input: Option<(String, String)>,
        pages: Option<Arc<Pages>>,
    ) -> anyhow::Result<Self> {
        Self::start_with_send_gate(account, input, pages, None).await
    }

    async fn start_with_send_gate(
        account: WeixinAccountConfig,
        input: Option<(String, String)>,
        pages: Option<Arc<Pages>>,
        held_send: Option<Arc<HeldSend>>,
    ) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (observed, received) = tokio::sync::watch::channel(Observed::default());
        let router = axum::Router::new()
            .route("/ilink/bot/getupdates", axum::routing::post(poll))
            .route("/ilink/bot/sendmessage", axum::routing::post(send))
            .with_state(Provider {
                account,
                input,
                pages,
                held_send,
                observed,
            });
        let stop = CancellationToken::new();
        let closing = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(closing.cancelled_owned())
                .await
        });
        Ok(Self {
            address,
            observed: received,
            stop,
            server,
        })
    }

    async fn close(self) -> anyhow::Result<Observed> {
        self.stop.cancel();
        self.server.await??;
        Ok(self.observed.borrow().clone())
    }
}

async fn poll(
    axum::extract::State(state): axum::extract::State<Provider>,
    headers: axum::http::HeaderMap,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    state
        .observed
        .send_modify(|seen| seen.polls = seen.polls.saturating_add(1));
    if !authenticated(&headers, &state.account)
        || request
            .get("get_updates_buf")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || !protocol_base(&request)
    {
        state
            .observed
            .send_modify(|seen| seen.protocol_errors = seen.protocol_errors.saturating_add(1));
        return axum::Json(serde_json::json!({"ret": 403}));
    }
    let cursor = request
        .get("get_updates_buf")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    state
        .observed
        .send_modify(|seen| seen.last_cursor = cursor.to_string());
    let mut next_cursor = "original-persisted-page".to_string();
    let selected = if let Some(pages) = state.pages.as_ref() {
        let index = if cursor.is_empty() {
            Some(0)
        } else {
            cursor
                .strip_prefix("fixture-page-")
                .and_then(|value| value.parse::<usize>().ok())
        };
        let Some(index) = index else {
            state
                .observed
                .send_modify(|seen| seen.protocol_errors = seen.protocol_errors.saturating_add(1));
            return axum::Json(serde_json::json!({"ret": 403}));
        };
        next_cursor = cursor.to_string();
        if index < pages.admitted_pages.load(Ordering::SeqCst)
            && let Some(message) = pages.messages.get(index)
        {
            next_cursor = format!("fixture-page-{}", index.saturating_add(1));
            Some(message)
        } else {
            None
        }
    } else {
        state.input.as_ref().filter(|_| cursor.is_empty())
    };
    let messages = match selected {
        Some((provider_id, text)) => vec![serde_json::json!({
            "message_id": provider_id, "message_type": 1,
            "from_user_id": state.account.user_id,
            "item_list": [{"type": 1, "text_item": {"text": text}}],
        })],
        _ => Vec::new(),
    };
    // Bound repeated empty fixture polls without modifying the real adapter.
    tokio::time::sleep(Duration::from_millis(10)).await;
    axum::Json(serde_json::json!({"ret": 0, "msgs": messages, "get_updates_buf": next_cursor}))
}

async fn send(
    axum::extract::State(state): axum::extract::State<Provider>,
    headers: axum::http::HeaderMap,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    state
        .observed
        .send_modify(|seen| seen.send_requests = seen.send_requests.saturating_add(1));
    let valid_body = protocol_base(&request)
        && request
            .pointer("/msg/from_user_id")
            .and_then(serde_json::Value::as_str)
            == Some("")
        && request
            .pointer("/msg/to_user_id")
            .and_then(serde_json::Value::as_str)
            == Some(state.account.user_id.as_str())
        && request
            .pointer("/msg/client_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty())
        && request
            .pointer("/msg/message_type")
            .and_then(serde_json::Value::as_u64)
            == Some(2)
        && request
            .pointer("/msg/message_state")
            .and_then(serde_json::Value::as_u64)
            == Some(2)
        && request.pointer("/msg/context_token").is_none()
        && request
            .pointer("/msg/item_list")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.len() == 1
                    && items.first().is_some_and(|item| {
                        item.get("type").and_then(serde_json::Value::as_u64) == Some(1)
                            && item
                                .pointer("/text_item/text")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|text| !text.is_empty())
                    })
            });
    if !authenticated(&headers, &state.account) || !valid_body {
        state
            .observed
            .send_modify(|seen| seen.protocol_errors = seen.protocol_errors.saturating_add(1));
        return axum::Json(serde_json::json!({"ret": 403}));
    }
    if let Some(gate) = state.held_send.as_ref()
        && request
            .pointer("/msg/item_list/0/text_item/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with(LONG_REPLY_PREFIX))
    {
        let first = gate.request.send_if_modified(|captured| {
            if captured.is_none() {
                *captured = Some(request.clone());
                true
            } else {
                false
            }
        });
        if first {
            // send_requests仅证明收到；回执开放前不能放进Accepted观察。
            gate.release.notified().await;
        }
    }
    state.observed.send_modify(|seen| {
        if seen.sends.len() < 16 {
            seen.sends.push(request);
        }
    });
    // This response is parsed by the original Framework sender. The App must
    // finish that real HTTP future; putting a message in a queue is insufficient.
    axum::Json(serde_json::json!({"ret": 0}))
}

fn authenticated(headers: &axum::http::HeaderMap, account: &WeixinAccountConfig) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", account.bot_token))
}

fn protocol_base(request: &serde_json::Value) -> bool {
    request
        .pointer("/base_info/channel_version")
        .and_then(serde_json::Value::as_str)
        == Some("2.4.10")
        && request
            .pointer("/base_info/bot_agent")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|agent| !agent.is_empty())
}

// 与旧冷重开用例共用应用原 owner；只替换模型依赖和临时产品数据根。
async fn application(
    root: &Path,
    llm: Arc<echo_agent::testing::MockLlmClient>,
) -> anyhow::Result<(crate::runtime::ApplicationLifecycleOwner, Arc<AppState>)> {
    let primary = echo_agent::agent::AgentHandle::new(
        echo_agent::agent::ReactAgentBuilder::new()
            .model("local-a2-consumer")
            .llm_client(llm.clone())
            .build()?,
    );
    // 与new_for_test已经声明的模型配置相同；真实渠道保存不能把
    // 夹具pool的模型覆盖为空，配置模型的原Critic仍由factory安装并由Mock transport替换。
    let mut app_config = crate::config::EkoConfig::default();
    app_config.model.default_model_id = Some("test:test-model".to_string());
    app_config.model_providers.insert(
        "test".to_string(),
        crate::config::ModelProviderConfig {
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            ..Default::default()
        },
    );
    app_config
        .configured_models
        .push(crate::config::ConfiguredModel {
            id: "test:test-model".to_string(),
            provider: "test".to_string(),
            model: "test-model".to_string(),
            ..Default::default()
        });
    let pool = Arc::new(
        crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false).await,
    );
    pool.set_llm_client_override_for_test(llm.clone()).await;
    let mut state = AppState::from_shared(
        primary.clone(),
        None,
        Arc::new(crate::hitl::HitlDispatcher::new()),
        None,
        None,
        app_config,
        Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
            root.join("mcp.json"),
            Default::default(),
        )),
        crate::product_data_io::ProductDataIoService::new(),
    )?;
    state.set_pool(pool.clone());
    // 真实启动流程会先绑定应用所有者，再准入即时创建的 TaskRun driver。
    // 冷启动子进程也保留同一所有权，并使用本场景自己的持久化任务根。
    let task_runtime = Arc::new(
        crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
            root.join("tasks"),
            "global",
        )?,
    );
    state.tasks.runtime = Some(task_runtime.clone());
    state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
        root.join("events"),
        Default::default(),
    )?);
    state.workspace.registry = Arc::new(
        crate::workspace::registry::WorkspaceRegistry::with_base_dir(
            root.join("workspace-registry"),
        )?,
    );
    state.workspace.global_execution_root = root.join("product-data");
    std::fs::create_dir_all(&state.workspace.global_execution_root)?;
    let state = Arc::new(state.with_config_path(root.join("eko.yaml")));
    let mut lifecycle = crate::runtime::ApplicationLifecycleOwner::new(CancellationToken::new());
    lifecycle.bind_app_state(state.clone());
    lifecycle.bind_primary_agent(primary);
    lifecycle.bind_pool(pool);
    lifecycle.bind_task_runtime(task_runtime);
    lifecycle.bind_product_data_io(state.session.product_data_io.clone());
    Ok((lifecycle, state))
}

struct Boot {
    lifecycle: crate::runtime::ApplicationLifecycleOwner,
    state: Arc<AppState>,
    coordinator: Arc<ChannelSessionCoordinator>,
    handle: ChannelHandle,
    handler: Arc<dyn MessageHandler>,
    llm: Arc<echo_agent::testing::MockLlmClient>,
    held: Arc<std::sync::Mutex<Vec<input_pump::ChannelInputPumpOwner<ConversationInputIdentity>>>>,
}

impl Boot {
    async fn start(
        root: &Path,
        account: &WeixinAccountConfig,
        address: SocketAddr,
        hold_before_claim: bool,
        local_channel: LocalChannel,
    ) -> anyhow::Result<Self> {
        // 原 factory 依次执行真实 IntentRouter、回答与默认 Critic；三类请求
        // 分别供给合法响应，业务回答与验证请求独立计数，保留仅发送一次的断言。
        let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_responses([
            r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#,
            BUSINESS_REPLY,
            r#"{"score":9.0,"passed":true,"feedback":"Original durable reply verified","suggestions":[]}"#,
        ]));
        let (lifecycle, state) = application(root, llm.clone()).await?;
        let runtime = state.current_control_runtime().await?;
        let store = Arc::new(FileStore::new(root.join("protocol.json"))?) as Arc<dyn Store>;
        let plugin = local_channel(
            WeixinConfig {
                credentials: credentials(account),
                store,
            },
            address,
        )?;
        let delivery = plugin
            .delivery_handle()
            .ok_or_else(|| anyhow::anyhow!("original sender unavailable"))?;
        let coordinator = Arc::new(ChannelSessionCoordinator::new());
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let factory_state = state.clone();
        let factory_owner = coordinator.clone();
        let end_owner = coordinator.clone();
        let factory_held = held.clone();
        let binding = account.clone();
        let sessions = SessionHandler::new(
            SessionConfig::default().with_reset_commands(Vec::new()),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let mut handler = AppChannelMessageHandler::new(
                    factory_state.clone(),
                    Arc::new(crate::webhook::WebhookEmitter::new()),
                    factory_state.session.foreground_turns.clone(),
                    instance.clone(),
                    factory_owner.clone(),
                )
                .with_bound_control_runtime(runtime.clone())
                .with_weixin_delivery(delivery.clone())
                .with_weixin_credential_identity(&binding.account_id, &binding.generation);
                if hold_before_claim {
                    // Hold the ORIGINAL pump election, as the existing 40-input
                    // fixture does. Admission still uses real poll/context/submit.
                    match handler.input_pump.kick() {
                        Ok(input_pump::ChannelInputPumpKick::Started(owner)) => {
                            factory_held
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push(owner);
                        }
                        Ok(input_pump::ChannelInputPumpKick::Notified) => {
                            handler.initialization_error =
                                Some("test did not acquire the original pump election".into());
                        }
                        Err(error) => {
                            handler.initialization_error = Some(error.to_string());
                        }
                    }
                }
                Box::new(handler) as Box<dyn MessageHandler>
            },
        )
        .with_durable_ingress()
        .with_on_session_end(move |info| end_owner.record_session_end(info));
        let handler: Arc<dyn MessageHandler> = Arc::new(
            ScannerHandler::new(Arc::new(sessions), account.clone())
                .with_coordinator(coordinator.clone()),
        );
        let mut manager = ChannelManager::new();
        manager.register(Box::new(plugin))?;
        let handle = manager
            .get_owned("weixin")
            .ok_or_else(|| anyhow::anyhow!("original registered channel missing"))?;
        handle.start(handler.clone()).await?;
        // Cold recovery deliberately races two ORIGINAL initialization tasks.
        // They share the same SessionHandler, journal and pump election.
        for _ in 0..if hold_before_claim { 1 } else { 2 } {
            coordinator
                .start_channel_recovery_initialization(
                    state.clone(),
                    handler.clone(),
                    handle.clone(),
                    account.account_id.clone(),
                    account.generation.clone(),
                )
                .map_err(anyhow::Error::msg)?;
        }
        let observation = handle.transport_status().await;
        let mut status = observation
            .receiver
            .ok_or_else(|| anyhow::anyhow!("original Ready watch unavailable"))?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let ready = {
                    let value = status.borrow_and_update();
                    value.phase == ChannelTransportPhase::Ready
                        && value.evidence == Some(ChannelTransportEvidence::WeixinAuthenticatedPoll)
                };
                if ready {
                    return Ok::<_, anyhow::Error>(());
                }
                status.changed().await?;
            }
        })
        .await??;
        Ok(Self {
            lifecycle,
            state,
            coordinator,
            handle,
            handler,
            llm,
            held,
        })
    }

    async fn wait_persisted(
        &self,
        provider_id: &str,
    ) -> anyhow::Result<ConversationInputProjection> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (inputs, _) = self
                    .state
                    .storage
                    .chat_events
                    .channel_input_bindings()
                    .await?;
                if let Some(input) = inputs.into_iter().find(|input| {
                    input
                        .payload
                        .channel_binding
                        .as_ref()
                        .is_some_and(|binding| binding.provider_message_id == provider_id)
                }) {
                    return Ok::<_, anyhow::Error>(input);
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
    }

    async fn close(self) -> anyhow::Result<Vec<String>> {
        self.coordinator
            .begin_input_pump_shutdown()
            .map_err(anyhow::Error::msg)?;
        self.held
            .lock()
            .map_err(|_| anyhow::anyhow!("original held pump unavailable"))?
            .clear();
        self.handle.stop().await?;
        let receipt = self
            .coordinator
            .observe_input_pump_settlement()
            .await
            .map_err(anyhow::Error::msg)?;
        self.state.session.foreground_turns.shutdown().await?;
        let stopped = self.handle.management_snapshot().await;
        anyhow::ensure!(
            stopped.shutdown_settled && stopped.active_invocations == 0,
            "original physical channel owner not settled"
        );
        drop(self.handler);
        let lifecycle = self
            .lifecycle
            .settle(crate::runtime::ApplicationLifecycleReason::Shutdown, None)
            .await
            .wait_final()
            .await;
        anyhow::ensure!(
            lifecycle.is_clean(),
            "原应用所有者未完整结算：{lifecycle:?}"
        );
        Ok(receipt.failures)
    }
}

fn segment_paths(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    if !root.exists() {
        return Ok(found);
    }
    for directory in std::fs::read_dir(root)? {
        let directory = directory?;
        if !directory.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(directory.path())? {
            let file = file?;
            if file.file_type()?.is_file()
                && file
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                found.push(file.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn leave_uncommitted_tail(path: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    // An incomplete new JSON frame; no confirmed frame is removed or rewritten.
    file.write_all(b"{")?;
    file.sync_data()?;
    Ok(())
}

fn leave_complete_corruption(path: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(b"{\"invalid_complete_frame\":true}\n")?;
    file.sync_data()?;
    Ok(())
}

fn account(name: &str) -> WeixinAccountConfig {
    WeixinAccountConfig {
        account_id: format!("account-{name}"),
        bot_token: format!("local-fixture-only-{name}"),
        base_url: "https://example.invalid".into(),
        user_id: format!("scanner-{name}"),
        generation: format!("persisted-generation-{name}"),
    }
}

// Child stdout is a test observation, never a product proof file or journal.
// The parent retains exact child bytes in its normal test output as base64.
fn emit(value: serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;
    println!("\nA2_CONSUMER_RECEIPT:{}", serde_json::to_string(&value)?);
    std::io::stdout().flush()?;
    Ok(())
}

async fn child_entry(local_channel: LocalChannel) -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::var("EKO_WEIXIN_A2_ROOT")?);
    let phase = std::env::var("EKO_WEIXIN_A2_PHASE")?;
    if phase == "managed-reconnect" {
        return managed_reconnect_case().await;
    }
    if phase == "managed-long-stop" {
        return managed_long_stop_case().await;
    }
    if phase == "repair-and-crash" {
        let log = Arc::new(crate::chat_event_log::ChatEventLog::open(
            root.join("events"),
            Default::default(),
        )?);
        let (inputs, complete) = log.channel_input_bindings().await?;
        anyhow::ensure!(
            complete && inputs.len() == 1,
            "repair boot did not retain the complete original input"
        );
        let input = inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("original input missing"))?;
        log.prove_unclaimed_channel_input(input.receipt.identity.clone())
            .await?;
        emit(serde_json::json!({"phase":phase,"model_calls":0,"repair_event_added":false}))?;
        // No App recovery event or new runtime is created between the original
        // physical repair and this second process exit. Kernel releases leases.
        std::process::exit(0);
    }
    let name = std::env::var("EKO_WEIXIN_A2_ACCOUNT")?;
    let config = account(&name);
    let address: SocketAddr = std::env::var("EKO_WEIXIN_A2_PROVIDER")?.parse()?;
    let preparing = matches!(phase.as_str(), "persist-and-crash" | "claim-and-crash");
    let boot = Boot::start(&root, &config, address, preparing, local_channel).await?;
    let provider_id = format!("provider-{name}");
    if preparing {
        let input = boot.wait_persisted(&provider_id).await?;
        anyhow::ensure!(
            boot.llm.call_count() == 0,
            "model ran before the held original claim"
        );
        let identity = input.receipt.identity.clone();
        if phase == "claim-and-crash" {
            let claimed = boot
                .state
                .storage
                .chat_events
                .start_selected_conversation_input_checked(
                    identity.clone(),
                    input.receipt.queue_revision,
                    "confirmed-original-attempt".into(),
                    true,
                    None,
                )
                .await?;
            anyhow::ensure!(
                claimed.receipt.attempt == Some(1),
                "original SyncData claim did not confirm"
            );
        } else {
            boot.state
                .storage
                .chat_events
                .prove_unclaimed_channel_input(identity.clone())
                .await?;
        }
        emit(serde_json::json!({"phase":phase,"identity":identity,"model_calls":0}))?;
        // Intentional process termination before destructors: this covers lost
        // RAM route/old fence, not merely dropping a local candidate variable.
        std::process::exit(0);
    }
    let expected: usize = std::env::var("EKO_WEIXIN_A2_EXPECT_CALLS")?.parse()?;
    let pending = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let busy = boot
                .coordinator
                .has_pending_service_work()
                .map_err(anyhow::Error::msg)?;
            if !busy {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if pending.is_err()
        && let Ok((inputs, complete)) = boot
            .state
            .storage
            .chat_events
            .channel_input_bindings()
            .await
    {
        let runs = boot.state.tasks.runtime.as_ref().map(|store| {
                inputs.iter().filter_map(|input| input.receipt.turn_id.as_deref()).map(|turn_id| {
                    let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
                    serde_json::json!({
                        "run_id": run_id,
                        "status": store.get_run(&run_id).map(|run| run.map(|run| run.status.as_str().to_string())).map_err(|error| error.to_string()),
                        "driver_active": store.is_run_active(&run_id),
                        "continuation": crate::tasks::task_runtime::continuation::runtime_state_for_test(store, &run_id),
                    })
                }).collect::<Vec<_>>()
            });
        eprintln!(
            "A2_PENDING_DIAGNOSTIC:{}",
            serde_json::json!({
                "inventory_complete": complete,
                "inputs": inputs.iter().map(|input| &input.receipt).collect::<Vec<_>>(),
                "runs": runs,
                "model_calls": boot.llm.call_count(),
            })
        );
    }
    pending??;
    let calls = boot.llm.call_count();
    let requests = boot.llm.all_calls();
    let tool_counts = boot.llm.all_tool_counts();
    let formats = boot.llm.all_response_formats();
    anyhow::ensure!(
        requests.len() == calls && tool_counts.len() == calls && formats.len() == calls,
        "模型请求及工具计数记录不完整"
    );
    let mut routing_calls = 0_usize;
    let mut answer_calls = 0_usize;
    let mut critique_calls = 0_usize;
    let mut unknown_calls = 0_usize;
    let mut request_classes = Vec::new();
    for ((messages, tools), format) in requests.iter().zip(&tool_counts).zip(&formats) {
        let routing = *tools == 0
            && format.is_none()
            && messages.len() == 1
            && messages.first().is_some_and(|message| {
                message.role == echo_agent::llm::types::Role::User
                    && message.text_content().is_some_and(|text| {
                        text.starts_with("You are an intent classifier for an AI agent system.")
                    })
            });
        let answer = *tools > 0
            && format.is_none()
            && messages.iter().any(|message| {
                message.role == echo_agent::llm::types::Role::User
                    && message.text_content().as_deref() == Some("original durable input")
            });
        let critique = *tools == 0
            && matches!(format,
            Some(echo_agent::llm::ResponseFormat::JsonSchema { json_schema }) if json_schema.name == "critique_output");
        let class = match (routing, answer, critique) {
            (true, false, false) => {
                routing_calls = routing_calls.saturating_add(1);
                "routing"
            }
            (false, true, false) => {
                answer_calls = answer_calls.saturating_add(1);
                "answer"
            }
            (false, false, true) => {
                critique_calls = critique_calls.saturating_add(1);
                "critique"
            }
            _ => {
                unknown_calls = unknown_calls.saturating_add(1);
                "unknown"
            }
        };
        request_classes.push(class);
    }
    let exact_pipeline = routing_calls == expected
        && answer_calls == expected
        && critique_calls == expected
        && unknown_calls == 0
        && calls == expected.saturating_mul(3);
    let (inputs, complete) = boot
        .state
        .storage
        .chat_events
        .channel_input_bindings()
        .await?;
    if !exact_pipeline {
        // 仅记录本 fixture 的身份、类型化结果和少量调用特征；不采集系统提示词、
        // provider 凭据或 context token，以区分路由、压缩与实际回答调用。
        eprintln!(
            "A2_RECOVERY_DIAGNOSTIC:{}",
            serde_json::to_string(&serde_json::json!({
                "phase": phase,
                "model_calls": calls,
                "routing_calls": routing_calls,
                "answer_calls": answer_calls,
                "critique_calls": critique_calls,
                "unknown_calls": unknown_calls,
                "request_classes": request_classes,
                "inventory_complete": complete,
                "inputs": inputs.iter().map(|input| &input.receipt).collect::<Vec<_>>(),
                "request_tool_counts": boot.llm.all_tool_counts(),
                "request_formats": boot.llm.all_response_formats(),
                "request_messages": boot.llm.all_calls().iter().map(|messages| serde_json::json!({
                    "roles": messages.iter().map(|message| message.role.as_str()).collect::<Vec<_>>(),
                    "last_user_preview": messages.iter().rev()
                        .find(|message| message.role == echo_agent::llm::types::Role::User)
                        .and_then(|message| message.content.as_text())
                        .map(|text| text.chars().take(180).collect::<String>()),
                })).collect::<Vec<_>>(),
            }))?,
        );
    }
    anyhow::ensure!(
        exact_pipeline,
        "冷恢复总模型调用 {calls}，分类 {routing_calls}，回答 {answer_calls}，验证 {critique_calls}，未知 {unknown_calls}；三类各应为 {expected}且无未知请求"
    );
    let current = inputs
        .iter()
        .find(|input| {
            input
                .payload
                .channel_binding
                .as_ref()
                .is_some_and(|binding| {
                    binding.account_id == config.account_id
                        && binding.provider_message_id == provider_id
                })
        })
        .ok_or_else(|| anyhow::anyhow!("original input disappeared after cold recovery"))?;
    if expected == 1 || phase == "recover-completed" {
        anyhow::ensure!(
            current.receipt.phase == crate::conversation_input::ConversationInputPhase::TurnSettled
                && current.receipt.drained
                && current.receipt.attempt == Some(1)
                && current.receipt.outcome
                    == Some(crate::conversation_input::ConversationInputOutcome::Completed),
            "original completed input was not preserved: {:?}",
            current.receipt
        );
    }
    if phase == "recover-claimed" {
        anyhow::ensure!(
            current.receipt.attempt == Some(1)
                && current.receipt.phase
                    == crate::conversation_input::ConversationInputPhase::RecoveryRequired,
            "original confirmed claim disappeared or was mistaken for completion: {:?}",
            current.receipt
        );
    }
    if expected == 0 {
        for input in &inputs {
            if input
                .payload
                .channel_binding
                .as_ref()
                .is_some_and(|binding| binding.account_id == config.account_id)
            {
                anyhow::ensure!(
                    boot.state
                        .storage
                        .chat_events
                        .prove_unclaimed_channel_input(input.receipt.identity.clone())
                        .await
                        .is_err(),
                    "completed or previously claimed input became never-started after restart"
                );
            }
        }
    }
    let failures = boot.close().await?;
    if phase == "recover-isolated" {
        anyhow::ensure!(
            !complete
                && failures.len() == 2
                && failures.iter().all(|failure| failure
                    == "original channel stream discovery isolated uncertain history"),
            "isolation did not preserve its exact original error or hid extra failures: {failures:?}"
        );
    } else {
        anyhow::ensure!(
            complete && failures.is_empty(),
            "unexpected original task group failure: {failures:?}"
        );
    }
    emit(
        serde_json::json!({"phase":phase,"model_calls":calls,"routing_calls":routing_calls,"answer_calls":answer_calls,"critique_calls":critique_calls,"unknown_calls":unknown_calls,"request_classes":request_classes,"inventory_complete":complete,"original_group_failures":failures}),
    )?;
    Ok(())
}

async fn child(
    root: &Path,
    name: &str,
    phase: &str,
    expected_calls: usize,
    child_test_name: &str,
) -> anyhow::Result<(serde_json::Value, Observed)> {
    use base64::Engine;
    let preparing = matches!(phase, "persist-and-crash" | "claim-and-crash");
    let provider = LocalProvider::start(
        account(name),
        preparing.then(|| {
            (
                format!("provider-{name}"),
                "original durable input".to_string(),
            )
        }),
    )
    .await?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            child_test_name,
            "--nocapture",
            "--test-threads=1",
        ])
        .env("EKO_DATA_DIR", root.join("child-user-data"))
        .env("EKO_WEIXIN_A2_ROOT", root)
        .env("EKO_WEIXIN_A2_PHASE", phase)
        .env("EKO_WEIXIN_A2_ACCOUNT", name)
        .env("EKO_WEIXIN_A2_PROVIDER", provider.address.to_string())
        .env("EKO_WEIXIN_A2_EXPECT_CALLS", expected_calls.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut process = command.spawn()?;
    let pid = process.id();
    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("original child stdout missing"))?;
    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("original child stderr missing"))?;
    let read = |stream: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            stream.take(4 * 1024 * 1024).read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        })
    };
    let stdout_reader = read(Box::new(stdout));
    let stderr_reader = read(Box::new(stderr));
    let (status, timed_out, kill_error) =
        match tokio::time::timeout(Duration::from_secs(20), process.wait()).await {
            Ok(status) => (status, false, None),
            Err(_) => {
                let kill_error = process.start_kill().err().map(|error| error.to_string());
                // A fixture timeout is not proof that the process disappeared.
                // Wait on the same Child and join both original pipe readers.
                (process.wait().await, true, kill_error)
            }
        };
    // Join both readers and the server before propagating any wait/read error.
    let stdout = stdout_reader.await;
    let stderr = stderr_reader.await;
    let observed = provider.close().await;
    let status = status?;
    let stdout = stdout??;
    let stderr = stderr??;
    let observed = observed?;
    println!(
        "A2_CHILD_ORIGINAL:{}",
        serde_json::to_string(&serde_json::json!({
            "pid":pid,"phase":phase,"exit":status.code(),"timed_out":timed_out,"kill_error":kill_error,
            "stdout_base64":base64::engine::general_purpose::STANDARD.encode(&stdout),
            "stderr_base64":base64::engine::general_purpose::STANDARD.encode(&stderr),
        "real_http_polls":observed.polls,"real_http_send_requests":observed.send_requests,
        "real_http_accepted_sends":observed.sends.len(),"protocol_errors":observed.protocol_errors,
        "real_http_business_replies":observed.business_reply_count(),
        "accepted_message_texts":observed.sends.iter().filter_map(|request|
            request.pointer("/msg/item_list").and_then(serde_json::Value::as_array)
        ).flatten().filter_map(|item| item.pointer("/text_item/text").and_then(serde_json::Value::as_str)).collect::<Vec<_>>(),
        }))?
    );
    anyhow::ensure!(
        stdout.len() < 4 * 1024 * 1024 && stderr.len() < 4 * 1024 * 1024,
        "child output reached the retained raw limit"
    );
    anyhow::ensure!(
        !timed_out && status.success(),
        "original child {phase} failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    anyhow::ensure!(
        observed.protocol_errors == 0 && observed.send_requests == observed.sends.len(),
        "provider rejected an authentication/body mismatch or a send was not accepted"
    );
    let text = std::str::from_utf8(&stdout)?;
    let receipt = text
        .lines()
        .find_map(|line| line.strip_prefix("A2_CONSUMER_RECEIPT:"))
        .ok_or_else(|| anyhow::anyhow!("original child did not return its own observation"))?;
    Ok((serde_json::from_str(receipt)?, observed))
}

async fn clean_cold_case(child_test_name: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (_, seed) = child(root.path(), "A", "persist-and-crash", 0, child_test_name).await?;
    anyhow::ensure!(
        seed.polls > 0 && seed.sends.is_empty(),
        "first original host ran an effect"
    );
    let (_, recovered) = child(root.path(), "A", "recover", 1, child_test_name).await?;
    anyhow::ensure!(
        recovered.polls > 0 && recovered.business_reply_count() == 1,
        "real original sender did not reply once"
    );
    anyhow::ensure!(
        recovered
            .sends
            .first()
            .and_then(|request| request.pointer("/msg/to_user_id"))
            .and_then(serde_json::Value::as_str)
            == Some("scanner-A"),
        "recovery changed the original recipient"
    );
    let (_, again) = child(root.path(), "A", "recover-completed", 0, child_test_name).await?;
    anyhow::ensure!(
        again.sends.is_empty(),
        "completed input resent on the next cold process"
    );
    Ok(())
}

async fn repaired_twice_case(child_test_name: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    child(root.path(), "A", "persist-and-crash", 0, child_test_name).await?;
    let paths = segment_paths(&root.path().join("events"))?;
    anyhow::ensure!(paths.len() == 1, "unexpected original input stream layout");
    let path = paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("original active segment missing"))?;
    let confirmed_prefix = std::fs::read(path)?;
    leave_uncommitted_tail(path)?;
    child(root.path(), "A", "repair-and-crash", 0, child_test_name).await?;
    anyhow::ensure!(
        std::fs::read(path)? == confirmed_prefix,
        "repair changed the original confirmed prefix"
    );
    let (_, recovered) = child(root.path(), "A", "recover", 1, child_test_name).await?;
    anyhow::ensure!(
        recovered.business_reply_count() == 1,
        "post-repair second crash prevented legitimate recovery"
    );
    let (_, again) = child(root.path(), "A", "recover-completed", 0, child_test_name).await?;
    anyhow::ensure!(
        again.sends.is_empty(),
        "post-repair recovery resent after completion"
    );
    Ok(())
}

async fn claimed_tail_case(child_test_name: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    child(root.path(), "A", "claim-and-crash", 0, child_test_name).await?;
    let paths = segment_paths(&root.path().join("events"))?;
    anyhow::ensure!(paths.len() == 1, "unexpected claimed input stream layout");
    leave_uncommitted_tail(
        paths
            .first()
            .ok_or_else(|| anyhow::anyhow!("original claim segment missing"))?,
    )?;
    let (_, recovered) = child(root.path(), "A", "recover-claimed", 0, child_test_name).await?;
    anyhow::ensure!(
        recovered.sends.is_empty(),
        "confirmed claim was rerun or resent after a torn suffix"
    );
    Ok(())
}

async fn corrupt_isolation_case(child_test_name: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    child(root.path(), "A", "persist-and-crash", 0, child_test_name).await?;
    let good_paths = segment_paths(&root.path().join("events"))?;
    let (bad_seed, _) = child(root.path(), "B", "persist-and-crash", 0, child_test_name).await?;
    let bad_identity: ConversationInputIdentity = serde_json::from_value(
        bad_seed
            .get("identity")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("original bad input identity missing"))?,
    )?;
    let bad_paths = segment_paths(&root.path().join("events"))?
        .into_iter()
        .filter(|path| !good_paths.contains(path))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        bad_paths.len() == 1,
        "independent original bad stream missing"
    );
    let bad = bad_paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("bad segment missing"))?;
    leave_complete_corruption(bad)?;
    let corrupt = std::fs::read(bad)?;
    let (receipt, recovered) =
        child(root.path(), "A", "recover-isolated", 1, child_test_name).await?;
    anyhow::ensure!(
        receipt.get("inventory_complete") == Some(&serde_json::Value::Bool(false)),
        "bad stream became an absence proof"
    );
    anyhow::ensure!(
        recovered.business_reply_count() == 1,
        "independent safe stream failed real recovery"
    );
    anyhow::ensure!(
        recovered
            .sends
            .first()
            .and_then(|request| request.pointer("/msg/to_user_id"))
            .and_then(serde_json::Value::as_str)
            == Some("scanner-A"),
        "bad stream affected original reply ownership"
    );
    anyhow::ensure!(
        std::fs::read(bad)? == corrupt,
        "complete corruption was silently truncated"
    );
    let log = Arc::new(crate::chat_event_log::ChatEventLog::open(
        root.path().join("events"),
        Default::default(),
    )?);
    let rejected = log
        .prove_unclaimed_channel_input(bad_identity)
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("bad stream received a never-started proof"))?;
    println!(
        "A2_ORIGINAL_CORRUPTION_ERROR:{}",
        serde_json::to_string(&rejected.to_string())?
    );
    Ok(())
}

const CHILD_TEST: &str =
    "channel_runtime::management::weixin_auth::a2_consumer_tests::consumer_process_entry";

async fn control_original(
    channels: &ChannelRuntime,
    state: &AppState,
    action: ChannelControlAction,
) -> anyhow::Result<()> {
    let revision = state.config.configuration_snapshot().await.1;
    let epoch = match channels.handle("weixin").map_err(anyhow::Error::msg)? {
        Some(handle) => Some(
            handle
                .management_snapshot()
                .await
                .mutation_epoch
                .to_string(),
        ),
        None => None,
    };
    let applied = channels
        .configured_revisions
        .lock()
        .map_err(|_| anyhow::anyhow!("original applied revision unavailable"))?
        .get("weixin")
        .cloned();
    channels
        .control(ManagedChannel::Weixin, revision, epoch, applied, action)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

async fn original_channel_idle(coordinator: &ChannelSessionCoordinator) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !coordinator
                .has_pending_service_work()
                .map_err(anyhow::Error::msg)?
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

async fn input_projection(
    state: &AppState,
    provider_id: &str,
) -> anyhow::Result<ConversationInputProjection> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (inputs, complete) = state.storage.chat_events.channel_input_bindings().await?;
            anyhow::ensure!(complete, "original journal inventory incomplete");
            if let Some(input) = inputs.into_iter().find(|input| {
                input
                    .payload
                    .channel_binding
                    .as_ref()
                    .is_some_and(|binding| binding.provider_message_id == provider_id)
            }) {
                return Ok::<_, anyhow::Error>(input);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

async fn completed_network_input(
    state: &AppState,
    provider: &LocalProvider,
    provider_id: &str,
    expected_sends: usize,
) -> anyhow::Result<ConversationInputProjection> {
    use crate::conversation_input::{ConversationInputOutcome, ConversationInputPhase};
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let input = input_projection(state, provider_id).await?;
            if input.receipt.phase == ConversationInputPhase::TurnSettled
                && input.receipt.outcome == Some(ConversationInputOutcome::Completed)
                && input.receipt.drained
                && provider.observed.borrow().business_reply_count() == expected_sends
            {
                let runtime = state
                    .chat_runtime_for_scope(&input.receipt.identity.address.workspace_id)
                    .await?;
                let tasks = runtime
                    .task_runtime()
                    .ok_or_else(|| anyhow::anyhow!("original scoped task runtime missing"))?;
                let target = input.task_target.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("completed input lacks original TaskRun target")
                })?;
                let run = tasks
                    .get_run(&target.run_id)?
                    .ok_or_else(|| anyhow::anyhow!("original TaskRun missing"))?;
                if run.status == crate::tasks::task_runtime::types::TaskRunStatus::Completed
                    && !tasks.is_run_active(&target.run_id)
                {
                    return Ok::<_, anyhow::Error>(input);
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

async fn managed_reconnect_case() -> anyhow::Result<()> {
    use crate::conversation_input::{
        ChannelExecutionIntent, ChannelInputBinding, ConversationInputFact,
        ConversationInputOutcome, ConversationInputPhase, ConversationInputSource,
        FrozenChannelContext, stable_scoped_input_id,
    };
    let temporary = tempfile::tempdir()?;
    let suffix = temporary
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| anyhow::anyhow!("fixture root has no name"))?;
    let account = account(suffix);
    let pages = Arc::new(Pages {
        admitted_pages: std::sync::atomic::AtomicUsize::new(1),
        messages: vec![
            ("managed-first".into(), "original durable input".into()),
            ("managed-first".into(), "original durable input".into()),
            ("managed-second".into(), "original durable input".into()),
            (
                "managed-workspace-b".into(),
                "original durable input".into(),
            ),
            ("managed-third".into(), "original durable input".into()),
            (
                "managed-abandoned".into(),
                "historical unknown input".into(),
            ),
        ],
    });
    let provider =
        LocalProvider::start_with_pages(account.clone(), None, Some(pages.clone())).await?;
    let responses = (0..4).flat_map(|_| [
        r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#,
        BUSINESS_REPLY,
        r#"{"score":9.0,"passed":true,"feedback":"Original durable reply verified","suggestions":[]}"#,
    ]);
    let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_responses(responses));
    let (mut lifecycle, state) = application(temporary.path(), llm.clone()).await?;
    let mut runtime = ChannelRuntime::from_state(Arc::downgrade(&state));
    runtime.weixin_fixture_address = Some(provider.address);
    let channels = Arc::new(runtime);
    lifecycle.bind_channel_runtime(channels.clone());
    let mut observation_step = "configure";
    let observed = async {
        let revision = state.config.configuration_snapshot().await.1;
        let saved_account = account.clone();
        state.update_channels_config_owned(revision, move |config| {
            config.weixin.enabled = true;
            config.weixin.account = Some(saved_account);
            Ok(())
        }).await?;
        observation_step = "start";
        control_original(&channels, &state, ChannelControlAction::Start).await?;
        observation_step = "first-input";
        let first = completed_network_input(&state, &provider, "managed-first", 1).await?;
        let coordinator = channels.coordinators.lock()
            .map_err(|_| anyhow::anyhow!("original coordinator registry unavailable"))?
            .get("weixin").and_then(|owners| owners.last()).cloned()
            .ok_or_else(|| anyhow::anyhow!("original coordinator missing"))?;
        let original_incarnation = coordinator.find_session_wake("weixin", &first.receipt.identity.address.conversation_id)
            .and_then(|(_, wake)| wake.wiring.session_instance.as_ref()
                .map(echo_agent::channels::ChannelSessionInstance::incarnation_id))
            .ok_or_else(|| anyhow::anyhow!("original incarnation missing"))?;
        observation_step = "stop-after-first";
        control_original(&channels, &state, ChannelControlAction::Stop).await?;
        anyhow::ensure!(coordinator.find_session_wake("weixin", &first.receipt.identity.address.conversation_id).is_none(),
            "Stop did not run the production session-end callback");
        // 同一 manager/coordinator，原 cursor 继续读到首条重复投递和第二条新消息。
        pages.admitted_pages.store(3, Ordering::SeqCst);
        control_original(&channels, &state, ChannelControlAction::Reconnect).await?;
        observation_step = "second-input";
        let second = completed_network_input(&state, &provider, "managed-second", 2).await?;
        let same_coordinator = channels.coordinators.lock()
            .map_err(|_| anyhow::anyhow!("original coordinator registry unavailable"))?
            .get("weixin").and_then(|owners| owners.last()).cloned()
            .ok_or_else(|| anyhow::anyhow!("reconnected coordinator missing"))?;
        anyhow::ensure!(Arc::ptr_eq(&coordinator, &same_coordinator), "reconnect replaced original coordinator");
        let reopened_incarnation = coordinator.find_session_wake("weixin", &second.receipt.identity.address.conversation_id)
            .and_then(|(_, wake)| wake.wiring.session_instance.as_ref()
                .map(echo_agent::channels::ChannelSessionInstance::incarnation_id))
            .ok_or_else(|| anyhow::anyhow!("reconnected incarnation missing"))?;
        anyhow::ensure!(original_incarnation != reopened_incarnation, "old incarnation reused after Stop");
        control_original(&channels, &state, ChannelControlAction::Reconnect).await?;
        original_channel_idle(&coordinator).await?;
        observation_step = "seed-original-unknown";
        let service = state.conversation_inputs();
        let address = second.receipt.identity.address.clone();
        let source = serde_json::to_string(&("weixin", &account.account_id, &account.generation,
            &account.user_id, "managed-abandoned"))?;
        let saved = service.submit_channel(address.clone(), stable_scoped_input_id(&address,
            ConversationInputSource::Channel, &source)?, "historical unknown input".into(), Vec::new(), None,
            ChannelInputBinding {
                version: 1, channel_id: "weixin".into(), account_id: account.account_id.clone(),
                credential_generation: account.generation.clone(), peer_id: account.user_id.clone(),
                provider_message_id: "managed-abandoned".into(), workspace_generation: "global".into(),
                context: FrozenChannelContext::Absent, execution: ChannelExecutionIntent::Chat,
            }).await?;
        let claimed = service.dispatch_selected(saved.identity.clone(), saved.queue_revision, "historical-managed-turn".into()).await?;
        let attempt = claimed.active_attempt.ok_or_else(|| anyhow::anyhow!("historical attempt missing"))?;
        let tasks = state.tasks.runtime.as_ref()
            .ok_or_else(|| anyhow::anyhow!("original task runtime missing"))?;
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("historical-managed-turn");
        tasks.create_run(&run_id, &address.workspace_id, &address.conversation_id, "historical-managed-turn",
            crate::tasks::task_runtime::types::DomainProfile::General, "historical unknown input", "",
            crate::tasks::task_runtime::types::AttendedMode::Attended)?;
        tasks.transition_run(&run_id, crate::tasks::task_runtime::types::TaskRunStatus::Running)?;
        tasks.transition_run(&run_id, crate::tasks::task_runtime::types::TaskRunStatus::Failed)?;
        service.bind_task_target(attempt.clone(), run_id.clone()).await?;
        let original_run = serde_json::to_value(tasks.get_run(&run_id)?)?;
        service.mailbox_accepted(attempt.clone()).await?;
        let failed = service.turn_settled(attempt.clone(), ConversationInputOutcome::Failed, false).await?;
        // 复用重启路径的identity-only uncertainty，不能把已settled attempt再写成运行中恢复。
        state.storage.chat_events.mark_channel_input_uncertain_checked(saved.identity.clone(),
            Some(failed.queue_revision), "historical side effect UNKNOWN".into()).await?;
        let before = state.storage.chat_events.replay(&address.workspace_id,
            Some(&address.conversation_id), &saved.identity.input_id, 0)?;
        // A保持UNKNOWN；同peer的B由同一原pump按其已保存身份独立推进。
        let workspace_b = state.workspace.registry.create_at("fixture-b",
            crate::workspace::WorkspaceKind::General, temporary.path().join("workspace-b"))?;
        let mut address_b = address.clone();
        address_b.workspace_id = workspace_b.id.to_string();
        let b_source = serde_json::to_string(&("weixin", &account.account_id, &account.generation,
            &account.user_id, "managed-workspace-b"))?;
        let saved_b = service.submit_channel(address_b.clone(), stable_scoped_input_id(&address_b,
            ConversationInputSource::Channel, &b_source)?, "original durable input".into(), Vec::new(), None,
            ChannelInputBinding {
                version: 1, channel_id: "weixin".into(), account_id: account.account_id.clone(),
                credential_generation: account.generation.clone(), peer_id: account.user_id.clone(),
                provider_message_id: "managed-workspace-b".into(), workspace_generation: workspace_b.opaque_product_data_generation(),
                context: FrozenChannelContext::Absent, execution: ChannelExecutionIntent::Chat,
            }).await?;
        // 原poll处理该消息的Absent关联，再以准确已保存binding唤醒原pump，不能手造sender凭据。
        pages.admitted_pages.store(4, Ordering::SeqCst);
        observation_step = "workspace-b-input";
        let independent = completed_network_input(&state, &provider, "managed-workspace-b", 3).await?;
        original_channel_idle(&coordinator).await?;
        anyhow::ensure!(independent.receipt.identity == saved_b.identity, "B execution changed its original DATA");
        let b_runtime = state.chat_runtime_for_scope(workspace_b.id.as_str()).await?;
        let b_tasks = b_runtime.task_runtime().ok_or_else(|| anyhow::anyhow!("original B task runtime missing"))?;
        let b_target = independent.task_target.as_ref().ok_or_else(|| anyhow::anyhow!("B target missing"))?;
        let b_run = b_tasks.get_run(&b_target.run_id)?.ok_or_else(|| anyhow::anyhow!("B run missing"))?;
        anyhow::ensure!(b_run.workspace_id == workspace_b.id.as_str(), "B driver ran in default A workspace");
        let after_b = state.storage.chat_events.replay(&address.workspace_id,
            Some(&address.conversation_id), &saved.identity.input_id, 0)?;
        anyhow::ensure!(serde_json::to_value(&after_b.events)? == serde_json::to_value(&before.events)?,
            "B progression rewrote A UNKNOWN history");
        let blocked_a = service.list(&address).await?;
        anyhow::ensure!(blocked_a.items.iter().any(|input| input.receipt.identity == saved.identity
            && input.receipt.phase == ConversationInputPhase::RecoveryRequired), "B progression cleared A blocker");
        // 同一默认A的真实transport/pump已按B保存的binding执行过B；
        // 其合法历史UNKNOWN也必须按原B身份处置，不以默认A或GUI焦点替换B。
        observation_step = "historical-workspace-b-abandon";
        let historical_b_provider = "managed-historical-workspace-b";
        let historical_b_source = serde_json::to_string(&("weixin", &account.account_id, &account.generation,
            &account.user_id, historical_b_provider))?;
        let historical_b = service.submit_channel(address_b.clone(), stable_scoped_input_id(&address_b,
            ConversationInputSource::Channel, &historical_b_source)?, "historical B effect UNKNOWN".into(), Vec::new(), None,
            ChannelInputBinding {
                version: 1, channel_id: "weixin".into(), account_id: account.account_id.clone(),
                credential_generation: account.generation.clone(), peer_id: account.user_id.clone(),
                provider_message_id: historical_b_provider.into(), workspace_generation: workspace_b.opaque_product_data_generation(),
                context: FrozenChannelContext::Absent, execution: ChannelExecutionIntent::Chat,
            }).await?;
        let historical_b_turn = "historical-managed-workspace-b-turn";
        let started_b = service.dispatch_selected(historical_b.identity.clone(), historical_b.queue_revision,
            historical_b_turn.into()).await?;
        let b_attempt = started_b.active_attempt.ok_or_else(|| anyhow::anyhow!("B original attempt missing"))?;
        let historical_b_run = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(historical_b_turn);
        b_tasks.create_run(&historical_b_run, &address_b.workspace_id, &address_b.conversation_id, historical_b_turn,
            crate::tasks::task_runtime::types::DomainProfile::General, "historical B effect UNKNOWN", "",
            crate::tasks::task_runtime::types::AttendedMode::Attended)?;
        b_tasks.transition_run(&historical_b_run, crate::tasks::task_runtime::types::TaskRunStatus::Running)?;
        b_tasks.transition_run(&historical_b_run, crate::tasks::task_runtime::types::TaskRunStatus::Failed)?;
        service.bind_task_target(b_attempt.clone(), historical_b_run.clone()).await?;
        service.mailbox_accepted(b_attempt.clone()).await?;
        let failed_b = service.turn_settled(b_attempt, ConversationInputOutcome::Failed, false).await?;
        state.storage.chat_events.mark_channel_input_uncertain_checked(historical_b.identity.clone(),
            Some(failed_b.queue_revision), "B original side effect UNKNOWN".into()).await?;
        let b_original_run = serde_json::to_value(b_tasks.get_run(&historical_b_run)?)?;
        let b_active = state.session.foreground_turns.begin_scoped(&address_b.workspace_id,
            crate::foreground_turn::ForegroundTurnSurface::Gui, &address_b.conversation_id, historical_b_turn)?;
        let b_revision = service.list(&address_b).await?.queue_revision;
        let active_b_refused = channels.abandon_recovery_input(historical_b.identity.clone(), b_revision).await;
        b_active.settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled).await?;
        // 两个无可用原scope的历史输入仍必须保留，不产生claim或取消。
        let mut invalid_scope_results = Vec::new();
        for (scope, generation, provider_id) in [
            (address_b.workspace_id.as_str(), "stale-B-generation", "managed-stale-workspace-b"),
            ("missing-original-workspace", "missing-original-generation", "managed-missing-workspace"),
        ] {
            let invalid_address = crate::conversation_input::ConversationInputAddress {
                workspace_id: scope.to_string(), conversation_id: address.conversation_id.clone(),
            };
            let invalid_source = serde_json::to_string(&("weixin", &account.account_id, &account.generation,
                &account.user_id, provider_id))?;
            let invalid = service.submit_channel(invalid_address.clone(), stable_scoped_input_id(&invalid_address,
                ConversationInputSource::Channel, &invalid_source)?, "historical unavailable workspace".into(), Vec::new(), None,
                ChannelInputBinding {
                    version: 1, channel_id: "weixin".into(), account_id: account.account_id.clone(),
                    credential_generation: account.generation.clone(), peer_id: account.user_id.clone(),
                    provider_message_id: provider_id.into(), workspace_generation: generation.into(),
                    context: FrozenChannelContext::Absent, execution: ChannelExecutionIntent::Chat,
                }).await?;
            state.storage.chat_events.mark_channel_input_uncertain_checked(invalid.identity.clone(),
                Some(invalid.queue_revision), "original workspace unavailable".into()).await?;
            let before_invalid = service.list(&invalid_address).await?;
            invalid_scope_results.push(channels.abandon_recovery_input(invalid.identity.clone(), before_invalid.queue_revision).await);
            let after_invalid = service.list(&invalid_address).await?;
            anyhow::ensure!(after_invalid.queue_revision == before_invalid.queue_revision
                && after_invalid.items.iter().any(|input| input.receipt.identity == invalid.identity
                    && input.receipt.phase == ConversationInputPhase::RecoveryRequired && input.receipt.attempt.is_none()),
                "invalid original workspace action mutated or claimed the historical input");
        }
        let b_before_cancel = state.storage.chat_events.replay(&address_b.workspace_id,
            Some(&address_b.conversation_id), &historical_b.identity.input_id, 0)?;
        let a_before_b_cancel = state.storage.chat_events.replay(&address.workspace_id,
            Some(&address.conversation_id), &saved.identity.input_id, 0)?;
        let current_b_revision = service.list(&address_b).await?.queue_revision;
        let b_cancelled_value = channels.abandon_recovery_input(historical_b.identity.clone(), current_b_revision)
            .await.map_err(|error| anyhow::anyhow!("original historical workspace-B abandonment: {error}"))?;
        let b_cancelled: crate::conversation_input::ConversationInputReceipt = serde_json::from_value(
            b_cancelled_value.get("receipt").cloned().ok_or_else(|| anyhow::anyhow!("original B cancel receipt missing"))?)?;
        anyhow::ensure!(b_cancelled.identity == historical_b.identity && b_cancelled.phase == ConversationInputPhase::Cancelled
            && b_cancelled.attempt == Some(1) && !b_cancelled.drained
            && b_cancelled.reason.as_deref() == Some("B original side effect UNKNOWN"), "B original identity/outcome was rewritten");
        anyhow::ensure!(matches!(active_b_refused, Err(error) if error.contains("active foreground turn")),
            "B active foreground owner was not protected by its own original scope");
        for refused in invalid_scope_results {
            anyhow::ensure!(matches!(refused, Err(error) if error.contains("原工作空间代次已变化")),
                "missing/stale original workspace did not retain its exact generation refusal");
        }
        let b_after_cancel = state.storage.chat_events.replay(&address_b.workspace_id,
            Some(&address_b.conversation_id), &historical_b.identity.input_id, 0)?;
        anyhow::ensure!(b_after_cancel.events.len() == b_before_cancel.events.len().saturating_add(1),
            "B explicit cancel appended facts beyond its one Cancelled");
        for (old, new) in b_before_cancel.events.iter().zip(&b_after_cancel.events) {
            anyhow::ensure!(serde_json::to_value(old)? == serde_json::to_value(new)?, "B original raw prefix changed");
        }
        anyhow::ensure!(serde_json::to_value(b_tasks.get_run(&historical_b_run)?)? == b_original_run,
            "B explicit cancel rewrote original Failed TaskRun");
        let a_after_b_cancel = state.storage.chat_events.replay(&address.workspace_id,
            Some(&address.conversation_id), &saved.identity.input_id, 0)?;
        anyhow::ensure!(serde_json::to_value(a_before_b_cancel.events)? == serde_json::to_value(a_after_b_cancel.events)?,
            "B explicit cancel changed A UNKNOWN stream");
        anyhow::ensure!(channels.weixin_workspace.lock().map_err(|_| anyhow::anyhow!("original default workspace unavailable"))?
            .as_deref() == Some("global"), "explicit B cancel retargeted transport default A");
        drop(b_run);
        drop(b_tasks);
        drop(b_runtime);
        pages.admitted_pages.store(5, Ordering::SeqCst);
        observation_step = "third-persisted";
        let pending = input_projection(&state, "managed-third").await?;
        anyhow::ensure!(pending.receipt.attempt.is_none(), "new message claimed through original unknown blocker");
        let frontier = service.list(&address).await?;
        anyhow::ensure!(frontier.items.iter().any(|input| input.receipt.identity == saved.identity
            && input.receipt.phase == ConversationInputPhase::RecoveryRequired), "original recovery blocker disappeared");
        anyhow::ensure!(llm.call_count() == 9 && provider.observed.borrow().business_reply_count() == 3,
            "UNKNOWN blocker caused a model call or automatic reply");
        observation_step = "abandon-original";
        channels.abandon_recovery_input(saved.identity.clone(), frontier.queue_revision)
            .await.map_err(anyhow::Error::msg)?;
        observation_step = "third-completed";
        let third = completed_network_input(&state, &provider, "managed-third", 4).await?;
        pages.admitted_pages.store(6, Ordering::SeqCst);
        observation_step = "duplicate-cursor";
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut observed = provider.observed.clone();
            loop {
                if observed.borrow_and_update().last_cursor == "fixture-page-6" { break; }
                observed.changed().await?;
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        control_original(&channels, &state, ChannelControlAction::Stop).await?;
        observation_step = "final-facts";
        let (inputs, complete) = state.storage.chat_events.channel_input_bindings().await?;
        anyhow::ensure!(complete && inputs.len() == 8, "duplicate provider delivery created another DATA identity");
        let abandoned = inputs.iter().find(|input| input.receipt.identity == saved.identity)
            .ok_or_else(|| anyhow::anyhow!("original abandoned input missing"))?;
        anyhow::ensure!(abandoned.receipt.phase == ConversationInputPhase::Cancelled
            && abandoned.receipt.attempt == Some(1) && !abandoned.receipt.drained
            && abandoned.receipt.outcome == Some(ConversationInputOutcome::Cancelled)
            && abandoned.receipt.reason.as_deref() == Some("historical side effect UNKNOWN"),
            "explicit abandonment rewrote original Failed/UNKNOWN facts");
        anyhow::ensure!(serde_json::to_value(tasks.get_run(&run_id)?)? == original_run,
            "explicit abandonment changed the original Failed TaskRun");
        let after = state.storage.chat_events.replay(&address.workspace_id,
            Some(&address.conversation_id), &saved.identity.input_id, 0)?;
        // replay返回整个conversation流；每个输入的事实必须按完整identity选择。
        let original_fact = |event: &&crate::chat_event_log::ChatEventEnvelope| matches!(&event.payload,
            crate::chat_driver::ChatDriverEvent::InputLifecycle(fact) if fact.identity() == &saved.identity);
        let before_original = before.events.iter().filter(original_fact).count();
        let after_original: Vec<_> = after.events.iter().filter(original_fact).collect();
        anyhow::ensure!(after_original.len() == before_original.saturating_add(1),
            "abandonment added facts beyond the one Cancelled for the original identity");
        for (original, current) in before.events.iter().zip(&after.events) {
            anyhow::ensure!(serde_json::to_value(original)? == serde_json::to_value(current)?,
                "original raw history changed");
        }
        anyhow::ensure!(after_original.last().is_some_and(|event| matches!(&event.payload,
            crate::chat_driver::ChatDriverEvent::InputLifecycle(fact)
            if matches!(fact.as_ref(), ConversationInputFact::Cancelled { identity, .. } if identity == &saved.identity))),
            "original final fact is not exact Cancelled");
        let (original_cancellations, original_claims, original_drains) = after_original.iter().fold(
            (0_usize, 0_usize, 0_usize), |(cancelled, claimed, drained), event| match &event.payload {
                crate::chat_driver::ChatDriverEvent::InputLifecycle(fact) => match fact.as_ref() {
                    ConversationInputFact::Cancelled {..} => (cancelled.saturating_add(1), claimed, drained),
                    ConversationInputFact::AttemptStarted {..} => (cancelled, claimed.saturating_add(1), drained),
                    ConversationInputFact::Drained {..} => (cancelled, claimed, drained.saturating_add(1)),
                    _ => (cancelled, claimed, drained),
                },
                _ => (cancelled, claimed, drained),
            });
        anyhow::ensure!((original_cancellations, original_claims, original_drains) == (1, 1, 0),
            "original UNKNOWN input gained another cancellation/claim/drain");
        for expected in [&first, &second, &independent, &third] {
            let replay = state.storage.chat_events.replay(&expected.receipt.identity.address.workspace_id,
                Some(&expected.receipt.identity.address.conversation_id), &expected.receipt.identity.input_id, 0)?;
            let (persisted, claimed) = replay.events.iter().fold((0_usize, 0_usize), |(persisted, claimed), event| {
                match &event.payload {
                    crate::chat_driver::ChatDriverEvent::InputLifecycle(fact) if fact.identity() == &expected.receipt.identity => match fact.as_ref() {
                        ConversationInputFact::Persisted {..} => (persisted.saturating_add(1), claimed),
                        ConversationInputFact::AttemptStarted {..} => (persisted, claimed.saturating_add(1)),
                        _ => (persisted, claimed),
                    },
                    _ => (persisted, claimed),
                }
            });
            anyhow::ensure!(persisted == 1 && claimed == 1 && expected.receipt.attempt == Some(1),
                "network input did not retain its single DATA and claim");
            let client_prefix = format!("eko-input:{}:", expected.receipt.identity.input_id);
            let accepted_replies = provider.observed.borrow().sends.iter().filter(|request| {
                request.pointer("/msg/client_id").and_then(Value::as_str)
                    .is_some_and(|client_id| client_id.starts_with(&client_prefix))
                    && request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str) == Some(BUSINESS_REPLY)
            }).count();
            anyhow::ensure!(accepted_replies == 1,
                "original provider did not accept exactly one reply for this durable input");
        }
        anyhow::ensure!(llm.call_count() == 12, "reconnect or duplicate replay called the model again");
        Ok::<_, anyhow::Error>(())
    }.await;
    if observed.is_err() {
        // 只观察本夹具的实际请求/事实，不输出prompt、凭据或原用户数据；先观察再原清理。
        let calls = llm.all_calls();
        let tools = llm.all_tool_counts();
        let formats = llm.all_response_formats();
        let request_types: Vec<_> = calls.iter().enumerate().take(32).map(|(index, messages)| {
            let tool_count = tools.get(index).copied();
            let format = formats.get(index).and_then(Option::as_ref);
            if matches!(format, Some(echo_agent::llm::ResponseFormat::JsonSchema {json_schema}) if json_schema.name == "critique_output") {
                "critic"
            } else if tool_count == Some(0) && format.is_none() && messages.first().is_some_and(|message| {
                message.role == echo_agent::llm::types::Role::User && message.text_content().is_some_and(|text|
                    text.starts_with("You are an intent classifier for an AI agent system."))
            }) {
                "routing"
            } else if tool_count.is_some_and(|count| count > 0) && format.is_none() {
                "answer"
            } else { "other" }
        }).collect();
        let input_snapshot = match state.storage.chat_events.channel_input_bindings().await {
            Ok((inputs, complete)) => serde_json::json!({
                "inventory_complete": complete, "input_count": inputs.len(),
                "inputs": inputs.iter().take(8).map(|input| {
                    let receipt = &input.receipt;
                    let run = if receipt.identity.address.workspace_id == "global" {
                        match (state.tasks.runtime.as_ref(), input.task_target.as_ref()) {
                            (Some(store), Some(target)) => match store.get_run(&target.run_id) {
                                Ok(Some(run)) => serde_json::json!({"status":run.status,"active":store.is_run_active(&target.run_id)}),
                                Ok(None) => serde_json::json!({"state":"missing"}),
                                Err(_) => serde_json::json!({"state":"read_error"}),
                            },
                            _ => serde_json::json!({"state":"unbound"}),
                        }
                    } else { serde_json::json!({"state":"not_observed_in_global_store"}) };
                    serde_json::json!({"identity":receipt.identity,"phase":receipt.phase,"outcome":receipt.outcome,
                        "attempt":receipt.attempt,"drained":receipt.drained,"run":run,
                        "reason_preview":receipt.reason.as_deref().map(|reason| reason.chars().take(160).collect::<String>())})
                }).collect::<Vec<_>>()
            }),
            Err(_) => serde_json::json!({"inspection":"unavailable"}),
        };
        let actual_provider = provider.observed.borrow().clone();
        let accepted: Vec<_> = actual_provider.sends.iter().take(16).map(|request| {
            let text = request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str).unwrap_or_default();
            let class = if text == BUSINESS_REPLY { "business" } else if serde_json::from_str::<Value>(text).ok().is_some_and(|v| v.get("intent").is_some()) {
                "routing_json"
            } else if serde_json::from_str::<Value>(text).ok().is_some_and(|v| v.get("score").is_some()) { "critic_json" } else { "other" };
            serde_json::json!({"class":class,"chars":text.chars().count(),"client_id":request.pointer("/msg/client_id").and_then(Value::as_str)
                .map(|value|value.chars().take(96).collect::<String>())})
        }).collect();
        eprintln!(
            "A2_MANAGED_DIAGNOSTIC:{}",
            serde_json::json!({
                "observation_step":observation_step,"model_calls":llm.call_count(),"request_types":request_types,
                "tool_counts":tools.into_iter().take(32).collect::<Vec<_>>(),"inputs":input_snapshot,
                "inner_provider":{"polls":actual_provider.polls,"send_requests":actual_provider.send_requests,
                    "accepted":actual_provider.sends.len(),"business_replies":actual_provider.business_reply_count(),
                    "protocol_errors":actual_provider.protocol_errors,"cursor":actual_provider.last_cursor,"accepted_classes":accepted}
            })
        );
    }
    // 包括断言失败也走原生命周期清理，避免 HTTP 或消息泵泄漏到下一用例。
    let settled = lifecycle
        .settle(crate::runtime::ApplicationLifecycleReason::Shutdown, None)
        .await
        .wait_final()
        .await;
    let provider_result = provider.close().await;
    observed?;
    anyhow::ensure!(
        settled.is_clean(),
        "original application owner not settled: {settled:?}"
    );
    let provider_result = provider_result?;
    anyhow::ensure!(
        provider_result.protocol_errors == 0
            && provider_result.send_requests == provider_result.sends.len()
            && provider_result.business_reply_count() == 4,
        "original provider did not accept exactly four business replies"
    );
    emit(serde_json::json!({
        "phase": "managed-reconnect",
        "model_calls": llm.call_count(),
        "real_http_business_replies": provider_result.business_reply_count(),
        "real_http_send_requests": provider_result.send_requests,
        "protocol_errors": provider_result.protocol_errors,
    }))?;
    Ok(())
}

#[tokio::test]
async fn original_manager_reconnect_and_exact_abandon_drive_network_inputs_once()
-> anyhow::Result<()> {
    // 复用已有子进程入口，在OnceLock首次读取前绑定绝对EKO_DATA_DIR，
    // 真实construct_plugin仍使用原protocol Store路径，不触碰父进程或用户数据。
    let temporary = tempfile::tempdir()?;
    let (receipt, parent_provider) = child(
        temporary.path(),
        "managed",
        "managed-reconnect",
        12,
        CHILD_TEST,
    )
    .await?;
    anyhow::ensure!(
        receipt.get("model_calls").and_then(Value::as_u64) == Some(12)
            && receipt
                .get("real_http_business_replies")
                .and_then(Value::as_u64)
                == Some(4)
            && receipt.get("protocol_errors").and_then(Value::as_u64) == Some(0),
        "original isolated control-path child did not verify the four accepted replies"
    );
    anyhow::ensure!(
        parent_provider.send_requests == 0,
        "managed child used the unrelated parent provider instead of its explicit endpoint"
    );
    Ok(())
}

async fn managed_long_stop_case() -> anyhow::Result<()> {
    use crate::conversation_input::{ConversationInputFact, FrozenChannelContext};
    use echo_agent::channels::{ChannelError, ChatType, OutboundMessage, ReactError};
    use tracing_subscriber::layer::SubscriberExt;
    let accepted_observations = ReplyReceiptCapture::default();
    // This phase runs only in the existing isolated single-test child process.
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(accepted_observations.clone()),
    )?;
    let temporary = tempfile::tempdir()?;
    let account = account("managed-long-stop");
    let pages = Arc::new(Pages {
        admitted_pages: std::sync::atomic::AtomicUsize::new(1),
        messages: vec![
            ("managed-long-first".into(), "original long reply".into()),
            ("managed-long-next".into(), "new incarnation reply".into()),
        ],
    });
    let (request, mut captured) = tokio::sync::watch::channel(None);
    let gate = Arc::new(HeldSend {
        request,
        release: tokio::sync::Notify::new(),
    });
    let provider = LocalProvider::start_with_send_gate(
        account.clone(),
        None,
        Some(pages.clone()),
        Some(gate.clone()),
    )
    .await?;
    let long_reply = format!("{LONG_REPLY_PREFIX}{}", "原会话长输出🙂".repeat(350));
    let chunks = crate::channel_runtime::channel_outbound_chunks(
        "weixin",
        &crate::channel_runtime::ChannelOutboundDraft::ordinary(long_reply.clone()),
    );
    anyhow::ensure!(
        chunks.len() >= 3,
        "fixture answer did not cross original UTF-8 chunk boundaries"
    );
    let routing = r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#;
    let critique =
        r#"{"score":9.0,"passed":true,"feedback":"Original long reply verified","suggestions":[]}"#;
    let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_responses([
        routing.to_string(),
        long_reply,
        critique.to_string(),
        routing.to_string(),
        BUSINESS_REPLY.to_string(),
        critique.to_string(),
    ]));
    let (mut lifecycle, state) = application(temporary.path(), llm.clone()).await?;
    let mut runtime = ChannelRuntime::from_state(Arc::downgrade(&state));
    runtime.weixin_fixture_address = Some(provider.address);
    let channels = Arc::new(runtime);
    lifecycle.bind_channel_runtime(channels.clone());
    let mut stop_owner = None;
    let mut observation_step = "configure";
    let observed = async {
        let revision = state.config.configuration_snapshot().await.1;
        let saved_account = account.clone();
        state.update_channels_config_owned(revision, move |config| {
            config.weixin.enabled = true;
            config.weixin.account = Some(saved_account);
            Ok(())
        }).await?;
        control_original(&channels, &state, ChannelControlAction::Start).await?;
        observation_step = "original-long-http";
        let held_request = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(request) = captured.borrow().clone() {
                    return Ok::<_, anyhow::Error>(request);
                }
                captured.changed().await?;
            }
        }).await??;
        let first = input_projection(&state, "managed-long-first").await?;
        let coordinator = channels.coordinators.lock()
            .map_err(|_| anyhow::anyhow!("original coordinator registry unavailable"))?
            .get("weixin").and_then(|owners| owners.last()).cloned()
            .ok_or_else(|| anyhow::anyhow!("original coordinator missing"))?;
        let (_, old_wake) = coordinator.find_session_wake("weixin", &first.receipt.identity.address.conversation_id)
            .ok_or_else(|| anyhow::anyhow!("original wake missing"))?;
        let old_instance = old_wake.wiring.session_instance.clone()
            .ok_or_else(|| anyhow::anyhow!("original incarnation missing"))?;
        let old_incarnation = old_instance.incarnation_id();
        let target = echo_agent::channels::weixin::encode_target(&account.account_id, &account.user_id)?;
        // 只保存真实旧instance的模板；它不持有delivery permit，不能替代物理结算。
        let late_template = old_instance.bind_outbound(
            OutboundMessage::new("weixin", &target, ChatType::Direct, "ORIGINAL_LATE_FENCE")
                .with_reply_to("managed-long-first"),
        );
        let old_prefix = format!("eko-input:{}:", first.receipt.identity.input_id);
        anyhow::ensure!(held_request.pointer("/msg/client_id").and_then(Value::as_str)
            .is_some_and(|value| value.starts_with(&old_prefix)), "held HTTP lost original input client identity");
        anyhow::ensure!(held_request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str)
            == chunks.first().map(String::as_str), "held request was not the original first long chunk");
        anyhow::ensure!(provider.observed.borrow().sends.iter().all(|request|
            request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str)
                != chunks.first().map(String::as_str)), "unanswered HTTP was already called Accepted");
        observation_step = "accepted-before-http-receipt";
        anyhow::ensure!(accepted_observations.snapshot()?.is_empty(), "queued/unanswered send was falsely recorded Accepted");
        observation_step = "stop-held-http";
        let stopping_channels = channels.clone();
        let stopping_state = state.clone();
        stop_owner = Some(tokio::spawn(async move {
            control_original(&stopping_channels, &stopping_state, ChannelControlAction::Stop).await
        }));
        // 原transport取消已发生，真实effectful HTTP仍等待原provider回执。
        tokio::time::timeout(Duration::from_secs(2), async {
            while !old_wake.wiring.service_cancel.is_cancelled() {
                tokio::task::yield_now().await;
            }
        }).await?;
        anyhow::ensure!(stop_owner.as_ref().is_some_and(|task| !task.is_finished()),
            "public Stop returned before its original HTTP sender physically settled");
        anyhow::ensure!(coordinator.find_session_wake("weixin", &first.receipt.identity.address.conversation_id)
            .and_then(|(_, wake)| wake.wiring.session_instance.as_ref()
                .map(echo_agent::channels::ChannelSessionInstance::incarnation_id)) == Some(old_incarnation.clone()),
            "old incarnation disappeared while its real send was unconfirmed");
        // 开放的是同一HTTP请求的真实ret0；原send/Stop继续负责结算，不伪造撤回或Unknown。
        observation_step = "release-original-http";
        gate.release.notify_one();
        stop_owner.take().ok_or_else(|| anyhow::anyhow!("original Stop owner missing"))?.await??;
        anyhow::ensure!(coordinator.find_session_wake("weixin", &first.receipt.identity.address.conversation_id).is_none(),
            "physical Stop omitted the original successful session-end callback");
        original_channel_idle(&coordinator).await?;
        let handle = channels.handle("weixin").map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("original channel handle missing"))?;
        anyhow::ensure!(handle.management_snapshot().await.shutdown_settled, "Stop lacked original physical settlement");
        observation_step = "accepted-after-original-http";
        let old_client_id = held_request.pointer("/msg/client_id").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("original accepted client identity missing"))?;
        accepted_observations.verify(&[old_client_id])?;
        let before_reconnect = provider.observed.borrow().clone();
        let old_count = before_reconnect.sends.iter().filter(|request|
            request.pointer("/msg/client_id").and_then(Value::as_str).is_some_and(|id| id.starts_with(&old_prefix))).count();
        anyhow::ensure!(old_count == 1 && before_reconnect.sends.iter().filter(|request|
            request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str)
                == chunks.first().map(String::as_str)).count() == 1,
            "original provider ret0 was not retained exactly once for the held chunk");
        anyhow::ensure!(before_reconnect.sends.iter().all(|request| {
            let text = request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str);
            chunks.iter().skip(1).all(|chunk| text != Some(chunk.as_str()))
        }), "Stop allowed remaining old long chunks after cancellation");
        observation_step = "reconnect";
        pages.admitted_pages.store(2, Ordering::SeqCst);
        control_original(&channels, &state, ChannelControlAction::Reconnect).await?;
        observation_step = "new-input";
        let second = completed_network_input(&state, &provider, "managed-long-next", 1).await?;
        original_channel_idle(&coordinator).await?;
        let current = channels.coordinators.lock()
            .map_err(|_| anyhow::anyhow!("current coordinator registry unavailable"))?
            .get("weixin").and_then(|owners| owners.last()).cloned()
            .ok_or_else(|| anyhow::anyhow!("current coordinator missing"))?;
        anyhow::ensure!(Arc::ptr_eq(&coordinator, &current), "reconnect replaced original coordinator");
        let (_, new_wake) = coordinator.find_session_wake("weixin", &second.receipt.identity.address.conversation_id)
            .ok_or_else(|| anyhow::anyhow!("new wake missing"))?;
        anyhow::ensure!(new_wake.wiring.session_instance.as_ref()
            .is_some_and(|instance| instance.incarnation_id() != old_incarnation), "reconnect reused old incarnation");
        for input in [&first, &second] {
            let binding = input.payload.channel_binding.as_ref().ok_or_else(|| anyhow::anyhow!("original binding missing"))?;
            anyhow::ensure!(binding.account_id == account.account_id && binding.credential_generation == account.generation
                && binding.workspace_generation == "global" && binding.context == FrozenChannelContext::Absent,
                "reply crossed original credential/workspace/context identity");
            let replay = state.storage.chat_events.replay(&input.receipt.identity.address.workspace_id,
                Some(&input.receipt.identity.address.conversation_id), &input.receipt.identity.input_id, 0)?;
            let (data, claims) = replay.events.iter().fold((0_usize, 0_usize), |(data, claims), event| {
                match &event.payload {
                    crate::chat_driver::ChatDriverEvent::InputLifecycle(fact) if fact.identity() == &input.receipt.identity => match fact.as_ref() {
                        ConversationInputFact::Persisted { .. } => (data.saturating_add(1), claims),
                        ConversationInputFact::AttemptStarted { .. } => (data, claims.saturating_add(1)),
                        _ => (data, claims),
                    },
                    _ => (data, claims),
                }
            });
            anyhow::ensure!((data, claims) == (1, 1), "Stop/reconnect duplicated original DATA/claim");
        }
        let after = provider.observed.borrow().clone();
        anyhow::ensure!(after.sends.iter().filter(|request| request.pointer("/msg/client_id").and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(&old_prefix))).count() == old_count,
            "old incarnation output entered the reopened session");
        let new_prefix = format!("eko-input:{}:", second.receipt.identity.input_id);
        anyhow::ensure!(new_prefix != old_prefix && after.sends.iter().filter(|request|
            request.pointer("/msg/client_id").and_then(Value::as_str).is_some_and(|id| id.starts_with(&new_prefix))
            && request.pointer("/msg/item_list/0/text_item/text").and_then(Value::as_str) == Some(BUSINESS_REPLY)).count() == 1,
            "new input lacked its own exactly-once original reply identity");
        observation_step = "accepted-after-new-http";
        let new_client_id = after.sends.iter().find(|request| request.pointer("/msg/client_id").and_then(Value::as_str).is_some_and(|id| id.starts_with(&new_prefix)))
            .and_then(|request| request.pointer("/msg/client_id")).and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("new accepted client identity missing"))?;
        accepted_observations.verify(&[old_client_id, new_client_id])?;
        observation_step = "late-old-fence";
        let new_delivery = channels.weixin_delivery.lock()
            .map_err(|_| anyhow::anyhow!("current delivery slot unavailable"))?
            .as_ref().map(|(_, delivery)| delivery.clone())
            .ok_or_else(|| anyhow::anyhow!("new real delivery handle missing"))?;
        let sends_before_stale = provider.observed.borrow().send_requests;
        let rejected = new_delivery.send_with_context(late_template, None, "late-original-fence").await;
        anyhow::ensure!(matches!(rejected, Err(ReactError::Channel(ref error))
            if matches!(error.as_ref(), ChannelError::StaleDelivery { incarnation_id } if incarnation_id == &old_incarnation)),
            "new accepting delivery did not reject the real old template with typed StaleDelivery");
        anyhow::ensure!(provider.observed.borrow().send_requests == sends_before_stale,
            "late original fence reached HTTP");
        accepted_observations.verify(&[old_client_id, new_client_id])?;
        anyhow::ensure!(llm.call_count() == 6, "Stop/reconnect ran another model attempt");
        observation_step = "final-stop";
        control_original(&channels, &state, ChannelControlAction::Stop).await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    if observed.is_err() {
        let seen = provider.observed.borrow().clone();
        eprintln!(
            "A2_LONG_SEND_DIAGNOSTIC:{}",
            serde_json::json!({
                "step":observation_step,"model_calls":llm.call_count(),"chunks_planned":chunks.len(),
                "send_requests":seen.send_requests,"accepted":seen.sends.len(),
                "protocol_errors":seen.protocol_errors,"held_request_seen":captured.borrow().is_some(),
                "original_stop_task_finished":stop_owner.as_ref().map(tokio::task::JoinHandle::is_finished),
            })
        );
    }
    // 任何失败也先开放同一provider回执并join原Stop，再沿原lifecycle清理。
    gate.release.notify_one();
    let pending_stop = match stop_owner.take() {
        Some(owner) => Some(owner.await),
        None => None,
    };
    let settled = lifecycle
        .settle(crate::runtime::ApplicationLifecycleReason::Shutdown, None)
        .await
        .wait_final()
        .await;
    let actual_provider = provider.close().await;
    if let Some(joined) = pending_stop {
        joined??;
    }
    observed?;
    anyhow::ensure!(
        settled.is_clean(),
        "long-send original application owner not settled: {settled:?}"
    );
    let actual_provider = actual_provider?;
    anyhow::ensure!(
        actual_provider.protocol_errors == 0
            && actual_provider.send_requests == 2
            && actual_provider.sends.len() == 2
            && actual_provider.business_reply_count() == 1,
        "long-send provider results not confirmed"
    );
    emit(
        serde_json::json!({"phase":"managed-long-stop","model_calls":llm.call_count(),
        "long_chunks_planned":chunks.len(),"real_http_business_replies":actual_provider.business_reply_count(),
        "real_http_send_requests":actual_provider.send_requests,"protocol_errors":actual_provider.protocol_errors,
        "original_stop_physically_joined":true,"late_old_fence_reached_http":false,
        "accepted_receipt_observations":accepted_observations.snapshot()?.len(),
        "accepted_receipt_fields_redacted":true}),
    )?;
    Ok(())
}

#[tokio::test]
async fn original_manager_stop_during_long_reply_reconnects_without_old_incarnation_output()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (receipt, parent_provider) = child(
        temporary.path(),
        "managed-long",
        "managed-long-stop",
        6,
        CHILD_TEST,
    )
    .await?;
    anyhow::ensure!(
        receipt.get("model_calls").and_then(Value::as_u64) == Some(6)
            && receipt
                .get("long_chunks_planned")
                .and_then(Value::as_u64)
                .is_some_and(|count| count >= 3)
            && receipt
                .get("real_http_business_replies")
                .and_then(Value::as_u64)
                == Some(1)
            && receipt.get("protocol_errors").and_then(Value::as_u64) == Some(0)
            && receipt
                .get("real_http_send_requests")
                .and_then(Value::as_u64)
                == Some(2)
            && receipt
                .get("original_stop_physically_joined")
                .and_then(Value::as_bool)
                == Some(true)
            && receipt
                .get("late_old_fence_reached_http")
                .and_then(Value::as_bool)
                == Some(false)
            && receipt
                .get("accepted_receipt_observations")
                .and_then(Value::as_u64)
                == Some(2)
            && receipt
                .get("accepted_receipt_fields_redacted")
                .and_then(Value::as_bool)
                == Some(true),
        "long-send original child lacked required owner/HTTP/fence evidence"
    );
    anyhow::ensure!(
        parent_provider.send_requests == 0,
        "long-send child used unrelated parent provider"
    );
    Ok(())
}

#[tokio::test]
async fn consumer_process_entry() -> anyhow::Result<()> {
    if std::env::var_os("EKO_WEIXIN_A2_PHASE").is_none() {
        return Ok(());
    }
    child_entry(WeixinChannel::for_local_fixture).await
}

#[tokio::test]
async fn p1_clean_cold_ready_drives_and_sends_once_then_restart_is_quiet() -> anyhow::Result<()> {
    clean_cold_case(CHILD_TEST).await
}

#[tokio::test]
async fn p2_original_tail_repair_then_second_crash_recovers_once() -> anyhow::Result<()> {
    repaired_twice_case(CHILD_TEST).await
}

#[tokio::test]
async fn n1_confirmed_attempt_survives_torn_tail_without_new_effect() -> anyhow::Result<()> {
    claimed_tail_case(CHILD_TEST).await
}

#[tokio::test]
async fn n2_complete_corruption_isolated_while_independent_input_recovers() -> anyhow::Result<()> {
    corrupt_isolation_case(CHILD_TEST).await
}
