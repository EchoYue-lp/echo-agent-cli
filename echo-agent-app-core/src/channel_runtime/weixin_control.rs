//! EKO's optional ingress observer, sharing the ordinary channel handler and
//! original control journal. Other platforms never opt into this path.
use super::*;
use crate::chat_event_log::WeixinControlAdmission;
use echo_agent::channels::{ChannelIngressDisposition, ChannelIngressReceipt, InboundMessage};

/// Facts reported by the original control-effect owner, before rendering.
/// These are result values, not a second runtime or durable state machine.
#[derive(Debug)]
pub(super) enum ControlEffectOutcome {
    Applied,
    Rejected { reason: String },
    NotAdmitted { reason: String },
    Unknown { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlEffectDisposition {
    Acknowledged(ChannelIngressDisposition),
    Retryable,
}

type EffectResult = Result<ControlEffectDisposition, String>;

/// The original supervised owner retains this receipt and the SAME journal
/// lease even when its rendered response observer is cancelled or dropped.
#[derive(Clone)]
pub(super) struct ControlEffectObserver {
    lease: Arc<crate::chat_event_log::WeixinControlLease>,
    reply: Option<Arc<WeixinControlReply>>,
    durable_ingress: bool,
    // The original native owner retains its response through physical send.
    // Durable lifecycle remains the original lease, not this RAM observation.
    owned_response: Arc<std::sync::atomic::AtomicBool>,
    confirmed_disposition: Arc<std::sync::Mutex<Option<ChannelIngressDisposition>>>,
    ingress: Arc<std::sync::Mutex<Option<ChannelIngressReceipt>>>,
    publisher: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<EffectResult>>>>,
}

/// Reply DATA belongs to the already claimed original control effect owner.
/// No response task/queue or execution authority is created by this capability.
struct WeixinControlReply {
    state: Arc<crate::state::AppState>,
    workspace_id: String,
    workspace_generation: String,
    delivery: echo_agent::channels::weixin::WeixinDeliveryHandle,
    context: Option<echo_agent::channels::weixin::WeixinContextRef>,
    template: echo_agent::channels::OutboundMessage,
    reset_instance: Option<echo_agent::channels::ChannelSessionInstance>,
    service_cancel: tokio_util::sync::CancellationToken,
    provider_message_id: String,
    client_id_prefix: String,
}

impl WeixinControlReply {
    async fn send(&self, text: String) {
        if self.service_cancel.is_cancelled() {
            return;
        }
        // 在原回复边界擦除scope初始化future的具体类型，避免向渠道的Send/Sync
        // 证明展开整条工作空间/生命周期链；仍由同一owner原位等待，不新增任务。
        let control: futures::future::BoxFuture<'_, _> = Box::pin(
            self.state
                .channel_control_for_generation(&self.workspace_id, &self.workspace_generation),
        );
        let (_publication, _control) = match control.await {
            Ok(scope) => scope,
            Err(_) => {
                tracing::warn!("Control reply workspace generation is unavailable");
                return;
            }
        };
        if self
            .delivery
            .validate_message_context(
                &self.template.to,
                &self.provider_message_id,
                self.context.as_ref(),
            )
            .await
            .is_err()
        {
            tracing::warn!("Control reply original message association is unavailable");
            return;
        }
        let chunks = channel_outbound_chunks("weixin", &ChannelOutboundDraft::ordinary(text));
        for (ordinal, text) in chunks.into_iter().enumerate() {
            if self.service_cancel.is_cancelled() {
                return;
            }
            let mut message = self.template.clone();
            message.text = text;
            if let Some(instance) = self.reset_instance.as_ref() {
                message = instance.bind_outbound(message);
            }
            let client_id = format!("{}:{ordinal}", self.client_id_prefix);
            match self
                .delivery
                .send_with_context(message, self.context.as_ref(), &client_id)
                .await
            {
                Ok(echo_agent::channels::weixin::WeixinSendOutcome::Accepted { .. }) => {}
                _ => {
                    tracing::warn!(
                        "Original control reply send was not confirmed; no automatic resend"
                    );
                    return;
                }
            }
        }
    }
}

impl ControlEffectObserver {
    pub(super) fn new(
        lease: crate::chat_event_log::WeixinControlLease,
    ) -> (Self, tokio::sync::oneshot::Receiver<EffectResult>) {
        let (publisher, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                lease: Arc::new(lease),
                reply: None,
                durable_ingress: false,
                owned_response: Arc::default(),
                confirmed_disposition: Arc::default(),
                ingress: Arc::default(),
                publisher: Arc::new(std::sync::Mutex::new(Some(publisher))),
            },
            receiver,
        )
    }

    fn with_reply(mut self, reply: Option<WeixinControlReply>) -> Self {
        self.reply = reply.map(Arc::new);
        self
    }

    pub(super) async fn send_response(&self, message: String) {
        self.await_owned_response(async {
            if let Some(reply) = self.reply.as_ref() {
                reply.send(message).await;
            }
        })
        .await;
    }

    async fn await_owned_response(&self, response: impl std::future::Future<Output = ()>) {
        response.await;
        // Never infer a fact from response completion. Only the original
        // journal reporter can provide this already-confirmed disposition.
        // Holding it through the SAME native response keeps sender backpressure
        // in the original pending frontier instead of accumulating ACKed tasks.
        let confirmed = self
            .confirmed_disposition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(disposition) = confirmed {
            self.publish_ingress(disposition);
        }
    }

    fn publish_ingress(&self, disposition: ChannelIngressDisposition) {
        if let Some(receipt) = self
            .ingress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            receipt.publish(disposition);
        }
    }

    fn with_ingress(mut self, receipt: ChannelIngressReceipt) -> Self {
        self.durable_ingress = true;
        *self
            .ingress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(receipt);
        self
    }

    pub(super) fn is_durable_ingress(&self) -> bool {
        self.durable_ingress
    }

    pub(super) fn defer_response(&self) {
        self.owned_response
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) async fn report(&self, outcome: ControlEffectOutcome) {
        let publisher = {
            self.publisher
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        };
        let Some(publisher) = publisher else {
            return;
        };
        let lease = Arc::clone(&self.lease);
        // A blocking physical append keeps the original lease even if this
        // observer future is dropped. Only its real durability result is ACKed.
        let result = tokio::task::spawn_blocking(move || {
            let disposition = match outcome {
                ControlEffectOutcome::Applied => {
                    lease.complete()?;
                    ControlEffectDisposition::Acknowledged(
                        ChannelIngressDisposition::ControlHandled,
                    )
                }
                ControlEffectOutcome::Rejected { reason } => {
                    lease.reject_with_reason(reason)?;
                    ControlEffectDisposition::Acknowledged(
                        ChannelIngressDisposition::TerminalRejected,
                    )
                }
                ControlEffectOutcome::NotAdmitted { reason } => {
                    lease.defer(reason)?;
                    ControlEffectDisposition::Retryable
                }
                ControlEffectOutcome::Unknown { reason } => {
                    lease.recovery_required(reason)?;
                    ControlEffectDisposition::Acknowledged(
                        ChannelIngressDisposition::ControlRecoveryRequired,
                    )
                }
            };
            Ok::<_, crate::chat_event_log::ChatEventLogError>(disposition)
        })
        .await;
        let result = match result {
            Ok(Ok(disposition)) => Ok(disposition),
            Ok(Err(_)) => Err("Original control result durability is not confirmed".to_string()),
            Err(_) => Err("Original control journal producer did not settle".to_string()),
        };
        if let Ok(ControlEffectDisposition::Acknowledged(disposition)) = &result {
            if self
                .owned_response
                .load(std::sync::atomic::Ordering::Acquire)
            {
                *self
                    .confirmed_disposition
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(*disposition);
            } else {
                self.publish_ingress(*disposition);
            }
        }
        let _ = publisher.send(result);
    }
}

fn control_identity(
    msg: &InboundMessage,
    credential_generation: &str,
) -> Result<(String, String), String> {
    if msg.message_id.trim().is_empty()
        || msg.chat_id.trim().is_empty()
        || msg.sender_id.trim().is_empty()
        || credential_generation.trim().is_empty()
    {
        return Err(
            "Weixin control requires the stable provider message and peer identity".to_string(),
        );
    }
    let identity = serde_json::to_vec(&(
        &msg.channel_id,
        &msg.chat_id,
        &msg.sender_id,
        credential_generation,
        &msg.message_id,
    ))
    .map_err(|_| "Unable to encode Weixin control identity".to_string())?;
    let payload = serde_json::to_vec(&msg.text)
        .map_err(|_| "Unable to encode Weixin control digest".to_string())?;
    Ok((
        format!("weixin-control:{:x}", Sha256::digest(identity)),
        format!("{:x}", Sha256::digest(payload)),
    ))
}

fn is_resume_control(message: &str) -> bool {
    let mut parts = message.split_whitespace();
    match parts.next() {
        Some("/task-resume") => true,
        Some("/task-run") => parts.next() == Some("resume"),
        _ => false,
    }
}

fn is_known_control(message: &str) -> bool {
    let token = message.split_whitespace().next().unwrap_or_default();
    matches!(
        token,
        "/reply"
            | "/reset"
            | "/stop"
            | "/cancel"
            | "/tools"
            | "/workflow"
            | "/compact"
            | "/compress"
            | "/extract"
            | "/agent-list"
            | "/agent-send"
            | "/agent-status"
            | "/agent-group"
    ) || is_task_run_control_command(token)
        || is_agent_management_command(message)
        || matches!(parse_developer_command(message), Ok(Some(_)))
        || matches!(
            crate::reflection::ReflectionCommand::parse(message),
            Ok(Some(_))
        )
        || matches!(
            parse_channel_extension_input(message, "classify-only", "classify-only"),
            Ok(Some(_))
        )
}

impl AppChannelMessageHandler {
    fn admit_weixin_data<'a>(
        &'a self,
        msg: InboundMessage,
        service_cancel: tokio_util::sync::CancellationToken,
        receipt: ChannelIngressReceipt,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<()>> {
        // 在 helper 自身入口构造堆上 future，准入调用者只持有指针；
        // 调用处再装箱仍会把大型按值构造临时量留在父 poll 栈帧。
        Box::pin(async move {
            use crate::conversation_input::{
                ChannelExecutionIntent, ChannelInputBinding, FrozenChannelContext,
            };
            let fail = echo_agent::error::ReactError::Other;
            let (account_id, generation) = self.weixin_identity.as_ref().ok_or_else(|| {
                fail("original Weixin credential identity unavailable".to_string())
            })?;
            let target = echo_agent::channels::weixin::decode_target(msg.reply_target())?;
            if &target.account_id != account_id || msg.sender_id != msg.chat_id {
                receipt.publish(ChannelIngressDisposition::TerminalRejected);
                return Ok(());
            }
            let delivery = self
                .weixin_delivery
                .as_ref()
                .ok_or_else(|| fail("original Weixin delivery owner unavailable".to_string()))?
                .clone();
            let explicit_steer = msg.text.split_whitespace().next() == Some("/steer");
            if explicit_steer
                && msg
                    .text
                    .trim()
                    .strip_prefix("/steer")
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
            {
                receipt.publish(ChannelIngressDisposition::TerminalRejected);
                return Ok(());
            }
            let conv = Self::conversation_id("weixin", msg.conversation_id(), &msg.sender_id);
            // Provider identity is resolved before any workspace-specific setup.
            // This full original query is not the GUI focus or the stream cache.
            let (inventory, inventory_complete) = self
                .app_state
                .storage
                .chat_events
                .channel_input_bindings()
                .await
                .map_err(|error| fail(error.to_string()))?;
            let saved = inventory
                .into_iter()
                .filter(|input| {
                    input
                        .payload
                        .channel_binding
                        .as_ref()
                        .is_some_and(|binding| {
                            binding.channel_id == "weixin"
                                && &binding.account_id == account_id
                                && &binding.credential_generation == generation
                                && binding.peer_id == target.peer
                                && binding.provider_message_id == msg.message_id
                        })
                })
                .collect::<Vec<_>>();
            if saved.len() > 1 {
                return Err(fail(
                    "provider message has ambiguous original input identities".to_string(),
                ));
            }
            let saved = saved.into_iter().next();
            if saved.is_none() && !inventory_complete {
                return Err(fail(
                    "original provider history discovery is incomplete".to_string(),
                ));
            }
            let (address, input_id, binding, domain_plugin) = if let Some(saved) = saved.as_ref() {
                saved
                    .payload
                    .validate_channel_digest(&saved.receipt.identity.address)
                    .map_err(|error| fail(error.to_string()))?;
                if saved.payload.text != msg.text
                    || !saved.payload.attachments.is_empty()
                    || saved.receipt.identity.address.conversation_id != conv
                {
                    return Err(fail(
                        "provider message conflicts with its original persisted DATA".to_string(),
                    ));
                }
                (
                    saved.receipt.identity.address.clone(),
                    saved.receipt.identity.input_id.clone(),
                    saved
                        .payload
                        .channel_binding
                        .clone()
                        .ok_or_else(|| fail("original binding unavailable".to_string()))?,
                    saved.payload.domain_plugin.clone(),
                )
            } else {
                let runtime = self
                    .bound_control_runtime
                    .as_ref()
                    .ok_or_else(|| fail("default Weixin workspace unavailable".to_string()))?;
                let control = self
                    .app_state
                    .workspace_control_for_scope(runtime.execution_scope().workspace_id())
                    .await
                    .map_err(|error| fail(error.to_string()))?;
                let original_generation =
                    self.bound_workspace_generation.as_deref().ok_or_else(|| {
                        fail("default Weixin workspace generation unavailable".to_string())
                    })?;
                control
                    .validate_generation(original_generation)
                    .map_err(|error| fail(error.to_string()))?;
                let mut address = channel_input_address(control.workspace_id(), &conv);
                let mut execution = ChannelExecutionIntent::Chat;
                if is_resume_control(&msg.text) {
                    match self.task_run_command_response(&msg.text, &conv, None).await {
                        Some(ChannelTaskRunControl::Resume {
                            expected,
                            continuation_enabled,
                            runtime,
                        }) => {
                            address.workspace_id =
                                runtime.execution_scope().workspace_id().to_string();
                            if expected.workspace_id != address.workspace_id
                                || expected.conversation_id != address.conversation_id
                            {
                                receipt.publish(ChannelIngressDisposition::TerminalRejected);
                                return Ok(());
                            }
                            execution = ChannelExecutionIntent::Resume {
                                expected,
                                driver_continuation_enabled: continuation_enabled,
                            };
                        }
                        _ => {
                            receipt.publish(ChannelIngressDisposition::TerminalRejected);
                            return Ok(());
                        }
                    }
                }
                let control = self
                    .app_state
                    .workspace_control_for_scope(&address.workspace_id)
                    .await
                    .map_err(|error| fail(error.to_string()))?;
                control
                    .validate_generation(original_generation)
                    .map_err(|error| fail(error.to_string()))?;
                let context = delivery
                    .capture_message_context(msg.reply_target(), &msg.message_id)
                    .await?;
                delivery
                    .validate_message_context(msg.reply_target(), &msg.message_id, context.as_ref())
                    .await?;
                let binding = ChannelInputBinding {
                    version: 1,
                    channel_id: "weixin".to_string(),
                    account_id: account_id.clone(),
                    credential_generation: generation.clone(),
                    peer_id: target.peer.clone(),
                    provider_message_id: msg.message_id.clone(),
                    workspace_generation: control.generation(),
                    context: context.map_or(FrozenChannelContext::Absent, |reference| {
                        FrozenChannelContext::Reference(reference.into())
                    }),
                    execution,
                };
                let provider_identity = serde_json::to_string(&(
                    "weixin",
                    account_id,
                    generation,
                    &target.peer,
                    &msg.message_id,
                ))
                .map_err(|error| fail(error.to_string()))?;
                let input_id = crate::conversation_input::stable_scoped_input_id(
                    &address,
                    crate::conversation_input::ConversationInputSource::Channel,
                    &provider_identity,
                )
                .map_err(|error| fail(error.to_string()))?;
                (address, input_id, binding, None)
            };
            let (publication, control) = self
                .app_state
                .channel_control_for_generation(
                    &address.workspace_id,
                    &binding.workspace_generation,
                )
                .await
                .map_err(|error| fail(error.to_string()))?;
            let runtime = control.runtime().clone();
            let context = match &binding.context {
                FrozenChannelContext::Absent => None,
                FrozenChannelContext::Reference(reference) => Some(
                    echo_agent::channels::weixin::WeixinContextRef::from(reference.clone()),
                ),
            };
            delivery
                .validate_message_context(
                    msg.reply_target(),
                    &binding.provider_message_id,
                    context.as_ref(),
                )
                .await?;
            let _route_admission = self
                .input_pump
                .begin_reply_admission()
                .map_err(|error| fail(error.to_string()))?;
            let submitted = self
                .app_state
                .conversation_inputs()
                .submit_channel(
                    address.clone(),
                    input_id,
                    msg.text.clone(),
                    Vec::new(),
                    domain_plugin,
                    binding.clone(),
                )
                .await
                .map_err(|error| fail(error.to_string()))?;
            drop(publication);
            if channel_input_has_terminal_fact(&submitted) {
                receipt.publish(ChannelIngressDisposition::DurableInput);
                return Ok(());
            }
            let surface =
                Self::active_surface_identity("weixin", msg.conversation_id(), &msg.sender_id);
            let live = self
                .input_pump
                .has_reply(&submitted.identity)
                .map_err(|error| fail(error.to_string()))?
                || self.active_turn(&surface).is_some_and(|active| {
                    active.conversation_id == address.conversation_id
                        && active.runtime.execution_scope().workspace_id() == address.workspace_id
                        && submitted.turn_id.as_deref() == Some(active.turn_id.as_str())
                });
            if live {
                receipt.publish(ChannelIngressDisposition::DurableInput);
                return Ok(());
            }
            if saved.is_some()
                && let Err(error) = self
                    .app_state
                    .storage
                    .chat_events
                    .prove_unclaimed_channel_input(submitted.identity.clone())
                    .await
            {
                // Preserve attempted/effect/send uncertainty. Never convert it
                // into cancellation or a fresh chat to make recovery look green.
                if submitted.blocks_replay() || submitted.is_dispatchable() {
                    self.app_state
                        .storage
                        .chat_events
                        .mark_channel_input_uncertain_checked(
                            submitted.identity.clone(),
                            Some(submitted.queue_revision),
                            error.to_string(),
                        )
                        .await
                        .map_err(|error| fail(error.to_string()))?;
                }
                receipt.publish(ChannelIngressDisposition::DurableInput);
                return Ok(());
            }
            let incarnation = self.session_instance.incarnation_id();
            let agent_conv = Self::agent_conversation_id(&conv, &incarnation);
            let cache_id = Self::cache_user_id(&conv, &incarnation);
            let template = self.session_instance.bind_outbound(
                echo_agent::channels::OutboundMessage::new(
                    "weixin",
                    msg.reply_target(),
                    msg.chat_type,
                    String::new(),
                )
                .with_reply_to(msg.message_id.clone()),
            );
            let (mut route, receiver) = input_pump::channel_input_reply_route(&msg);
            route.deferred_weixin = Some(input_pump::DeferredWeixinReply {
                scope: input_pump::WeixinReplyScope {
                    recovered: saved.is_some(),
                    state: self.app_state.clone(),
                    address: address.clone(),
                    binding,
                    service_cancel: service_cancel.clone(),
                },
                receiver,
                delivery,
                context,
                template,
                client_id_prefix: format!("eko-input:{}", submitted.identity.input_id),
            });
            let route = if explicit_steer && saved.is_none() && self.active_turn(&surface).is_some()
            {
                match self
                    .route_live_conversation_input(
                        &surface,
                        &conv,
                        &agent_conv,
                        submitted.clone(),
                        route,
                        true,
                    )
                    .await
                    .map_err(fail)?
                {
                    ChannelLiveInputRoute::WeixinAcceptedPending => {
                        receipt.publish(ChannelIngressDisposition::DurableInput);
                        return Ok(());
                    }
                    ChannelLiveInputRoute::PumpPending(route) => *route,
                    ChannelLiveInputRoute::Routed => {
                        return Err(fail(
                            "Weixin steer lost its original data owner".to_string(),
                        ));
                    }
                }
            } else {
                route
            };
            match self
                .input_pump
                .register_reply(submitted.identity.clone(), route)
            {
                Ok(()) => {}
                Err(input_pump::ChannelInputPumpError::ReplyAlreadyRegistered)
                    if self
                        .input_pump
                        .has_reply(&submitted.identity)
                        .map_err(|error| fail(error.to_string()))? =>
                {
                    // Another initialization published this SAME verified DATA
                    // into the original slot. Retain its original route/driver.
                    receipt.publish(ChannelIngressDisposition::DurableInput);
                    return Ok(());
                }
                Err(error) => return Err(fail(error.to_string())),
            }
            if service_cancel.is_cancelled() {
                return Err(fail(
                    "original channel service closed after durable submit".to_string(),
                ));
            }
            self.start_input_pump(address, agent_conv, cache_id)
                .await
                .map_err(fail)?;
            // DATA, route registration and original wake are complete. No response
            // stream, driver task or parked observer exists for queued input.
            let _ = runtime;
            receipt.publish(ChannelIngressDisposition::DurableInput);
            Ok(())
        })
    }

    pub(super) async fn admit_weixin_ingress(
        &self,
        msg: InboundMessage,
        cancel: tokio_util::sync::CancellationToken,
        receipt: ChannelIngressReceipt,
    ) -> echo_agent::error::Result<()> {
        let service = self
            .service_cancel
            .lock()
            .map_err(|_| {
                echo_agent::error::ReactError::Other(
                    "channel service owner unavailable".to_string(),
                )
            })?
            .clone()
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other(
                    "durable ingress service is not bound".to_string(),
                )
            })?;
        if msg.channel_id != "weixin"
            || cancel.is_cancelled()
            || service.is_cancelled()
            || self
                .admission
                .as_ref()
                .is_some_and(|gate| !gate.load(std::sync::atomic::Ordering::Acquire))
            || self.initialization_error.is_some()
            || self
                .incarnation_fault
                .lock()
                .map_err(|_| {
                    echo_agent::error::ReactError::Other(
                        "channel incarnation unavailable".to_string(),
                    )
                })?
                .is_some()
            || self.session_instance.channel_id() != msg.channel_id
            || self.session_instance.conversation_id() != msg.conversation_id()
            || self.session_instance.sender_id() != msg.sender_id
        {
            return Err(echo_agent::error::ReactError::Other(
                "original durable channel owner is not available".to_string(),
            ));
        }
        if msg.chat_type != echo_agent::channels::ChatType::Direct
            || !msg.attachments.is_empty()
            || msg.text.trim().is_empty()
        {
            receipt.publish(ChannelIngressDisposition::TerminalRejected);
            return Ok(());
        }
        if let Some((account, generation)) = self.weixin_identity.as_ref() {
            let target = echo_agent::channels::weixin::decode_target(msg.reply_target())?;
            let (inputs, _) = self
                .app_state
                .storage
                .chat_events
                .channel_input_bindings()
                .await
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            if inputs.iter().any(|input| {
                input
                    .payload
                    .channel_binding
                    .as_ref()
                    .is_some_and(|binding| {
                        &binding.account_id == account
                            && &binding.credential_generation == generation
                            && binding.peer_id == target.peer
                            && binding.provider_message_id == msg.message_id
                    })
            }) {
                return self.admit_weixin_data(msg, service, receipt).await;
            }
        }
        let token = msg.text.split_whitespace().next().unwrap_or_default();
        if !token.starts_with('/') || token == "/steer" || is_resume_control(&msg.text) {
            return self.admit_weixin_data(msg, service, receipt).await;
        }
        let generation = self
            .weixin_identity
            .as_ref()
            .map(|(_, generation)| generation.as_str())
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other(
                    "original Weixin credential generation unavailable".to_string(),
                )
            })?;
        let (identity, payload_hash) =
            control_identity(&msg, generation).map_err(echo_agent::error::ReactError::Other)?;
        let conversation =
            Self::conversation_id(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
        let runtime = self
            .control_runtime()
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        let reply = if let Some(delivery) = self.weixin_delivery.as_ref() {
            let workspace = self
                .app_state
                .workspace_control_for_scope(runtime.execution_scope().workspace_id())
                .await
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            let context = delivery
                .capture_message_context(msg.reply_target(), &msg.message_id)
                .await?;
            delivery
                .validate_message_context(msg.reply_target(), &msg.message_id, context.as_ref())
                .await?;
            let template = self.session_instance.bind_outbound(
                echo_agent::channels::OutboundMessage::new(
                    "weixin",
                    msg.reply_target(),
                    msg.chat_type,
                    String::new(),
                )
                .with_reply_to(msg.message_id.clone()),
            );
            Some(WeixinControlReply {
                state: self.app_state.clone(),
                workspace_id: workspace.workspace_id().to_string(),
                workspace_generation: workspace.generation(),
                delivery: delivery.clone(),
                context,
                template,
                reset_instance: (token == "/reset").then(|| self.session_instance.clone()),
                service_cancel: service.clone(),
                provider_message_id: msg.message_id.clone(),
                client_id_prefix: format!("eko-control:{identity}"),
            })
        } else {
            None
        };
        let admission = self
            .app_state
            .storage
            .chat_events
            .claim_weixin_control(
                runtime.execution_scope().workspace_id(),
                &conversation,
                &identity,
                &payload_hash,
            )
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        let lease = match admission {
            WeixinControlAdmission::Handled => {
                receipt.publish(ChannelIngressDisposition::ControlHandled);
                return Ok(());
            }
            WeixinControlAdmission::TerminalRejected => {
                receipt.publish(ChannelIngressDisposition::TerminalRejected);
                return Ok(());
            }
            WeixinControlAdmission::RecoveryRequired => {
                receipt.publish(ChannelIngressDisposition::ControlRecoveryRequired);
                return Ok(());
            }
            WeixinControlAdmission::InFlight => return Ok(()),
            WeixinControlAdmission::Execute(lease) => lease,
        };
        let (effect, result) = ControlEffectObserver::new(*lease);
        let effect = effect.with_ingress(receipt).with_reply(reply);
        // Result publication belongs to that same original effect owner, not
        // to EOF, rendering or this short admission call's lifetime.
        drop(result);
        if !is_known_control(&msg.text) {
            effect
                .report(ControlEffectOutcome::Rejected {
                    reason: "Unknown or malformed control; no operation admitted".to_string(),
                })
                .await;
            effect
                .send_response("Unknown or malformed Weixin control.".to_string())
                .await;
            return Ok(());
        }
        if token == "/reply" {
            let (outcome, message) = match self.hitl.try_resolve_message(&msg.text) {
                ChannelHumanLoopResolution::Resolved(message) => {
                    (ControlEffectOutcome::Applied, message)
                }
                ChannelHumanLoopResolution::Rejected(message) => (
                    ControlEffectOutcome::Rejected {
                        reason: "Original HITL request rejected or expired".to_string(),
                    },
                    message,
                ),
                ChannelHumanLoopResolution::NoPending => (
                    ControlEffectOutcome::Rejected {
                        reason: "Original HITL request rejected or expired".to_string(),
                    },
                    "Reply target is expired or unavailable.".to_string(),
                ),
            };
            effect.report(outcome).await;
            effect.send_response(message).await;
            return Ok(());
        }
        // Native control owners execute immediately or retain their original
        // supervised operation. No long output waiter is created per input.
        match self.handle_stream_common(msg, Some(effect.clone())).await? {
            ChannelHandlerResponse::Immediate { text, .. } => {
                if !effect
                    .owned_response
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    effect.send_response(text).await;
                }
            }
            ChannelHandlerResponse::Stream(stream) => {
                drop(stream);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn confirmed_control_ack_waits_for_original_owned_response()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let log = crate::chat_event_log::ChatEventLog::open(directory.path(), Default::default())?;
        let WeixinControlAdmission::Execute(lease) =
            log.claim_weixin_control("global", "peer", "held-response", "payload")?
        else {
            return Err("original control was not admitted".into());
        };
        let (effect, result) = ControlEffectObserver::new(*lease);
        let (published, mut observed) = echo_agent::channels::channel_ingress_receipt();
        let effect = effect.with_ingress(published);
        effect.defer_response();
        let owner = effect.clone();
        let (release, held_response) = tokio::sync::oneshot::channel();
        let original = tokio::spawn(async move {
            owner.report(ControlEffectOutcome::Applied).await;
            owner
                .await_owned_response(async {
                    let _ = held_response.await;
                })
                .await;
        });
        assert!(matches!(
            result.await??,
            ControlEffectDisposition::Acknowledged(ChannelIngressDisposition::ControlHandled)
        ));
        assert!(matches!(
            observed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(effect);
        assert!(matches!(
            observed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        release
            .send(())
            .map_err(|_| "original response owner disappeared")?;
        original.await?;
        assert_eq!(observed.await?, ChannelIngressDisposition::ControlHandled);
        Ok(())
    }

    #[tokio::test]
    async fn owned_response_without_control_fact_never_acks()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let log = crate::chat_event_log::ChatEventLog::open(directory.path(), Default::default())?;
        let WeixinControlAdmission::Execute(lease) =
            log.claim_weixin_control("global", "peer", "no-result", "payload")?
        else {
            return Err("original control was not admitted".into());
        };
        let (effect, result) = ControlEffectObserver::new(*lease);
        let (published, mut observed) = echo_agent::channels::channel_ingress_receipt();
        let effect = effect.with_ingress(published);
        effect.defer_response();
        effect.await_owned_response(async {}).await;
        assert!(matches!(
            observed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(effect);
        drop(result);
        assert!(
            observed.await.is_err(),
            "response completion/drop manufactured an ACK"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dropped_render_observer_keeps_original_effect_owner_and_durable_result()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) = log.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:owner-retained",
            "original-payload",
        )?
        else {
            return Err("original effect owner was not admitted".into());
        };
        let (render_observer, rendered_result) = ControlEffectObserver::new(*lease);
        let original_effect_owner = render_observer.clone();
        drop(rendered_result);
        drop(render_observer);
        assert!(matches!(
            log.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:owner-retained",
                "original-payload",
            )?,
            WeixinControlAdmission::InFlight
        ));
        original_effect_owner
            .report(ControlEffectOutcome::Unknown {
                reason: "Original operation could not confirm the cancellation barrier".to_string(),
            })
            .await;
        drop(original_effect_owner);
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:owner-retained",
                "original-payload",
            )?,
            WeixinControlAdmission::RecoveryRequired
        ));
        Ok(())
    }

    #[test]
    fn resume_aliases_share_data_classification_and_credentials_separate_control_identity()
    -> Result<(), String> {
        assert!(is_resume_control("/task-resume original"));
        assert!(is_resume_control("/task-run resume original"));
        assert!(!is_resume_control("/task-run status original"));
        let target = echo_agent::channels::weixin::encode_target("account", "peer")
            .map_err(|error| error.to_string())?;
        let message = InboundMessage::new(
            "weixin",
            &target,
            &target,
            echo_agent::channels::ChatType::Direct,
            "/reset",
            "provider",
        );
        assert_ne!(
            control_identity(&message, "old")?.0,
            control_identity(&message, "new")?.0
        );
        Ok(())
    }

    #[test]
    fn control_identity_uses_provider_id_and_separates_payload_conflicts() -> Result<(), String> {
        let mut message = InboundMessage::new(
            "weixin",
            "peer",
            "account-peer",
            echo_agent::channels::ChatType::Direct,
            "/reply original 中文🙂",
            "provider-1",
        );
        message.message_id = "provider-1".to_string();
        let (id, hash) = control_identity(&message, "generation")?;
        message.text = "/reset".to_string();
        let (same_id, different_hash) = control_identity(&message, "generation")?;
        assert_eq!(id, same_id);
        assert_ne!(hash, different_hash);
        message.chat_id = "another-account-peer".to_string();
        assert_ne!(id, control_identity(&message, "generation")?.0);
        Ok(())
    }
}
