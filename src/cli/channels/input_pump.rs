//! Channel-specific ownership for draining the durable conversation-input frontier.
//!
//! This module deliberately owns no queue, journal, or driver. The durable
//! frontier and its lifecycle facts remain behind the injected adapter. The
//! slot only linearizes one channel conversation's pump and retains ephemeral
//! response routes while the existing channel handler is alive.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use super::ChannelRenderEvent;

/// Original transport route for one exact durable input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChannelReplyCorrelation {
    pub(super) channel_id: String,
    pub(super) to: String,
    pub(super) chat_type: echo_agent::channels::ChatType,
}

impl ChannelReplyCorrelation {
    pub(super) fn from_inbound(message: &echo_agent::channels::InboundMessage) -> Self {
        Self {
            channel_id: message.channel_id.clone(),
            to: message.reply_target().to_string(),
            chat_type: message.chat_type,
        }
    }
}

/// Sender half retained until the exact durable input is claimed.
pub(super) struct ChannelInputReplyRoute {
    pub(super) render_tx: tokio::sync::mpsc::Sender<ChannelRenderEvent>,
    pub(super) lifecycle_cursor: Arc<Mutex<u64>>,
    pub(super) terminal_tx:
        tokio::sync::oneshot::Sender<echo_agent_app_core::api::chat_driver::TurnOutcome>,
}

impl ChannelInputReplyRoute {
    fn is_closed(&self) -> bool {
        self.render_tx.is_closed() || self.terminal_tx.is_closed()
    }
}

/// Receiver half returned by the inbound handler's existing response stream.
pub(super) struct ChannelInputReplyReceiver {
    pub(super) correlation: ChannelReplyCorrelation,
    pub(super) render_rx: tokio::sync::mpsc::Receiver<ChannelRenderEvent>,
    pub(super) terminal_rx:
        tokio::sync::oneshot::Receiver<echo_agent_app_core::api::chat_driver::TurnOutcome>,
}

pub(super) fn channel_input_reply_route(
    message: &echo_agent::channels::InboundMessage,
) -> (ChannelInputReplyRoute, ChannelInputReplyReceiver) {
    let correlation = ChannelReplyCorrelation::from_inbound(message);
    let (render_tx, render_rx) =
        tokio::sync::mpsc::channel(super::outbound::CHANNEL_EVENT_QUEUE_CAPACITY);
    let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
    (
        ChannelInputReplyRoute {
            render_tx,
            lifecycle_cursor: Arc::new(Mutex::new(0)),
            terminal_tx,
        },
        ChannelInputReplyReceiver {
            correlation,
            render_rx,
            terminal_rx,
        },
    )
}

#[derive(Debug)]
pub(super) enum ChannelInputPumpKick<Identity>
where
    Identity: Clone + Eq + Hash,
{
    Started(ChannelInputPumpOwner<Identity>),
    Notified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChannelInputPumpOwnerDecision {
    Continue { wake_epoch: u64 },
    Quiescent,
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChannelInputPumpError {
    StateUnavailable,
    EpochExhausted,
    OwnerMismatch,
    ShuttingDown,
    ReplyAlreadyRegistered,
    ReplyRecoveryInProgress,
    DurableDebt(String),
}

impl std::fmt::Display for ChannelInputPumpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateUnavailable => {
                formatter.write_str("channel input pump state is unavailable")
            }
            Self::EpochExhausted => formatter.write_str("channel input pump epoch is exhausted"),
            Self::OwnerMismatch => {
                formatter.write_str("channel input pump owner does not match the active generation")
            }
            Self::ShuttingDown => formatter.write_str("channel input pump is shutting down"),
            Self::ReplyAlreadyRegistered => {
                formatter.write_str("channel input reply route is already registered")
            }
            Self::ReplyRecoveryInProgress => {
                formatter.write_str("channel input route-less recovery is already in progress")
            }
            Self::DurableDebt(reason) => write!(formatter, "channel input durable debt: {reason}"),
        }
    }
}

impl std::error::Error for ChannelInputPumpError {}

struct ChannelInputPumpState<Identity>
where
    Identity: Clone + Eq + Hash,
{
    wake_epoch: u64,
    quiescent_epoch: u64,
    next_owner_id: u64,
    active_owner_id: Option<u64>,
    shutting_down: bool,
    reply_routes: HashMap<Identity, ChannelInputReplyRoute>,
    unroutable_recovery: Option<Identity>,
}

impl<Identity> Default for ChannelInputPumpState<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            wake_epoch: 0,
            quiescent_epoch: 0,
            next_owner_id: 0,
            active_owner_id: None,
            shutting_down: false,
            reply_routes: HashMap::new(),
            unroutable_recovery: None,
        }
    }
}

/// Ephemeral single-owner gate for one channel conversation.
///
/// `Identity` must be the complete durable identity, including revision and
/// payload hash. The slot never derives or weakens that identity itself.
pub(super) struct ChannelInputPumpSlot<Identity>
where
    Identity: Clone + Eq + Hash,
{
    state: Mutex<ChannelInputPumpState<Identity>>,
}

impl<Identity> Default for ChannelInputPumpSlot<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            state: Mutex::new(ChannelInputPumpState::default()),
        }
    }
}

impl<Identity> ChannelInputPumpSlot<Identity>
where
    Identity: Clone + Eq + Hash,
{
    pub(super) fn register_reply(
        &self,
        identity: Identity,
        route: ChannelInputReplyRoute,
    ) -> Result<(), ChannelInputPumpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        if state.unroutable_recovery.as_ref() == Some(&identity) {
            return Err(ChannelInputPumpError::ReplyRecoveryInProgress);
        }
        if state
            .reply_routes
            .get(&identity)
            .is_some_and(|current| !current.is_closed())
        {
            return Err(ChannelInputPumpError::ReplyAlreadyRegistered);
        }
        state.reply_routes.insert(identity, route);
        Ok(())
    }

    /// Record durable work and elect an owner under the same mutex.
    pub(super) fn kick(
        self: &Arc<Self>,
    ) -> Result<ChannelInputPumpKick<Identity>, ChannelInputPumpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        let next_wake_epoch = state
            .wake_epoch
            .checked_add(1)
            .ok_or(ChannelInputPumpError::EpochExhausted)?;
        if state.active_owner_id.is_some() {
            state.wake_epoch = next_wake_epoch;
            return Ok(ChannelInputPumpKick::Notified);
        }
        let next_owner_id = state
            .next_owner_id
            .checked_add(1)
            .ok_or(ChannelInputPumpError::EpochExhausted)?;
        state.wake_epoch = next_wake_epoch;
        state.next_owner_id = next_owner_id;
        state.active_owner_id = Some(next_owner_id);
        Ok(ChannelInputPumpKick::Started(ChannelInputPumpOwner {
            slot: Arc::clone(self),
            owner_id: next_owner_id,
            active: true,
        }))
    }

    pub(super) fn begin_shutdown(&self) -> Result<(), ChannelInputPumpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        state.shutting_down = true;
        state.reply_routes.clear();
        Ok(())
    }

    fn has_reply(&self, identity: &Identity) -> Result<bool, ChannelInputPumpError> {
        let state = self
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        Ok(state
            .reply_routes
            .get(identity)
            .is_some_and(|route| !route.is_closed()))
    }

    /// Re-elect without a new input after the existing task owner reports an
    /// abnormal exit. The handler service calls this from its existing task
    /// completion path; no unrelated inbound message is needed.
    pub(super) fn resume_after_owner_loss(
        self: &Arc<Self>,
    ) -> Result<Option<ChannelInputPumpOwner<Identity>>, ChannelInputPumpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        if state.shutting_down || state.active_owner_id.is_some() {
            return Ok(None);
        }
        if state.quiescent_epoch == state.wake_epoch {
            return Ok(None);
        }
        state.next_owner_id = state
            .next_owner_id
            .checked_add(1)
            .ok_or(ChannelInputPumpError::EpochExhausted)?;
        let owner_id = state.next_owner_id;
        state.active_owner_id = Some(owner_id);
        Ok(Some(ChannelInputPumpOwner {
            slot: Arc::clone(self),
            owner_id,
            active: true,
        }))
    }
}

/// Exact RAII ownership for one pump generation.
pub(super) struct ChannelInputPumpOwner<Identity>
where
    Identity: Clone + Eq + Hash,
{
    slot: Arc<ChannelInputPumpSlot<Identity>>,
    owner_id: u64,
    active: bool,
}

impl<Identity> std::fmt::Debug for ChannelInputPumpOwner<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelInputPumpOwner")
            .field("owner_id", &self.owner_id)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl<Identity> PartialEq for ChannelInputPumpOwner<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id && Arc::ptr_eq(&self.slot, &other.slot)
    }
}

impl<Identity> Eq for ChannelInputPumpOwner<Identity> where Identity: Clone + Eq + Hash {}

impl<Identity> ChannelInputPumpOwner<Identity>
where
    Identity: Clone + Eq + Hash,
{
    pub(super) fn observed_wake_epoch(&self) -> Result<u64, ChannelInputPumpError> {
        let state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        if state.shutting_down {
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        Ok(state.wake_epoch)
    }

    pub(super) fn is_shutting_down(&self) -> Result<bool, ChannelInputPumpError> {
        let state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        Ok(state.shutting_down)
    }

    pub(super) fn take_reply(
        &self,
        identity: &Identity,
    ) -> Result<Option<ChannelInputReplyRoute>, ChannelInputPumpError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        Ok(state.reply_routes.remove(identity))
    }

    fn restore_reply(
        &self,
        identity: Identity,
        route: ChannelInputReplyRoute,
    ) -> Result<(), ChannelInputPumpError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        if state.shutting_down {
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        if state
            .reply_routes
            .get(&identity)
            .is_some_and(|current| !current.is_closed())
        {
            return Ok(());
        }
        state.reply_routes.insert(identity, route);
        Ok(())
    }

    fn has_reply(&self, identity: &Identity) -> Result<bool, ChannelInputPumpError> {
        self.slot.has_reply(identity)
    }

    fn begin_unroutable_recovery(
        &self,
        identity: &Identity,
    ) -> Result<bool, ChannelInputPumpError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        if state.shutting_down {
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        if state
            .reply_routes
            .get(identity)
            .is_some_and(|route| !route.is_closed())
        {
            return Ok(false);
        }
        state.unroutable_recovery = Some(identity.clone());
        Ok(true)
    }

    fn finish_unroutable_recovery(&self, identity: &Identity) -> Result<(), ChannelInputPumpError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        if state.unroutable_recovery.as_ref() == Some(identity) {
            state.unroutable_recovery = None;
        }
        Ok(())
    }

    /// Release only if no kick raced the owner's empty frontier observation.
    pub(super) fn finish_if_quiescent(
        &mut self,
        observed_wake_epoch: u64,
    ) -> Result<ChannelInputPumpOwnerDecision, ChannelInputPumpError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ChannelInputPumpError::StateUnavailable)?;
        self.ensure_owner(&state)?;
        if state.shutting_down {
            state.active_owner_id = None;
            self.active = false;
            return Ok(ChannelInputPumpOwnerDecision::ShuttingDown);
        }
        if state.wake_epoch != observed_wake_epoch {
            return Ok(ChannelInputPumpOwnerDecision::Continue {
                wake_epoch: state.wake_epoch,
            });
        }
        state.quiescent_epoch = observed_wake_epoch;
        state.active_owner_id = None;
        self.active = false;
        Ok(ChannelInputPumpOwnerDecision::Quiescent)
    }

    fn ensure_owner(
        &self,
        state: &ChannelInputPumpState<Identity>,
    ) -> Result<(), ChannelInputPumpError> {
        if self.active && state.active_owner_id == Some(self.owner_id) {
            Ok(())
        } else {
            Err(ChannelInputPumpError::OwnerMismatch)
        }
    }

    fn release(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.slot.state.lock()
            && state.active_owner_id == Some(self.owner_id)
        {
            state.active_owner_id = None;
            state.unroutable_recovery = None;
        }
        self.active = false;
    }
}

impl<Identity> Drop for ChannelInputPumpOwner<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn drop(&mut self) {
        self.release();
    }
}

/// Thin boundary around the existing durable frontier and foreground driver.
///
/// `execute_claimed` must retain the existing foreground lease through durable
/// terminal persistence. The production adapter does that through the core
/// `drive_foreground_chat_with_ingress` helper; this module never settles a
/// lease or appends a lifecycle fact itself.
pub(super) trait ChannelInputPumpAdapter: Send + Sync + 'static {
    type Identity: Clone + Eq + Hash + Send + Sync + 'static;
    type Item: Send + Sync + 'static;

    fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>>;

    /// Persist a terminal disposition for an input whose original transport
    /// stream no longer exists. This must not claim or execute the input.
    fn recover_unroutable<'a>(
        &'a self,
        identity: &'a Self::Identity,
    ) -> BoxFuture<'a, Result<(), String>>;

    fn claim_next<'a>(
        &'a self,
        expected_identity: &'a Self::Identity,
    ) -> BoxFuture<'a, Result<Option<Self::Item>, String>>;

    fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity;

    /// Execute through the existing foreground helper and return only after its
    /// durable terminal callback has completed.
    fn execute_claimed<'a>(
        &'a self,
        item: &'a Self::Item,
        reply: ChannelInputReplyRoute,
    ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>>;

    /// Persist a fail-closed recovery disposition for a claim that cannot enter
    /// or conclusively finish the existing foreground driver.
    fn recover_claimed<'a>(
        &'a self,
        item: &'a Self::Item,
        reason: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
}

pub(super) struct ChannelInputExecutionError {
    reason: String,
    reply: Option<ChannelInputReplyRoute>,
}

impl ChannelInputExecutionError {
    pub(super) fn before_driver(reason: impl Into<String>, reply: ChannelInputReplyRoute) -> Self {
        Self {
            reason: reason.into(),
            reply: Some(reply),
        }
    }

    pub(super) fn after_driver(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            reply: None,
        }
    }
}

/// Drain until the injected durable authority reports an empty frontier whose
/// observation did not race another kick.
pub(super) async fn run_channel_input_pump<Adapter>(
    mut owner: ChannelInputPumpOwner<Adapter::Identity>,
    adapter: Arc<Adapter>,
) -> Result<(), ChannelInputPumpError>
where
    Adapter: ChannelInputPumpAdapter,
{
    const MAX_RETRY_ATTEMPTS: usize = 8;
    loop {
        let observed_wake_epoch = owner.observed_wake_epoch()?;
        let next_identity = {
            let mut attempts = 0_usize;
            loop {
                match adapter.peek_next_identity().await {
                    Ok(identity) => break identity,
                    Err(error) => {
                        if owner.is_shutting_down()? {
                            return Err(ChannelInputPumpError::ShuttingDown);
                        }
                        attempts = attempts.saturating_add(1);
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            return Err(ChannelInputPumpError::DurableDebt(format!(
                                "frontier peek failed after {attempts} attempts: {error}"
                            )));
                        }
                        tracing::error!(%error, attempts, "channel input frontier peek failed; owner will retry");
                        tokio::time::sleep(retry_delay(attempts)).await;
                    }
                }
            }
        };
        let Some(next_identity) = next_identity else {
            match owner.finish_if_quiescent(observed_wake_epoch)? {
                ChannelInputPumpOwnerDecision::Continue { .. } => continue,
                ChannelInputPumpOwnerDecision::Quiescent => return Ok(()),
                ChannelInputPumpOwnerDecision::ShuttingDown => {
                    return Err(ChannelInputPumpError::ShuttingDown);
                }
            }
        };
        if !owner.has_reply(&next_identity)? {
            if !owner.begin_unroutable_recovery(&next_identity)? {
                continue;
            }
            let mut attempts = 0_usize;
            loop {
                match adapter.recover_unroutable(&next_identity).await {
                    Ok(()) => {
                        owner.finish_unroutable_recovery(&next_identity)?;
                        break;
                    }
                    Err(error) => {
                        if owner.is_shutting_down()? {
                            return Err(ChannelInputPumpError::ShuttingDown);
                        }
                        attempts = attempts.saturating_add(1);
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            return Err(ChannelInputPumpError::DurableDebt(format!(
                                "route-less input recovery failed after {attempts} attempts: {error}"
                            )));
                        }
                        tracing::error!(%error, "route-less channel input recovery remains pending");
                        tokio::time::sleep(retry_delay(attempts)).await;
                    }
                }
            }
            continue;
        }
        let claimed = {
            let mut attempts = 0_usize;
            loop {
                match adapter.claim_next(&next_identity).await {
                    Ok(claimed) => break claimed,
                    Err(error) => {
                        if owner.is_shutting_down()? {
                            return Err(ChannelInputPumpError::ShuttingDown);
                        }
                        attempts = attempts.saturating_add(1);
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            return Err(ChannelInputPumpError::DurableDebt(format!(
                                "input claim failed after {attempts} attempts: {error}"
                            )));
                        }
                        tracing::error!(%error, attempts, "channel input claim failed; owner will retry");
                        tokio::time::sleep(retry_delay(attempts)).await;
                    }
                }
            }
        };
        let Some(item) = claimed else {
            match owner.finish_if_quiescent(observed_wake_epoch)? {
                ChannelInputPumpOwnerDecision::Continue { .. } => continue,
                ChannelInputPumpOwnerDecision::Quiescent => return Ok(()),
                ChannelInputPumpOwnerDecision::ShuttingDown => {
                    return Err(ChannelInputPumpError::ShuttingDown);
                }
            }
        };
        if adapter.identity(&item) != &next_identity {
            recover_claimed_until_durable(
                adapter.as_ref(),
                &item,
                "channel input frontier changed between route peek and atomic claim",
            )
            .await?;
            continue;
        }
        if owner.is_shutting_down()? {
            recover_claimed_until_durable(
                adapter.as_ref(),
                &item,
                "channel session shut down after the durable input was claimed",
            )
            .await?;
            return Err(ChannelInputPumpError::ShuttingDown);
        }
        let Some(reply) = owner.take_reply(adapter.identity(&item))? else {
            recover_claimed_until_durable(
                adapter.as_ref(),
                &item,
                "channel input reply route disappeared before execution",
            )
            .await?;
            continue;
        };
        if let Err(mut error) = adapter.execute_claimed(&item, reply).await {
            if let Err(recovery) =
                recover_claimed_until_durable(adapter.as_ref(), &item, &error.reason).await
            {
                if let Some(reply) = error.reply.take() {
                    owner.restore_reply(adapter.identity(&item).clone(), reply)?;
                }
                return Err(recovery);
            }
            if let Some(reply) = error.reply {
                owner.restore_reply(adapter.identity(&item).clone(), reply)?;
            }
        }
    }
}

async fn recover_claimed_until_durable<Adapter>(
    adapter: &Adapter,
    item: &Adapter::Item,
    reason: &str,
) -> Result<(), ChannelInputPumpError>
where
    Adapter: ChannelInputPumpAdapter,
{
    const MAX_RETRY_ATTEMPTS: usize = 8;
    let mut attempts = 0_usize;
    loop {
        match adapter.recover_claimed(item, reason).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                attempts = attempts.saturating_add(1);
                if attempts >= MAX_RETRY_ATTEMPTS {
                    return Err(ChannelInputPumpError::DurableDebt(format!(
                        "claimed input recovery failed after {attempts} attempts: {error}"
                    )));
                }
                tracing::error!(%error, "channel input recovery debt remains pending");
                tokio::time::sleep(retry_delay(attempts)).await;
            }
        }
    }
}

fn retry_delay(attempt: usize) -> std::time::Duration {
    let exponent = u32::try_from(attempt.saturating_sub(1).min(5)).unwrap_or(5);
    std::time::Duration::from_millis(25_u64.saturating_mul(1_u64 << exponent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct TestIdentity(&'static str);

    fn test_message(message_id: &str) -> echo_agent::channels::InboundMessage {
        echo_agent::channels::InboundMessage::new(
            "qq",
            "sender",
            "chat",
            echo_agent::channels::ChatType::Direct,
            "queued",
            message_id,
        )
    }

    #[test]
    fn simultaneous_kicks_elect_one_owner() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let kicks = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(32);
            for _ in 0..32 {
                let slot = Arc::clone(&slot);
                let barrier = Arc::clone(&barrier);
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    slot.kick().map_err(|error| error.to_string())
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "kick thread terminated unexpectedly".to_string())?
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        let owners = kicks
            .iter()
            .filter(|kick| matches!(kick, ChannelInputPumpKick::Started(_)))
            .count();
        let notifications = kicks
            .iter()
            .filter(|kick| matches!(kick, ChannelInputPumpKick::Notified))
            .count();
        assert_eq!(owners, 1);
        assert_eq!(notifications, 31);
        drop(kicks);
        assert!(matches!(
            slot.kick().map_err(|error| error.to_string())?,
            ChannelInputPumpKick::Started(_)
        ));
        Ok(())
    }

    #[test]
    fn owner_loss_re_elects_without_another_input() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        assert!(matches!(
            slot.kick().map_err(|error| error.to_string())?,
            ChannelInputPumpKick::Notified
        ));
        drop(owner);
        let recovered = slot
            .resume_after_owner_loss()
            .map_err(|error| error.to_string())?;
        assert!(recovered.is_some());
        Ok(())
    }

    #[test]
    fn route_registration_cannot_race_an_exact_unroutable_recovery() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let identity = TestIdentity("recovering-route");
        assert!(
            owner
                .begin_unroutable_recovery(&identity)
                .map_err(|error| error.to_string())?
        );
        let message = test_message("recovering-route");
        let (route, _receiver) = channel_input_reply_route(&message);
        assert!(matches!(
            slot.register_reply(identity.clone(), route),
            Err(ChannelInputPumpError::ReplyRecoveryInProgress)
        ));
        owner
            .finish_unroutable_recovery(&identity)
            .map_err(|error| error.to_string())?;
        let (route, _receiver) = channel_input_reply_route(&message);
        slot.register_reply(identity, route)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    struct EmptyRaceAdapter {
        claims: AtomicUsize,
        first_claim_entered: tokio::sync::Notify,
        release_first_claim: tokio::sync::Notify,
        settlements: AtomicUsize,
    }

    impl ChannelInputPumpAdapter for EmptyRaceAdapter {
        type Identity = TestIdentity;
        type Item = TestIdentity;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async move {
                Ok((self.claims.load(Ordering::SeqCst) < 2).then_some(TestIdentity("raced-input")))
            })
        }

        fn recover_unroutable<'a>(
            &'a self,
            _identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn claim_next<'a>(
            &'a self,
            _expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                let claim = self.claims.fetch_add(1, Ordering::SeqCst);
                match claim {
                    0 => {
                        self.first_claim_entered.notify_one();
                        self.release_first_claim.notified().await;
                        Ok(None)
                    }
                    1 => Ok(Some(TestIdentity("raced-input"))),
                    _ => Ok(None),
                }
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            item
        }

        fn execute_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                self.settlements.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn kick_racing_empty_observation_is_not_lost() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(EmptyRaceAdapter {
            claims: AtomicUsize::new(0),
            first_claim_entered: tokio::sync::Notify::new(),
            release_first_claim: tokio::sync::Notify::new(),
            settlements: AtomicUsize::new(0),
        });
        let message = test_message("raced-input");
        let (route, _receiver) = channel_input_reply_route(&message);
        slot.register_reply(TestIdentity("raced-input"), route)
            .map_err(|error| error.to_string())?;
        let running_slot = Arc::clone(&slot);
        let running_adapter = Arc::clone(&adapter);
        let running =
            tokio::spawn(async move { run_channel_input_pump(owner, running_adapter).await });
        adapter.first_claim_entered.notified().await;
        assert!(matches!(
            running_slot.kick().map_err(|error| error.to_string())?,
            ChannelInputPumpKick::Notified
        ));
        adapter.release_first_claim.notify_one();
        running
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(adapter.settlements.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.claims.load(Ordering::SeqCst), 2);
        Ok(())
    }

    struct ShutdownRaceAdapter {
        claim_entered: tokio::sync::Notify,
        release_claim: tokio::sync::Notify,
        executions: AtomicUsize,
        recoveries: AtomicUsize,
    }

    impl ChannelInputPumpAdapter for ShutdownRaceAdapter {
        type Identity = TestIdentity;
        type Item = TestIdentity;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async { Ok(Some(TestIdentity("shutdown-claim"))) })
        }

        fn recover_unroutable<'a>(
            &'a self,
            _identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn claim_next<'a>(
            &'a self,
            _expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                self.claim_entered.notify_one();
                self.release_claim.notified().await;
                Ok(Some(TestIdentity("shutdown-claim")))
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            item
        }

        fn execute_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                self.executions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.recoveries.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn shutdown_after_claim_recovers_without_starting_execution() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(ShutdownRaceAdapter {
            claim_entered: tokio::sync::Notify::new(),
            release_claim: tokio::sync::Notify::new(),
            executions: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
        });
        let message = test_message("shutdown-claim");
        let (route, _receiver) = channel_input_reply_route(&message);
        slot.register_reply(TestIdentity("shutdown-claim"), route)
            .map_err(|error| error.to_string())?;
        let running_adapter = Arc::clone(&adapter);
        let running =
            tokio::spawn(async move { run_channel_input_pump(owner, running_adapter).await });
        adapter.claim_entered.notified().await;
        slot.begin_shutdown().map_err(|error| error.to_string())?;
        adapter.release_claim.notify_one();
        let result = running.await.map_err(|error| error.to_string())?;
        assert!(matches!(result, Err(ChannelInputPumpError::ShuttingDown)));
        assert_eq!(adapter.executions.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.recoveries.load(Ordering::SeqCst), 1);
        Ok(())
    }

    struct ClosedSubscriberAdapter {
        item_available: Mutex<bool>,
        delivery_closed: AtomicUsize,
        settlements: AtomicUsize,
        unroutable_recoveries: AtomicUsize,
    }

    impl ChannelInputPumpAdapter for ClosedSubscriberAdapter {
        type Identity = TestIdentity;
        type Item = TestIdentity;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async move {
                let available = self
                    .item_available
                    .lock()
                    .map_err(|_| "test item state is unavailable".to_string())?;
                Ok((*available).then_some(TestIdentity("closed-subscriber")))
            })
        }

        fn recover_unroutable<'a>(
            &'a self,
            _identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let mut available = self
                    .item_available
                    .lock()
                    .map_err(|_| "test item state is unavailable".to_string())?;
                *available = false;
                self.unroutable_recoveries.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn claim_next<'a>(
            &'a self,
            _expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                let mut available = self
                    .item_available
                    .lock()
                    .map_err(|_| "test item state is unavailable".to_string())?;
                if std::mem::take(&mut *available) {
                    Ok(Some(TestIdentity("closed-subscriber")))
                } else {
                    Ok(None)
                }
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            item
        }

        fn execute_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                let closed = reply
                    .render_tx
                    .try_send(ChannelRenderEvent::Terminal(
                        echo_agent_app_core::api::chat_driver::TurnOutcome::Cancelled,
                    ))
                    .is_err();
                if closed {
                    self.delivery_closed.fetch_add(1, Ordering::SeqCst);
                }
                self.settlements.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn subscriber_drop_recovers_without_claim_or_execution() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let message = echo_agent::channels::InboundMessage::new(
            "qq",
            "sender",
            "chat",
            echo_agent::channels::ChatType::Direct,
            "queued",
            "transport-id",
        );
        let (route, receiver) = channel_input_reply_route(&message);
        drop(receiver);
        slot.register_reply(TestIdentity("closed-subscriber"), route)
            .map_err(|error| error.to_string())?;
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(ClosedSubscriberAdapter {
            item_available: Mutex::new(true),
            delivery_closed: AtomicUsize::new(0),
            settlements: AtomicUsize::new(0),
            unroutable_recoveries: AtomicUsize::new(0),
        });
        run_channel_input_pump(owner, Arc::clone(&adapter))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(adapter.delivery_closed.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.settlements.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.unroutable_recoveries.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn frontier_without_transport_route_is_not_claimed() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(ClosedSubscriberAdapter {
            item_available: Mutex::new(true),
            delivery_closed: AtomicUsize::new(0),
            settlements: AtomicUsize::new(0),
            unroutable_recoveries: AtomicUsize::new(0),
        });
        run_channel_input_pump(owner, Arc::clone(&adapter))
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            !*adapter
                .item_available
                .lock()
                .map_err(|_| "test item state is unavailable".to_string())?
        );
        assert_eq!(adapter.settlements.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.unroutable_recoveries.load(Ordering::SeqCst), 1);
        Ok(())
    }

    struct DurableRestartAdapter {
        service: echo_agent_app_core::api::conversation_input::ConversationInputService,
        address: echo_agent_app_core::api::conversation_input::ConversationInputAddress,
        claims: AtomicUsize,
        executions: AtomicUsize,
    }

    impl ChannelInputPumpAdapter for DurableRestartAdapter {
        type Identity = echo_agent_app_core::api::conversation_input::ConversationInputIdentity;
        type Item = echo_agent_app_core::api::conversation_input::ConversationInputProjection;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async move {
                self.service
                    .list(&self.address)
                    .await
                    .map(|frontier| {
                        frontier
                            .items
                            .first()
                            .map(|item| item.receipt.identity.clone())
                    })
                    .map_err(|error| error.to_string())
            })
        }

        fn recover_unroutable<'a>(
            &'a self,
            identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.service
                    .cancel(identity.clone())
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        }

        fn claim_next<'a>(
            &'a self,
            expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                let frontier = self
                    .service
                    .list(&self.address)
                    .await
                    .map_err(|error| error.to_string())?;
                if frontier
                    .items
                    .first()
                    .is_none_or(|item| item.receipt.identity != *expected_identity)
                {
                    return Ok(None);
                }
                self.claims.fetch_add(1, Ordering::SeqCst);
                self.service
                    .dispatch_selected(
                        expected_identity.clone(),
                        frontier.queue_revision,
                        "restart-new-turn".to_string(),
                    )
                    .await
                    .map(Some)
                    .map_err(|error| error.to_string())
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            &item.receipt.identity
        }

        fn execute_claimed<'a>(
            &'a self,
            item: &'a Self::Item,
            reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                let attempt = match item.active_attempt.clone() {
                    Some(attempt) => attempt,
                    None => {
                        return Err(ChannelInputExecutionError::before_driver(
                            "restart test attempt ordinal is missing",
                            reply,
                        ));
                    }
                };
                self.service
                    .mailbox_accepted(attempt.clone())
                    .await
                    .map_err(|error| ChannelInputExecutionError::after_driver(error.to_string()))?;
                self.service
                    .drained(attempt.clone())
                    .await
                    .map_err(|error| ChannelInputExecutionError::after_driver(error.to_string()))?;
                self.service
                    .turn_settled(
                        attempt,
                        echo_agent_app_core::api::conversation_input::ConversationInputOutcome::Completed,
                        true,
                    )
                    .await
                    .map_err(|error| {
                        ChannelInputExecutionError::after_driver(error.to_string())
                    })?;
                self.executions.fetch_add(1, Ordering::SeqCst);
                let _ = reply
                    .terminal_tx
                    .send(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn restart_cancels_route_less_head_before_executing_routed_successor()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            echo_agent_app_core::api::chat_event_log::ChatEventLog::open(
                temporary.path().join("channel-restart-log"),
                echo_agent_app_core::api::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = echo_agent_app_core::api::conversation_input::ConversationInputAddress {
            workspace_id: "workspace-restart".to_string(),
            conversation_id: "conversation-restart".to_string(),
        };
        let old = service
            .submit(
                address.clone(),
                "old-route-less".to_string(),
                "old payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let new = service
            .submit(
                address.clone(),
                "new-routed".to_string(),
                "new payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let slot = Arc::new(ChannelInputPumpSlot::default());
        let message = test_message("new-routed");
        let (route, receiver) = channel_input_reply_route(&message);
        slot.register_reply(new.identity.clone(), route)
            .map_err(|error| error.to_string())?;
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(DurableRestartAdapter {
            service: service.clone(),
            address: address.clone(),
            claims: AtomicUsize::new(0),
            executions: AtomicUsize::new(0),
        });

        run_channel_input_pump(owner, Arc::clone(&adapter))
            .await
            .map_err(|error| error.to_string())?;

        let outcome = receiver
            .terminal_rx
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            outcome,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed
        ));
        assert_eq!(adapter.claims.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.executions.load(Ordering::SeqCst), 1);
        assert!(
            service
                .list(&address)
                .await
                .map_err(|error| error.to_string())?
                .items
                .is_empty()
        );
        let old_terminal = service
            .submit(
                address,
                old.identity.input_id,
                "old payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            old_terminal.phase,
            echo_agent_app_core::api::conversation_input::ConversationInputPhase::Cancelled
        );
        assert!(old_terminal.attempt.is_none());
        assert!(!old_terminal.drained);
        Ok(())
    }

    struct MismatchedClaimAdapter {
        available: Mutex<bool>,
        exact_selection_observed: AtomicUsize,
        executions: AtomicUsize,
        recoveries: AtomicUsize,
    }

    impl ChannelInputPumpAdapter for MismatchedClaimAdapter {
        type Identity = TestIdentity;
        type Item = TestIdentity;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async move {
                let available = self
                    .available
                    .lock()
                    .map_err(|_| "test mismatch state is unavailable".to_string())?;
                Ok((*available).then_some(TestIdentity("routed-head")))
            })
        }

        fn recover_unroutable<'a>(
            &'a self,
            _identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn claim_next<'a>(
            &'a self,
            expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                if expected_identity == &TestIdentity("routed-head") {
                    self.exact_selection_observed.fetch_add(1, Ordering::SeqCst);
                }
                let mut available = self
                    .available
                    .lock()
                    .map_err(|_| "test mismatch state is unavailable".to_string())?;
                if std::mem::take(&mut *available) {
                    Ok(Some(TestIdentity("route-less-successor")))
                } else {
                    Ok(None)
                }
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            item
        }

        fn execute_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                self.executions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.recoveries.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn changed_frontier_identity_fails_closed_before_execution() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let message = test_message("routed-head");
        let (route, _receiver) = channel_input_reply_route(&message);
        slot.register_reply(TestIdentity("routed-head"), route)
            .map_err(|error| error.to_string())?;
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(MismatchedClaimAdapter {
            available: Mutex::new(true),
            exact_selection_observed: AtomicUsize::new(0),
            executions: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
        });

        run_channel_input_pump(owner, Arc::clone(&adapter))
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(adapter.exact_selection_observed.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.executions.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.recoveries.load(Ordering::SeqCst), 1);
        Ok(())
    }

    struct PreflightRetryAdapter {
        available: AtomicUsize,
        executions: AtomicUsize,
        recoveries: AtomicUsize,
        fail_recovery: bool,
    }

    impl ChannelInputPumpAdapter for PreflightRetryAdapter {
        type Identity = TestIdentity;
        type Item = TestIdentity;

        fn peek_next_identity(&self) -> BoxFuture<'_, Result<Option<Self::Identity>, String>> {
            Box::pin(async move {
                Ok((self.available.load(Ordering::SeqCst) == 1)
                    .then_some(TestIdentity("preflight-retry")))
            })
        }

        fn recover_unroutable<'a>(
            &'a self,
            _identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn claim_next<'a>(
            &'a self,
            expected_identity: &'a Self::Identity,
        ) -> BoxFuture<'a, Result<Option<Self::Item>, String>> {
            Box::pin(async move {
                Ok((self.available.load(Ordering::SeqCst) == 1).then(|| expected_identity.clone()))
            })
        }

        fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
            item
        }

        fn execute_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            reply: ChannelInputReplyRoute,
        ) -> BoxFuture<'a, Result<(), ChannelInputExecutionError>> {
            Box::pin(async move {
                if self.executions.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(ChannelInputExecutionError::before_driver(
                        "injected preflight failure",
                        reply,
                    ));
                }
                self.available.store(0, Ordering::SeqCst);
                let _ = reply
                    .terminal_tx
                    .send(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed);
                Ok(())
            })
        }

        fn recover_claimed<'a>(
            &'a self,
            _item: &'a Self::Item,
            _reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.recoveries.fetch_add(1, Ordering::SeqCst);
                if self.fail_recovery {
                    Err("injected permanent recovery failure".to_string())
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn preflight_failure_restores_the_exact_route_after_recovery() -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let message = test_message("preflight-retry");
        let (route, receiver) = channel_input_reply_route(&message);
        slot.register_reply(TestIdentity("preflight-retry"), route)
            .map_err(|error| error.to_string())?;
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(PreflightRetryAdapter {
            available: AtomicUsize::new(1),
            executions: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
            fail_recovery: false,
        });

        run_channel_input_pump(owner, Arc::clone(&adapter))
            .await
            .map_err(|error| error.to_string())?;

        let outcome = receiver
            .terminal_rx
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            outcome,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed
        ));
        assert_eq!(adapter.executions.load(Ordering::SeqCst), 2);
        assert_eq!(adapter.recoveries.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_recovery_debt_is_bounded_and_preserves_exact_reply_route()
    -> Result<(), String> {
        let slot = Arc::new(ChannelInputPumpSlot::<TestIdentity>::default());
        let identity = TestIdentity("preflight-retry");
        let message = test_message(identity.0);
        let (route, _receiver) = channel_input_reply_route(&message);
        slot.register_reply(identity.clone(), route)
            .map_err(|error| error.to_string())?;
        let owner = match slot.kick().map_err(|error| error.to_string())? {
            ChannelInputPumpKick::Started(owner) => owner,
            ChannelInputPumpKick::Notified => {
                return Err("first kick did not elect an owner".to_string());
            }
        };
        let adapter = Arc::new(PreflightRetryAdapter {
            available: AtomicUsize::new(1),
            executions: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
            fail_recovery: true,
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            run_channel_input_pump(owner, Arc::clone(&adapter)),
        )
        .await
        .map_err(|_| "permanent recovery debt exceeded its bounded wait".to_string())?;
        assert!(matches!(result, Err(ChannelInputPumpError::DurableDebt(_))));
        assert_eq!(adapter.recoveries.load(Ordering::SeqCst), 8);
        assert!(
            slot.has_reply(&identity)
                .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn reply_route_uses_exact_transport_message_id() {
        let message = echo_agent::channels::InboundMessage::new(
            "feishu",
            "sender",
            "chat",
            echo_agent::channels::ChatType::Group,
            "queued",
            "transport-message-42",
        );
        let correlation = ChannelReplyCorrelation::from_inbound(&message);
        assert_eq!(correlation.channel_id, "feishu");
        assert_eq!(correlation.to, message.reply_target());
        assert_eq!(correlation.chat_type, echo_agent::channels::ChatType::Group);
        assert_eq!(correlation.to, "chat");
    }
}
