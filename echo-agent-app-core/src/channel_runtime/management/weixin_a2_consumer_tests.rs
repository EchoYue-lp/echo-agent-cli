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

#[derive(Clone, Default)]
struct Observed {
    polls: usize,
    send_requests: usize,
    protocol_errors: usize,
    // Only requests accepted by the provider with ret:0 are recorded here.
    sends: Vec<serde_json::Value>,
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
    observed: tokio::sync::watch::Sender<Observed>,
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
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (observed, received) = tokio::sync::watch::channel(Observed::default());
        let router = axum::Router::new()
            .route("/ilink/bot/getupdates", axum::routing::post(poll))
            .route("/ilink/bot/sendmessage", axum::routing::post(send))
            .with_state(Provider {
                account,
                input,
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
    let messages = match (&state.input, cursor.is_empty()) {
        (Some((provider_id, text)), true) => vec![serde_json::json!({
            "message_id": provider_id, "message_type": 1,
            "from_user_id": state.account.user_id,
            "item_list": [{"type": 1, "text_item": {"text": text}}],
        })],
        _ => Vec::new(),
    };
    // Bound repeated empty fixture polls without modifying the real adapter.
    tokio::time::sleep(Duration::from_millis(10)).await;
    axum::Json(
        serde_json::json!({"ret": 0, "msgs": messages, "get_updates_buf": "original-persisted-page"}),
    )
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
        // 原 factory 先调用真实 IntentRouter，再执行回答。两条响应分别供给原
        // 分类请求和回答请求，避免分类提前耗尽唯一回答而触发无意义的恢复重试。
        let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_responses([
            r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#,
            BUSINESS_REPLY,
        ]));
        let primary = echo_agent::agent::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("local-a2-consumer")
                .llm_client(llm.clone())
                .build()?,
        );
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
            Default::default(),
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
        state.workspace.global_execution_root = root.join("product-data");
        std::fs::create_dir_all(&state.workspace.global_execution_root)?;
        let state = Arc::new(state);
        let mut lifecycle =
            crate::runtime::ApplicationLifecycleOwner::new(CancellationToken::new());
        lifecycle.bind_app_state(state.clone());
        lifecycle.bind_primary_agent(primary);
        lifecycle.bind_pool(pool);
        lifecycle.bind_task_runtime(task_runtime);
        lifecycle.bind_product_data_io(state.session.product_data_io.clone());
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
        .with_durable_ingress();
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
    if pending.is_err() {
        if let Ok((inputs, complete)) = boot
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
    }
    pending??;
    let calls = boot.llm.call_count();
    let requests = boot.llm.all_calls();
    let tool_counts = boot.llm.all_tool_counts();
    anyhow::ensure!(
        requests.len() == calls && tool_counts.len() == calls,
        "模型请求及工具计数记录不完整"
    );
    let routing_calls = requests
        .iter()
        .zip(&tool_counts)
        .filter(|(messages, tools)| {
            **tools == 0
                && messages.len() == 1
                && messages.first().is_some_and(|message| {
                    message.role == echo_agent::llm::types::Role::User
                        && message.content.as_text().is_some_and(|text| {
                            text.starts_with("You are an intent classifier for an AI agent system.")
                        })
                })
        })
        .count();
    let answer_calls = calls.saturating_sub(routing_calls);
    let (inputs, complete) = boot
        .state
        .storage
        .chat_events
        .channel_input_bindings()
        .await?;
    if routing_calls != expected || answer_calls != expected {
        // 仅记录本 fixture 的身份、类型化结果和少量调用特征；不采集系统提示词、
        // provider 凭据或 context token，以区分路由、压缩与实际回答调用。
        eprintln!(
            "A2_RECOVERY_DIAGNOSTIC:{}",
            serde_json::to_string(&serde_json::json!({
                "phase": phase,
                "model_calls": calls,
                "routing_calls": routing_calls,
                "answer_calls": answer_calls,
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
        routing_calls == expected && answer_calls == expected,
        "冷恢复总模型调用 {calls}，分类 {routing_calls}，回答 {answer_calls}；分类及回答各应为 {expected}"
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
        serde_json::json!({"phase":phase,"model_calls":calls,"routing_calls":routing_calls,"answer_calls":answer_calls,"inventory_complete":complete,"original_group_failures":failures}),
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
