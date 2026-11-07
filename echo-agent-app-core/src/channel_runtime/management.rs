//! Eclip 对原 ChannelManager 与会话/消息泵 owner 的应用控制。
//! Configuration publication remains ConfigState-owned. Transport observations
//! are projections; neither a closed watch nor a discarded waiter settles work.

use super::ChannelSessionCoordinator;
#[path = "management/weixin_auth.rs"]
mod weixin_auth;
use crate::state::AppState;
use crate::tasks::task_runtime::weixin_delivery::WeixinNotificationRecoveryReport;
use echo_agent::channels::{
    ChannelHandle, ChannelManager, ChannelPlugin, ChannelTransportEvidence, ChannelTransportPhase,
    FeishuChannel, FeishuConfig, MessageHandler, QqChannel, QqConfig, SessionHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
pub use weixin_auth::WeixinQrStatus;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedChannel {
    Qqbot,
    Feishu,
    Weixin,
}
impl ManagedChannel {
    pub fn id(self) -> &'static str {
        match self {
            Self::Qqbot => "qqbot",
            Self::Feishu => "feishu",
            Self::Weixin => "weixin",
        }
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
pub enum ChannelSecretUpdate {
    #[default]
    Preserve,
    Set(String),
    Clear,
}
impl std::fmt::Debug for ChannelSecretUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Preserve => "Preserve",
            Self::Set(_) => "Set([redacted])",
            Self::Clear => "Clear",
        })
    }
}
impl ChannelSecretUpdate {
    fn apply(self, original: &mut String) -> Result<(), String> {
        match self {
            Self::Preserve => {}
            Self::Clear => original.clear(),
            Self::Set(value) if value.trim().is_empty() => {
                return Err("Use explicit clear to remove a credential".to_string());
            }
            Self::Set(value) => *original = value,
        }
        Ok(())
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfigurationPatch {
    pub channel: ManagedChannel,
    pub expected_revision: String,
    pub enabled: bool,
    pub app_id: String,
    #[serde(default)]
    pub secret: ChannelSecretUpdate,
    pub mode: Option<String>,
    pub webhook_bind: Option<String>,
    pub webhook_path: Option<String>,
    #[serde(default)]
    pub verification_token: ChannelSecretUpdate,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelControlAction {
    Start,
    Stop,
    Reconnect,
}

#[derive(Clone)]
struct ChannelCleanupReceipt {
    result: Result<(), String>,
    retry_cleanup: bool,
    physically_settled: bool,
    failed_facts: Option<ChannelShutdownFacts>,
}
struct ChannelShutdownObservation {
    receiver: tokio::sync::watch::Receiver<Option<ChannelCleanupReceipt>>,
    // Only an observer of the already-awaited physical producer. Never repolled.
    _observer: tokio::task::JoinHandle<()>,
}
// These are cached observations inside the original cleanup receipt, not an
// identity/counter/store. External wait() calls can increment root progress;
// only changed original resource facts justify a legal cleanup retry.
#[derive(Clone, PartialEq, Eq)]
struct ChannelInstanceFacts {
    id: String,
    epoch: u64,
    handler: bool,
    active: usize,
    settled: bool,
    phase: ChannelTransportPhase,
    evidence: Option<ChannelTransportEvidence>,
    error: Option<String>,
}
#[derive(Clone, PartialEq, Eq)]
struct ChannelSessionFacts {
    id: String,
    index: usize,
    pump_receipt: Option<Result<Vec<String>, String>>,
    active_roots: Vec<(String, String, String, String)>,
}
#[derive(Clone, PartialEq, Eq)]
struct ChannelShutdownFacts {
    channels: Vec<ChannelInstanceFacts>,
    sessions: Vec<ChannelSessionFacts>,
}

type Coordinators = Arc<Mutex<HashMap<String, Vec<Arc<ChannelSessionCoordinator>>>>>;

pub struct ChannelRuntime {
    state: Weak<AppState>,
    gui_hitl_factory: crate::hitl::ChannelGuiHitlFactorySlot,
    manager: Arc<Mutex<ChannelManager>>,
    coordinators: Coordinators,
    operation: Arc<tokio::sync::Mutex<()>>,
    accepting: Arc<AtomicBool>,
    shutdown: Mutex<Option<ChannelShutdownObservation>>,
    retention: Mutex<Weak<crate::runtime::ApplicationLifecycleRetention>>,
    // Receipt metadata for original adapter construction, not connection state.
    configured_revisions: Mutex<HashMap<String, String>>,
    completed_cleanup_failures: Mutex<HashMap<String, Vec<String>>>,
    transport_observers: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    observer_cancel: tokio_util::sync::CancellationToken,
    weixin_auth: Arc<weixin_auth::AuthOwner>,
    weixin_auth_action: tokio::sync::Mutex<()>,
    weixin_workspace: Arc<Mutex<Option<String>>>,
    // 仅测试依赖绑定：仍由原构造入口创建真实 poll/sender，不模拟准入或执行。
    #[cfg(test)]
    weixin_fixture_address: Option<std::net::SocketAddr>,
    weixin_delivery: Mutex<
        Option<(
            crate::config::WeixinAccountConfig,
            echo_agent::channels::weixin::WeixinDeliveryHandle,
        )>,
    >,
}
impl ChannelRuntime {
    pub fn new(state: &Arc<AppState>) -> Self {
        Self::from_state(Arc::downgrade(state))
    }
    fn from_state(state: Weak<AppState>) -> Self {
        Self {
            state,
            gui_hitl_factory: Arc::default(),
            manager: Arc::new(Mutex::new(ChannelManager::new())),
            coordinators: Arc::default(),
            operation: Arc::default(),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown: Mutex::new(None),
            retention: Mutex::new(Weak::new()),
            configured_revisions: Mutex::default(),
            completed_cleanup_failures: Mutex::default(),
            transport_observers: Arc::default(),
            observer_cancel: tokio_util::sync::CancellationToken::new(),
            weixin_auth: Arc::default(),
            weixin_auth_action: tokio::sync::Mutex::new(()),
            weixin_workspace: Arc::default(),
            #[cfg(test)]
            weixin_fixture_address: None,
            weixin_delivery: Mutex::default(),
        }
    }
    #[cfg(test)]
    pub(crate) fn with_original_test_plugin(
        state: Weak<AppState>,
        plugin: Box<dyn ChannelPlugin>,
    ) -> Result<Arc<Self>, String> {
        let runtime = Arc::new(Self::from_state(state));
        runtime
            .manager
            .lock()
            .map_err(|_| "test manager unavailable".to_string())?
            .register(plugin)
            .map_err(|error| error.to_string())?;
        Ok(runtime)
    }
    #[cfg(test)]
    pub(crate) async fn retry_original_stop_for_test(&self) -> Result<(), String> {
        let handle = self
            .handle("qqbot")?
            .ok_or_else(|| "original test handle absent".to_string())?;
        self.stop_selected(&handle).await.map(|_| ())
    }

    pub fn install_gui_hitl_factory(&self, factory: crate::hitl::ChannelGuiHitlFactory) {
        *self
            .gui_hitl_factory
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(factory);
    }

    pub(crate) fn bind_lifecycle_retention(
        &self,
        root: &Arc<crate::runtime::ApplicationLifecycleRetention>,
    ) {
        *self
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(root);
        for coordinator in self
            .coordinators
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .flatten()
        {
            coordinator.bind_lifecycle_retention(Arc::downgrade(root));
        }
    }
    fn state(&self) -> Result<Arc<AppState>, String> {
        self.state
            .upgrade()
            .ok_or_else(|| "Original channel application owner is unavailable".to_string())
    }
    fn notify_progress(&self) {
        if let Some(root) = self
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade()
        {
            root.note_progress();
        }
    }
    fn handle(&self, id: &str) -> Result<Option<ChannelHandle>, String> {
        self.manager
            .lock()
            .map_err(|_| "Original channel manager is unavailable".to_string())
            .map(|manager| manager.get_owned(id))
    }
    pub async fn configuration(&self) -> Result<Value, String> {
        let state = self.state()?;
        let (config, revision) = state.config.configuration_snapshot().await;
        Ok(configuration_view(&config.channels, &revision))
    }
    pub async fn save_configuration(
        &self,
        patch: ChannelConfigurationPatch,
    ) -> Result<Value, String> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err("Channel management is shutting down".to_string());
        }
        let state = self.state()?;
        let ChannelConfigurationPatch {
            channel,
            expected_revision,
            enabled,
            app_id,
            secret,
            mode,
            webhook_bind,
            webhook_path,
            verification_token,
        } = patch;
        let receipt = state
            .update_channels_config_owned(expected_revision, move |config| {
                match channel {
                    ManagedChannel::Qqbot => {
                        config.qq.enabled = enabled;
                        config.qq.app_id = app_id;
                        secret.apply(&mut config.qq.client_secret)?;
                    }
                    ManagedChannel::Weixin => {
                        // Credentials only enter through the backend QR commit.
                        config.weixin.enabled = enabled;
                    }
                    ManagedChannel::Feishu => {
                        config.feishu.enabled = enabled;
                        config.feishu.app_id = app_id;
                        secret.apply(&mut config.feishu.app_secret)?;
                        if let Some(mode) = mode {
                            if !matches!(mode.as_str(), "long_poll" | "webhook") {
                                return Err("Unknown Feishu mode".to_string());
                            }
                            config.feishu.mode = mode;
                        }
                        if let Some(bind) = webhook_bind {
                            config.feishu.webhook_bind = bind;
                        }
                        if let Some(path) = webhook_path {
                            if !path.starts_with('/') {
                                return Err("Webhook path must start with /".to_string());
                            }
                            config.feishu.webhook_path = path;
                        }
                        let mut token = config
                            .feishu
                            .webhook_verification_token
                            .clone()
                            .unwrap_or_default();
                        verification_token.apply(&mut token)?;
                        config.feishu.webhook_verification_token =
                            (!token.is_empty()).then_some(token);
                    }
                }
                if enabled {
                    if matches!(channel, ManagedChannel::Weixin) {
                        weixin_auth::credentials(
                            config
                                .weixin
                                .account
                                .as_ref()
                                .ok_or_else(|| "Scan Weixin before enabling".to_string())?,
                        )
                        .validate()
                        .map_err(|_| "Invalid Weixin credentials".to_string())?;
                    } else {
                        let _validated = plugin_from_config(channel, config)?;
                    }
                }
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())?;
        // Saving never starts/restarts a transport. The GUI gets a separate
        // control receipt and actual observation when it explicitly applies it.
        Ok(configuration_view(
            &receipt.config.channels,
            &receipt.configuration_revision,
        ))
    }
    pub async fn status(&self) -> Result<Value, String> {
        let state = self.state()?;
        let (_, revision) = state.config.configuration_snapshot().await;
        let mut channels = Vec::new();
        for channel in [
            ManagedChannel::Qqbot,
            ManagedChannel::Feishu,
            ManagedChannel::Weixin,
        ] {
            let id = channel.id();
            let handle = self.handle(id)?;
            let configured = self
                .configured_revisions
                .lock()
                .map_err(|_| "Channel configuration receipt unavailable".to_string())?
                .get(id)
                .cloned();
            let completed_failures = self
                .completed_cleanup_failures
                .lock()
                .map_err(|_| "Original cleanup receipts unavailable".to_string())?
                .get(id)
                .cloned()
                .unwrap_or_default();
            let pumps = self
                .coordinators
                .lock()
                .map_err(|_| "Channel session owners unavailable".to_string())?
                .get(id)
                .cloned()
                .unwrap_or_default();
            let pump_settled = pumps.iter().all(|coordinator| {
                coordinator.input_pump_tasks.lock().is_ok_and(|tasks| {
                    tasks.shutting_down
                        && tasks.tasks.lock().is_ok_and(|original| original.is_empty())
                        && tasks
                            .physical_receipt
                            .lock()
                            .is_ok_and(|receipt| receipt.as_ref().is_some_and(Result::is_ok))
                })
            });
            if let Some(handle) = handle {
                let snapshot = handle.management_snapshot().await;
                let phase = match snapshot.transport.phase {
                    ChannelTransportPhase::Stopped => "stopped",
                    ChannelTransportPhase::Connecting => "connecting",
                    ChannelTransportPhase::Ready => "ready",
                    ChannelTransportPhase::Disconnected => "disconnected",
                    ChannelTransportPhase::Failed => "failed",
                    ChannelTransportPhase::Unknown => "unknown",
                };
                let evidence = snapshot.transport.evidence.map(transport_evidence_name);
                let observed =
                    chrono::DateTime::<chrono::Utc>::from(snapshot.transport.observed_at)
                        .to_rfc3339();
                channels.push(json!({"id":id,"registered":true,"completed_cleanup_failures":completed_failures,"configuration_revision":revision,"applied_configuration_revision":configured,
                    "mutation_epoch":snapshot.mutation_epoch.to_string(),"handler_attached":snapshot.handler_attached,
                    "active_invocations":snapshot.active_invocations,"shutdown_settled":snapshot.shutdown_settled && pump_settled,
                    "pump_shutdown_settled":pump_settled,"transport":{"phase":phase,"evidence":evidence,"error":snapshot.transport.last_error,"observed_at":observed}}));
            } else {
                channels.push(json!({"id":id,"registered":false,"completed_cleanup_failures":completed_failures,"configuration_revision":revision,"applied_configuration_revision":Value::Null,
                "mutation_epoch":Value::Null,"handler_attached":false,"active_invocations":0,"shutdown_settled":pump_settled,
                "pump_shutdown_settled":pump_settled,"transport":{"phase":"unknown","evidence":Value::Null,"error":Value::Null,"observed_at":Value::Null}}));
            }
        }
        Ok(json!({"configuration_revision":revision,"channels":channels}))
    }
    /// 用户明确放弃一条阻塞原微信会话的待恢复输入。
    ///
    /// 这是产品策略上的薄适配：输入身份、账户/凭据代次与工作空间代次全部
    /// 来自原日志中不可变的 binding；当前 phase 与队列 revision 由原 journal
    /// authority 在同一锁内复核，界面观察到的版本不能替代它。成功后只追加
    /// Cancelled 并唤醒原 slot 的既有处理循环，不重写原失败、UNKNOWN 或
    /// TaskRun 结论，也不创建第二个 owner。
    pub async fn abandon_recovery_input(
        &self,
        identity: crate::conversation_input::ConversationInputIdentity,
        expected_queue_revision: u64,
    ) -> Result<Value, String> {
        use crate::conversation_input::ConversationInputPhase;
        let operation = Arc::clone(&self.operation).lock_owned().await;
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| "Channel runtime owner unavailable".to_string())?;
        let service = state.conversation_inputs();
        let snapshot = service
            .list(&identity.address)
            .await
            .map_err(|error| error.to_string())?;
        let Some(item) = snapshot
            .items
            .iter()
            .find(|entry| entry.receipt.identity == identity)
        else {
            // frontier 不包含已取消项；重复动作只观察原日志的准确终态，
            // 不因其它队列项变化再次取消，也不借当前账户改写原绑定。
            let (inputs, _) = state
                .storage
                .chat_events
                .channel_input_bindings()
                .await
                .map_err(|error| error.to_string())?;
            if let Some(mut previous) = inputs.into_iter().find(|entry| {
                entry.receipt.identity == identity
                    && entry.receipt.phase == ConversationInputPhase::Cancelled
            }) {
                previous
                    .payload
                    .validate_channel_digest(&identity.address)
                    .map_err(|error| error.to_string())?;
                previous.receipt.duplicate = true;
                return Ok(json!({
                    "queue_revision": previous.receipt.queue_revision,
                    "receipt": previous.receipt,
                    "pump": {"observation": "already_abandoned"},
                }));
            }
            return Err("待恢复消息已不在原队列；请重新查看".to_string());
        };
        if item.receipt.phase != ConversationInputPhase::RecoveryRequired {
            return Err("只有待恢复（RecoveryRequired）的原渠道消息可以放弃".to_string());
        }
        if snapshot.queue_revision != expected_queue_revision {
            return Err("原队列已变化；请重新查看后再放弃".to_string());
        }
        let binding = item
            .payload
            .channel_binding
            .as_ref()
            .ok_or_else(|| "该消息不属于原渠道会话".to_string())?;
        binding.validate_data().map_err(|error| error.to_string())?;
        item.payload
            .validate_channel_digest(&identity.address)
            .map_err(|error| error.to_string())?;
        let coordinators = self
            .coordinators
            .lock()
            .map_err(|_| "Original channel sessions unavailable".to_string())?
            .get(&binding.channel_id)
            .cloned()
            .unwrap_or_default();
        let mut owner = None;
        for coordinator in coordinators.iter().rev() {
            if let Some(found) = coordinator
                .find_session_wake(&binding.channel_id, &identity.address.conversation_id)
            {
                owner = Some(found.1);
                break;
            }
        }
        let wake = owner.ok_or_else(|| {
            "原渠道会话运行实例不可用；请重连该渠道后再处理待恢复消息".to_string()
        })?;
        let Some((account_id, credential_generation)) = wake.wiring.weixin_identity.as_ref() else {
            return Err("原渠道凭据代次不可用；未执行放弃".to_string());
        };
        if account_id != &binding.account_id
            || credential_generation != &binding.credential_generation
        {
            return Err("原账户或凭据代次已变化；保留原输入，未执行放弃".to_string());
        }
        // 历史输入的scope来自原DATA；wake的默认scope只路由新消息，不能替换它。
        // 原publication租约和control保有到同一owned IO结束，防止校验后被删除/重建。
        let original_scope: futures::future::BoxFuture<'_, _> =
            Box::pin(state.channel_control_for_generation(
                &identity.address.workspace_id,
                &binding.workspace_generation,
            ));
        let (publication, original_control) = original_scope
            .await
            .map_err(|_| "原工作空间代次已变化或不可用；保留原输入，未执行放弃".to_string())?;
        // 本地仍需防止显式放弃与原对话删除/新轮次并发，避免误取消刚获准入
        // 的输入；复用原身份锁与所有 surface 的原子 idle suspension。
        let admission = original_control
            .runtime()
            .begin_managed_replacement(
                &state.session.foreground_turns,
                &identity.address.conversation_id,
            )
            .await
            .map_err(|error| format!("原对话仍有运行或恢复义务；未执行放弃：{error}"))?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("abandon channel recovery input")
            .map_err(|error| error.to_string())?;
        let settlement = flow.clone();
        let observed = flow
            .run_async("commit abandoned channel recovery input", async move {
                let _operation = operation;
                let _publication = publication;
                let _original_control = original_control;
                // 原 supervisor 持有整个物理流程，调用者丢弃观察不能释放保护，
                // 也不能丢掉已提交取消之后对原 pump 的唤醒。
                let result = async move {
                    let receipt = service
                        .cancel_pending_recovery(identity.clone(), expected_queue_revision)
                        .await
                        .map_err(|error| error.to_string())?;
                    drop(admission);
                    let pump = wake.start_after_abandon().await;
                    let frontier = service
                        .list(&identity.address)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(json!({
                        "receipt": receipt,
                        "queue_revision": frontier.queue_revision,
                        "pump": match pump {
                            Ok(started) => json!({"observation": started}),
                            Err(error) => json!({"observation": "unavailable", "error": error}),
                        },
                    }))
                }
                .await;
                // 这里结算原物理流程；业务结果仍通过原 Result/输入日志表达。
                settlement.settle(None);
                result
            })
            .await;
        match observed {
            Ok(result) => result,
            Err(error) => {
                flow.settle(Some(error.to_string()));
                Err(error.to_string())
            }
        }
    }

    async fn handler(
        &self,
        state: Arc<AppState>,
        id: &str,
        timeout: u64,
    ) -> Result<(Arc<dyn MessageHandler>, Option<String>), String> {
        let existing = self
            .coordinators
            .lock()
            .map_err(|_| "Original session coordinator unavailable".to_string())?
            .get(id)
            .and_then(|owners| owners.last())
            .cloned();
        let coordinator = if let Some(coordinator) = existing {
            coordinator.reopen_input_pump_admission()?;
            coordinator
        } else {
            let coordinator = Arc::new(ChannelSessionCoordinator::new());
            coordinator.bind_lifecycle_retention(self.root());
            self.coordinators
                .lock()
                .map_err(|_| "Original session coordinator unavailable".to_string())?
                .entry(id.to_string())
                .or_default()
                .push(coordinator.clone());
            coordinator
        };
        let weixin_account = if id == "weixin" {
            Some(self.weixin_configured_account()?)
        } else {
            None
        };
        let weixin_delivery = weixin_account
            .as_ref()
            .map(|account| self.weixin_delivery_handle(&account.account_id, &account.generation))
            .transpose()?;
        // Bind the original workspace/runtime once per transport instance.
        // GUI project selection must not retarget later messages or HITL.
        let bound_product_data = if id == "weixin" {
            Some(
                state
                    .current_product_data()
                    .await
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let bound_runtime = bound_product_data
            .as_ref()
            .map(|scope| scope.runtime().clone());
        let bound_workspace_generation =
            bound_product_data.as_ref().map(|scope| scope.generation());
        let bound_workspace = bound_runtime
            .as_ref()
            .map(|runtime| runtime.execution_scope().workspace_id().to_string());
        let weixin_identity = weixin_account
            .as_ref()
            .map(|account| (account.account_id.clone(), account.generation.clone()));
        let recovery_coordinator = coordinator.clone();
        let end = coordinator.clone();
        let admission = self.accepting.clone();
        let gui_hitl = self.gui_hitl_factory.clone();
        let foreground = state.session.foreground_turns.clone();
        let webhook = state.webhook.emitter.clone();
        let handler = SessionHandler::new(
            super::channel_session_config(timeout),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let mut handler = super::AppChannelMessageHandler::new(
                    state.clone(),
                    webhook.clone(),
                    foreground.clone(),
                    instance.clone(),
                    coordinator.clone(),
                )
                .with_admission_gate(admission.clone())
                .with_gui_hitl_factory(gui_hitl.clone());
                if let Some(runtime) = bound_runtime.as_ref() {
                    handler = handler.with_bound_control_runtime(runtime.clone());
                    handler.bound_workspace_generation = bound_workspace_generation.clone();
                }
                if let Some(delivery) = weixin_delivery.as_ref() {
                    handler = handler.with_weixin_delivery(delivery.clone());
                    if let Some((account, generation)) = weixin_identity.as_ref() {
                        handler = handler.with_weixin_credential_identity(account, generation);
                    }
                }
                Box::new(handler) as Box<dyn MessageHandler>
            },
        )
        .with_on_session_end(move |info| end.record_session_end(info));
        let handler: Arc<dyn MessageHandler> = if weixin_account.is_some() {
            Arc::new(handler.with_durable_ingress())
        } else {
            Arc::new(handler)
        };
        if let Some(account) = weixin_account {
            return Ok((
                Arc::new(
                    weixin_auth::ScannerHandler::new(handler, account)
                        .with_coordinator(recovery_coordinator),
                ),
                bound_workspace,
            ));
        }
        Ok((handler, bound_workspace))
    }
    async fn stop_selected(&self, handle: &ChannelHandle) -> Result<Vec<String>, String> {
        let coordinators = self
            .coordinators
            .lock()
            .map_err(|_| "Original channel sessions unavailable".to_string())?
            .get(handle.id())
            .cloned()
            .unwrap_or_default();
        let mut unknown = Vec::new();
        let mut completed_failures = Vec::new();
        for coordinator in &coordinators {
            if let Err(error) = coordinator.begin_input_pump_shutdown() {
                unknown.push(error);
            }
        }
        if let Err(error) = handle.stop().await {
            unknown.push(error.to_string());
        }
        for coordinator in &coordinators {
            match coordinator.observe_input_pump_settlement().await {
                Ok(receipt) => completed_failures.extend(receipt.failures),
                Err(error) => unknown.push(error),
            }
            if !coordinator
                .active_turns
                .lock()
                .map_err(|_| "Original foreground ownership unavailable".to_string())?
                .is_empty()
            {
                unknown.push("Original channel foreground ownership remains unsettled".to_string());
            }
        }
        self.completed_cleanup_failures
            .lock()
            .map_err(|_| "Original cleanup receipts unavailable".to_string())?
            .insert(handle.id().to_string(), completed_failures.clone());
        if !handle.management_snapshot().await.shutdown_settled {
            unknown.push("Original channel cleanup remains unconfirmed".to_string());
        }
        if unknown.is_empty() {
            if handle.id() == "weixin" {
                self.weixin_workspace
                    .lock()
                    .map_err(|_| "Weixin original workspace binding unavailable".to_string())?
                    .take();
            }
            self.notify_progress();
            Ok(completed_failures)
        } else {
            Err(unknown.join("; "))
        }
    }
    pub async fn control(
        &self,
        channel: ManagedChannel,
        expected_revision: String,
        expected_mutation_epoch: Option<String>,
        expected_applied_revision: Option<String>,
        action: ChannelControlAction,
    ) -> Result<Value, String> {
        let _operation = self.operation.lock().await;
        if !matches!(action, ChannelControlAction::Stop) && !self.accepting.load(Ordering::Acquire)
        {
            return Err("Channel management is shutting down".to_string());
        }
        let state = self.state()?;
        let (config, revision) = state.config.configuration_snapshot().await;
        let id = channel.id();
        let current = self.handle(id)?;
        let actual_epoch = if let Some(handle) = current.as_ref() {
            Some(
                handle
                    .management_snapshot()
                    .await
                    .mutation_epoch
                    .to_string(),
            )
        } else {
            None
        };
        let actual_applied = self
            .configured_revisions
            .lock()
            .map_err(|_| "Original configuration receipt unavailable".to_string())?
            .get(id)
            .cloned();
        if actual_epoch != expected_mutation_epoch || actual_applied != expected_applied_revision {
            return Err(
                "Channel instance changed; reload its original epoch before controlling it"
                    .to_string(),
            );
        }
        // Cleanup targets the existing instance even after unrelated model
        // saves or desired-config removal. Only start/reconnect consume desired.
        if !matches!(action, ChannelControlAction::Stop) && revision != expected_revision {
            return Err("Channel configuration changed; reload before applying".to_string());
        }
        if matches!(channel, ManagedChannel::Weixin) {
            self.weixin_auth.cancel();
            self.weixin_auth.wait().await?;
        }
        if matches!(action, ChannelControlAction::Stop) {
            if let Some(handle) = current {
                self.stop_selected(&handle).await?;
            }
            return self.status().await;
        }
        let enabled = match channel {
            ManagedChannel::Qqbot => config.channels.qq.enabled,
            ManagedChannel::Feishu => config.channels.feishu.enabled,
            ManagedChannel::Weixin => config.channels.weixin.enabled,
        };
        if !enabled {
            return Err("Enable and save this channel before connecting".to_string());
        }
        if let Some(handle) = current.as_ref() {
            let snapshot = handle.management_snapshot().await;
            if matches!(action, ChannelControlAction::Start)
                && snapshot.handler_attached
                && (!matches!(channel, ManagedChannel::Weixin)
                    || actual_applied.as_deref() == Some(expected_revision.as_str()))
            {
                return self.status().await;
            }
            self.stop_selected(handle).await?;
            if state.config.configuration_snapshot().await.1 != expected_revision {
                return Err(
                    "Configuration changed during cleanup; reload before connecting".to_string(),
                );
            }
            let (plugin, delivery) = self.construct_plugin(channel, &config.channels)?;
            handle
                .replace_stopped_plugin(plugin)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(delivery) = delivery {
                *self
                    .weixin_delivery
                    .lock()
                    .map_err(|_| "Weixin original sender slot unavailable".to_string())? =
                    Some(delivery);
            }
        } else {
            let (plugin, delivery) = self.construct_plugin(channel, &config.channels)?;
            self.manager
                .lock()
                .map_err(|_| "Original channel manager unavailable".to_string())?
                .register(plugin)
                .map_err(|error| error.to_string())?;
            if let Some(delivery) = delivery {
                *self
                    .weixin_delivery
                    .lock()
                    .map_err(|_| "Weixin original sender slot unavailable".to_string())? =
                    Some(delivery);
            }
        }
        self.configured_revisions
            .lock()
            .map_err(|_| "Channel configuration receipt unavailable".to_string())?
            .insert(id.to_string(), expected_revision);
        let handle = self
            .handle(id)?
            .ok_or_else(|| "Original channel registration missing".to_string())?;
        let (handler, bound_workspace) = self
            .handler(state.clone(), id, config.channels.session.timeout_minutes)
            .await?;
        let started = handle.start(handler.clone()).await;
        if matches!(channel, ManagedChannel::Weixin) && started.is_ok() {
            *self
                .weixin_workspace
                .lock()
                .map_err(|_| "Weixin original workspace binding unavailable".to_string())? =
                bound_workspace;
        }
        self.watch_original_transport(&handle).await?;
        started.map_err(|error| error.to_string())?;
        if id == "weixin" {
            let account = self.weixin_configured_account()?;
            let coordinator = self
                .coordinators
                .lock()
                .map_err(|_| "original channel coordinator unavailable".to_string())?
                .get(id)
                .and_then(|owners| owners.last())
                .cloned()
                .ok_or_else(|| "original channel coordinator missing".to_string())?;
            coordinator.start_channel_recovery_initialization(
                state,
                handler,
                handle.clone(),
                account.account_id,
                account.generation,
            )?;
        }
        self.status().await
    }
    pub async fn start_configured(&self) -> Vec<String> {
        let (_, failures, recovery) = self.start_configured_with_receipt().await;
        for reason in recovery.pending {
            tracing::warn!(%reason, "Weixin completion notification recovery remains pending");
        }
        failures
    }
    pub(crate) async fn start_configured_with_receipt(
        &self,
    ) -> (usize, Vec<String>, WeixinNotificationRecoveryReport) {
        let Ok(state) = self.state() else {
            return (
                0,
                vec!["Original channel application owner unavailable".to_string()],
                WeixinNotificationRecoveryReport::default(),
            );
        };
        let mut failures = Vec::new();
        let mut started = 0usize;
        for channel in [
            ManagedChannel::Qqbot,
            ManagedChannel::Feishu,
            ManagedChannel::Weixin,
        ] {
            let (config, revision) = state.config.configuration_snapshot().await;
            let enabled = match channel {
                ManagedChannel::Qqbot => config.channels.qq.enabled,
                ManagedChannel::Feishu => config.channels.feishu.enabled,
                ManagedChannel::Weixin => config.channels.weixin.enabled,
            };
            let epoch = match self.handle(channel.id()) {
                Ok(Some(handle)) => Some(
                    handle
                        .management_snapshot()
                        .await
                        .mutation_epoch
                        .to_string(),
                ),
                Ok(None) => None,
                Err(error) => {
                    failures.push(error);
                    continue;
                }
            };
            let applied = self
                .configured_revisions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(channel.id())
                .cloned();
            if enabled {
                match self
                    .control(
                        channel,
                        revision,
                        epoch,
                        applied,
                        ChannelControlAction::Start,
                    )
                    .await
                {
                    Ok(_) => started = started.saturating_add(1),
                    Err(error) => failures.push(format!("{}: {error}", channel.id())),
                }
            }
        }
        // This is the original configured-channel startup owner. Recovery is
        // deliberately AFTER start and outside the instance operation lock;
        // GUI factory installation/readiness order remains unchanged.
        let sender: Arc<dyn crate::tasks::task_runtime::weixin_delivery::WeixinCronDelivery> =
            state.scheduler.weixin_notifications.clone();
        let recovery = state
            .recover_weixin_notifications_after_channel_start(sender, &state.scheduler.cancel_token)
            .await;
        // 原通知恢复报告保留自己的 ledger/结算语义，不伪装成 transport 启动失败。
        (started, failures, recovery)
    }
    async fn watch_original_transport(&self, handle: &ChannelHandle) -> Result<(), String> {
        let observation = handle.transport_status().await;
        let Some(mut receiver) = observation.receiver else {
            return Ok(());
        };
        receiver.borrow_and_update();
        let root = self.root();
        let cancel = self.observer_cancel.clone();
        let observer=tokio::runtime::Handle::try_current().map_err(|error|error.to_string())?.spawn(async move {
            loop {
                let observed=tokio::select! {_=cancel.cancelled()=>break,changed=receiver.changed()=>changed};
                // An observation change only wakes the original retention. It
                // cannot grant physical completion, including a closed watch.
                if let Some(root)=root.upgrade(){root.note_progress();}
                if observed.is_err(){break;}
            }
        });
        self.transport_observers
            .lock()
            .map_err(|_| "Original transport observers unavailable".to_string())?
            .push(observer);
        Ok(())
    }

    fn close_admission(&self) -> Result<(), String> {
        self.accepting.store(false, Ordering::Release);
        self.weixin_auth.cancel();
        let coordinators = self
            .coordinators
            .lock()
            .map_err(|_| "Channel session owners unavailable".to_string())?
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for coordinator in coordinators {
            if let Err(error) = coordinator.begin_input_pump_shutdown() {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
    fn no_registered_owners(&self) -> Result<bool, String> {
        Ok(!self.weixin_auth.has_operation()
            && self
                .manager
                .lock()
                .map_err(|_| "Original manager unavailable".to_string())?
                .is_empty()
            && self
                .coordinators
                .lock()
                .map_err(|_| "Original sessions unavailable".to_string())?
                .is_empty())
    }
    fn root(&self) -> Weak<crate::runtime::ApplicationLifecycleRetention> {
        self.retention
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub fn begin_shutdown(&self) -> Result<(), String> {
        self.close_admission()?;
        if self.no_registered_owners()? {
            return Ok(());
        }
        let mut slot = self
            .shutdown
            .lock()
            .map_err(|_| "Original channel cleanup slot unavailable".to_string())?;
        if slot.is_none() {
            self.start_original_cleanup(&mut slot)?;
        }
        Ok(())
    }
    /// Outside normal_drained: return completed failure facts separately from
    /// unknown ownership. Never repoll a Ready/Err producer or treat wait() as
    /// physical progress; compare only original manager/session facts.
    pub(crate) async fn observe_shutdown(&self) -> Result<Option<Vec<String>>, String> {
        self.begin_shutdown()?;
        if self.no_registered_owners()? {
            return Ok(Some(Vec::new()));
        }
        let (receipt, closed) = {
            let slot = self
                .shutdown
                .lock()
                .map_err(|_| "Original cleanup slot unavailable".to_string())?;
            let observation = slot
                .as_ref()
                .ok_or_else(|| "Original cleanup not started".to_string())?;
            // 同步读取原watch快照；持原读guard时检查closed，避免None与稍后已发布Some混时。
            let current = observation.receiver.borrow();
            let closed = observation.receiver.has_changed().is_err();
            (current.clone(), closed)
        };
        let Some(receipt) = receipt else {
            return if closed {
                Err("Channel cleanup observation closed without a receipt".to_string())
            } else {
                // 原watch开放且没有回执就是在途；根继续保留owner，不能制造错误事实。
                Ok(None)
            };
        };
        if receipt.physically_settled {
            return Ok(Some(receipt.result.err().into_iter().collect()));
        }
        let original_error = receipt
            .result
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "Original channel cleanup remains unconfirmed".to_string());
        if receipt.retry_cleanup {
            let facts = original_shutdown_facts(&self.manager, &self.coordinators)
                .await
                .map_err(|error| format!("{original_error}; cleanup facts observation: {error}"))?;
            if receipt
                .failed_facts
                .as_ref()
                .is_some_and(|previous| previous != &facts)
            {
                let mut slot = self
                    .shutdown
                    .lock()
                    .map_err(|_| format!("{original_error}; original cleanup slot unavailable"))?;
                self.start_original_cleanup(&mut slot).map_err(|error| {
                    format!("{original_error}; cleanup retry admission: {error}")
                })?;
                // 即使这是根第一次观察该旧收据，也不能把真实failed stop改为新Pending。
                return Err(original_error);
            }
        }
        Err(original_error)
    }
    fn start_original_cleanup(
        &self,
        slot: &mut Option<ChannelShutdownObservation>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
        let manager = self.manager.clone();
        let coordinators = self.coordinators.clone();
        let operation = self.operation.clone();
        let observers = self.transport_observers.clone();
        let observer_cancel = self.observer_cancel.clone();
        let root = self.root();
        let authentication = self.weixin_auth.clone();
        let weixin_workspace = self.weixin_workspace.clone();
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let producer = runtime.spawn(async move {
            let _operation = operation.lock().await;
            let mut failures = Vec::new();
            let mut physically_settled = true;
            if let Err(error) = authentication.wait().await {
                physically_settled = false;
                failures.push(error);
            } else if let Some(error) = authentication.completed_failure() {
                failures.push(error);
            }
            let mut retry_cleanup = true;
            let stop = manager
                .lock()
                .map_err(|_| "Original manager unavailable".to_string())?
                .stop_all_owned();
            if let Err(error) = stop.await {
                physically_settled = false;
                failures.push(error.to_string());
            }
            let sessions = coordinators
                .lock()
                .map_err(|_| "Original sessions unavailable".to_string())?
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            for coordinator in sessions {
                if !coordinator
                    .active_turns
                    .lock()
                    .map_err(|_| "Original foreground map unavailable".to_string())?
                    .is_empty()
                {
                    physically_settled = false;
                    failures.push("Original foreground remains unsettled".to_string());
                }
                match coordinator.observe_input_pump_settlement().await {
                    Ok(receipt) => failures.extend(receipt.failures),
                    Err(error) => {
                        physically_settled = false;
                        retry_cleanup = false;
                        failures.push(error);
                    }
                }
            }
            let failed_facts = if physically_settled {
                None
            } else {
                Some(original_shutdown_facts(&manager, &coordinators).await?)
            };
            if physically_settled {
                weixin_workspace
                    .lock()
                    .map_err(|_| "Weixin original workspace binding unavailable".to_string())?
                    .take();
                observer_cancel.cancel();
                let owned = std::mem::take(
                    &mut *observers
                        .lock()
                        .map_err(|_| "Original observers unavailable".to_string())?,
                );
                for observer in owned {
                    if let Err(error) = observer.await {
                        failures.push(format!("transport observation join: {error}"));
                    }
                }
            }
            Ok::<_, String>(ChannelCleanupReceipt {
                result: if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                },
                retry_cleanup,
                physically_settled,
                failed_facts,
            })
        });
        let observer = runtime.spawn(async move {
            let receipt = match producer.await {
                Ok(Ok(receipt)) => receipt,
                Ok(Err(error)) => ChannelCleanupReceipt {
                    result: Err(error),
                    retry_cleanup: false,
                    physically_settled: false,
                    failed_facts: None,
                },
                Err(error) => ChannelCleanupReceipt {
                    result: Err(format!("Original cleanup producer unavailable: {error}")),
                    retry_cleanup: false,
                    physically_settled: false,
                    failed_facts: None,
                },
            };
            sender.send_replace(Some(receipt));
            if let Some(root) = root.upgrade() {
                root.note_progress();
            }
        });
        *slot = Some(ChannelShutdownObservation {
            receiver,
            _observer: observer,
        });
        Ok(())
    }
    pub async fn shutdown_and_wait(&self) -> Result<(), String> {
        self.begin_shutdown()?;
        let mut receiver = self
            .shutdown
            .lock()
            .map_err(|_| "Original channel cleanup slot unavailable".to_string())?
            .as_ref()
            .map(|slot| slot.receiver.clone());
        let Some(receiver) = receiver.as_mut() else {
            return Ok(());
        };
        loop {
            if let Some(receipt) = receiver.borrow().clone() {
                return receipt.result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "Channel cleanup observation closed without a receipt".to_string())?;
        }
    }
}
async fn original_shutdown_facts(
    manager: &Arc<Mutex<ChannelManager>>,
    coordinators: &Coordinators,
) -> Result<ChannelShutdownFacts, String> {
    let handles = {
        let manager = manager
            .lock()
            .map_err(|_| "Original manager unavailable".to_string())?;
        ["qqbot", "feishu", "weixin"]
            .into_iter()
            .filter_map(|id| manager.get_owned(id))
            .collect::<Vec<_>>()
    };
    let mut channels = Vec::new();
    for handle in handles {
        let snapshot = handle.management_snapshot().await;
        channels.push(ChannelInstanceFacts {
            id: handle.id().to_string(),
            epoch: snapshot.mutation_epoch,
            handler: snapshot.handler_attached,
            active: snapshot.active_invocations,
            settled: snapshot.shutdown_settled,
            phase: snapshot.transport.phase,
            evidence: snapshot.transport.evidence,
            error: snapshot.transport.last_error,
        });
    }
    let owners = {
        let owners = coordinators
            .lock()
            .map_err(|_| "Original sessions unavailable".to_string())?;
        let mut entries = owners
            .iter()
            .map(|(id, owners)| (id.clone(), owners.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    };
    let mut sessions = Vec::new();
    for (id, owners) in owners {
        for (index, owner) in owners.into_iter().enumerate() {
            let pump_receipt = owner
                .input_pump_tasks
                .lock()
                .map_err(|_| "Original pump slot unavailable".to_string())?
                .physical_receipt
                .lock()
                .map_err(|_| "Original pump receipt unavailable".to_string())?
                .clone()
                .map(|receipt| receipt.map(|receipt| receipt.failures));
            let mut active_roots = owner
                .active_turns
                .lock()
                .map_err(|_| "Original foreground map unavailable".to_string())?
                .values()
                .map(|turn| {
                    (
                        turn.runtime.execution_scope().workspace_id().to_string(),
                        turn.runtime
                            .workspace_io_receipt()
                            .host_generation()
                            .to_string(),
                        turn.conversation_id.clone(),
                        turn.turn_id.clone(),
                    )
                })
                .collect::<Vec<_>>();
            active_roots.sort();
            sessions.push(ChannelSessionFacts {
                id: id.clone(),
                index,
                pump_receipt,
                active_roots,
            });
        }
    }
    Ok(ChannelShutdownFacts { channels, sessions })
}

fn configuration_view(config: &crate::config::ChannelsConfig, revision: &str) -> Value {
    json!({"revision":revision,"qq":{"enabled":config.qq.enabled,"app_id":config.qq.app_id,"has_secret":!config.qq.client_secret.is_empty()},
        "feishu":{"enabled":config.feishu.enabled,"app_id":config.feishu.app_id,"has_secret":!config.feishu.app_secret.is_empty(),"mode":config.feishu.mode,
            "webhook_bind":config.feishu.webhook_bind,"webhook_path":config.feishu.webhook_path,"has_verification_token":config.feishu.webhook_verification_token.as_ref().is_some_and(|token|!token.is_empty())},
        "weixin":{"enabled":config.weixin.enabled,"has_credential":config.weixin.account.is_some(),"account_id":config.weixin.account.as_ref().map(|account|&account.account_id)},
        "session":{"timeout_minutes":config.session.timeout_minutes}})
}

fn transport_evidence_name(evidence: ChannelTransportEvidence) -> &'static str {
    match evidence {
        ChannelTransportEvidence::QqSessionReady => "qq_session_ready",
        ChannelTransportEvidence::QqSessionResumed => "qq_session_resumed",
        ChannelTransportEvidence::FeishuWebSocketHandshake => "feishu_websocket_handshake",
        ChannelTransportEvidence::LocalWebhookListener => "local_webhook_listener",
        ChannelTransportEvidence::WeixinAuthenticatedPoll => "weixin_authenticated_poll",
    }
}

fn plugin_from_config(
    channel: ManagedChannel,
    config: &crate::config::ChannelsConfig,
) -> Result<Box<dyn ChannelPlugin>, String> {
    match channel {
        ManagedChannel::Weixin => {
            Err("Weixin requires the original runtime credential owner".to_string())
        }
        ManagedChannel::Qqbot => QqChannel::new(QqConfig {
            app_id: config.qq.app_id.clone(),
            client_secret: config.qq.client_secret.clone(),
        })
        .map(|plugin| Box::new(plugin) as Box<dyn ChannelPlugin>)
        .map_err(|error| error.to_string()),
        ManagedChannel::Feishu => {
            let config = match config.feishu.mode.as_str() {
                "long_poll" => FeishuConfig::new_long_poll(
                    config.feishu.app_id.clone(),
                    config.feishu.app_secret.clone(),
                ),
                "webhook" => FeishuConfig::new_webhook(
                    config.feishu.app_id.clone(),
                    config.feishu.app_secret.clone(),
                    config.feishu.webhook_bind.clone(),
                    config.feishu.webhook_path.clone(),
                    config.feishu.webhook_verification_token.clone(),
                ),
                _ => return Err("Unknown Feishu mode".to_string()),
            };
            FeishuChannel::new(config)
                .map(|plugin| Box::new(plugin) as Box<dyn ChannelPlugin>)
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    pub(crate) struct ProbeControl {
        pub stops: Arc<AtomicUsize>,
        pub fail: Arc<AtomicBool>,
        pub panic: Arc<AtomicBool>,
        pub started: Arc<tokio::sync::Notify>,
        pub release: Option<tokio::sync::oneshot::Sender<()>>,
    }
    struct ProbeChannel {
        control: ProbeControl,
        held: Option<tokio::sync::oneshot::Receiver<()>>,
        transport: tokio::sync::watch::Sender<echo_agent::channels::ChannelTransportSnapshot>,
    }
    const CAPABILITIES: echo_agent::channels::ChannelCapabilities =
        echo_agent::channels::ChannelCapabilities {
            chat_types: &[echo_agent::channels::ChatType::Direct],
            supports_media: false,
            supports_threads: false,
        };
    pub(crate) fn channel(
        held: bool,
        fail: bool,
        panic: bool,
    ) -> (Box<dyn ChannelPlugin>, ProbeControl) {
        let (release, wait) = tokio::sync::oneshot::channel();
        let control = ProbeControl {
            stops: Arc::default(),
            fail: Arc::new(AtomicBool::new(fail)),
            panic: Arc::new(AtomicBool::new(panic)),
            started: Arc::default(),
            release: Some(release),
        };
        let owned = ProbeControl {
            stops: control.stops.clone(),
            fail: control.fail.clone(),
            panic: control.panic.clone(),
            started: control.started.clone(),
            release: None,
        };
        let transport =
            tokio::sync::watch::channel(echo_agent::channels::ChannelTransportSnapshot::unknown())
                .0;
        (
            Box::new(ProbeChannel {
                control: owned,
                held: held.then_some(wait),
                transport,
            }),
            control,
        )
    }
    #[async_trait::async_trait]
    impl ChannelPlugin for ProbeChannel {
        fn id(&self) -> &str {
            "qqbot"
        }
        fn capabilities(&self) -> &echo_agent::channels::ChannelCapabilities {
            &CAPABILITIES
        }
        fn transport_status(
            &self,
        ) -> Option<tokio::sync::watch::Receiver<echo_agent::channels::ChannelTransportSnapshot>>
        {
            Some(self.transport.subscribe())
        }
        async fn start(
            &mut self,
            _handler: Arc<dyn MessageHandler>,
        ) -> echo_agent::error::Result<()> {
            self.transport
                .send_replace(echo_agent::channels::ChannelTransportSnapshot {
                    phase: ChannelTransportPhase::Ready,
                    evidence: Some(ChannelTransportEvidence::QqSessionReady),
                    last_error: None,
                    observed_at: std::time::SystemTime::now(),
                });
            Ok(())
        }
        async fn stop(&mut self) -> echo_agent::error::Result<()> {
            self.control.stops.fetch_add(1, Ordering::SeqCst);
            self.control.started.notify_one();
            if let Some(held) = self.held.take() {
                held.await
                    .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            }
            // Deliberate third-party unwind fixture; production APIs never panic.
            if self.control.panic.load(Ordering::SeqCst) {
                std::panic::resume_unwind(Box::new("injected channel cleanup unwind"));
            }
            if self.control.fail.load(Ordering::SeqCst) {
                self.transport
                    .send_replace(echo_agent::channels::ChannelTransportSnapshot::unknown());
                return Err(echo_agent::error::ReactError::Other(
                    "mock_stop_error".to_string(),
                ));
            }
            self.transport
                .send_replace(echo_agent::channels::ChannelTransportSnapshot {
                    phase: ChannelTransportPhase::Stopped,
                    evidence: None,
                    last_error: None,
                    observed_at: std::time::SystemTime::now(),
                });
            Ok(())
        }
        async fn send(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }
    pub(crate) struct Handler;
    #[async_trait::async_trait]
    impl MessageHandler for Handler {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            Ok(echo_agent::channels::OutboundMessage::new(
                &message.channel_id,
                message.reply_target(),
                message.chat_type,
                "fixture",
            ))
        }
        async fn reply(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn state(temp: &std::path::Path) -> anyhow::Result<Arc<AppState>> {
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                .build()?,
        );
        Ok(Arc::new(
            AppState::from_shared(
                agent,
                None,
                Arc::new(crate::hitl::HitlDispatcher::new()),
                None,
                None,
                Default::default(),
                Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                    temp.join("mcp.json"),
                    Default::default(),
                )),
                crate::product_data_io::ProductDataIoService::new(),
            )?
            .with_config_path(temp.join("eko.yaml")),
        ))
    }

    struct RecoveryInputFixture {
        state: Arc<AppState>,
        channels: Arc<ChannelRuntime>,
        handler: Arc<dyn MessageHandler>,
        cancel: echo_agent::agent::CancellationToken,
        input: crate::conversation_input::ConversationInputReceipt,
        _channel: echo_agent::channels::weixin::WeixinChannel,
        _temporary: tempfile::TempDir,
    }

    async fn bootstrap_recovery_handler(
        handler: &Arc<dyn MessageHandler>,
        target: &str,
        cancel: &echo_agent::agent::CancellationToken,
    ) -> anyhow::Result<()> {
        let (receipt, observed) = echo_agent::channels::channel_ingress_receipt();
        handler
            .admit_with_ingress(
                echo_agent::channels::InboundMessage::new(
                    "weixin",
                    target,
                    target,
                    echo_agent::channels::ChatType::Direct,
                    "",
                    "bootstrap-empty",
                ),
                cancel.child_token(),
                receipt,
            )
            .await?;
        let bootstrap = tokio::time::timeout(std::time::Duration::from_secs(2), observed).await??;
        anyhow::ensure!(
            bootstrap == echo_agent::channels::ChannelIngressDisposition::TerminalRejected,
            "empty bootstrap unexpectedly entered execution"
        );
        Ok(())
    }

    impl RecoveryInputFixture {
        async fn new(binding_generation: &str) -> anyhow::Result<Self> {
            Self::new_with_binding(binding_generation, "global").await
        }

        async fn new_with_binding(
            binding_generation: &str,
            workspace_generation: &str,
        ) -> anyhow::Result<Self> {
            use crate::conversation_input::{
                ChannelExecutionIntent, ChannelInputBinding, ConversationInputAddress,
                ConversationInputSource, FrozenChannelContext, stable_scoped_input_id,
            };

            let temporary = tempfile::tempdir()?;
            let mut state = Arc::try_unwrap(state(temporary.path()).await?)
                .map_err(|_| anyhow::anyhow!("fixture AppState already has another owner"))?;
            state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
                temporary.path().join("events"),
                Default::default(),
            )?);
            state.workspace.global_execution_root = temporary.path().join("product-data");
            std::fs::create_dir_all(&state.workspace.global_execution_root)?;
            state.tasks.runtime = Some(Arc::new(
                crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
                    temporary.path().join("tasks"),
                    "global",
                )?,
            ));
            let state = Arc::new(state);
            let suffix = temporary
                .path()
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("fixture");
            let account = crate::config::WeixinAccountConfig {
                account_id: format!("offline-account-{suffix}"),
                bot_token: "offline-fixture".to_string(),
                base_url: "https://example.invalid".to_string(),
                user_id: "peer".to_string(),
                generation: "original-generation".to_string(),
            };
            // 原 producer 仅提供本机 handle，不 start/poll/send，也不读取真实凭据。
            let channel = echo_agent::channels::weixin::WeixinChannel::new(
                echo_agent::channels::weixin::WeixinConfig {
                    credentials: weixin_auth::credentials(&account),
                    store: Arc::new(echo_agent::memory::InMemoryStore::new()),
                },
            )?;
            let delivery = channel
                .delivery_handle()
                .ok_or_else(|| anyhow::anyhow!("original delivery handle missing"))?;
            let channels = Arc::new(ChannelRuntime::new(&state));
            *channels
                .weixin_delivery
                .lock()
                .map_err(|_| anyhow::anyhow!("original delivery slot unavailable"))? =
                Some((account.clone(), delivery));
            let (handler, _) = channels
                .handler(state.clone(), "weixin", 30)
                .await
                .map_err(anyhow::Error::msg)?;
            let cancel = echo_agent::agent::CancellationToken::new();
            handler.bind_durable_ingress(cancel.clone())?;
            let target =
                echo_agent::channels::weixin::encode_target(&account.account_id, &account.user_id)?;
            bootstrap_recovery_handler(&handler, &target, &cancel).await?;
            let address = ConversationInputAddress {
                workspace_id: "global".to_string(),
                conversation_id: crate::channel_runtime::AppChannelMessageHandler::conversation_id(
                    "weixin", &target, &target,
                ),
            };
            let provider_message = "original-provider-input";
            let source = serde_json::to_string(&(
                "weixin",
                &account.account_id,
                binding_generation,
                &account.user_id,
                provider_message,
            ))?;
            let input_id =
                stable_scoped_input_id(&address, ConversationInputSource::Channel, &source)?;
            let service = state.conversation_inputs();
            let saved = service
                .submit_channel(
                    address.clone(),
                    input_id,
                    "原待恢复消息".to_string(),
                    Vec::new(),
                    None,
                    ChannelInputBinding {
                        version: 1,
                        channel_id: "weixin".to_string(),
                        account_id: account.account_id,
                        credential_generation: binding_generation.to_string(),
                        peer_id: account.user_id,
                        provider_message_id: provider_message.to_string(),
                        workspace_generation: workspace_generation.to_string(),
                        context: FrozenChannelContext::Absent,
                        execution: ChannelExecutionIntent::Chat,
                    },
                )
                .await?;
            service
                .dispatch_selected(
                    saved.identity.clone(),
                    saved.queue_revision,
                    "original-input-turn".to_string(),
                )
                .await?;
            let attempt = service
                .list(&address)
                .await?
                .items
                .into_iter()
                .find(|item| item.receipt.identity == saved.identity)
                .and_then(|item| item.active_attempt)
                .ok_or_else(|| anyhow::anyhow!("original attempt missing"))?;
            service
                .recovery_required(attempt, "原 owner 丢失，副作用未知".to_string())
                .await?;
            let input = service
                .list(&address)
                .await?
                .items
                .into_iter()
                .find(|item| item.receipt.identity == saved.identity)
                .map(|item| item.receipt)
                .ok_or_else(|| anyhow::anyhow!("recovery input missing"))?;
            Ok(Self {
                state,
                channels,
                handler,
                cancel,
                input,
                _channel: channel,
                _temporary: temporary,
            })
        }

        async fn close(self) -> anyhow::Result<()> {
            self.cancel.cancel();
            self.handler.close().await?;
            self.channels
                .shutdown_and_wait()
                .await
                .map_err(anyhow::Error::msg)
        }
    }

    #[tokio::test]
    async fn recovery_handler_reconnect_reuses_the_original_coordinator_and_input()
    -> anyhow::Result<()> {
        let fixture = RecoveryInputFixture::new("original-generation").await?;
        let identity = fixture.input.identity.clone();
        let coordinator = fixture
            .channels
            .coordinators
            .lock()
            .map_err(|_| anyhow::anyhow!("coordinator registry unavailable"))?
            .get("weixin")
            .and_then(|entries| entries.last())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("original coordinator missing"))?;
        let (surface, old_wake) = coordinator
            .find_session_wake("weixin", &identity.address.conversation_id)
            .ok_or_else(|| anyhow::anyhow!("original session wake missing"))?;
        let old_incarnation = old_wake
            .wiring
            .session_instance
            .as_ref()
            .map(echo_agent::channels::ChannelSessionInstance::incarnation_id)
            .ok_or_else(|| anyhow::anyhow!("original incarnation missing"))?;
        drop(old_wake);
        fixture.handler.close().await?;
        let (reopened, _) = fixture
            .channels
            .handler(fixture.state.clone(), "weixin", 30)
            .await
            .map_err(anyhow::Error::msg)?;
        let reopened_cancel = echo_agent::agent::CancellationToken::new();
        reopened.bind_durable_ingress(reopened_cancel.clone())?;
        bootstrap_recovery_handler(&reopened, &surface.chat_id, &reopened_cancel).await?;
        let new_incarnation = coordinator
            .find_session_wake("weixin", &identity.address.conversation_id)
            .and_then(|(_, wake)| {
                wake.wiring
                    .session_instance
                    .as_ref()
                    .map(echo_agent::channels::ChannelSessionInstance::incarnation_id)
            });
        drop(coordinator);
        let result = fixture
            .channels
            .abandon_recovery_input(identity, fixture.input.queue_revision)
            .await;
        reopened_cancel.cancel();
        reopened.close().await?;
        drop(reopened);
        fixture.close().await?;
        result.map_err(anyhow::Error::msg)?;
        assert!(new_incarnation.is_some_and(|current| current != old_incarnation));
        Ok(())
    }

    #[tokio::test]
    async fn abandon_recovery_high_level_is_idempotent_for_the_exact_terminal_fact()
    -> anyhow::Result<()> {
        use crate::conversation_input::{ConversationInputFact, ConversationInputPhase};
        let fixture = RecoveryInputFixture::new("original-generation").await?;
        let identity = fixture.input.identity.clone();
        let revision = fixture.input.queue_revision;
        let first = fixture
            .channels
            .abandon_recovery_input(identity.clone(), revision)
            .await;
        let duplicate = fixture
            .channels
            .abandon_recovery_input(identity.clone(), revision)
            .await;
        let replay = fixture.state.storage.chat_events.replay(
            &identity.address.workspace_id,
            Some(&identity.address.conversation_id),
            &identity.input_id,
            0,
        );
        fixture.close().await?;
        let replay = replay?;
        let first = first.map_err(anyhow::Error::msg)?;
        let duplicate = duplicate.map_err(anyhow::Error::msg)?;
        let decode =
            |value: &Value| -> anyhow::Result<crate::conversation_input::ConversationInputReceipt> {
                Ok(serde_json::from_value(
                    value
                        .get("receipt")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("receipt missing"))?,
                )?)
            };
        let first = decode(&first)?;
        let duplicate = decode(&duplicate)?;
        assert_eq!(first.phase, ConversationInputPhase::Cancelled);
        assert!(!first.drained);
        assert_eq!(first.reason.as_deref(), Some("原 owner 丢失，副作用未知"));
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.identity, identity);
        let cancellations = replay.events.iter().filter(|event| {
            matches!(&event.payload, crate::chat_driver::ChatDriverEvent::InputLifecycle(fact)
                if matches!(fact.as_ref(), ConversationInputFact::Cancelled { identity: cancelled, .. } if cancelled == &identity))
        }).count();
        assert_eq!(cancellations, 1);
        Ok(())
    }

    #[tokio::test]
    async fn abandon_recovery_high_level_retains_owned_io_after_caller_drop() -> anyhow::Result<()>
    {
        use crate::conversation_input::{ConversationInputFact, ConversationInputPhase};
        use crate::foreground_turn::{ForegroundTurnError, ForegroundTurnSurface};
        let fixture = RecoveryInputFixture::new("original-generation").await?;
        let identity = fixture.input.identity.clone();
        let coordinator = fixture
            .channels
            .coordinators
            .lock()
            .map_err(|_| anyhow::anyhow!("coordinator registry unavailable"))?
            .get("weixin")
            .and_then(|entries| entries.last())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("original coordinator missing"))?;
        let (_, wake) = coordinator
            .find_session_wake("weixin", &identity.address.conversation_id)
            .ok_or_else(|| anyhow::anyhow!("original session wake missing"))?;
        let slot = wake
            .wiring
            .reply_slot
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original reply slot missing"))?;
        drop(wake);
        drop(coordinator);
        let wake_before = slot.observed_wake_epoch_for_test()?;
        let io = fixture.state.session.product_data_io.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        io.install_test_barrier(
            "commit abandoned channel recovery input",
            entered_tx,
            release_rx,
        );
        let channels = fixture.channels.clone();
        let action_identity = identity.clone();
        let revision = fixture.input.queue_revision;
        let caller = tokio::spawn(async move {
            channels
                .abandon_recovery_input(action_identity, revision)
                .await
        });
        let entered = tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx).await;
        caller.abort();
        let caller_exit = caller.await;
        let blocked_turn = fixture.state.session.foreground_turns.begin_scoped(
            "global",
            ForegroundTurnSurface::Gui,
            &identity.address.conversation_id,
            "must-wait-for-original-io",
        );
        let suspended = match blocked_turn {
            Err(ForegroundTurnError::ConversationAdmissionSuspended { .. }) => true,
            Err(_) => false,
            Ok(lease) => {
                lease
                    .settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled)
                    .await?;
                false
            }
        };
        let (_, revision) = fixture.state.config.configuration_snapshot().await;
        let stop = fixture.channels.control(
            ManagedChannel::Weixin,
            revision,
            None,
            None,
            ChannelControlAction::Stop,
        );
        let (stop_waited, released, stop_result) = {
            tokio::pin!(stop);
            match tokio::time::timeout(std::time::Duration::from_millis(25), stop.as_mut()).await {
                Err(_) => {
                    let released = release_tx.send(()).is_ok();
                    (true, released, stop.await)
                }
                Ok(result) => {
                    let released = release_tx.send(()).is_ok();
                    (false, released, result)
                }
            }
        };
        let input = fixture
            .state
            .storage
            .chat_events
            .channel_input_bindings()
            .await?
            .0
            .into_iter()
            .find(|item| item.receipt.identity == identity)
            .ok_or_else(|| anyhow::anyhow!("original input fact disappeared"))?;
        let wake_after = slot.observed_wake_epoch_for_test()?;
        let replay = fixture.state.storage.chat_events.replay(
            "global",
            Some(&identity.address.conversation_id),
            &identity.input_id,
            0,
        );
        fixture.close().await?;
        let io_settled = io.join_shutdown().await;
        entered.map_err(|_| anyhow::anyhow!("original IO was never admitted"))??;
        stop_result.map_err(anyhow::Error::msg)?;
        io_settled.map_err(anyhow::Error::msg)?;
        assert!(caller_exit.is_err_and(|error| error.is_cancelled()));
        assert!(suspended);
        assert!(stop_waited);
        assert!(released);
        assert_eq!(input.receipt.phase, ConversationInputPhase::Cancelled);
        assert!(!input.receipt.drained);
        assert_eq!(
            wake_after,
            wake_before
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("wake counter exhausted"))?
        );
        let cancellations = replay?
            .events
            .iter()
            .filter(|event| {
                matches!(&event.payload, crate::chat_driver::ChatDriverEvent::InputLifecycle(fact)
                    if matches!(fact.as_ref(), ConversationInputFact::Cancelled { identity: cancelled, .. } if cancelled == &identity))
            })
            .count();
        assert_eq!(cancellations, 1);
        Ok(())
    }

    #[tokio::test]
    async fn abandon_recovery_high_level_refuses_a_live_original_foreground() -> anyhow::Result<()>
    {
        use crate::conversation_input::ConversationInputPhase;
        let fixture = RecoveryInputFixture::new("original-generation").await?;
        let identity = fixture.input.identity.clone();
        let revision = fixture.input.queue_revision;
        let original_turn = fixture
            .input
            .turn_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original input turn missing"))?;
        let lease = fixture.state.session.foreground_turns.begin_scoped(
            "global",
            crate::foreground_turn::ForegroundTurnSurface::Channel,
            &identity.address.conversation_id,
            original_turn,
        )?;
        let result = fixture
            .channels
            .abandon_recovery_input(identity.clone(), fixture.input.queue_revision)
            .await;
        let after = fixture
            .state
            .conversation_inputs()
            .list(&identity.address)
            .await;
        let still_active = fixture
            .state
            .session
            .foreground_turns
            .snapshot_scoped(
                "global",
                crate::foreground_turn::ForegroundTurnSurface::Channel,
                &identity.address.conversation_id,
            )
            .is_some();
        lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled)
            .await?;
        fixture.close().await?;
        let after = after?;
        assert!(matches!(result, Err(error) if error.contains("active foreground turn")));
        assert!(still_active);
        assert_eq!(after.queue_revision, revision);
        assert!(
            after
                .items
                .iter()
                .any(|item| item.receipt.identity == identity
                    && item.receipt.phase == ConversationInputPhase::RecoveryRequired)
        );
        Ok(())
    }

    #[tokio::test]
    async fn abandon_recovery_high_level_refuses_stale_revision_and_credential_generation()
    -> anyhow::Result<()> {
        let fixture = RecoveryInputFixture::new("original-generation").await?;
        let stale = fixture
            .channels
            .abandon_recovery_input(
                fixture.input.identity.clone(),
                fixture.input.queue_revision.saturating_sub(1),
            )
            .await;
        let after = fixture
            .state
            .conversation_inputs()
            .list(&fixture.input.identity.address)
            .await;
        let revision = fixture.input.queue_revision;
        fixture.close().await?;
        let after = after?;
        assert!(matches!(stale, Err(error) if error.contains("原队列已变化")));
        assert_eq!(after.queue_revision, revision);
        let fixture = RecoveryInputFixture::new("old-credential-generation").await?;
        let wrong_generation = fixture
            .channels
            .abandon_recovery_input(fixture.input.identity.clone(), fixture.input.queue_revision)
            .await;
        let after = fixture
            .state
            .conversation_inputs()
            .list(&fixture.input.identity.address)
            .await;
        let revision = fixture.input.queue_revision;
        fixture.close().await?;
        let after = after?;
        assert!(matches!(wrong_generation, Err(error) if error.contains("原账户或凭据代次已变化")));
        assert_eq!(after.queue_revision, revision);
        let fixture = RecoveryInputFixture::new_with_binding(
            "original-generation",
            "stale-workspace-generation",
        )
        .await?;
        let wrong_workspace = fixture
            .channels
            .abandon_recovery_input(fixture.input.identity.clone(), fixture.input.queue_revision)
            .await;
        let after = fixture
            .state
            .conversation_inputs()
            .list(&fixture.input.identity.address)
            .await;
        let revision = fixture.input.queue_revision;
        fixture.close().await?;
        let after = after?;
        assert!(matches!(wrong_workspace, Err(error) if error.contains("原工作空间代次已变化")));
        assert_eq!(after.queue_revision, revision);
        Ok(())
    }

    #[tokio::test]
    async fn startup_notification_recovery_pending_is_not_a_channel_start_failure()
    -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let mut state = Arc::try_unwrap(state(temporary.path()).await?)
            .map_err(|_| anyhow::anyhow!("fixture AppState already shared"))?;
        let registry_root = temporary.path().join("workspaces");
        state.workspace.registry = Arc::new(
            crate::workspace::registry::WorkspaceRegistry::with_base_dir(registry_root.clone())?,
        );
        std::fs::write(
            registry_root.join("registry.json"),
            b"invalid registry fixture",
        )?;
        let state = Arc::new(state);
        let channels = ChannelRuntime::new(&state);
        let (started, failures, recovery) = channels.start_configured_with_receipt().await;
        channels
            .shutdown_and_wait()
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(started, 0);
        assert!(failures.is_empty());
        assert!(
            recovery
                .pending
                .iter()
                .any(|reason| reason.starts_with("workspace registry:"))
        );
        Ok(())
    }

    #[test]
    fn management_snapshot_serializes_weixin_authenticated_poll_evidence() {
        let evidence = serde_json::json!({
            "transport": {
                "evidence": transport_evidence_name(
                    ChannelTransportEvidence::WeixinAuthenticatedPoll,
                )
            }
        });
        assert_eq!(
            evidence["transport"]["evidence"],
            "weixin_authenticated_poll"
        );
    }

    #[tokio::test]
    async fn shutdown_retry_reports_original_failure_before_a_fresh_pending_observation()
    -> anyhow::Result<()> {
        let (plugin, probe) = test_support::channel(false, true, false);
        let channels = ChannelRuntime::with_original_test_plugin(Weak::new(), plugin)
            .map_err(anyhow::Error::msg)?;
        channels.begin_shutdown().map_err(anyhow::Error::msg)?;
        let mut first = {
            let slot = channels
                .shutdown
                .lock()
                .map_err(|_| anyhow::anyhow!("original channel observation unavailable"))?;
            slot.as_ref()
                .ok_or_else(|| anyhow::anyhow!("original cleanup not started"))?
                .receiver
                .clone()
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if first.borrow().is_some() {
                    return Ok::<_, anyhow::Error>(());
                }
                first.changed().await?;
            }
        })
        .await??;
        // 修改fixture回复本身不是完成证据；先真实等待同一原handle的第二次stop。
        probe.fail.store(false, Ordering::SeqCst);
        channels
            .retry_original_stop_for_test()
            .await
            .map_err(anyhow::Error::msg)?;
        // 根尚未观察过旧failure。合法retry必须仍返回那个实际error，不能只报新Pending。
        let observed = channels.observe_shutdown().await;
        let joined = channels.shutdown_and_wait().await;
        assert!(
            joined.is_ok(),
            "original cleanup retry did not physically finish: {joined:?}"
        );
        assert!(
            matches!(observed, Err(error) if error.contains("mock_stop_error")),
            "first observer lost the original failed stop fact when retry became legal"
        );
        assert_eq!(probe.stops.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn gui_all_start_failures_return_original_start_receipt() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let revision = state.config.configuration_snapshot().await.1;
        state
            .update_channels_config_owned(revision, |config| {
                config.qq.enabled = true;
                Ok(())
            })
            .await?;
        let channels = Arc::new(ChannelRuntime::new(&state));
        let (started, failures, _) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channels.start_configured_with_receipt(),
        )
        .await?;
        assert_eq!(started, 0);
        assert!(!failures.is_empty());
        channels
            .shutdown_and_wait()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_receipt_keeps_gui_empty_state_and_one_success_beside_one_failure()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let empty = ChannelRuntime::new(&state);
        let (started, failures, _) = empty.start_configured_with_receipt().await;
        assert_eq!(started, 0);
        assert!(failures.is_empty());
        let revision = state.config.configuration_snapshot().await.1;
        state
            .update_channels_config_owned(revision, |config| {
                config.qq.enabled = true;
                config.feishu.enabled = true;
                Ok(())
            })
            .await?;
        let (plugin, probe) = test_support::channel(false, false, false);
        let channels = ChannelRuntime::with_original_test_plugin(Arc::downgrade(&state), plugin)
            .map_err(anyhow::Error::msg)?;
        channels
            .handle("qqbot")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("original handle absent"))?
            .start(Arc::new(test_support::Handler))
            .await?;
        let (started, failures, _) = channels.start_configured_with_receipt().await;
        assert_eq!(started, 1);
        assert_eq!(failures.len(), 1);
        assert!(
            failures
                .iter()
                .all(|failure| failure.starts_with("feishu:"))
        );
        channels
            .shutdown_and_wait()
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn stale_channel_save_is_checked_inside_the_existing_owned_config_mutation()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let original = state.config.configuration_snapshot().await.1;
        state
            .update_channels_config_owned(original.clone(), |config| {
                config.qq.app_id = "committed-original-owner".into();
                Ok(())
            })
            .await?;
        let committed = state.config.configuration_snapshot().await.1;
        assert_ne!(original, committed);
        assert!(
            state
                .update_channels_config_owned(original, |config| {
                    config.qq.app_id = "stale-overwrite".into();
                    Ok(())
                })
                .await
                .is_err()
        );
        let (config, revision) = state.config.configuration_snapshot().await;
        assert_eq!(revision, committed);
        assert_eq!(config.channels.qq.app_id, "committed-original-owner");
        assert_eq!(
            crate::config::load_config_file(&temp.path().join("eko.yaml"))
                .map_err(anyhow::Error::msg)?
                .channels
                .qq
                .app_id,
            "committed-original-owner"
        );
        Ok(())
    }

    #[tokio::test]
    async fn global_model_save_does_not_strand_old_instance_and_late_stop_cannot_stop_successor()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let original = state.config.configuration_snapshot().await.1;
        let (plugin, old) = test_support::channel(false, false, false);
        let channels = ChannelRuntime::with_original_test_plugin(Arc::downgrade(&state), plugin)
            .map_err(anyhow::Error::msg)?;
        channels
            .configured_revisions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert("qqbot".to_string(), original.clone());
        let handle = channels
            .handle("qqbot")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("handle absent"))?;
        handle.start(Arc::new(test_support::Handler)).await?;
        let old_epoch = handle
            .management_snapshot()
            .await
            .mutation_epoch
            .to_string();
        state
            .upsert_model_provider_owned(crate::state::ModelProviderMutation {
                id: "inactive-fixture".to_string(),
                provider: crate::config::ModelProviderConfig {
                    base_url: Some("http://127.0.0.1:9/v1".to_string()),
                    requires_api_key: false,
                    ..Default::default()
                },
                preserve_auth_token: false,
            })
            .await?;
        let desired = state.config.configuration_snapshot().await.1;
        assert_ne!(desired, original);
        let observed = channels.status().await.map_err(anyhow::Error::msg)?;
        assert_eq!(
            observed
                .pointer("/channels/0/applied_configuration_revision")
                .and_then(Value::as_str),
            Some(original.as_str())
        );
        channels
            .control(
                ManagedChannel::Qqbot,
                original.clone(),
                Some(old_epoch.clone()),
                Some(original.clone()),
                ChannelControlAction::Stop,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(old.stops.load(Ordering::SeqCst), 1);
        let (plugin, new) = test_support::channel(false, false, false);
        handle.replace_stopped_plugin(plugin).await?;
        channels
            .configured_revisions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert("qqbot".to_string(), desired.clone());
        handle.start(Arc::new(test_support::Handler)).await?;
        assert!(
            channels
                .control(
                    ManagedChannel::Qqbot,
                    original.clone(),
                    Some(old_epoch),
                    Some(original),
                    ChannelControlAction::Stop
                )
                .await
                .is_err()
        );
        assert_eq!(new.stops.load(Ordering::SeqCst), 0);
        channels
            .retry_original_stop_for_test()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }
    #[tokio::test]
    async fn desired_removal_retains_unknown_original_transport_and_cleanup_identity()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let original = state.config.configuration_snapshot().await.1;
        let (plugin, probe) = test_support::channel(false, true, false);
        let channels = ChannelRuntime::with_original_test_plugin(Arc::downgrade(&state), plugin)
            .map_err(anyhow::Error::msg)?;
        channels
            .configured_revisions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert("qqbot".to_string(), original.clone());
        let handle = channels
            .handle("qqbot")
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("handle absent"))?;
        handle.start(Arc::new(test_support::Handler)).await?;
        let epoch = handle
            .management_snapshot()
            .await
            .mutation_epoch
            .to_string();
        state
            .update_channels_config_owned(original.clone(), |config| {
                config.qq.enabled = false;
                config.qq.client_secret.clear();
                Ok(())
            })
            .await?;
        assert!(
            channels
                .control(
                    ManagedChannel::Qqbot,
                    original.clone(),
                    Some(epoch),
                    Some(original.clone()),
                    ChannelControlAction::Stop
                )
                .await
                .is_err()
        );
        let observed = channels.status().await.map_err(anyhow::Error::msg)?;
        assert_eq!(
            observed
                .pointer("/channels/0/registered")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            observed
                .pointer("/channels/0/shutdown_settled")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            observed
                .pointer("/channels/0/applied_configuration_revision")
                .and_then(Value::as_str),
            Some(original.as_str())
        );
        probe.fail.store(false, Ordering::SeqCst);
        channels
            .retry_original_stop_for_test()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }
}
