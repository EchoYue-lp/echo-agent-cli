//! Authentication is owned by the existing channel lifecycle, not the GUI waiter.
//! The single bounded slot retains the actual producer's join observation.

#[cfg(all(test, feature = "channels"))]
mod a2_consumer_tests {
    include!("weixin_a2_consumer_tests.rs");
}

use super::*;
use crate::config::WeixinAccountConfig;
use echo_agent::agent::CancellationToken;
use echo_agent::channels::weixin::{
    WeixinChannel, WeixinConfig, WeixinDeliveryHandle,
    api::WeixinApi,
    auth::{QrStatus, WeixinCredentials},
    decode_target,
};
use echo_agent::channels::{
    ChannelIngressDisposition, ChannelIngressReceipt, ChatType, InboundMessage, OutboundMessage,
};
use echo_agent::error::{ChannelError, ReactError};
use futures::stream::BoxStream;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

#[derive(Clone, Serialize)]
pub struct WeixinQrStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_id: Option<String>,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<String>,
    pub has_credential: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub configuration_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl std::fmt::Debug for WeixinQrStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeixinQrStatus")
            .field("phase", &self.phase)
            .field("has_credential", &self.has_credential)
            .field("configuration_revision", &self.configuration_revision)
            .finish_non_exhaustive()
    }
}

struct AuthOperation {
    id: String,
    cancel: tokio_util::sync::CancellationToken,
    codes: mpsc::Sender<String>,
    done: watch::Receiver<Option<Result<(), String>>>,
    // This observer awaits the one original producer exactly once.
    _observer: tokio::task::JoinHandle<()>,
}
#[derive(Default)]
pub(super) struct AuthOwner {
    slot: Mutex<Option<AuthOperation>>,
    // Reservation while the previous producer is settling; stop/exit also
    // invalidate this intent so an older refresh cannot resurrect afterwards.
    intent: Mutex<Option<String>>,
    view: Mutex<Option<WeixinQrStatus>>,
}
impl AuthOwner {
    fn lock_slot(&self) -> std::sync::MutexGuard<'_, Option<AuthOperation>> {
        self.slot.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(super) fn has_operation(&self) -> bool {
        self.lock_slot().is_some()
    }
    pub(super) fn cancel(&self) {
        self.intent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(slot) = self.lock_slot().as_ref() {
            slot.cancel.cancel();
        }
    }
    fn reserve(&self, id: &str) {
        *self
            .intent
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(id.to_string());
    }
    fn owns_intent(&self, id: &str) -> bool {
        self.intent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_deref()
            == Some(id)
    }
    pub(super) async fn wait(&self) -> Result<(), String> {
        let receiver = self.lock_slot().as_ref().map(|slot| slot.done.clone());
        if let Some(mut receiver) = receiver {
            loop {
                if receiver.borrow().is_some() {
                    // A failed operation can still be physically settled. Its
                    // error is retained separately, never retried as a future.
                    return Ok(());
                }
                receiver.changed().await.map_err(|_| {
                    "Weixin authentication ended without its original receipt".to_string()
                })?;
            }
        }
        Ok(())
    }
    pub(super) fn completed_failure(&self) -> Option<String> {
        self.lock_slot()
            .as_ref()
            .and_then(|slot| slot.done.borrow().clone())
            .and_then(Result::err)
    }
    fn publish(&self, id: &str, phase: &str, qr_svg: Option<String>, error: Option<String>) {
        let mut view = self.view.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(view) = view
            .as_mut()
            .filter(|view| view.challenge_id.as_deref() == Some(id))
        {
            view.phase = phase.to_owned();
            view.qr_svg = qr_svg;
            view.error = error;
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AuthIdentity {
    revision: String,
    epoch: Option<u64>,
    applied: Option<String>,
    generation: Option<String>,
}

pub(super) fn credentials(account: &WeixinAccountConfig) -> WeixinCredentials {
    WeixinCredentials {
        account_id: account.account_id.clone(),
        bot_token: account.bot_token.clone(),
        base_url: account.base_url.clone(),
        user_id: account.user_id.clone(),
        generation: account.generation.clone(),
    }
}

pub(super) type ConstructedPlugin = (
    Box<dyn ChannelPlugin>,
    Option<(WeixinAccountConfig, WeixinDeliveryHandle)>,
);

impl ChannelRuntime {
    async fn weixin_identity(&self) -> Result<AuthIdentity, String> {
        let state = self.state()?;
        let (config, revision) = state.config.configuration_snapshot().await;
        let epoch = if let Some(handle) = self.handle("weixin")? {
            Some(handle.management_snapshot().await.mutation_epoch)
        } else {
            None
        };
        let applied = self
            .configured_revisions
            .lock()
            .map_err(|_| "Original configuration receipt unavailable".to_string())?
            .get("weixin")
            .cloned();
        Ok(AuthIdentity {
            revision,
            epoch,
            applied,
            generation: config
                .channels
                .weixin
                .account
                .map(|account| account.generation),
        })
    }

    pub(super) fn weixin_configured_account(&self) -> Result<WeixinAccountConfig, String> {
        self.weixin_delivery
            .lock()
            .map_err(|_| "Weixin original delivery owner unavailable".to_string())?
            .as_ref()
            .map(|(account, _)| account.clone())
            .ok_or_else(|| "Weixin original instance is absent".to_string())
    }

    /// Projection of the original connected instance's captured runtime, not GUI selection.
    /// Capture destination, context and workspace under the original instance
    /// operation. Reading handle and workspace separately could mix a retired
    /// instance with its successor during an explicit rebind.
    pub async fn capture_weixin_notification(
        &self,
        definition_id: &str,
        expected_revision: &str,
    ) -> Result<crate::config::WeixinNotificationConfig, String> {
        let _operation = self.operation.lock().await;
        let state = self.state()?;
        let (config, revision) = state.config.configuration_snapshot().await;
        if revision != expected_revision {
            return Err(
                "Channel configuration changed; reload before selecting a destination".to_string(),
            );
        }
        let account = config
            .channels
            .weixin
            .account
            .as_ref()
            .ok_or_else(|| "Connect Weixin before enabling notifications".to_string())?;
        let handle = self.weixin_delivery_handle(&account.account_id, &account.generation)?;
        let conversation =
            echo_agent::channels::weixin::encode_target(&account.account_id, &account.user_id)
                .map_err(|_| "Weixin destination is invalid".to_string())?;
        let workspace_id = self.weixin_workspace_id()?;
        let context = handle
            .capture_context(&conversation)
            .await
            .map_err(|_| "Unable to read the original Weixin conversation".to_string())?
            .ok_or_else(|| {
                "Send a direct message from your phone before selecting this destination"
                    .to_string()
            })?;
        Ok(crate::config::WeixinNotificationConfig {
            definition_id: definition_id.to_string(),
            account_id: account.account_id.clone(),
            credential_generation: account.generation.clone(),
            peer_id: account.user_id.clone(),
            context_ref: serde_json::to_string(&context)
                .map_err(|_| "Unable to freeze Weixin context reference".to_string())?,
            workspace_id,
            channel_conversation: conversation,
        })
    }

    pub fn weixin_workspace_id(&self) -> Result<String, String> {
        self.weixin_workspace
            .lock()
            .map_err(|_| "Weixin original workspace binding unavailable".to_string())?
            .clone()
            .ok_or_else(|| "Connect Weixin before choosing a notification destination".to_string())
    }

    pub fn weixin_delivery_handle(
        &self,
        account_id: &str,
        generation: &str,
    ) -> Result<WeixinDeliveryHandle, String> {
        self.weixin_delivery
            .lock()
            .map_err(|_| "Weixin original delivery owner unavailable".to_string())?
            .as_ref()
            .filter(|(account, _)| {
                account.account_id == account_id && account.generation == generation
            })
            .map(|(_, handle)| handle.clone())
            .ok_or_else(|| "Weixin credential generation is no longer current".to_string())
    }

    pub(super) fn construct_plugin(
        &self,
        channel: ManagedChannel,
        config: &crate::config::ChannelsConfig,
    ) -> Result<ConstructedPlugin, String> {
        if !matches!(channel, ManagedChannel::Weixin) {
            return super::plugin_from_config(channel, config).map(|plugin| (plugin, None));
        }
        let account = config
            .weixin
            .account
            .as_ref()
            .ok_or_else(|| "Scan Weixin before connecting".to_string())?
            .clone();
        let store = Arc::new(
            echo_agent::memory::FileStore::new(crate::data_root::user_data_path(
                "channels/weixin/protocol.json",
            ))
            .map_err(|_| "Weixin protocol state could not be opened".to_string())?,
        );
        let plugin = WeixinChannel::new(WeixinConfig {
            credentials: credentials(&account),
            store,
        })
        .and_then(|plugin| plugin.with_bot_agent(&format!("EKO/{}", env!("CARGO_PKG_VERSION"))))
        .map_err(|_| "Weixin channel could not be initialized".to_string())?;
        let handle = plugin
            .delivery_handle()
            .ok_or_else(|| "Weixin original sender unavailable".to_string())?;
        Ok((Box::new(plugin), Some((account, handle))))
    }

    pub async fn weixin_status(
        &self,
        challenge_id: Option<String>,
    ) -> Result<WeixinQrStatus, String> {
        let state = self.state()?;
        let (config, revision) = state.config.configuration_snapshot().await;
        let mut view = self
            .weixin_auth
            .view
            .lock()
            .map_err(|_| "Weixin authentication observation unavailable".to_string())?
            .clone()
            .unwrap_or(WeixinQrStatus {
                challenge_id: None,
                phase: "idle".to_string(),
                qr_svg: None,
                has_credential: false,
                account_id: None,
                configuration_revision: revision.clone(),
                error: None,
            });
        if challenge_id
            .as_ref()
            .is_some_and(|id| view.challenge_id.as_ref() != Some(id))
        {
            return Err("Weixin challenge changed; reload its current status".to_string());
        }
        view.has_credential = config.channels.weixin.account.is_some();
        view.account_id = config
            .channels
            .weixin
            .account
            .as_ref()
            .map(|account| account.account_id.clone());
        view.configuration_revision = revision;
        Ok(view)
    }

    pub async fn weixin_begin(
        self: &Arc<Self>,
        expected_revision: String,
    ) -> Result<WeixinQrStatus, String> {
        self.begin_weixin_auth(expected_revision, None, Duration::from_secs(300))
            .await
    }

    // 测试只注入原 HTTP 客户端和更短 deadline；slot、producer、提交均走原路径。
    async fn begin_weixin_auth(
        self: &Arc<Self>,
        expected_revision: String,
        api: Option<WeixinApi>,
        lifetime: Duration,
    ) -> Result<WeixinQrStatus, String> {
        let _action = self.weixin_auth_action.lock().await;
        let id = uuid::Uuid::new_v4().to_string();
        {
            let _operation = self.operation.lock().await;
            if self.state()?.config.configuration_snapshot().await.1 != expected_revision {
                return Err("Channel configuration changed; reload before scanning".to_string());
            }
            self.weixin_auth.cancel();
            self.weixin_auth.reserve(&id);
        }
        self.weixin_auth.wait().await?;
        let _operation = self.operation.lock().await;
        if !self.weixin_auth.owns_intent(&id) {
            return Err(
                "Weixin authentication request was superseded by channel control".to_string(),
            );
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err("Channel management is shutting down".to_string());
        }
        let identity = self.weixin_identity().await?;
        if identity.revision != expected_revision {
            return Err("Channel configuration changed; reload before scanning".to_string());
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let (codes, received_codes) = mpsc::channel(1);
        let (done, receiver) = watch::channel(None);
        *self
            .weixin_auth
            .view
            .lock()
            .map_err(|_| "Weixin authentication observation unavailable".to_string())? =
            Some(WeixinQrStatus {
                challenge_id: Some(id.clone()),
                phase: "waiting_scan".to_string(),
                qr_svg: None,
                has_credential: false,
                account_id: None,
                configuration_revision: identity.revision.clone(),
                error: None,
            });
        let owner = self.clone();
        let producer_id = id.clone();
        let producer_cancel = cancel.clone();
        // Producer starts after slot publication: it may never commit an unowned challenge.
        let (start, started) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            if started.await.is_err() {
                return Ok(());
            }
            let outcome = owner
                .run_weixin_auth(
                    &producer_id,
                    identity,
                    producer_cancel.clone(),
                    received_codes,
                    api,
                    lifetime,
                )
                .await;
            if producer_cancel.is_cancelled() {
                owner
                    .weixin_auth
                    .publish(&producer_id, "cancelled", None, None);
            } else if outcome.is_err() {
                owner.weixin_auth.publish(
                    &producer_id,
                    "failed",
                    None,
                    Some("Weixin authentication failed; retry scanning".to_string()),
                );
            }
            outcome
        });
        let root = self.root();
        let auth = self.weixin_auth.clone();
        let observer_id = id.clone();
        let observer = tokio::spawn(async move {
            // Poll/validation failures are settled failed attempts, not live ownership.
            // Awaiting the original JoinHandle is the physical settlement fact.
            let result = match producer.await {
                Ok(result) => result,
                Err(_) => {
                    let error = "Weixin authentication producer failed".to_string();
                    auth.publish(&observer_id, "failed", None, Some(error.clone()));
                    Err(error)
                }
            };
            done.send_replace(Some(result));
            if let Some(root) = root.upgrade() {
                root.note_progress();
            }
        });
        *self.weixin_auth.lock_slot() = Some(AuthOperation {
            id: id.clone(),
            cancel,
            codes,
            done: receiver,
            _observer: observer,
        });
        let _ = start.send(());
        self.weixin_status(Some(id)).await
    }

    pub async fn weixin_verify(
        &self,
        challenge_id: String,
        code: String,
    ) -> Result<WeixinQrStatus, String> {
        if code.is_empty()
            || code.chars().count() > 32
            || !code.chars().all(|ch| ch.is_ascii_digit())
        {
            return Err("Enter the numeric verification code".to_string());
        }
        let _operation = self.operation.lock().await;
        let current = self.weixin_status(Some(challenge_id.clone())).await?;
        if current.phase != "need_verify_code" {
            return Err("Weixin challenge is not awaiting a verification code".to_string());
        }
        let slot = self.weixin_auth.lock_slot();
        let slot = slot
            .as_ref()
            .filter(|slot| slot.id == challenge_id && !slot.cancel.is_cancelled())
            .ok_or_else(|| "Weixin challenge is no longer active".to_string())?;
        slot.codes
            .try_send(code)
            .map_err(|_| "A verification attempt is already pending".to_string())?;
        Ok(current)
    }

    pub async fn weixin_cancel(&self, challenge_id: String) -> Result<WeixinQrStatus, String> {
        let _action = self.weixin_auth_action.lock().await;
        {
            let _operation = self.operation.lock().await;
            let slot = self.weixin_auth.lock_slot();
            let slot = slot
                .as_ref()
                .filter(|slot| slot.id == challenge_id)
                .ok_or_else(|| "Weixin challenge changed".to_string())?;
            slot.cancel.cancel();
        }
        self.weixin_auth.wait().await?;
        self.weixin_status(Some(challenge_id)).await
    }

    pub async fn weixin_forget(&self, expected_revision: String) -> Result<(), String> {
        let _action = self.weixin_auth_action.lock().await;
        let _operation = self.operation.lock().await;
        let state = self.state()?;
        if state.config.configuration_snapshot().await.1 != expected_revision {
            return Err("Channel configuration changed; reload before forgetting".to_string());
        }
        self.weixin_auth.cancel();
        self.weixin_auth.wait().await?;
        let (config, revision) = state.config.configuration_snapshot().await;
        if revision != expected_revision {
            return Err("Channel configuration changed; reload before forgetting".to_string());
        }
        let account = config.channels.weixin.account;
        if let Some(handle) = self.handle("weixin")? {
            self.stop_selected(&handle).await?;
        }
        let retained = self
            .weixin_delivery
            .lock()
            .map_err(|_| "Weixin original sender unavailable".to_string())?
            .as_ref()
            .cloned();
        state
            .update_channels_config_owned(expected_revision, |channels| {
                channels.weixin.account = None;
                channels.weixin.enabled = false;
                Ok(())
            })
            .await
            .map_err(|_| {
                "Weixin credentials could not be removed; reload configuration".to_string()
            })?;
        // Config removal is committed before deleting recoverable protocol state.
        // On a cleanup failure retain this original handle for an explicit retry.
        if let Some((_, delivery)) = retained {
            delivery
                .clear_protocol_state()
                .await
                .map_err(|_| "Weixin protocol state could not be removed".to_string())?;
        } else if let Some(account) = account {
            // No transport was constructed this process. The original operation
            // still owns quiescence, and only this saved namespace is removed.
            let store = Arc::new(
                echo_agent::memory::FileStore::new(crate::data_root::user_data_path(
                    "channels/weixin/protocol.json",
                ))
                .map_err(|_| "Weixin protocol state could not be opened".to_string())?,
            );
            WeixinChannel::forget_protocol_state(WeixinConfig {
                credentials: credentials(&account),
                store,
            })
            .await
            .map_err(|_| "Weixin protocol state could not be removed".to_string())?;
        }
        self.weixin_delivery
            .lock()
            .map_err(|_| "Weixin original sender unavailable".to_string())?
            .take();
        *self
            .weixin_auth
            .view
            .lock()
            .map_err(|_| "Weixin authentication observation unavailable".to_string())? = None;
        Ok(())
    }

    async fn run_weixin_auth(
        &self,
        id: &str,
        identity: AuthIdentity,
        cancel: tokio_util::sync::CancellationToken,
        mut codes: mpsc::Receiver<String>,
        api: Option<WeixinApi>,
        lifetime: Duration,
    ) -> Result<(), String> {
        let api = match api {
            Some(api) => api,
            None => WeixinApi::new()
                .and_then(|api| api.with_bot_agent(&format!("EKO/{}", env!("CARGO_PKG_VERSION"))))
                .map_err(|_| "Weixin HTTPS client unavailable".to_string())?,
        };
        // This host never imports another application's saved local_token_list.
        let mut challenge = tokio::select! {biased;_ = cancel.cancelled()=>return Ok(()), result=api.get_qrcode(&[])=>result.map_err(|_|"Weixin QR challenge unavailable".to_string())?};
        let svg = qr_svg(&challenge.qrcode_img_content)?;
        self.weixin_auth
            .publish(id, "waiting_scan", Some(svg.clone()), None);
        let deadline = tokio::time::Instant::now() + lifetime;
        let mut needs_code = false;
        loop {
            let code = if needs_code {
                tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),_=tokio::time::sleep_until(deadline)=>{self.weixin_auth.publish(id,"expired",None,None);return Ok(());},code=codes.recv()=>Some(code.ok_or_else(||"Weixin verification input closed".to_string())?)}
            } else {
                codes.try_recv().ok()
            };
            let observed = tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),_=tokio::time::sleep_until(deadline)=>{self.weixin_auth.publish(id,"expired",None,None);return Ok(());},result=api.get_qrcode_status(&challenge,code.as_deref())=>result};
            // 已消费验证码不因超时自动重发；继续只观察同一 challenge 的状态。
            needs_code = false;
            let status = match observed {
                Ok(status) => status,
                Err(ReactError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
                    // 单次长轮询超时不等于二维码失效。原绝对 deadline 不重新计时。
                    tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),_=tokio::time::sleep_until(deadline)=>{self.weixin_auth.publish(id,"expired",None,None);return Ok(());},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
                    continue;
                }
                Err(_) => return Err("Weixin QR status unavailable".to_string()),
            };
            match status {
                QrStatus::Wait => {
                    self.weixin_auth
                        .publish(id, "waiting_scan", Some(svg.clone()), None)
                }
                QrStatus::Scanned => {
                    self.weixin_auth
                        .publish(id, "waiting_confirmation", Some(svg.clone()), None)
                }
                QrStatus::NeedVerifyCode => {
                    needs_code = true;
                    self.weixin_auth
                        .publish(id, "need_verify_code", Some(svg.clone()), None);
                }
                QrStatus::Expired => {
                    self.weixin_auth.publish(id, "expired", None, None);
                    return Ok(());
                }
                QrStatus::VerifyCodeBlocked => {
                    self.weixin_auth
                        .publish(id, "verification_blocked", None, None);
                    return Ok(());
                }
                QrStatus::Redirect { base_url } => challenge.polling_base_url = base_url,
                QrStatus::AlreadyBound => {
                    self.weixin_auth.publish(id,"failed",None,Some("Weixin returned no new credentials; disconnect the previous host and scan again".to_string()));
                    return Ok(());
                }
                QrStatus::Confirmed(account) => {
                    // Cancellation wins before lock acquisition. No network poll holds operation.
                    let _operation = tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),guard=self.operation.lock()=>guard};
                    if cancel.is_cancelled()
                        || !self.accepting.load(Ordering::Acquire)
                        || self.weixin_identity().await? != identity
                        || !self.weixin_auth.owns_intent(id)
                        || !self
                            .weixin_auth
                            .lock_slot()
                            .as_ref()
                            .is_some_and(|slot| slot.id == id)
                    {
                        self.weixin_auth.publish(
                            id,
                            "cancelled",
                            None,
                            Some(
                                "Channel configuration or original instance changed; scan again"
                                    .to_string(),
                            ),
                        );
                        return Ok(());
                    }
                    account
                        .validate()
                        .map_err(|_| "Weixin confirmed identity was incomplete".to_string())?;
                    let account = WeixinAccountConfig {
                        account_id: account.account_id,
                        bot_token: account.bot_token,
                        base_url: account.base_url,
                        user_id: account.user_id,
                        generation: account.generation,
                    };
                    self.state()?
                        .update_channels_config_owned(identity.revision.clone(), move |channels| {
                            channels.weixin.account = Some(account);
                            channels.weixin.enabled = true;
                            Ok(())
                        })
                        .await
                        .map_err(|_| {
                            "Weixin credential commit failed; reload configuration".to_string()
                        })?;
                    self.weixin_auth.publish(id, "authorized", None, None);
                    // Starting is a separate real lifecycle operation; authorization never says Ready.
                    return Ok(());
                }
            }
            tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
        }
    }
}

fn qr_svg(payload: &str) -> Result<String, String> {
    let code = qrcodegen::QrCode::encode_text(payload, qrcodegen::QrCodeEcc::Medium)
        .map_err(|_| "Weixin QR content cannot be encoded".to_string())?;
    let size = code.size();
    let canvas = size
        .checked_add(8)
        .ok_or_else(|| "Invalid QR dimensions".to_string())?;
    let mut path = String::new();
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x, y) {
                path.push_str(&format!(
                    "M{},{}h1v1h-1z",
                    x.saturating_add(4),
                    y.saturating_add(4)
                ));
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {canvas} {canvas}\" shape-rendering=\"crispEdges\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><path d=\"{path}\" fill=\"black\"/></svg>"
    ))
}

/// This is an EKO policy around the original SessionHandler, before session creation.
/// A different remote peer must not trigger local tools on the user's machine.
pub(super) struct ScannerHandler {
    inner: Arc<dyn MessageHandler>,
    account: WeixinAccountConfig,
    coordinator: Option<Arc<super::super::ChannelSessionCoordinator>>,
}
impl ScannerHandler {
    pub(super) fn new(inner: Arc<dyn MessageHandler>, account: WeixinAccountConfig) -> Self {
        Self {
            inner,
            account,
            coordinator: None,
        }
    }
    pub(super) fn with_coordinator(
        mut self,
        coordinator: Arc<super::super::ChannelSessionCoordinator>,
    ) -> Self {
        self.coordinator = Some(coordinator);
        self
    }
    fn allows(&self, msg: &InboundMessage) -> bool {
        msg.channel_id == "weixin"
            && msg.chat_type == ChatType::Direct
            && msg.attachments.is_empty()
            && msg.sender_id == msg.chat_id
            && decode_target(&msg.chat_id).is_ok_and(|target| {
                target.account_id == self.account.account_id && target.peer == self.account.user_id
            })
    }
    fn denied() -> ReactError {
        ReactError::Channel(Box::new(ChannelError::Other(
            "Weixin message is not from the authorized scanner or contains unsupported media"
                .to_string(),
        )))
    }
}
#[async_trait::async_trait]
impl MessageHandler for ScannerHandler {
    async fn handle(&self, msg: InboundMessage) -> echo_agent::error::Result<OutboundMessage> {
        if !self.allows(&msg) {
            return Err(Self::denied());
        }
        self.inner.handle(msg).await
    }
    async fn reply(&self, msg: OutboundMessage) -> echo_agent::error::Result<()> {
        self.inner.reply(msg).await
    }
    async fn handle_with_cancel(
        &self,
        msg: InboundMessage,
        cancel: CancellationToken,
    ) -> echo_agent::error::Result<OutboundMessage> {
        if !self.allows(&msg) {
            return Err(Self::denied());
        }
        self.inner.handle_with_cancel(msg, cancel).await
    }
    async fn reply_with_cancel(
        &self,
        msg: OutboundMessage,
        cancel: CancellationToken,
    ) -> echo_agent::error::Result<()> {
        self.inner.reply_with_cancel(msg, cancel).await
    }
    async fn close(&self) -> echo_agent::error::Result<()> {
        // Service close cancels only the child originally bound by transport.
        // It must observe the SAME coordinator group even when called outside
        // ChannelRuntime::stop_selected. Session expiry closes inner sessions,
        // not this service wrapper, so it cannot shut down unrelated sessions.
        if let Some(coordinator) = self.coordinator.as_ref() {
            let cancel = coordinator
                .ingress_service_cancel
                .lock()
                .map_err(|_| {
                    ReactError::Other("original service cancellation owner unavailable".to_string())
                })?
                .clone();
            if let Some(cancel) = cancel {
                cancel.cancel();
            }
        }
        let inner = self.inner.close().await;
        let pumps = match self.coordinator.as_ref() {
            Some(coordinator) => coordinator
                .observe_input_pump_settlement()
                .await
                .map(|_| ()),
            None => Ok(()),
        };
        match (inner, pumps) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(ReactError::Other(error)),
            (Err(inner), Err(pumps)) => Err(ReactError::Other(format!(
                "{inner}; original channel group: {pumps}"
            ))),
        }
    }
    fn settles_on_cancel(&self) -> bool {
        self.inner.settles_on_cancel()
    }
    fn has_pending_work(&self) -> bool {
        self.inner.has_pending_work()
    }
    async fn handle_stream<'a>(
        &'a self,
        msg: InboundMessage,
    ) -> echo_agent::error::Result<BoxStream<'a, echo_agent::error::Result<OutboundMessage>>> {
        if !self.allows(&msg) {
            return Err(Self::denied());
        }
        self.inner.handle_stream(msg).await
    }
    async fn handle_stream_with_cancel<'a>(
        &'a self,
        msg: InboundMessage,
        cancel: CancellationToken,
    ) -> echo_agent::error::Result<BoxStream<'a, echo_agent::error::Result<OutboundMessage>>> {
        if !self.allows(&msg) {
            return Err(Self::denied());
        }
        self.inner.handle_stream_with_cancel(msg, cancel).await
    }
    fn bind_durable_ingress(&self, cancel: CancellationToken) -> echo_agent::error::Result<()> {
        self.inner.bind_durable_ingress(cancel.clone())?;
        if let Some(coordinator) = self.coordinator.as_ref() {
            *coordinator.ingress_service_cancel.lock().map_err(|_| {
                ReactError::Other("original channel service token unavailable".to_string())
            })? = Some(cancel);
        }
        Ok(())
    }
    async fn admit_with_ingress(
        &self,
        msg: InboundMessage,
        cancel: CancellationToken,
        receipt: ChannelIngressReceipt,
    ) -> echo_agent::error::Result<()> {
        if !self.allows(&msg) {
            receipt.publish(ChannelIngressDisposition::TerminalRejected);
            return Ok(());
        }
        self.inner.admit_with_ingress(msg, cancel, receipt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn account() -> WeixinAccountConfig {
        WeixinAccountConfig {
            account_id: "account-a".to_string(),
            bot_token: "secret-token".to_string(),
            base_url: "https://ilinkai.weixin.qq.com".to_string(),
            user_id: "scanner-a".to_string(),
            generation: "credential-generation-a".to_string(),
        }
    }

    #[derive(Clone, Copy)]
    enum QrFixtureCase {
        TimeoutOnce,
        InvalidProtocol,
        Unauthorized,
        HeldConfirmation,
        VerifyTimeout,
        AlwaysTimeout,
    }

    struct QrFixtureProvider {
        case: QrFixtureCase,
        polls: AtomicUsize,
        challenges: Mutex<Vec<String>>,
        codes: Mutex<Vec<Option<String>>>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    struct AuthHttpFixture {
        _temp: tempfile::TempDir,
        state: Arc<crate::state::AppState>,
        runtime: Arc<ChannelRuntime>,
        provider: Arc<QrFixtureProvider>,
        address: std::net::SocketAddr,
        stop: tokio_util::sync::CancellationToken,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
        config_path: std::path::PathBuf,
    }

    async fn fixture_qr_begin() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "qrcode":"original-qr-fixture", "qrcode_img_content":"https://example.invalid/qr-fixture"
        }))
    }

    async fn fixture_qr_status(
        axum::extract::State(provider): axum::extract::State<Arc<QrFixtureProvider>>,
        axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let poll = provider.polls.fetch_add(1, Ordering::SeqCst);
        provider
            .challenges
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(query.get("qrcode").cloned().unwrap_or_default());
        provider
            .codes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(query.get("verify_code").cloned());
        provider.entered.notify_one();
        match provider.case {
            QrFixtureCase::Unauthorized => {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            QrFixtureCase::InvalidProtocol => {
                return axum::Json(serde_json::json!({"status":"confirmed"})).into_response();
            }
            QrFixtureCase::HeldConfirmation => provider.release.notified().await,
            QrFixtureCase::VerifyTimeout if poll == 0 => {
                return axum::Json(serde_json::json!({"status":"need_verifycode"})).into_response();
            }
            QrFixtureCase::TimeoutOnce if poll == 0 => {
                tokio::time::sleep(Duration::from_millis(150)).await
            }
            QrFixtureCase::VerifyTimeout if poll == 1 => {
                tokio::time::sleep(Duration::from_millis(150)).await
            }
            QrFixtureCase::AlwaysTimeout => tokio::time::sleep(Duration::from_millis(150)).await,
            _ => {}
        }
        axum::Json(serde_json::json!({
            "status":"confirmed", "ilink_bot_id":"account-a", "bot_token":"fixture-private-token",
            "baseurl":"https://ilinkai.weixin.qq.com", "ilink_user_id":"scanner-a"
        }))
        .into_response()
    }

    impl AuthHttpFixture {
        async fn new(case: QrFixtureCase) -> anyhow::Result<Self> {
            // 不更改进程环境。验证调度器必须在启动测试进程前指定自有数据根。
            let data_root = std::env::var_os("EKO_DATA_DIR")
                .ok_or_else(|| anyhow::anyhow!("认证回归必须由调度器指定 EKO_DATA_DIR"))?;
            anyhow::ensure!(
                std::path::Path::new(&data_root).is_absolute(),
                "测试数据根必须为绝对路径"
            );
            let temp = tempfile::tempdir()?;
            let config_path = temp.path().join("eko.yaml");
            let agent = crate::agent_handle::AgentHandle::new(
                echo_agent::agent::ReactAgentBuilder::new()
                    .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                    .build()?,
            );
            let state = Arc::new(
                crate::state::AppState::from_shared(
                    agent,
                    None,
                    Arc::new(crate::hitl::HitlDispatcher::new()),
                    None,
                    None,
                    Default::default(),
                    Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                        temp.path().join("mcp.json"),
                        Default::default(),
                    )),
                    crate::product_data_io::ProductDataIoService::new(),
                )?
                .with_config_path(config_path.clone()),
            );
            let runtime = Arc::new(ChannelRuntime::new(&state));
            let provider = Arc::new(QrFixtureProvider {
                case,
                polls: AtomicUsize::new(0),
                challenges: Mutex::new(Vec::new()),
                codes: Mutex::new(Vec::new()),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            let listener =
                tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
            let address = listener.local_addr()?;
            let router = axum::Router::new()
                .route(
                    "/ilink/bot/get_bot_qrcode",
                    axum::routing::post(fixture_qr_begin),
                )
                .route(
                    "/ilink/bot/get_qrcode_status",
                    axum::routing::get(fixture_qr_status),
                )
                .with_state(provider.clone());
            let stop = tokio_util::sync::CancellationToken::new();
            let server_cancel = stop.clone();
            let server = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(server_cancel.cancelled_owned())
                    .await
            });
            Ok(Self {
                _temp: temp,
                state,
                runtime,
                provider,
                address,
                stop,
                server,
                config_path,
            })
        }

        async fn begin(&self, lifetime: Duration) -> anyhow::Result<String> {
            let revision = self.state.config.configuration_snapshot().await.1;
            let api = WeixinApi::for_local_auth_fixture(self.address)?;
            let view = self
                .runtime
                .begin_weixin_auth(revision, Some(api), lifetime)
                .await
                .map_err(anyhow::Error::msg)?;
            view.challenge_id
                .ok_or_else(|| anyhow::anyhow!("原认证槽未发布 challenge"))
        }

        async fn settle(&self, challenge: &str) -> anyhow::Result<WeixinQrStatus> {
            tokio::time::timeout(Duration::from_secs(3), self.runtime.weixin_auth.wait())
                .await?
                .map_err(anyhow::Error::msg)?;
            self.runtime
                .weixin_status(Some(challenge.to_string()))
                .await
                .map_err(anyhow::Error::msg)
        }

        async fn close(self) -> anyhow::Result<()> {
            self.runtime.weixin_auth.cancel();
            // 等待条件失败也先撤回准入、释放本机 held response，再观察两个原 owner。
            self.provider.release.notify_one();
            let authentication = self.runtime.weixin_auth.wait().await;
            self.stop.cancel();
            let server = tokio::time::timeout(Duration::from_secs(3), self.server).await;
            let server = match server {
                Ok(Ok(result)) => result.map_err(anyhow::Error::from),
                Ok(Err(error)) => Err(anyhow::Error::from(error)),
                Err(error) => Err(anyhow::Error::from(error)),
            };
            match (authentication, server) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) => Err(anyhow::Error::msg(error)),
                (Ok(()), Err(error)) => Err(error),
                (Err(authentication), Err(server)) => Err(anyhow::anyhow!(
                    "认证结算失败：{authentication}；HTTP 结算失败：{server}"
                )),
            }
        }

        fn verify_same_challenge(&self) -> anyhow::Result<()> {
            let challenges = self
                .provider
                .challenges
                .lock()
                .map_err(|_| anyhow::anyhow!("夹具 challenge 观察锁损坏"))?;
            anyhow::ensure!(!challenges.is_empty(), "未经过原 poll HTTP 请求");
            anyhow::ensure!(
                challenges
                    .iter()
                    .all(|challenge| challenge == "original-qr-fixture"),
                "轮询重建或切换了 challenge"
            );
            Ok(())
        }
    }

    #[tokio::test]
    async fn actual_auth_timeout_then_same_challenge_commits_once() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::TimeoutOnce).await?;
        let challenge = fixture.begin(Duration::from_secs(2)).await?;
        let view = fixture.settle(&challenge).await?;
        let polls = fixture.provider.polls.load(Ordering::SeqCst);
        let same_challenge = fixture.verify_same_challenge();
        let (config, revision) = fixture.state.config.configuration_snapshot().await;
        let disk = crate::config::load_config_file(&fixture.config_path);
        let observed_revision = fixture
            .runtime
            .weixin_status(Some(challenge))
            .await
            .map_err(anyhow::Error::msg)?
            .configuration_revision;
        fixture.close().await?;

        assert_eq!(view.phase, "authorized");
        assert_eq!(polls, 2);
        same_challenge?;
        let saved = config
            .channels
            .weixin
            .account
            .ok_or_else(|| anyhow::anyhow!("原配置未保存凭据"))?;
        assert_eq!(saved.account_id, "account-a");
        assert_eq!(saved.bot_token, "fixture-private-token");
        assert!(!saved.generation.is_empty());
        assert_eq!(
            disk.map_err(anyhow::Error::msg)?
                .channels
                .weixin
                .account
                .map(|account| account.generation),
            Some(saved.generation)
        );
        assert_eq!(observed_revision, revision);
        assert!(!serde_json::to_string(&view)?.contains("fixture-private-token"));
        Ok(())
    }

    #[tokio::test]
    async fn actual_auth_permanent_protocol_and_auth_errors_do_not_retry() -> anyhow::Result<()> {
        for case in [QrFixtureCase::InvalidProtocol, QrFixtureCase::Unauthorized] {
            let fixture = AuthHttpFixture::new(case).await?;
            let challenge = fixture.begin(Duration::from_secs(2)).await?;
            let view = fixture.settle(&challenge).await?;
            assert_eq!(view.phase, "failed");
            assert_eq!(fixture.provider.polls.load(Ordering::SeqCst), 1);
            assert!(
                fixture
                    .state
                    .config
                    .configuration_snapshot()
                    .await
                    .0
                    .channels
                    .weixin
                    .account
                    .is_none()
            );
            assert!(!fixture.config_path.exists());
            fixture.close().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn actual_auth_cancel_rejects_late_confirmation() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::HeldConfirmation).await?;
        let challenge = fixture.begin(Duration::from_secs(2)).await?;
        tokio::time::timeout(Duration::from_secs(1), fixture.provider.entered.notified()).await?;
        let cancelled = fixture
            .runtime
            .weixin_cancel(challenge.clone())
            .await
            .map_err(anyhow::Error::msg)?;
        fixture.provider.release.notify_one();
        assert_eq!(cancelled.phase, "cancelled");
        assert_eq!(fixture.settle(&challenge).await?.phase, "cancelled");
        assert!(
            fixture
                .state
                .config
                .configuration_snapshot()
                .await
                .0
                .channels
                .weixin
                .account
                .is_none()
        );
        assert!(!fixture.config_path.exists());
        fixture.close().await
    }

    #[tokio::test]
    async fn actual_auth_repeated_timeouts_keep_original_expiry() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::AlwaysTimeout).await?;
        let challenge = fixture.begin(Duration::from_millis(650)).await?;
        assert_eq!(fixture.settle(&challenge).await?.phase, "expired");
        let polls = fixture.provider.polls.load(Ordering::SeqCst);
        assert!((2..=3).contains(&polls));
        fixture.verify_same_challenge()?;
        assert!(
            fixture
                .state
                .config
                .configuration_snapshot()
                .await
                .0
                .channels
                .weixin
                .account
                .is_none()
        );
        fixture.close().await
    }

    #[tokio::test]
    async fn actual_auth_credential_change_rejects_late_confirmation() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::HeldConfirmation).await?;
        let elapsed = std::time::Instant::now();
        let challenge = fixture.begin(Duration::from_secs(2)).await?;
        tokio::time::timeout(Duration::from_secs(1), fixture.provider.entered.notified()).await?;
        eprintln!(
            "AUTH_CREDENTIAL_PHASE:held-confirmation elapsed_ms={}",
            elapsed.elapsed().as_millis()
        );
        let revision = fixture.state.config.configuration_snapshot().await.1;
        let mut replacement = account();
        replacement.generation = "replacement-generation".to_string();
        fixture
            .state
            .update_channels_config_owned(revision, move |channels| {
                channels.weixin.account = Some(replacement);
                Ok(())
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        eprintln!(
            "AUTH_CREDENTIAL_PHASE:replacement-owned-persist-return elapsed_ms={}",
            elapsed.elapsed().as_millis()
        );
        fixture.provider.release.notify_one();
        let observed = fixture.settle(&challenge).await;
        if let Ok(settled) = &observed {
            eprintln!(
                "AUTH_CREDENTIAL_PHASE:original-settle phase={} elapsed_ms={}",
                settled.phase,
                elapsed.elapsed().as_millis()
            );
        }
        let generation = fixture
            .state
            .config
            .configuration_snapshot()
            .await
            .0
            .channels
            .weixin
            .account
            .map(|account| account.generation);
        let closed = fixture.close().await;
        let settled = observed?;
        assert_eq!(settled.phase, "cancelled");
        assert_eq!(generation.as_deref(), Some("replacement-generation"));
        closed
    }

    #[tokio::test]
    async fn actual_auth_channel_epoch_change_rejects_late_confirmation() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::HeldConfirmation).await?;
        let observed = async {
            let config = WeixinConfig {
                credentials: credentials(&account()),
                store: Arc::new(echo_agent::memory::FileStore::new(
                    fixture._temp.path().join("protocol.json"),
                )?),
            };
            fixture
                .runtime
                .manager
                .lock()
                .map_err(|_| anyhow::anyhow!("原 manager 锁损坏"))?
                .register(Box::new(WeixinChannel::new(config.clone())?))?;
            let challenge = fixture.begin(Duration::from_secs(2)).await?;
            tokio::time::timeout(Duration::from_secs(1), fixture.provider.entered.notified())
                .await?;
            let handle = fixture
                .runtime
                .handle("weixin")
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow::anyhow!("原 channel entry 缺失"))?;
            let before = handle.management_snapshot().await.mutation_epoch;
            handle.stop().await?;
            // Stop 只确认原 entry 已静止。正式替换才推进 epoch；不启动微信或手改计数。
            handle
                .replace_stopped_plugin(Box::new(WeixinChannel::new(config)?))
                .await?;
            let after = handle.management_snapshot().await.mutation_epoch;
            fixture.provider.release.notify_one();
            let phase = fixture.settle(&challenge).await?.phase;
            let no_credential = fixture
                .state
                .config
                .configuration_snapshot()
                .await
                .0
                .channels
                .weixin
                .account
                .is_none();
            let manifest_absent = !fixture.config_path.exists();
            handle.stop().await?;
            Ok::<_, anyhow::Error>((before, after, phase, no_credential, manifest_absent))
        }
        .await;
        // 包含中途 Result 失败的路径也必须显式回收原认证 producer 和 HTTP 服务。
        let closed = fixture.close().await;
        let (before, after, phase, no_credential, manifest_absent) = match (observed, closed) {
            (Ok(facts), Ok(())) => facts,
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(observation), Err(cleanup)) => {
                return Err(anyhow::anyhow!(
                    "夹具观察失败：{observation}；资源结算失败：{cleanup}"
                ));
            }
        };
        assert_ne!(after, before);
        assert_eq!(phase, "cancelled");
        assert!(no_credential);
        assert!(manifest_absent);
        Ok(())
    }

    #[tokio::test]
    async fn actual_auth_verify_timeout_does_not_replay_numeric_code() -> anyhow::Result<()> {
        let fixture = AuthHttpFixture::new(QrFixtureCase::VerifyTimeout).await?;
        let challenge = fixture.begin(Duration::from_secs(2)).await?;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if fixture
                    .runtime
                    .weixin_status(Some(challenge.clone()))
                    .await
                    .is_ok_and(|view| view.phase == "need_verify_code")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        fixture
            .runtime
            .weixin_verify(challenge.clone(), "123456".to_string())
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(fixture.settle(&challenge).await?.phase, "authorized");
        fixture.verify_same_challenge()?;
        let codes = fixture
            .provider
            .codes
            .lock()
            .map_err(|_| anyhow::anyhow!("夹具校验码观察锁损坏"))?
            .clone();
        assert_eq!(codes, vec![None, Some("123456".to_string()), None]);
        fixture.close().await
    }

    struct AdmissionProbe(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl MessageHandler for AdmissionProbe {
        async fn handle(&self, msg: InboundMessage) -> echo_agent::error::Result<OutboundMessage> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(OutboundMessage::new(
                "weixin",
                msg.chat_id,
                ChatType::Direct,
                "probe",
            ))
        }
        async fn reply(&self, _msg: OutboundMessage) -> echo_agent::error::Result<()> {
            Ok(())
        }
        fn bind_durable_ingress(
            &self,
            _cancel: CancellationToken,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
        async fn admit_with_ingress(
            &self,
            msg: InboundMessage,
            _cancel: CancellationToken,
            receipt: ChannelIngressReceipt,
        ) -> echo_agent::error::Result<()> {
            let _ = self.handle(msg).await?;
            receipt.publish(ChannelIngressDisposition::DurableInput);
            Ok(())
        }
    }

    #[tokio::test]
    async fn service_close_joins_original_group_and_cancels_no_transport_parent()
    -> Result<(), Box<dyn std::error::Error>> {
        let coordinator = Arc::new(super::super::super::ChannelSessionCoordinator::new());
        let handler = Arc::new(
            ScannerHandler::new(
                Arc::new(AdmissionProbe(Arc::new(AtomicUsize::new(0)))),
                account(),
            )
            .with_coordinator(coordinator.clone()),
        );
        let parent = CancellationToken::new();
        handler.bind_durable_ingress(parent.child_token())?;
        let service = coordinator
            .ingress_service_cancel
            .lock()
            .map_err(|_| "service lock unavailable")?
            .clone()
            .ok_or("service child missing")?;
        let (cancelled, observed_cancel) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        coordinator
            .input_pump_tasks
            .lock()
            .map_err(|_| "original task group unavailable")?
            .tasks
            .lock()
            .map_err(|_| "original JoinSet unavailable")?
            .spawn(async move {
                service.cancelled().await;
                let _ = cancelled.send(());
                released.await.map_err(|error| error.to_string())?;
                Ok(())
            });
        let close_handler = handler.clone();
        let closing = tokio::spawn(async move { close_handler.close().await });
        observed_cancel.await?;
        assert!(!parent.is_cancelled());
        assert!(
            !closing.is_finished(),
            "a cancelled child is not physical settlement"
        );
        release
            .send(())
            .map_err(|_| "original task release missing")?;
        closing.await??;
        let receipt = coordinator.observe_input_pump_settlement().await?;
        assert!(receipt.failures.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_close_observer_retains_the_original_group_until_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let coordinator = Arc::new(super::super::super::ChannelSessionCoordinator::new());
        let handler = Arc::new(
            ScannerHandler::new(
                Arc::new(AdmissionProbe(Arc::new(AtomicUsize::new(0)))),
                account(),
            )
            .with_coordinator(coordinator.clone()),
        );
        handler.bind_durable_ingress(CancellationToken::new())?;
        let service = coordinator
            .ingress_service_cancel
            .lock()
            .map_err(|_| "service lock unavailable")?
            .clone()
            .ok_or("service child missing")?;
        let (cancelled, observed_cancel) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        coordinator
            .input_pump_tasks
            .lock()
            .map_err(|_| "original task group unavailable")?
            .tasks
            .lock()
            .map_err(|_| "original JoinSet unavailable")?
            .spawn(async move {
                service.cancelled().await;
                let _ = cancelled.send(());
                released.await.map_err(|error| error.to_string())?;
                Ok(())
            });
        let close_handler = handler.clone();
        let closing = tokio::spawn(async move { close_handler.close().await });
        observed_cancel.await?;
        closing.abort();
        let _ = closing.await;
        release
            .send(())
            .map_err(|_| "original task release missing")?;
        handler.close().await?;
        assert!(
            coordinator
                .observe_input_pump_settlement()
                .await?
                .failures
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn scanner_rejects_cross_account_peer_and_forged_sender_before_original_admission()
    -> Result<(), String> {
        let admissions = Arc::new(AtomicUsize::new(0));
        let handler = ScannerHandler::new(Arc::new(AdmissionProbe(admissions.clone())), account());
        for (account_id, peer, forged_sender) in [
            ("account-b", "scanner-a", false),
            ("account-a", "other-peer", false),
            ("account-a", "scanner-a", true),
        ] {
            let target = echo_agent::channels::weixin::encode_target(account_id, peer)
                .map_err(|error| error.to_string())?;
            let sender = if forged_sender {
                "forged-sender".to_string()
            } else {
                target.clone()
            };
            let msg = InboundMessage::new(
                "weixin",
                sender,
                &target,
                ChatType::Direct,
                "/reply secret",
                "message-a",
            );
            let (receipt, observed) = echo_agent::channels::channel_ingress_receipt();
            handler
                .admit_with_ingress(msg, CancellationToken::new(), receipt)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                observed.await.map_err(|error| error.to_string())?,
                ChannelIngressDisposition::TerminalRejected
            );
            assert_eq!(admissions.load(Ordering::SeqCst), 0);
        }
        let target = echo_agent::channels::weixin::encode_target("account-a", "scanner-a")
            .map_err(|error| error.to_string())?;
        let msg = InboundMessage::new(
            "weixin",
            &target,
            &target,
            ChatType::Direct,
            "中文🙂",
            "message-b",
        );
        let (receipt, observed) = echo_agent::channels::channel_ingress_receipt();
        handler
            .admit_with_ingress(msg, CancellationToken::new(), receipt)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            observed.await.map_err(|error| error.to_string())?,
            ChannelIngressDisposition::DurableInput
        );
        assert_eq!(admissions.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_auth_does_not_complete_until_original_producer_is_joined()
    -> Result<(), String> {
        let owner = AuthOwner::default();
        let cancel = tokio_util::sync::CancellationToken::new();
        let (codes, _received_codes) = mpsc::channel(1);
        let (release, released) = tokio::sync::oneshot::channel();
        let (done, observed) = watch::channel(None);
        let producer = tokio::spawn(async move {
            let _ = released.await;
        });
        let observer = tokio::spawn(async move {
            let result = producer
                .await
                .map_err(|_| "original producer join failed".to_string());
            done.send_replace(Some(result));
        });
        *owner.lock_slot() = Some(AuthOperation {
            id: "challenge-a".to_string(),
            cancel: cancel.clone(),
            codes,
            done: observed,
            _observer: observer,
        });
        owner.reserve("refresh-waiting-for-old-producer");
        assert!(owner.owns_intent("refresh-waiting-for-old-producer"));
        owner.cancel();
        assert!(cancel.is_cancelled());
        let settlement = owner.wait();
        futures::pin_mut!(settlement);
        assert!(matches!(
            futures::poll!(&mut settlement),
            std::task::Poll::Pending
        ));
        release
            .send(())
            .map_err(|_| "release receiver missing".to_string())?;
        settlement.await?;
        assert!(!owner.owns_intent("refresh-waiting-for-old-producer"));
        Ok(())
    }

    #[tokio::test]
    async fn auth_operation_failure_remains_observable_after_physical_settlement()
    -> Result<(), String> {
        let owner = AuthOwner::default();
        let (codes, _received) = mpsc::channel(1);
        let (done, observed) = watch::channel(None);
        let observer = tokio::spawn(async move {
            done.send_replace(Some(Err("credential commit failed".to_string())));
        });
        *owner.lock_slot() = Some(AuthOperation {
            id: "failed-challenge".to_string(),
            cancel: tokio_util::sync::CancellationToken::new(),
            codes,
            done: observed,
            _observer: observer,
        });
        owner.wait().await?;
        assert_eq!(
            owner.completed_failure().as_deref(),
            Some("credential commit failed")
        );
        // An observation can be repeated without repolling a settled future.
        owner.wait().await?;
        assert_eq!(
            owner.completed_failure().as_deref(),
            Some("credential commit failed")
        );
        Ok(())
    }

    #[test]
    fn auth_diagnostics_redact_qr_payload_and_account_secrets() {
        let view = WeixinQrStatus {
            challenge_id: Some("challenge-secret".to_string()),
            phase: "need_verify_code".to_string(),
            qr_svg: Some("qr-secret".to_string()),
            has_credential: true,
            account_id: Some("account-secret".to_string()),
            configuration_revision: "revision".to_string(),
            error: Some("error-secret".to_string()),
        };
        let diagnostic = format!("{view:?}");
        for secret in [
            "qr-secret",
            "account-secret",
            "error-secret",
            "challenge-secret",
        ] {
            assert!(!diagnostic.contains(secret));
        }
        assert!(!format!("{:?}", credentials(&account())).contains("secret-token"));
    }
}
