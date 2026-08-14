//! Application-owned foreground turn admission and cancellation.
//!
//! The framework owns execution and same-turn steering. EKO owns the product
//! rule that one `(surface, conversation)` pair has at most one foreground
//! turn, and that cancellation never releases that ownership before the
//! existing [`crate::chat_driver::TurnOutcome`] has settled.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use echo_agent::agent::{AgentEvent, AgentHandle, CancellationToken};
use tokio::sync::watch;

use crate::chat_driver::{ChatDriverEvent, ChatSink, TurnOutcome, drive_chat};
use crate::chat_resources::ChatResources;
use crate::prepared_turn::PreparedUserTurn;

/// Interactive product surface that owns a foreground turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, rename = "ForegroundTurnSurface")]
pub enum ForegroundTurnSurface {
    Gui,
    Tui,
    Cli,
    Channel,
}

impl fmt::Display for ForegroundTurnSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Channel => "channel",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ForegroundTurnKey {
    surface: ForegroundTurnSurface,
    conversation_id: String,
}

/// Read-only identity and cancellation state for an active foreground turn.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export, rename = "ForegroundTurnSnapshot")]
pub struct ForegroundTurnSnapshot {
    pub surface: ForegroundTurnSurface,
    pub conversation_id: String,
    pub turn_id: String,
    pub cancellation_requested: bool,
}

/// Terminal receipt delivered after the foreground execution future settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundTurnSettlement {
    pub surface: ForegroundTurnSurface,
    pub conversation_id: String,
    pub turn_id: String,
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForegroundTurnError {
    #[error("foreground turn conversation id is empty")]
    EmptyConversationId,
    #[error("foreground turn id is empty")]
    EmptyTurnId,
    #[error(
        "foreground turn is busy for {surface}:{conversation_id}; active turn is {active_turn_id}"
    )]
    Busy {
        surface: ForegroundTurnSurface,
        conversation_id: String,
        active_turn_id: String,
    },
    #[error("no active foreground turn for {surface}:{conversation_id}")]
    NoActiveTurn {
        surface: ForegroundTurnSurface,
        conversation_id: String,
    },
    #[error(
        "foreground turn mismatch for {surface}:{conversation_id}; expected {expected_turn_id}, actual {actual_turn_id}"
    )]
    TurnMismatch {
        surface: ForegroundTurnSurface,
        conversation_id: String,
        expected_turn_id: String,
        actual_turn_id: String,
    },
    #[error("foreground turn admission is suspended for a workspace transition")]
    AdmissionSuspended,
    #[error("foreground turn control is shutting down")]
    ShuttingDown,
    #[error("a foreground turn is active; workspace transition admission cannot be suspended")]
    ActiveTurns,
    #[error("foreground turn control state is unavailable")]
    StateUnavailable,
}

struct ActiveForegroundTurn {
    key: ForegroundTurnKey,
    turn_id: String,
    cancel: CancellationToken,
    settlement_tx: watch::Sender<Option<ForegroundTurnSettlement>>,
}

impl ActiveForegroundTurn {
    fn snapshot(&self) -> ForegroundTurnSnapshot {
        ForegroundTurnSnapshot {
            surface: self.key.surface,
            conversation_id: self.key.conversation_id.clone(),
            turn_id: self.turn_id.clone(),
            cancellation_requested: self.cancel.is_cancelled(),
        }
    }
}

#[derive(Default)]
struct ForegroundTurnState {
    active: HashMap<ForegroundTurnKey, Arc<ActiveForegroundTurn>>,
    admission_suspended: bool,
    shutting_down: bool,
}

#[derive(Default)]
struct ForegroundTurnControlInner {
    state: Mutex<ForegroundTurnState>,
}

/// Single application authority for foreground turn identity and cancellation.
#[derive(Clone, Default)]
pub struct ForegroundTurnControl {
    inner: Arc<ForegroundTurnControlInner>,
}

/// Pauses new foreground turns while an application workspace transition is
/// settling. The active-turn check and suspension bit share one mutex, closing
/// the gap where a turn could enter after a read-only idle snapshot.
#[must_use]
pub(crate) struct ForegroundAdmissionSuspension {
    control: ForegroundTurnControl,
    active: bool,
}

impl Drop for ForegroundAdmissionSuspension {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .control
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.shutting_down {
            state.admission_suspended = false;
        }
        self.active = false;
    }
}

impl ForegroundTurnControl {
    /// Acquire one exact foreground turn. The returned lease owns its token.
    pub fn begin(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<ForegroundTurnLease, ForegroundTurnError> {
        let conversation_id = conversation_id.into();
        if conversation_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyConversationId);
        }
        let turn_id = turn_id.into();
        if turn_id.trim().is_empty() {
            return Err(ForegroundTurnError::EmptyTurnId);
        }
        let key = ForegroundTurnKey {
            surface,
            conversation_id,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ForegroundTurnError::ShuttingDown);
        }
        if state.admission_suspended {
            return Err(ForegroundTurnError::AdmissionSuspended);
        }
        if let Some(existing) = state.active.get(&key) {
            return Err(ForegroundTurnError::Busy {
                surface,
                conversation_id: key.conversation_id.clone(),
                active_turn_id: existing.turn_id.clone(),
            });
        }
        let cancel = CancellationToken::new();
        let (settlement_tx, _) = watch::channel(None);
        let entry = Arc::new(ActiveForegroundTurn {
            key: key.clone(),
            turn_id,
            cancel,
            settlement_tx,
        });
        state.active.insert(key, Arc::clone(&entry));
        Ok(ForegroundTurnLease {
            control: self.clone(),
            entry,
            settled: false,
        })
    }

    /// Snapshot the active turn for one exact product scope.
    pub fn snapshot(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
    ) -> Option<ForegroundTurnSnapshot> {
        let state = self.inner.state.lock().ok()?;
        state
            .active
            .get(&ForegroundTurnKey {
                surface,
                conversation_id: conversation_id.to_string(),
            })
            .map(|entry| entry.snapshot())
    }

    /// Snapshot every active turn for one surface in deterministic order.
    pub fn snapshots(
        &self,
        surface: ForegroundTurnSurface,
    ) -> Result<Vec<ForegroundTurnSnapshot>, ForegroundTurnError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        let mut snapshots = state
            .active
            .values()
            .filter(|entry| entry.key.surface == surface)
            .map(|entry| entry.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.conversation_id
                .cmp(&right.conversation_id)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        Ok(snapshots)
    }

    /// True when any surface owns a turn for this conversation.
    pub fn has_active_conversation(&self, conversation_id: &str) -> bool {
        match self.inner.state.lock() {
            Ok(state) => state
                .active
                .keys()
                .any(|key| key.conversation_id == conversation_id),
            Err(_) => true,
        }
    }

    pub fn has_active_turns(&self) -> bool {
        match self.inner.state.lock() {
            Ok(state) => !state.active.is_empty(),
            Err(_) => true,
        }
    }

    /// Atomically verify idleness and suspend new foreground admission.
    pub(crate) fn suspend_admission_if_idle(
        &self,
    ) -> Result<ForegroundAdmissionSuspension, ForegroundTurnError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        if state.shutting_down {
            return Err(ForegroundTurnError::ShuttingDown);
        }
        if state.admission_suspended {
            return Err(ForegroundTurnError::AdmissionSuspended);
        }
        if !state.active.is_empty() {
            return Err(ForegroundTurnError::ActiveTurns);
        }
        state.admission_suspended = true;
        Ok(ForegroundAdmissionSuspension {
            control: self.clone(),
            active: true,
        })
    }

    /// Request cancellation only when the caller's turn id is still current.
    ///
    /// The returned waiter observes settlement; requesting cancellation does
    /// not remove ownership from the registry.
    pub fn request_cancel(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlementWaiter, ForegroundTurnError> {
        let key = ForegroundTurnKey {
            surface,
            conversation_id: conversation_id.to_string(),
        };
        let entry =
            {
                let state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| ForegroundTurnError::StateUnavailable)?;
                state.active.get(&key).cloned().ok_or_else(|| {
                    ForegroundTurnError::NoActiveTurn {
                        surface,
                        conversation_id: conversation_id.to_string(),
                    }
                })?
            };
        if entry.turn_id != expected_turn_id {
            return Err(ForegroundTurnError::TurnMismatch {
                surface,
                conversation_id: conversation_id.to_string(),
                expected_turn_id: expected_turn_id.to_string(),
                actual_turn_id: entry.turn_id.clone(),
            });
        }
        let settlement_rx = entry.settlement_tx.subscribe();
        entry.cancel.cancel();
        Ok(ForegroundTurnSettlementWaiter { settlement_rx })
    }

    /// Request exact cancellation and wait for the execution future to settle.
    pub async fn cancel_and_wait(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        expected_turn_id: &str,
    ) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        self.request_cancel(surface, conversation_id, expected_turn_id)?
            .wait()
            .await
    }

    /// Permanently close foreground admission, cancel every exact active turn,
    /// and wait for their existing driver leases to publish settlement.
    pub async fn shutdown(&self) -> Result<(), ForegroundTurnError> {
        let waiters = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| ForegroundTurnError::StateUnavailable)?;
            state.shutting_down = true;
            state.admission_suspended = true;
            state
                .active
                .values()
                .map(|entry| {
                    let settlement_rx = entry.settlement_tx.subscribe();
                    entry.cancel.cancel();
                    ForegroundTurnSettlementWaiter { settlement_rx }
                })
                .collect::<Vec<_>>()
        };
        for waiter in waiters {
            waiter.wait().await?;
        }
        Ok(())
    }

    fn settle(&self, entry: &Arc<ActiveForegroundTurn>, outcome: TurnOutcome) {
        let settlement = ForegroundTurnSettlement {
            surface: entry.key.surface,
            conversation_id: entry.key.conversation_id.clone(),
            turn_id: entry.turn_id.clone(),
            outcome,
        };
        if let Ok(mut state) = self.inner.state.lock()
            && state
                .active
                .get(&entry.key)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            state.active.remove(&entry.key);
        }
        entry.settlement_tx.send_replace(Some(settlement));
    }
}

/// Wait handle returned by an exact-id cancellation request.
pub struct ForegroundTurnSettlementWaiter {
    settlement_rx: watch::Receiver<Option<ForegroundTurnSettlement>>,
}

impl ForegroundTurnSettlementWaiter {
    pub async fn wait(mut self) -> Result<ForegroundTurnSettlement, ForegroundTurnError> {
        loop {
            if let Some(settlement) = self.settlement_rx.borrow().clone() {
                return Ok(settlement);
            }
            self.settlement_rx
                .changed()
                .await
                .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        }
    }
}

/// RAII ownership for one foreground turn.
///
/// Normal execution and explicit cancellation call [`Self::settle`] only after
/// the outer driver future returns its existing `TurnOutcome`. Dropping an
/// unfinished lease means that outer future was abandoned: Drop requests token
/// cancellation and publishes a defensive `Cancelled` receipt, but does not
/// claim that an independently running framework future was awaited.
pub struct ForegroundTurnLease {
    control: ForegroundTurnControl,
    entry: Arc<ActiveForegroundTurn>,
    settled: bool,
}

impl ForegroundTurnLease {
    pub fn surface(&self) -> ForegroundTurnSurface {
        self.entry.key.surface
    }

    pub fn conversation_id(&self) -> &str {
        &self.entry.key.conversation_id
    }

    pub fn turn_id(&self) -> &str {
        &self.entry.turn_id
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.entry.cancel.clone()
    }

    pub fn settle(mut self, outcome: TurnOutcome) -> ForegroundTurnSettlement {
        let settlement = ForegroundTurnSettlement {
            surface: self.surface(),
            conversation_id: self.conversation_id().to_string(),
            turn_id: self.turn_id().to_string(),
            outcome: outcome.clone(),
        };
        self.control.settle(&self.entry, outcome);
        self.settled = true;
        settlement
    }
}

impl Drop for ForegroundTurnLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.entry.cancel.cancel();
        self.control.settle(&self.entry, TurnOutcome::Cancelled);
        self.settled = true;
    }
}

struct CancellationAwareChatSink {
    inner: Arc<dyn ChatSink>,
    cancel: CancellationToken,
    delivery: Arc<DownstreamDeliveryState>,
}

#[derive(Default)]
struct DownstreamDeliveryState {
    rejected: AtomicBool,
    terminal_delivered: AtomicBool,
}

impl DownstreamDeliveryState {
    fn terminal_was_delivered(&self) -> bool {
        self.terminal_delivered.load(Ordering::Acquire)
    }

    fn terminal_delivery_failed(&self) -> bool {
        self.rejected.load(Ordering::Acquire) && !self.terminal_was_delivered()
    }
}

impl ChatSink for CancellationAwareChatSink {
    fn on_event(&self, event: ChatDriverEvent) -> bool {
        if self.delivery.rejected.load(Ordering::Acquire) {
            return false;
        }
        let terminal = matches!(
            &event,
            ChatDriverEvent::Agent(envelope)
                if matches!(
                    &envelope.payload,
                    AgentEvent::FinalAnswer(_) | AgentEvent::Cancelled | AgentEvent::Error { .. }
                )
        );
        let accepted = self.inner.on_event(event);
        if accepted && terminal {
            self.delivery
                .terminal_delivered
                .store(true, Ordering::Release);
        } else if !accepted {
            self.delivery.rejected.store(true, Ordering::Release);
            self.cancel.cancel();
        }
        accepted
    }
}

fn normalize_downstream_outcome(
    result: Result<TurnOutcome, String>,
    delivery: &DownstreamDeliveryState,
) -> Result<TurnOutcome, String> {
    if delivery.terminal_delivery_failed() {
        // Events already accepted by the consumer remain delivered; only the
        // authoritative terminal result is replaced when that consumer closed
        // before accepting a terminal event.
        return Ok(TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message(
                "downstream_disconnect",
                "chat event consumer closed before terminal delivery",
            ),
        ));
    }
    result
}

/// Run the existing shared chat driver under one application foreground lease.
///
/// This adds no second execution state machine. It binds the driver's existing
/// `TurnOutcome` to the product owner, and wraps the downstream sink so a closed
/// renderer cancels the exact same token before the driver settles.
pub async fn drive_foreground_chat(
    lease: ForegroundTurnLease,
    agent: &AgentHandle,
    turn: &PreparedUserTurn,
    resources: Arc<ChatResources>,
) -> Result<TurnOutcome, String> {
    let identity_error = if resources.conv_id.as_deref() != Some(lease.conversation_id()) {
        Some("foreground conversation id does not match chat resources".to_string())
    } else if resources.root_message_id != lease.turn_id() {
        Some("foreground turn id does not match chat resources".to_string())
    } else {
        None
    };
    if let Some(error) = identity_error {
        let outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_turn",
            error.clone(),
        ));
        lease.settle(outcome);
        return Err(error);
    }

    let cancel = lease.cancellation_token();
    let delivery = Arc::new(DownstreamDeliveryState::default());
    let sink: Arc<dyn ChatSink> = Arc::new(CancellationAwareChatSink {
        inner: Arc::clone(&resources.sink),
        cancel: cancel.clone(),
        delivery: Arc::clone(&delivery),
    });
    let controlled_resources = Arc::new(ChatResources {
        pool: resources.pool.clone(),
        store: resources.store.clone(),
        sink,
        webhook_emitter: resources.webhook_emitter.clone(),
        conv_id: resources.conv_id.clone(),
        root_message_id: resources.root_message_id.clone(),
        attachments: resources.attachments.clone(),
        cancel,
        mode_hint: resources.mode_hint.clone(),
        interaction_mode: resources.interaction_mode,
        review_integration: resources.review_integration.clone(),
        layer_manager: resources.layer_manager.clone(),
        memory_generation: resources.memory_generation.clone(),
    });
    let result = normalize_downstream_outcome(
        drive_chat(agent, turn, controlled_resources).await,
        delivery.as_ref(),
    );
    let settlement_outcome = result.clone().unwrap_or_else(|error| {
        TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_turn",
            error,
        ))
    });
    lease.settle(settlement_outcome);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClosedSink;

    impl ChatSink for ClosedSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }
    }

    #[test]
    fn scopes_admission_by_surface_and_conversation() -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let gui = control.begin(ForegroundTurnSurface::Gui, "conversation", "gui-turn")?;
        let busy = control.begin(ForegroundTurnSurface::Gui, "conversation", "second");
        assert!(matches!(
            busy,
            Err(ForegroundTurnError::Busy {
                active_turn_id,
                ..
            }) if active_turn_id == "gui-turn"
        ));
        let tui = control.begin(ForegroundTurnSurface::Tui, "conversation", "tui-turn")?;
        assert_eq!(
            control.snapshots(ForegroundTurnSurface::Gui)?,
            vec![ForegroundTurnSnapshot {
                surface: ForegroundTurnSurface::Gui,
                conversation_id: "conversation".to_string(),
                turn_id: "gui-turn".to_string(),
                cancellation_requested: false,
            }]
        );
        gui.settle(TurnOutcome::Completed);
        tui.settle(TurnOutcome::Completed);
        assert!(!control.has_active_turns());
        Ok(())
    }

    #[test]
    fn workspace_transition_suspension_is_atomic_and_reopens_admission()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let active = control.begin(ForegroundTurnSurface::Gui, "conversation", "active")?;
        assert!(matches!(
            control.suspend_admission_if_idle(),
            Err(ForegroundTurnError::ActiveTurns)
        ));
        active.settle(TurnOutcome::Completed);

        let transition = control.suspend_admission_if_idle()?;
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Tui, "blocked", "turn"),
            Err(ForegroundTurnError::AdmissionSuspended)
        ));
        drop(transition);

        let reopened = control.begin(ForegroundTurnSurface::Tui, "reopened", "turn")?;
        reopened.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn exact_cancel_rejects_stale_and_cross_conversation_ids()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-a")?;
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-b", "turn-a"),
            Err(ForegroundTurnError::NoActiveTurn { .. })
        ));
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "stale"),
            Err(ForegroundTurnError::TurnMismatch { .. })
        ));
        assert!(!lease.cancellation_token().is_cancelled());

        let waiter =
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "turn-a")?;
        assert!(lease.cancellation_token().is_cancelled());
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation-a")
                .is_some()
        );
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-b"),
            Err(ForegroundTurnError::Busy { .. })
        ));
        lease.settle(TurnOutcome::Cancelled);
        let settlement = waiter.wait().await?;
        assert_eq!(settlement.outcome, TurnOutcome::Cancelled);
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation-a")
                .is_none()
        );
        let next = control.begin(ForegroundTurnSurface::Gui, "conversation-a", "turn-b")?;
        assert!(matches!(
            control.request_cancel(ForegroundTurnSurface::Gui, "conversation-a", "turn-a"),
            Err(ForegroundTurnError::TurnMismatch { .. })
        ));
        assert!(!next.cancellation_token().is_cancelled());
        next.settle(TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn closed_sink_cancels_same_token_and_waiter_blocks_until_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Channel, "sender", "turn")?;
        let token = lease.cancellation_token();
        let sink = CancellationAwareChatSink {
            inner: Arc::new(ClosedSink),
            cancel: token.clone(),
            delivery: Arc::new(DownstreamDeliveryState::default()),
        };
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert!(token.is_cancelled());

        let waiter = control.request_cancel(ForegroundTurnSurface::Channel, "sender", "turn")?;
        let mut wait_task = tokio::spawn(waiter.wait());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait_task)
                .await
                .is_err()
        );
        let outcome =
            normalize_downstream_outcome(Ok(TurnOutcome::Cancelled), sink.delivery.as_ref())
                .map_err(|_| ForegroundTurnError::StateUnavailable)?;
        lease.settle(outcome);
        let settlement = wait_task
            .await
            .map_err(|_| ForegroundTurnError::StateUnavailable)??;
        assert_eq!(settlement.turn_id, "turn");
        assert!(matches!(
            settlement.outcome,
            TurnOutcome::Failed(failure) if failure.code == "downstream_disconnect"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dropped_lease_cancels_and_settles() -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Cli, "conversation", "turn")?;
        let token = lease.cancellation_token();
        let waiter = control.request_cancel(ForegroundTurnSurface::Cli, "conversation", "turn")?;
        drop(lease);
        assert!(token.is_cancelled());
        let settlement = waiter.wait().await?;
        assert_eq!(settlement.outcome, TurnOutcome::Cancelled);
        assert!(!control.has_active_turns());
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_closes_admission_and_waits_for_exact_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let token = lease.cancellation_token();
        let shutdown = control.shutdown();
        let settlement = async move {
            token.cancelled().await;
            lease.settle(TurnOutcome::Cancelled);
        };
        let (shutdown_result, ()) = tokio::join!(shutdown, settlement);
        shutdown_result?;
        assert!(matches!(
            control.begin(ForegroundTurnSurface::Gui, "conversation", "next"),
            Err(ForegroundTurnError::ShuttingDown)
        ));
        control.shutdown().await
    }

    #[tokio::test]
    async fn explicit_cancel_and_wait_remains_cancelled_after_driver_settlement()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Tui, "conversation", "turn")?;
        let cancellation =
            control.cancel_and_wait(ForegroundTurnSurface::Tui, "conversation", "turn");
        let settlement = async move {
            tokio::task::yield_now().await;
            lease.settle(TurnOutcome::Cancelled);
        };
        let (result, _) = tokio::join!(cancellation, settlement);
        assert_eq!(result?.outcome, TurnOutcome::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn settlement_publishes_one_existing_failed_outcome_to_every_waiter()
    -> Result<(), ForegroundTurnError> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let first = control.request_cancel(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let second = control.request_cancel(ForegroundTurnSurface::Gui, "conversation", "turn")?;
        let outcome = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "test",
            "terminal failure",
        ));
        let receipt = lease.settle(outcome.clone());
        assert_eq!(receipt.outcome, outcome);
        assert_eq!(first.wait().await?.outcome, outcome);
        assert_eq!(second.wait().await?.outcome, outcome);
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Gui, "conversation")
                .is_none()
        );
        Ok(())
    }
}
