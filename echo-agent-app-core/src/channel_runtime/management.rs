//! EKO controls around the original ChannelManager and session/pump owners.
//! Configuration publication remains ConfigState-owned. Transport observations
//! are projections; neither a closed watch nor a discarded waiter settles work.

use super::ChannelSessionCoordinator;
use crate::state::AppState;
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedChannel {
    Qqbot,
    Feishu,
}
impl ManagedChannel {
    pub fn id(self) -> &'static str {
        match self {
            Self::Qqbot => "qqbot",
            Self::Feishu => "feishu",
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
                    let _validated = plugin_from_config(channel, config)?;
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
        for channel in [ManagedChannel::Qqbot, ManagedChannel::Feishu] {
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
                let evidence = snapshot.transport.evidence.map(|evidence| match evidence {
                    ChannelTransportEvidence::QqSessionReady => "qq_session_ready",
                    ChannelTransportEvidence::QqSessionResumed => "qq_session_resumed",
                    ChannelTransportEvidence::FeishuWebSocketHandshake => {
                        "feishu_websocket_handshake"
                    }
                    ChannelTransportEvidence::LocalWebhookListener => "local_webhook_listener",
                });
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
    fn handler(
        &self,
        state: Arc<AppState>,
        id: &str,
        timeout: u64,
    ) -> Result<Arc<dyn MessageHandler>, String> {
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
        let end = coordinator.clone();
        let admission = self.accepting.clone();
        let gui_hitl = self.gui_hitl_factory.clone();
        let foreground = state.session.foreground_turns.clone();
        let webhook = state.webhook.emitter.clone();
        Ok(Arc::new(
            SessionHandler::new(
                super::channel_session_config(timeout),
                move |instance: &echo_agent::channels::ChannelSessionInstance| {
                    Box::new(
                        super::AppChannelMessageHandler::new(
                            state.clone(),
                            webhook.clone(),
                            foreground.clone(),
                            instance.clone(),
                            coordinator.clone(),
                        )
                        .with_admission_gate(admission.clone())
                        .with_gui_hitl_factory(gui_hitl.clone()),
                    ) as Box<dyn MessageHandler>
                },
            )
            .with_on_session_end(move |info| end.record_session_end(info)),
        ))
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
        if matches!(action, ChannelControlAction::Stop) {
            if let Some(handle) = current {
                self.stop_selected(&handle).await?;
            }
            return self.status().await;
        }
        let enabled = match channel {
            ManagedChannel::Qqbot => config.channels.qq.enabled,
            ManagedChannel::Feishu => config.channels.feishu.enabled,
        };
        if !enabled {
            return Err("Enable and save this channel before connecting".to_string());
        }
        if let Some(handle) = current.as_ref() {
            let snapshot = handle.management_snapshot().await;
            if matches!(action, ChannelControlAction::Start) && snapshot.handler_attached {
                return self.status().await;
            }
            self.stop_selected(handle).await?;
            if state.config.configuration_snapshot().await.1 != expected_revision {
                return Err(
                    "Configuration changed during cleanup; reload before connecting".to_string(),
                );
            }
            handle
                .replace_stopped_plugin(plugin_from_config(channel, &config.channels)?)
                .await
                .map_err(|error| error.to_string())?;
        } else {
            self.manager
                .lock()
                .map_err(|_| "Original channel manager unavailable".to_string())?
                .register(plugin_from_config(channel, &config.channels)?)
                .map_err(|error| error.to_string())?;
        }
        self.configured_revisions
            .lock()
            .map_err(|_| "Channel configuration receipt unavailable".to_string())?
            .insert(id.to_string(), expected_revision);
        let handle = self
            .handle(id)?
            .ok_or_else(|| "Original channel registration missing".to_string())?;
        let handler = self.handler(state, id, config.channels.session.timeout_minutes)?;
        let started = handle.start(handler).await;
        self.watch_original_transport(&handle).await?;
        started.map_err(|error| error.to_string())?;
        self.status().await
    }
    pub async fn start_configured(&self) -> Vec<String> {
        self.start_configured_with_receipt().await.1
    }
    pub(crate) async fn start_configured_with_receipt(&self) -> (usize, Vec<String>) {
        let Ok(state) = self.state() else {
            return (
                0,
                vec!["Original channel application owner unavailable".to_string()],
            );
        };
        let mut failures = Vec::new();
        let mut started = 0usize;
        for channel in [ManagedChannel::Qqbot, ManagedChannel::Feishu] {
            let (config, revision) = state.config.configuration_snapshot().await;
            let enabled = match channel {
                ManagedChannel::Qqbot => config.channels.qq.enabled,
                ManagedChannel::Feishu => config.channels.feishu.enabled,
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
        (started, failures)
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
        Ok(self
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
    pub(crate) async fn observe_shutdown(&self) -> Result<Vec<String>, String> {
        self.begin_shutdown()?;
        if self.no_registered_owners()? {
            return Ok(Vec::new());
        }
        let (receipt, closed) = {
            let slot = self
                .shutdown
                .lock()
                .map_err(|_| "Original cleanup slot unavailable".to_string())?;
            let observation = slot
                .as_ref()
                .ok_or_else(|| "Original cleanup not started".to_string())?;
            (
                observation.receiver.borrow().clone(),
                observation.receiver.has_changed().is_err(),
            )
        };
        let Some(receipt) = receipt else {
            return Err(if closed {
                "Channel cleanup observation closed without a receipt"
            } else {
                "Original channel cleanup is still pending"
            }
            .to_string());
        };
        if receipt.physically_settled {
            return Ok(receipt.result.err().into_iter().collect());
        }
        if receipt.retry_cleanup {
            let facts = original_shutdown_facts(&self.manager, &self.coordinators).await?;
            if receipt
                .failed_facts
                .as_ref()
                .is_some_and(|previous| previous != &facts)
            {
                let mut slot = self
                    .shutdown
                    .lock()
                    .map_err(|_| "Original cleanup slot unavailable".to_string())?;
                self.start_original_cleanup(&mut slot)?;
                return Err("Original channel cleanup retry is still pending".to_string());
            }
        }
        Err(receipt
            .result
            .err()
            .unwrap_or_else(|| "Original channel cleanup remains unconfirmed".to_string()))
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
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let producer = runtime.spawn(async move {
            let _operation = operation.lock().await;
            let mut failures = Vec::new();
            let mut physically_settled = true;
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
        ["qqbot", "feishu"]
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
        "session":{"timeout_minutes":config.session.timeout_minutes}})
}
fn plugin_from_config(
    channel: ManagedChannel,
    config: &crate::config::ChannelsConfig,
) -> Result<Box<dyn ChannelPlugin>, String> {
    match channel {
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
    #[tokio::test]
    async fn standalone_all_start_failures_return_without_waiting_for_shutdown()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let revision = state.config.configuration_snapshot().await.1;
        state
            .update_channels_config_owned(revision, |config| {
                config.qq.enabled = true;
                Ok(())
            })
            .await?;
        // Original QQ constructor rejects these missing credentials before any
        // network/session starts. This fixture never contacts a real platform.
        let channels = Arc::new(ChannelRuntime::new(&state));
        let cancel = echo_agent::agent::CancellationToken::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::super::run_channels_mode(super::super::ChannelsModeArgs {
                runtime: channels,
                shutdown: cancel.clone(),
            }),
        )
        .await?;
        assert!(result.is_err());
        assert!(!cancel.is_cancelled());
        Ok(())
    }

    #[tokio::test]
    async fn startup_receipt_keeps_gui_empty_state_and_one_success_beside_one_failure()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = state(temp.path()).await?;
        let empty = ChannelRuntime::new(&state);
        let (started, failures) = empty.start_configured_with_receipt().await;
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
        let (started, failures) = channels.start_configured_with_receipt().await;
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
