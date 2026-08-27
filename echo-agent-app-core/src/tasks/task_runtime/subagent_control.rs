//! Durable EKO control commands for exact Subagent attempts.
//!
//! The framework owns live mailbox delivery and cancellation. This module
//! validates TaskRun identity, records commands in `events.jsonl`, and keeps a
//! process-only route to the exact framework executor currently dispatching an
//! attempt. It does not own a second mailbox, scheduler, or retry loop.

use std::collections::HashMap;
use std::sync::Arc;

use echo_agent::agent::subagent::{
    SubagentAttemptIdentity, SubagentControlPhase as FrameworkSubagentControlPhase,
    SubagentExecutor,
};

use super::run_authority::RuntimeJournalEvent;
use super::store::{StoreError, TaskRuntimeStore};
use super::types::{
    RuntimeEventKind, SubagentControlActorSource, SubagentControlIdentity, SubagentControlOutcome,
    SubagentControlPhase, SubagentControlReceipt, SubagentControlStatus, SubagentGuidanceKind,
    TaskRunStatus,
};

#[derive(Clone)]
pub(super) struct ActiveSubagentControlTarget {
    identity: SubagentControlIdentity,
    executor: Arc<SubagentExecutor>,
}

impl ActiveSubagentControlTarget {
    pub(super) fn belongs_to_run(&self, run_id: &str) -> bool {
        self.identity.run_id == run_id
    }
}

/// Removes only the exact route registered for this physical execution.
pub(crate) struct SubagentControlTargetGuard {
    store: Arc<TaskRuntimeStore>,
    execution_id: String,
    command_identity: SubagentControlIdentity,
}

impl Drop for SubagentControlTargetGuard {
    fn drop(&mut self) {
        let mut targets = self
            .store
            .active_subagent_controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if targets
            .get(&self.execution_id)
            .is_some_and(|target| same_attempt(&target.identity, &self.command_identity))
        {
            targets.remove(&self.execution_id);
        }
    }
}

/// One application service shared by GUI, TUI, CLI, and channel adapters.
#[derive(Clone)]
pub struct SubagentControlService {
    store: Arc<TaskRuntimeStore>,
    blocking: super::executor::TaskRuntimeBlockingAdapter,
    #[cfg(test)]
    command_test_barrier: Option<Arc<SubagentControlTestBarrier>>,
    #[cfg(test)]
    settlement_failures: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
struct SubagentControlTestBarrier {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl SubagentControlService {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        Self {
            blocking: super::executor::TaskRuntimeBlockingAdapter::new(store.clone()),
            store,
            #[cfg(test)]
            command_test_barrier: None,
            #[cfg(test)]
            settlement_failures: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    fn with_command_test_barrier(mut self, barrier: Arc<SubagentControlTestBarrier>) -> Self {
        self.command_test_barrier = Some(barrier);
        self
    }

    #[cfg(test)]
    async fn wait_at_command_test_barrier(&self) {
        if let Some(barrier) = self.command_test_barrier.as_ref() {
            barrier.entered.notify_one();
            barrier.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn wait_at_command_test_barrier(&self) {}

    #[cfg(test)]
    fn fail_next_settlements(&self, count: usize) {
        self.settlement_failures
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    fn consume_settlement_failure(&self) -> bool {
        self.settlement_failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
    }

    #[cfg(not(test))]
    fn consume_settlement_failure(&self) -> bool {
        false
    }

    /// Persist guidance for exactly one not-yet-started attempt. The task
    /// dispatcher transfers it to the framework queue immediately before
    /// `dispatch_attempt`, so process restarts cannot silently retarget it.
    pub fn queue_guidance(
        &self,
        identity: SubagentControlIdentity,
        instruction: &str,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        validate_instruction(instruction)?;
        let run_id = identity.run_id.clone();
        self.store.with_run_lock(&run_id, || {
            if let Some(receipt) = existing_receipt(&self.store, &identity)? {
                return Ok(receipt);
            }
            validate_plan_target(&self.store, &identity)?;
            validate_next_attempt(&self.store, &identity)?;
            append_guidance_event(
                &self.store,
                &identity,
                RuntimeEventKind::SubagentGuidanceQueued,
                SubagentGuidanceKind::NextAttempt,
                actor_source,
                Some(instruction),
                serde_json::json!({}),
            )?;
            Ok(pending_receipt(identity))
        })
    }

    /// Async-surface wrapper for queued guidance. The synchronous method stays
    /// available to blocking adapters and tests; GUI/channel callers use this
    /// bounded path so journal I/O never runs on a Tokio async executor thread.
    pub async fn queue_guidance_async(
        &self,
        identity: SubagentControlIdentity,
        instruction: String,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        self.blocking
            .run_store("queue Subagent guidance", move |store| {
                Self::new(store).queue_guidance(identity, &instruction, actor_source)
            })
            .await
    }

    /// Deliver one message to the existing safe point of an exact active
    /// attempt. The durable queued boundary is written before framework IO.
    pub async fn send_message(
        &self,
        identity: SubagentControlIdentity,
        instruction: &str,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        validate_instruction(instruction)?;
        let service = self.clone();
        let instruction = instruction.to_string();
        self.blocking
            .run_async_owned("deliver live Subagent guidance", async move {
                service
                    .send_message_owned(identity, instruction, actor_source)
                    .await
            })
            .await
    }

    async fn send_message_owned(
        &self,
        identity: SubagentControlIdentity,
        instruction: String,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        validate_instruction(&instruction)?;
        let command_run_id = identity.run_id.clone();
        let begin_identity = identity.clone();
        let owned_instruction = instruction.clone();
        let begin = self
            .blocking
            .run_store("begin live Subagent guidance", move |store| {
                store.with_run_lock(&command_run_id, || {
                    if let Some(receipt) = existing_receipt(&store, &begin_identity)? {
                        return Ok(ControlBegin::Existing(receipt));
                    }
                    validate_plan_target(&store, &begin_identity)?;
                    match exact_active_target(&store, &begin_identity) {
                        Ok(target) => {
                            append_guidance_event(
                                &store,
                                &begin_identity,
                                RuntimeEventKind::SubagentGuidanceQueued,
                                SubagentGuidanceKind::LiveMessage,
                                actor_source,
                                Some(&owned_instruction),
                                serde_json::json!({}),
                            )?;
                            Ok(ControlBegin::New(target))
                        }
                        Err(error) => {
                            let detail = error.to_string();
                            store.commit_runtime_events(
                                &begin_identity.run_id,
                                vec![
                                    guidance_event(
                                        &begin_identity,
                                        RuntimeEventKind::SubagentGuidanceQueued,
                                        SubagentGuidanceKind::LiveMessage,
                                        actor_source,
                                        Some(&owned_instruction),
                                        serde_json::json!({}),
                                    ),
                                    guidance_event(
                                        &begin_identity,
                                        RuntimeEventKind::SubagentGuidanceRejected,
                                        SubagentGuidanceKind::LiveMessage,
                                        actor_source,
                                        None,
                                        serde_json::json!({ "reason": detail }),
                                    ),
                                ],
                            )?;
                            Ok(ControlBegin::Existing(rejected_receipt(
                                begin_identity.clone(),
                                detail,
                            )))
                        }
                    }
                })
            })
            .await?;
        let ControlBegin::New(target) = begin else {
            return begin.into_receipt();
        };
        self.wait_at_command_test_barrier().await;

        let delivery_executor = target.executor.clone();
        let delivery_execution_id = identity.execution_id.clone();
        let delivery_attempt = identity.attempt;
        let delivery_instruction = instruction.clone();
        let delivery = match tokio::spawn(async move {
            delivery_executor
                .send_message_tracked(
                    &delivery_execution_id,
                    delivery_attempt,
                    &delivery_instruction,
                )
                .await
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(delivery) => delivery,
            Err(error) => Err(format!(
                "framework Subagent guidance task failed to join: {error}"
            )),
        };
        let run_id = identity.run_id.clone();
        let settlement_identity = identity.clone();
        let mut delay = std::time::Duration::from_millis(25);
        loop {
            if self.consume_settlement_failure() {
                tokio::time::sleep(delay).await;
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(1));
                continue;
            }
            let operation_run_id = run_id.clone();
            let operation_identity = settlement_identity.clone();
            let operation_delivery = delivery.clone();
            let settled = self
                .blocking
                .run_store("settle live Subagent guidance", move |store| {
                    store.with_run_lock(&operation_run_id, || match operation_delivery {
                        Ok(receipt) => {
                            let turn_id = receipt.receipt().turn_id().to_string();
                            store.commit_runtime_events(
                                &operation_identity.run_id,
                                vec![
                                    guidance_event(
                                        &operation_identity,
                                        RuntimeEventKind::SubagentGuidanceMailboxAccepted,
                                        SubagentGuidanceKind::LiveMessage,
                                        actor_source,
                                        None,
                                        serde_json::json!({
                                            "framework_turn_id": turn_id,
                                        }),
                                    ),
                                    guidance_event(
                                        &operation_identity,
                                        RuntimeEventKind::SubagentGuidanceDelivered,
                                        SubagentGuidanceKind::LiveMessage,
                                        actor_source,
                                        None,
                                        serde_json::json!({
                                            "framework_turn_id": turn_id,
                                            "boundary": "mailbox_accepted",
                                        }),
                                    ),
                                ],
                            )?;
                            Ok(Ok((receipt, turn_id)))
                        }
                        Err(detail) => {
                            append_guidance_event(
                                &store,
                                &operation_identity,
                                RuntimeEventKind::SubagentGuidanceRejected,
                                SubagentGuidanceKind::LiveMessage,
                                actor_source,
                                None,
                                serde_json::json!({ "reason": detail }),
                            )?;
                            Ok(Err(detail))
                        }
                    })
                })
                .await;
            match settled {
                Ok(Ok((receipt, turn_id))) => {
                    if let Err(error) = self.spawn_live_guidance_observer(
                        settlement_identity.clone(),
                        actor_source,
                        receipt,
                    ) {
                        self.blocking
                            .record_lifecycle_debt("observe Subagent guidance lifecycle", &error);
                    }
                    return Ok(SubagentControlReceipt {
                        identity: settlement_identity,
                        status: SubagentControlStatus::Delivered,
                        phase: SubagentControlPhase::MailboxAccepted,
                        outcome: None,
                        drained: None,
                        detail: None,
                        framework_turn_id: Some(turn_id),
                    });
                }
                Ok(Err(detail)) => {
                    return Ok(rejected_receipt(settlement_identity, detail));
                }
                Err(error) => {
                    tracing::warn!(%error, command_id = %identity.command_id, "retrying live Subagent guidance settlement");
                    tokio::time::sleep(delay).await;
                    delay = delay
                        .saturating_mul(2)
                        .min(std::time::Duration::from_secs(1));
                }
            }
        }
    }

    /// Keep framework drain and turn settlement durable after the caller has
    /// received mailbox acceptance. The framework receipt remains the only
    /// real-time lifecycle authority; typed app events are a rebuildable EKO
    /// projection and never drive delivery themselves.
    fn spawn_live_guidance_observer(
        &self,
        identity: SubagentControlIdentity,
        actor_source: SubagentControlActorSource,
        receipt: echo_agent::agent::subagent::SubagentMessageReceipt,
    ) -> Result<(), StoreError> {
        let reservation = self
            .blocking
            .reserve_settlement("observe Subagent guidance lifecycle")?;
        let blocking = self.blocking.clone();
        let observer_blocking = blocking.clone();
        let observer_identity = identity.clone();
        self.blocking.spawn_reserved_settlement(
            "observe Subagent guidance lifecycle",
            reservation,
            async move {
                let mut receipt = receipt;
                let drained = receipt.wait_for_drained().await;
                let turn_id = receipt.receipt().turn_id().to_string();
                if drained.was_drained() {
                    let drained_identity = observer_identity.clone();
                    let drained_turn_id = turn_id.clone();
                    observer_blocking
                        .run_store("persist Subagent guidance drain", move |store| {
                            append_guidance_event(
                                &store,
                                &drained_identity,
                                RuntimeEventKind::SubagentGuidanceDrained,
                                SubagentGuidanceKind::LiveMessage,
                                actor_source,
                                None,
                                serde_json::json!({
                                    "framework_turn_id": drained_turn_id,
                                    "drained": true,
                                }),
                            )
                        })
                        .await?;
                }
                let settled = receipt.wait_for_turn_settled().await;
                let (outcome, drained) = match settled {
                    echo_agent::agent::AgentSteerState::TurnSettled { outcome, drained } => {
                        (framework_outcome_name(outcome), drained)
                    }
                    state => ("dropped".to_string(), state.was_drained()),
                };
                let settled_identity = observer_identity;
                blocking
                    .run_store("persist Subagent guidance settlement", move |store| {
                        append_guidance_event(
                            &store,
                            &settled_identity,
                            RuntimeEventKind::SubagentGuidanceSettled,
                            SubagentGuidanceKind::LiveMessage,
                            actor_source,
                            None,
                            serde_json::json!({
                                "framework_turn_id": turn_id,
                                "outcome": outcome,
                                "drained": drained,
                            }),
                        )
                    })
                    .await
            },
        );
        Ok(())
    }

    /// Interrupt one exact active attempt without pausing or cancelling its
    /// parent TaskRun. The framework waits for dispatch settlement.
    pub async fn interrupt_subagent(
        &self,
        identity: SubagentControlIdentity,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        let service = self.clone();
        self.blocking
            .run_async_owned("interrupt exact Subagent attempt", async move {
                service
                    .interrupt_subagent_owned(identity, actor_source)
                    .await
            })
            .await
    }

    async fn interrupt_subagent_owned(
        &self,
        identity: SubagentControlIdentity,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        let command_run_id = identity.run_id.clone();
        let begin_identity = identity.clone();
        let begin = self
            .blocking
            .run_store("begin Subagent interrupt", move |store| {
                store.with_run_lock(&command_run_id, || {
                    if let Some(receipt) = existing_receipt(&store, &begin_identity)? {
                        return Ok(ControlBegin::Existing(receipt));
                    }
                    validate_plan_target(&store, &begin_identity)?;
                    match exact_active_target(&store, &begin_identity) {
                        Ok(target) => {
                            append_interrupt_event(
                                &store,
                                &begin_identity,
                                RuntimeEventKind::SubagentInterruptRequested,
                                actor_source,
                                serde_json::json!({}),
                            )?;
                            Ok(ControlBegin::New(target))
                        }
                        Err(error) => {
                            let detail = error.to_string();
                            store.commit_runtime_events(
                                &begin_identity.run_id,
                                vec![
                                    interrupt_event(
                                        &begin_identity,
                                        RuntimeEventKind::SubagentInterruptRequested,
                                        actor_source,
                                        serde_json::json!({}),
                                    ),
                                    interrupt_event(
                                        &begin_identity,
                                        RuntimeEventKind::SubagentInterruptSettled,
                                        actor_source,
                                        serde_json::json!({ "accepted": false, "reason": detail }),
                                    ),
                                ],
                            )?;
                            Ok(ControlBegin::Existing(rejected_receipt(
                                begin_identity.clone(),
                                detail,
                            )))
                        }
                    }
                })
            })
            .await?;
        let ControlBegin::New(target) = begin else {
            return begin.into_receipt();
        };
        self.wait_at_command_test_barrier().await;

        let interrupt_executor = target.executor.clone();
        let interrupt_execution_id = identity.execution_id.clone();
        let interrupt_attempt = identity.attempt;
        let outcome = match tokio::spawn(async move {
            interrupt_executor
                .interrupt_subagent(&interrupt_execution_id, interrupt_attempt)
                .await
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => Err(format!("framework Subagent interrupt panicked: {error}")),
        };
        let (status, detail, payload) = match outcome {
            Ok(outcome) => {
                let terminal_status = outcome
                    .terminal_status
                    .map(|status| status.as_str().to_string());
                let detail = terminal_status.clone().or_else(|| {
                    Some(format!(
                        "previous_status={}",
                        phase_name(outcome.previous_status)
                    ))
                });
                (
                    SubagentControlStatus::Settled,
                    detail,
                    serde_json::json!({
                        "accepted": true,
                        "requested": outcome.requested,
                        "settled": outcome.settled,
                        "previous_status": phase_name(outcome.previous_status),
                        "terminal_status": terminal_status,
                    }),
                )
            }
            Err(detail) => (
                SubagentControlStatus::Rejected,
                Some(detail.clone()),
                serde_json::json!({ "accepted": false, "reason": detail }),
            ),
        };
        let run_id = identity.run_id.clone();
        let settlement_identity = identity.clone();
        let mut delay = std::time::Duration::from_millis(25);
        loop {
            if self.consume_settlement_failure() {
                tokio::time::sleep(delay).await;
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(1));
                continue;
            }
            let operation_run_id = run_id.clone();
            let operation_identity = settlement_identity.clone();
            let operation_status = status;
            let operation_detail = detail.clone();
            let operation_payload = payload.clone();
            match self
                .blocking
                .run_store("settle Subagent interrupt", move |store| {
                    store.with_run_lock(&operation_run_id, || {
                        append_interrupt_event(
                            &store,
                            &operation_identity,
                            RuntimeEventKind::SubagentInterruptSettled,
                            actor_source,
                            operation_payload,
                        )?;
                        Ok(SubagentControlReceipt {
                            identity: operation_identity,
                            status: operation_status,
                            phase: SubagentControlPhase::TurnSettled,
                            outcome: None,
                            drained: None,
                            detail: operation_detail,
                            framework_turn_id: None,
                        })
                    })
                })
                .await
            {
                Ok(receipt) => return Ok(receipt),
                Err(error) => {
                    tracing::warn!(%error, command_id = %identity.command_id, "retrying Subagent interrupt settlement");
                    tokio::time::sleep(delay).await;
                    delay = delay
                        .saturating_mul(2)
                        .min(std::time::Duration::from_secs(1));
                }
            }
        }
    }
}

enum ControlBegin {
    Existing(SubagentControlReceipt),
    New(ActiveSubagentControlTarget),
}

impl ControlBegin {
    fn into_receipt(self) -> Result<SubagentControlReceipt, StoreError> {
        match self {
            Self::Existing(receipt) => Ok(receipt),
            Self::New(_) => Err(StoreError::InvalidPlan(
                "new Subagent control target was consumed before delivery".to_string(),
            )),
        }
    }
}

impl TaskRuntimeStore {
    /// Persist assignment and publish its process-scoped executor route under
    /// the same per-run lock, closing the assigned/controllable race.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_controlled_subagent_assigned(
        self: &Arc<Self>,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        agent_name: &str,
        task_subject: &str,
        plan_revision: u64,
        attempt: u32,
        replay_safe: bool,
        dispatch_hook: bool,
        executor: Arc<SubagentExecutor>,
    ) -> Result<SubagentControlTargetGuard, StoreError> {
        let identity = SubagentControlIdentity {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            execution_id: execution_id.to_string(),
            plan_revision,
            attempt,
            command_id: String::new(),
        };
        self.with_run_lock(run_id, || {
            self.record_subagent_assigned(
                run_id,
                task_id,
                execution_id,
                agent_name,
                task_subject,
                plan_revision,
                attempt,
                replay_safe,
                dispatch_hook,
            )?;
            self.active_subagent_controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    execution_id.to_string(),
                    ActiveSubagentControlTarget {
                        identity: identity.clone(),
                        executor,
                    },
                );
            Ok(())
        })?;
        Ok(SubagentControlTargetGuard {
            store: self.clone(),
            execution_id: execution_id.to_string(),
            command_identity: identity,
        })
    }

    /// Transfer all pending exact-attempt guidance to the framework queue once.
    pub(crate) fn deliver_pending_subagent_guidance(
        &self,
        target: &SubagentControlIdentity,
        executor: &SubagentExecutor,
    ) -> Result<usize, StoreError> {
        self.with_run_lock(&target.run_id, || {
            // Audit allowlist: queued guidance transfer folds the complete
            // durable command journal for one logical attempt.
            let events = self.list_events(&target.run_id, 0)?;
            let states = command_states(&events)?;
            let pending = events
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::SubagentGuidanceQueued
                        && event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            == Some(SubagentGuidanceKind::NextAttempt.as_str())
                        && event.task_id.as_deref() == Some(target.task_id.as_str())
                        && payload_u64(event, "plan_revision") == Some(target.plan_revision)
                        && payload_u64(event, "attempt") == Some(u64::from(target.attempt))
                })
                .filter_map(|event| {
                    let command_id = event
                        .payload
                        .get("command_id")
                        .and_then(serde_json::Value::as_str)?;
                    (states.get(command_id) == Some(&SubagentControlStatus::Pending))
                        .then_some((command_id.to_string(), event))
                })
                .collect::<Vec<_>>();
            let mut delivered = 0usize;
            for (command_id, event) in pending {
                let instruction = event
                    .payload
                    .get("instruction")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let identity = SubagentControlIdentity {
                    run_id: target.run_id.clone(),
                    task_id: target.task_id.clone(),
                    execution_id: target.execution_id.clone(),
                    plan_revision: target.plan_revision,
                    attempt: target.attempt,
                    command_id,
                };
                let actor_source = event
                    .payload
                    .get("actor_source")
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_actor_source)
                    .unwrap_or(SubagentControlActorSource::Cli);
                match executor.queue_guidance(&target.task_id, target.attempt, instruction) {
                    Ok(receipt) => {
                        append_guidance_event(
                            self,
                            &identity,
                            RuntimeEventKind::SubagentGuidanceDelivered,
                            SubagentGuidanceKind::NextAttempt,
                            actor_source,
                            None,
                            serde_json::json!({
                                "queued_count": receipt.queued_count,
                                "target_execution_id": target.execution_id,
                            }),
                        )?;
                        delivered = delivered.saturating_add(1);
                    }
                    Err(error) => {
                        append_guidance_event(
                            self,
                            &identity,
                            RuntimeEventKind::SubagentGuidanceRejected,
                            SubagentGuidanceKind::NextAttempt,
                            actor_source,
                            None,
                            serde_json::json!({ "reason": error.to_string() }),
                        )?;
                    }
                }
            }
            Ok(delivered)
        })
    }
}

fn validate_instruction(instruction: &str) -> Result<(), StoreError> {
    if instruction.trim().is_empty() {
        return Err(StoreError::InvalidPlan(
            "Subagent instruction must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_plan_target(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
) -> Result<(), StoreError> {
    for (field, value) in [
        ("run_id", identity.run_id.as_str()),
        ("task_id", identity.task_id.as_str()),
        ("execution_id", identity.execution_id.as_str()),
        ("command_id", identity.command_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(StoreError::InvalidPlan(format!(
                "Subagent control identity field {field} must not be empty"
            )));
        }
    }
    if identity.plan_revision == 0 || identity.attempt == 0 {
        return Err(StoreError::InvalidPlan(
            "Subagent control plan revision and attempt must be positive".to_string(),
        ));
    }
    let run = store
        .get_run(&identity.run_id)?
        .ok_or_else(|| StoreError::RunNotFound(identity.run_id.clone()))?;
    if matches!(
        run.status,
        TaskRunStatus::Completed | TaskRunStatus::Cancelled
    ) {
        return Err(StoreError::InvalidPlan(format!(
            "TaskRun {} is terminal as {}",
            identity.run_id,
            run.status.as_str()
        )));
    }
    let plan = store
        .get_plan(&identity.run_id)?
        .ok_or_else(|| StoreError::PlanNotFound(identity.run_id.clone()))?;
    if plan.revision != identity.plan_revision {
        return Err(StoreError::PlanConflict {
            run_id: identity.run_id.clone(),
            expected: identity.plan_revision,
            current: plan.revision,
        });
    }
    if !plan.tasks.iter().any(|task| task.id == identity.task_id) {
        return Err(StoreError::TaskNotFound(identity.task_id.clone()));
    }
    Ok(())
}

fn validate_next_attempt(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
) -> Result<(), StoreError> {
    let plan = store
        .get_plan(&identity.run_id)?
        .ok_or_else(|| StoreError::PlanNotFound(identity.run_id.clone()))?;
    let task = plan
        .tasks
        .iter()
        .find(|task| task.id == identity.task_id)
        .ok_or_else(|| StoreError::TaskNotFound(identity.task_id.clone()))?;
    let latest_attempt = store
        .list_subagent_runs(&identity.run_id)?
        .into_iter()
        .filter(|run| run.task_id == identity.task_id)
        .map(|run| run.attempt)
        .max()
        .unwrap_or(0);
    let expected = latest_attempt
        .max(task.retry_count)
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidPlan("Subagent attempt overflow".to_string()))?;
    if identity.attempt != expected {
        return Err(StoreError::InvalidPlan(format!(
            "next Subagent attempt mismatch for {}: expected {}, got {}",
            identity.task_id, expected, identity.attempt
        )));
    }
    Ok(())
}

fn exact_active_target(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
) -> Result<ActiveSubagentControlTarget, StoreError> {
    let target = store
        .active_subagent_controls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&identity.execution_id)
        .cloned()
        .ok_or_else(|| {
            StoreError::InvalidPlan(format!(
                "Subagent execution {} is not active in this process",
                identity.execution_id
            ))
        })?;
    if !same_attempt(&target.identity, identity) {
        return Err(StoreError::InvalidPlan(format!(
            "Subagent control identity does not match active execution {}",
            identity.execution_id
        )));
    }
    Ok(target)
}

fn same_attempt(left: &SubagentControlIdentity, right: &SubagentControlIdentity) -> bool {
    left.run_id == right.run_id
        && left.task_id == right.task_id
        && left.execution_id == right.execution_id
        && left.plan_revision == right.plan_revision
        && left.attempt == right.attempt
}

fn existing_receipt(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
) -> Result<Option<SubagentControlReceipt>, StoreError> {
    // Audit allowlist: command receipt replay compares every event sharing the
    // idempotency key and rejects cross-identity reuse before folding state.
    let events = store.list_events(&identity.run_id, 0)?;
    let matches = events
        .iter()
        .filter(|event| {
            event
                .payload
                .get("command_id")
                .and_then(serde_json::Value::as_str)
                == Some(identity.command_id.as_str())
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }
    let mut projection = None;
    let mut bound_execution_id = identity.execution_id.clone();
    for event in matches {
        validate_command_event_identity(event, identity, &mut bound_execution_id)?;
        projection = fold_command_event(projection, event);
    }
    let projection = projection.unwrap_or_default();
    Ok(Some(SubagentControlReceipt {
        identity: identity.clone(),
        status: projection.status,
        phase: projection.phase,
        outcome: projection.outcome,
        drained: projection.drained,
        detail: projection.detail,
        framework_turn_id: projection.framework_turn_id,
    }))
}

fn command_states(
    events: &[super::types::RuntimeTaskEvent],
) -> Result<HashMap<String, SubagentControlStatus>, StoreError> {
    let mut projections = HashMap::<String, CommandFoldState>::new();
    for event in events {
        let Some(command_id) = event
            .payload
            .get("command_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let mut state = match projections.remove(command_id) {
            Some(state) => state,
            None => CommandFoldState::from_event(event, command_id)?,
        };
        validate_command_event_identity(event, &state.identity, &mut state.bound_execution_id)?;
        if let Some(projection) = fold_command_event(Some(state.projection), event) {
            state.projection = projection;
            projections.insert(command_id.to_string(), state);
        }
    }
    Ok(projections
        .into_iter()
        .map(|(command_id, state)| (command_id, state.projection.status))
        .collect())
}

struct CommandFoldState {
    identity: SubagentControlIdentity,
    bound_execution_id: String,
    projection: CommandProjection,
}

impl CommandFoldState {
    fn from_event(
        event: &super::types::RuntimeTaskEvent,
        command_id: &str,
    ) -> Result<Self, StoreError> {
        let task_id = event.task_id.clone().ok_or_else(|| {
            StoreError::InvalidPlan(format!(
                "Subagent command id {command_id} is missing its task identity"
            ))
        })?;
        let execution_id = payload_string(event, "execution_id").ok_or_else(|| {
            StoreError::InvalidPlan(format!(
                "Subagent command id {command_id} is missing its execution identity"
            ))
        })?;
        let plan_revision = payload_u64(event, "plan_revision").ok_or_else(|| {
            StoreError::InvalidPlan(format!(
                "Subagent command id {command_id} is missing its plan revision"
            ))
        })?;
        let attempt = payload_u64(event, "attempt")
            .and_then(|attempt| u32::try_from(attempt).ok())
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "Subagent command id {command_id} has an invalid attempt"
                ))
            })?;
        Ok(Self {
            identity: SubagentControlIdentity {
                run_id: event.run_id.clone(),
                task_id,
                execution_id: execution_id.clone(),
                plan_revision,
                attempt,
                command_id: command_id.to_string(),
            },
            bound_execution_id: execution_id,
            projection: CommandProjection::default(),
        })
    }
}

#[derive(Debug, Clone)]
struct CommandProjection {
    status: SubagentControlStatus,
    phase: SubagentControlPhase,
    outcome: Option<SubagentControlOutcome>,
    drained: Option<bool>,
    detail: Option<String>,
    framework_turn_id: Option<String>,
}

impl Default for CommandProjection {
    fn default() -> Self {
        Self {
            status: SubagentControlStatus::Pending,
            phase: SubagentControlPhase::Persisted,
            outcome: None,
            drained: None,
            detail: None,
            framework_turn_id: None,
        }
    }
}

fn fold_command_event(
    current: Option<CommandProjection>,
    event: &super::types::RuntimeTaskEvent,
) -> Option<CommandProjection> {
    if !matches!(
        event.event_type,
        RuntimeEventKind::SubagentGuidanceQueued
            | RuntimeEventKind::SubagentGuidanceDelivered
            | RuntimeEventKind::SubagentGuidanceMailboxAccepted
            | RuntimeEventKind::SubagentGuidanceDrained
            | RuntimeEventKind::SubagentGuidanceSettled
            | RuntimeEventKind::SubagentGuidanceRejected
            | RuntimeEventKind::SubagentInterruptRequested
            | RuntimeEventKind::SubagentInterruptSettled
    ) {
        return current;
    }
    let mut projection = current.unwrap_or_default();
    match event.event_type {
        RuntimeEventKind::SubagentGuidanceQueued | RuntimeEventKind::SubagentInterruptRequested => {
            projection = CommandProjection::default();
        }
        RuntimeEventKind::SubagentGuidanceDelivered
        | RuntimeEventKind::SubagentGuidanceMailboxAccepted => {
            projection.status = SubagentControlStatus::Delivered;
            projection.phase = SubagentControlPhase::MailboxAccepted;
        }
        RuntimeEventKind::SubagentGuidanceDrained => {
            let drained = payload_bool(event, "drained").unwrap_or(false);
            projection.status = SubagentControlStatus::Delivered;
            projection.phase = if drained {
                SubagentControlPhase::Drained
            } else {
                SubagentControlPhase::MailboxAccepted
            };
            projection.drained = Some(drained);
        }
        RuntimeEventKind::SubagentGuidanceSettled => {
            projection.status = SubagentControlStatus::Settled;
            projection.phase = SubagentControlPhase::TurnSettled;
            projection.outcome = payload_string(event, "outcome")
                .as_deref()
                .and_then(SubagentControlOutcome::parse);
            projection.drained = payload_bool(event, "drained");
        }
        RuntimeEventKind::SubagentGuidanceRejected => {
            projection.status = SubagentControlStatus::Rejected;
            projection.phase = SubagentControlPhase::Persisted;
            projection.detail = payload_string(event, "reason");
        }
        RuntimeEventKind::SubagentInterruptSettled => {
            let accepted = payload_bool(event, "accepted").unwrap_or(true);
            projection.status = if accepted {
                SubagentControlStatus::Settled
            } else {
                SubagentControlStatus::Rejected
            };
            projection.phase = if accepted {
                SubagentControlPhase::TurnSettled
            } else {
                SubagentControlPhase::Persisted
            };
            projection.outcome = payload_string(event, "terminal_status")
                .as_deref()
                .and_then(SubagentControlOutcome::parse);
            projection.detail = payload_string(event, "reason")
                .or_else(|| payload_string(event, "terminal_status"));
        }
        _ => return Some(projection),
    }
    if let Some(turn_id) = payload_string(event, "framework_turn_id") {
        projection.framework_turn_id = Some(turn_id);
    }
    Some(projection)
}

fn validate_command_event_identity(
    event: &super::types::RuntimeTaskEvent,
    identity: &SubagentControlIdentity,
    bound_execution_id: &mut String,
) -> Result<(), StoreError> {
    let event_execution_id = payload_string(event, "execution_id");
    let stable_identity_matches = event.run_id == identity.run_id
        && payload_string(event, "run_id").as_deref() == Some(identity.run_id.as_str())
        && event.task_id.as_deref() == Some(identity.task_id.as_str())
        && payload_u64(event, "plan_revision") == Some(identity.plan_revision)
        && payload_u64(event, "attempt") == Some(u64::from(identity.attempt))
        && event_execution_id.as_deref() == event.step_id.as_deref();
    let exact_execution_matches =
        event_execution_id.as_deref() == Some(bound_execution_id.as_str());
    let next_attempt_handoff = event.event_type == RuntimeEventKind::SubagentGuidanceDelivered
        && event
            .payload
            .get("kind")
            .and_then(serde_json::Value::as_str)
            == Some(SubagentGuidanceKind::NextAttempt.as_str())
        && event_execution_id.as_deref()
            == event
                .payload
                .get("target_execution_id")
                .and_then(serde_json::Value::as_str);
    if stable_identity_matches && (exact_execution_matches || next_attempt_handoff) {
        if next_attempt_handoff && let Some(execution_id) = event_execution_id {
            *bound_execution_id = execution_id;
        }
        Ok(())
    } else {
        Err(StoreError::InvalidPlan(format!(
            "Subagent command id {} is already bound to another identity",
            identity.command_id
        )))
    }
}

fn append_guidance_event(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
    event_type: RuntimeEventKind,
    kind: SubagentGuidanceKind,
    actor_source: SubagentControlActorSource,
    instruction: Option<&str>,
    extra: serde_json::Value,
) -> Result<(), StoreError> {
    store.commit_runtime_events(
        &identity.run_id,
        vec![guidance_event(
            identity,
            event_type,
            kind,
            actor_source,
            instruction,
            extra,
        )],
    )?;
    Ok(())
}

fn guidance_event(
    identity: &SubagentControlIdentity,
    event_type: RuntimeEventKind,
    kind: SubagentGuidanceKind,
    actor_source: SubagentControlActorSource,
    instruction: Option<&str>,
    extra: serde_json::Value,
) -> RuntimeJournalEvent {
    let mut payload = command_payload(identity, actor_source);
    payload.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.as_str().to_string()),
    );
    if let Some(instruction) = instruction {
        payload.insert(
            "instruction".to_string(),
            serde_json::Value::String(instruction.to_string()),
        );
    }
    merge_payload(&mut payload, extra);
    RuntimeJournalEvent::for_append(
        &identity.run_id,
        Some(&identity.task_id),
        Some(&identity.execution_id),
        event_type,
        serde_json::Value::Object(payload),
    )
}

fn append_interrupt_event(
    store: &TaskRuntimeStore,
    identity: &SubagentControlIdentity,
    event_type: RuntimeEventKind,
    actor_source: SubagentControlActorSource,
    extra: serde_json::Value,
) -> Result<(), StoreError> {
    store.commit_runtime_events(
        &identity.run_id,
        vec![interrupt_event(identity, event_type, actor_source, extra)],
    )?;
    Ok(())
}

fn interrupt_event(
    identity: &SubagentControlIdentity,
    event_type: RuntimeEventKind,
    actor_source: SubagentControlActorSource,
    extra: serde_json::Value,
) -> RuntimeJournalEvent {
    let mut payload = command_payload(identity, actor_source);
    merge_payload(&mut payload, extra);
    RuntimeJournalEvent::for_append(
        &identity.run_id,
        Some(&identity.task_id),
        Some(&identity.execution_id),
        event_type,
        serde_json::Value::Object(payload),
    )
}

fn command_payload(
    identity: &SubagentControlIdentity,
    actor_source: SubagentControlActorSource,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        (
            "run_id".to_string(),
            serde_json::Value::String(identity.run_id.clone()),
        ),
        (
            "task_id".to_string(),
            serde_json::Value::String(identity.task_id.clone()),
        ),
        (
            "execution_id".to_string(),
            serde_json::Value::String(identity.execution_id.clone()),
        ),
        (
            "plan_revision".to_string(),
            serde_json::Value::from(identity.plan_revision),
        ),
        (
            "attempt".to_string(),
            serde_json::Value::from(identity.attempt),
        ),
        (
            "command_id".to_string(),
            serde_json::Value::String(identity.command_id.clone()),
        ),
        (
            "actor_source".to_string(),
            serde_json::Value::String(actor_source.as_str().to_string()),
        ),
    ])
}

fn merge_payload(
    target: &mut serde_json::Map<String, serde_json::Value>,
    extra: serde_json::Value,
) {
    let serde_json::Value::Object(extra) = extra else {
        return;
    };
    target.extend(extra);
}

fn payload_string(event: &super::types::RuntimeTaskEvent, key: &str) -> Option<String> {
    event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn payload_u64(event: &super::types::RuntimeTaskEvent, key: &str) -> Option<u64> {
    event.payload.get(key).and_then(serde_json::Value::as_u64)
}

fn payload_bool(event: &super::types::RuntimeTaskEvent, key: &str) -> Option<bool> {
    event.payload.get(key).and_then(serde_json::Value::as_bool)
}

fn pending_receipt(identity: SubagentControlIdentity) -> SubagentControlReceipt {
    SubagentControlReceipt {
        identity,
        status: SubagentControlStatus::Pending,
        phase: SubagentControlPhase::Persisted,
        outcome: None,
        drained: None,
        detail: None,
        framework_turn_id: None,
    }
}

fn rejected_receipt(identity: SubagentControlIdentity, detail: String) -> SubagentControlReceipt {
    SubagentControlReceipt {
        identity,
        status: SubagentControlStatus::Rejected,
        phase: SubagentControlPhase::Persisted,
        outcome: None,
        drained: None,
        detail: Some(detail),
        framework_turn_id: None,
    }
}

fn parse_actor_source(value: &str) -> Option<SubagentControlActorSource> {
    match value {
        "gui" => Some(SubagentControlActorSource::Gui),
        "tui" => Some(SubagentControlActorSource::Tui),
        "cli" => Some(SubagentControlActorSource::Cli),
        "channel" => Some(SubagentControlActorSource::Channel),
        _ => None,
    }
}

fn phase_name(phase: FrameworkSubagentControlPhase) -> &'static str {
    match phase {
        FrameworkSubagentControlPhase::Starting => "starting",
        FrameworkSubagentControlPhase::Running => "running",
        FrameworkSubagentControlPhase::InterruptRequested => "interrupt_requested",
        FrameworkSubagentControlPhase::Settled => "settled",
    }
}

fn framework_outcome_name(outcome: echo_agent::agent::AgentSteerTurnOutcome) -> String {
    match outcome {
        echo_agent::agent::AgentSteerTurnOutcome::Completed => "completed".to_string(),
        echo_agent::agent::AgentSteerTurnOutcome::Failed => "failed".to_string(),
        echo_agent::agent::AgentSteerTurnOutcome::Cancelled => "cancelled".to_string(),
        echo_agent::agent::AgentSteerTurnOutcome::Dropped => "dropped".to_string(),
    }
}

fn framework_identity(
    identity: &SubagentControlIdentity,
) -> Result<SubagentAttemptIdentity, StoreError> {
    SubagentAttemptIdentity::new(
        identity.task_id.clone(),
        identity.execution_id.clone(),
        identity.attempt,
    )
    .map_err(|error| StoreError::InvalidPlan(error.to_string()))
}

pub(crate) fn attempt_identity(
    run_id: &str,
    task_id: &str,
    execution_id: &str,
    plan_revision: u64,
    attempt: u32,
) -> Result<(SubagentControlIdentity, SubagentAttemptIdentity), StoreError> {
    let identity = SubagentControlIdentity {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        execution_id: execution_id.to_string(),
        plan_revision,
        attempt,
        command_id: String::new(),
    };
    let framework = framework_identity(&identity)?;
    Ok((identity, framework))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, TaskPlan, task_goal_sha256,
    };

    struct SteerableSlowAgent;

    impl echo_agent::agent::Agent for SteerableSlowAgent {
        fn name(&self) -> &str {
            "steerable-slow"
        }

        fn model_name(&self) -> &str {
            "test"
        }

        fn system_prompt(&self) -> &str {
            ""
        }

        fn execute<'a>(
            &'a self,
            _task: &'a str,
        ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<String>> {
            Box::pin(std::future::pending())
        }

        fn execute_stream<'a>(
            &'a self,
            _task: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            echo_agent::error::Result<
                futures::stream::BoxStream<
                    'a,
                    echo_agent::error::Result<echo_agent::agent::AgentEvent>,
                >,
            >,
        > {
            Box::pin(async {
                Ok(Box::pin(futures::stream::pending()) as futures::stream::BoxStream<'a, _>)
            })
        }

        fn steer_input(
            &self,
            expected_turn_id: Option<&str>,
            _message: echo_agent::prelude::Message,
        ) -> Result<String, echo_agent::agent::TurnSteerError> {
            expected_turn_id
                .map(str::to_string)
                .ok_or(echo_agent::agent::TurnSteerError::NoActiveTurn)
        }

        fn steer_input_tracked(
            &self,
            expected_turn_id: Option<&str>,
            _message: echo_agent::prelude::Message,
        ) -> Result<echo_agent::agent::AgentSteerReceipt, echo_agent::agent::TurnSteerError>
        {
            let turn_id = expected_turn_id
                .map(str::to_string)
                .ok_or(echo_agent::agent::TurnSteerError::NoActiveTurn)?;
            let (sender, receiver) =
                tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::Accepted);
            tokio::spawn(async move {
                let _ = sender.send(echo_agent::agent::AgentSteerState::Drained);
                tokio::task::yield_now().await;
                let _ = sender.send(echo_agent::agent::AgentSteerState::TurnSettled {
                    outcome: echo_agent::agent::AgentSteerTurnOutcome::Completed,
                    drained: true,
                });
            });
            Ok(echo_agent::agent::AgentSteerReceipt::new(
                "test-steer".to_string(),
                turn_id,
                receiver,
            ))
        }
    }

    fn store_with_plan(run_id: &str, task_ids: &[&str]) -> Result<Arc<TaskRuntimeStore>, String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                run_id,
                "workspace",
                "conversation",
                "message",
                DomainProfile::General,
                "goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let tasks = task_ids
            .iter()
            .enumerate()
            .map(|(index, task_id)| PlanTask {
                id: (*task_id).to_string(),
                title: format!("Task {task_id}"),
                description: "execute".to_string(),
                sort_order: i64::try_from(index).unwrap_or(i64::MAX),
                ..PlanTask::default()
            })
            .collect();
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks,
            })
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    fn last_frame_event_types(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<Vec<String>, String> {
        let contents =
            std::fs::read_to_string(store.active_shadow_root().join(run_id).join("events.jsonl"))
                .map_err(|error| error.to_string())?;
        let frame: serde_json::Value = serde_json::from_str(
            contents
                .lines()
                .last()
                .ok_or_else(|| "Subagent control journal has no frame".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "Subagent control frame has no records".to_string())?
            .iter()
            .map(|record| {
                record
                    .get("event")
                    .and_then(|event| event.get("event_type"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "Subagent control record has no event type".to_string())
            })
            .collect()
    }

    fn identity(
        run_id: &str,
        task_id: &str,
        attempt: u32,
        command_id: &str,
    ) -> SubagentControlIdentity {
        SubagentControlIdentity {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            execution_id: format!("pending:{run_id}:{task_id}:1:{attempt}"),
            plan_revision: 1,
            attempt,
            command_id: command_id.to_string(),
        }
    }

    #[test]
    fn concurrent_duplicate_guidance_appends_one_durable_command() -> Result<(), String> {
        let store = store_with_plan("run-idempotent", &["task-1"])?;
        let service = SubagentControlService::new(store.clone());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let service = service.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                service.queue_guidance(
                    identity("run-idempotent", "task-1", 1, "command-1"),
                    "inspect the latest diff",
                    SubagentControlActorSource::Cli,
                )
            }));
        }
        barrier.wait();
        for handle in handles {
            let receipt = handle
                .join()
                .map_err(|_| "guidance thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
            if receipt.status != SubagentControlStatus::Pending {
                return Err(format!("unexpected receipt status: {:?}", receipt.status));
            }
        }
        let queued = store
            .list_events("run-idempotent", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::SubagentGuidanceQueued)
            .count();
        assert_eq!(queued, 1);
        Ok(())
    }

    #[test]
    fn command_id_cannot_be_rebound_to_another_attempt() -> Result<(), String> {
        let store = store_with_plan("run-rebind", &["task-1", "task-2"])?;
        let service = SubagentControlService::new(store);
        service
            .queue_guidance(
                identity("run-rebind", "task-1", 1, "command-1"),
                "first",
                SubagentControlActorSource::Gui,
            )
            .map_err(|error| error.to_string())?;
        let error = service
            .queue_guidance(
                identity("run-rebind", "task-2", 1, "command-1"),
                "second",
                SubagentControlActorSource::Gui,
            )
            .err()
            .ok_or_else(|| "command id rebind was accepted".to_string())?;
        assert!(error.to_string().contains("already bound"));
        Ok(())
    }

    #[test]
    fn replay_rejects_a_later_event_that_rebinds_command_identity() -> Result<(), String> {
        let store = store_with_plan("run-event-rebind", &["task-1", "task-2"])?;
        let original = identity("run-event-rebind", "task-1", 1, "command-1");
        let rebound = identity("run-event-rebind", "task-2", 1, "command-1");
        store
            .commit_runtime_events(
                &original.run_id,
                vec![
                    guidance_event(
                        &original,
                        RuntimeEventKind::SubagentGuidanceQueued,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Cli,
                        Some("first"),
                        serde_json::json!({}),
                    ),
                    guidance_event(
                        &rebound,
                        RuntimeEventKind::SubagentGuidanceDelivered,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Cli,
                        None,
                        serde_json::json!({ "framework_turn_id": "turn-2" }),
                    ),
                ],
            )
            .map_err(|error| error.to_string())?;
        let error = existing_receipt(&store, &original)
            .err()
            .ok_or_else(|| "later command identity rebind was accepted".to_string())?;
        assert!(error.to_string().contains("already bound"));
        let events = store
            .list_events(&original.run_id, 0)
            .map_err(|error| error.to_string())?;
        let error = command_states(&events)
            .err()
            .ok_or_else(|| "pending-command fold accepted a rebound identity".to_string())?;
        assert!(error.to_string().contains("already bound"));
        Ok(())
    }

    #[test]
    fn legacy_delivered_fixture_uses_the_canonical_projection() -> Result<(), String> {
        let store = store_with_plan("run-legacy-delivered", &["task-1"])?;
        let identity = identity("run-legacy-delivered", "task-1", 1, "command-1");
        store
            .commit_runtime_events(
                &identity.run_id,
                vec![
                    guidance_event(
                        &identity,
                        RuntimeEventKind::SubagentGuidanceQueued,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Cli,
                        Some("legacy guidance"),
                        serde_json::json!({}),
                    ),
                    guidance_event(
                        &identity,
                        RuntimeEventKind::SubagentGuidanceDelivered,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Cli,
                        None,
                        serde_json::json!({ "framework_turn_id": "legacy-turn" }),
                    ),
                ],
            )
            .map_err(|error| error.to_string())?;
        let events = store
            .list_events(&identity.run_id, 0)
            .map_err(|error| error.to_string())?;
        let states = command_states(&events).map_err(|error| error.to_string())?;
        let replay = existing_receipt(&store, &identity)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "legacy receipt replay was missing".to_string())?;
        assert_eq!(states.get(&identity.command_id), Some(&replay.status));
        assert_eq!(replay.status, SubagentControlStatus::Delivered);
        assert_eq!(replay.phase, SubagentControlPhase::MailboxAccepted);
        assert_eq!(replay.framework_turn_id.as_deref(), Some("legacy-turn"));
        assert_eq!(replay.outcome, None);
        assert_eq!(replay.drained, None);
        Ok(())
    }

    #[test]
    fn queued_guidance_transfers_once_to_exact_framework_attempt() -> Result<(), String> {
        let store = store_with_plan("run-delivery", &["task-1"])?;
        let service = SubagentControlService::new(store.clone());
        let queued_identity = identity("run-delivery", "task-1", 1, "command-1");
        service
            .queue_guidance(
                queued_identity.clone(),
                "inspect the latest diff",
                SubagentControlActorSource::Tui,
            )
            .map_err(|error| error.to_string())?;
        let executor = SubagentExecutor::new(
            Arc::new(echo_agent::agent::subagent::SubagentRegistry::new()),
            echo_agent::agent::subagent::SubagentExecutorConfig::default(),
        );
        let target = SubagentControlIdentity {
            execution_id: "run-delivery:task-1:1:1:claim".to_string(),
            command_id: String::new(),
            ..identity("run-delivery", "task-1", 1, "unused")
        };
        assert_eq!(
            store
                .deliver_pending_subagent_guidance(&target, &executor)
                .map_err(|error| error.to_string())?,
            1
        );
        assert_eq!(
            store
                .deliver_pending_subagent_guidance(&target, &executor)
                .map_err(|error| error.to_string())?,
            0
        );
        let events = store
            .list_events("run-delivery", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::SubagentGuidanceDelivered)
                .count(),
            1
        );
        let replay = service
            .queue_guidance(
                queued_identity,
                "inspect the latest diff",
                SubagentControlActorSource::Tui,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.status, SubagentControlStatus::Delivered);
        assert_eq!(replay.phase, SubagentControlPhase::MailboxAccepted);
        Ok(())
    }

    #[tokio::test]
    async fn late_live_controls_persist_typed_rejections() -> Result<(), String> {
        let store = store_with_plan("run-late", &["task-1"])?;
        let service = SubagentControlService::new(store.clone());
        let target = SubagentControlIdentity {
            execution_id: "run-late:task-1:1:1:settled".to_string(),
            command_id: "message-1".to_string(),
            ..identity("run-late", "task-1", 1, "unused")
        };
        let message = service
            .send_message(target.clone(), "late", SubagentControlActorSource::Channel)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(message.status, SubagentControlStatus::Rejected);
        assert_eq!(
            last_frame_event_types(&store, "run-late")?,
            ["subagent_guidance_queued", "subagent_guidance_rejected"]
        );

        let interrupt = service
            .interrupt_subagent(
                SubagentControlIdentity {
                    command_id: "interrupt-1".to_string(),
                    ..target.clone()
                },
                SubagentControlActorSource::Channel,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(interrupt.status, SubagentControlStatus::Rejected);
        let rejected_replay = service
            .interrupt_subagent(
                SubagentControlIdentity {
                    command_id: "interrupt-1".to_string(),
                    ..target.clone()
                },
                SubagentControlActorSource::Channel,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(rejected_replay.status, SubagentControlStatus::Rejected);
        assert_eq!(rejected_replay.phase, SubagentControlPhase::Persisted);
        assert_eq!(
            last_frame_event_types(&store, "run-late")?,
            ["subagent_interrupt_requested", "subagent_interrupt_settled"]
        );
        let events = store
            .list_events("run-late", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            command_states(&events)
                .map_err(|error| error.to_string())?
                .get("interrupt-1"),
            Some(&SubagentControlStatus::Rejected)
        );
        assert!(
            events
                .iter()
                .any(|event| { event.event_type == RuntimeEventKind::SubagentGuidanceRejected })
        );
        assert!(events.iter().any(|event| {
            event.event_type == RuntimeEventKind::SubagentInterruptSettled
                && event
                    .payload
                    .get("accepted")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        }));
        Ok(())
    }

    #[tokio::test]
    async fn active_message_receipt_tracks_mailbox_drain_and_turn_settlement() -> Result<(), String>
    {
        use echo_agent::agent::CancellationToken;
        use echo_agent::agent::subagent::{
            DispatchRequest, ExecutionMode as FrameworkExecutionMode, SubagentDefinition,
            SubagentStatus,
        };

        let store = store_with_plan("run-live-message", &["task-1"])?;
        let registry = Arc::new(echo_agent::agent::subagent::SubagentRegistry::new());
        registry
            .register(
                SubagentDefinition::new("slow", "Slow Subagent"),
                Box::new(SteerableSlowAgent),
            )
            .await;
        let executor = Arc::new(SubagentExecutor::new(
            registry,
            echo_agent::agent::subagent::SubagentExecutorConfig::default(),
        ));
        let execution_id = "run-live-message:task-1:1:1:claim-1";
        let (_control_identity, framework_identity) =
            attempt_identity("run-live-message", "task-1", execution_id, 1, 1)
                .map_err(|error| error.to_string())?;
        let _route = store
            .record_controlled_subagent_assigned(
                "run-live-message",
                "task-1",
                execution_id,
                "slow",
                "Slow Subagent",
                1,
                1,
                true,
                false,
                executor.clone(),
            )
            .map_err(|error| error.to_string())?;
        let handle = executor
            .dispatch_background_attempt(
                DispatchRequest {
                    agent_name: "slow".to_string(),
                    task: "hold the active turn".to_string(),
                    mode_override: Some(FrameworkExecutionMode::Sync),
                    cancel: CancellationToken::new(),
                    parent_agent: "parent".to_string(),
                    parent_context: None,
                    delegation_policy: DispatchRequest::policy_from_depth(0),
                    runtime_context: None,
                    message: None,
                    prompt_payload: None,
                    constraints: Vec::new(),
                    background: false,
                },
                framework_identity,
            )
            .await
            .map_err(|error| error.to_string())?;

        let identity = SubagentControlIdentity {
            run_id: "run-live-message".to_string(),
            task_id: "task-1".to_string(),
            execution_id: execution_id.to_string(),
            plan_revision: 1,
            attempt: 1,
            command_id: "message-1".to_string(),
        };
        let service = SubagentControlService::new(store.clone());
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.send_message(
                identity,
                "continue with the active context",
                SubagentControlActorSource::Gui,
            ),
        )
        .await
        .map_err(|_| "active Subagent message did not reach its safe point".to_string())?
        .map_err(|error| error.to_string())?;
        assert_eq!(
            receipt.status,
            SubagentControlStatus::Delivered,
            "active message receipt was {receipt:?}"
        );
        assert!(receipt.framework_turn_id.is_some());

        let events = store
            .list_events("run-live-message", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::SubagentGuidanceQueued)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::SubagentGuidanceDelivered)
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|event| event.event_type != RuntimeEventKind::SubagentGuidanceRejected)
        );

        handle.cancel();
        match handle.join().await {
            Ok(result) => assert_eq!(result.outcome.status, SubagentStatus::Cancelled),
            Err(echo_agent::error::ReactError::Agent(error))
                if matches!(*error, echo_agent::error::AgentError::Cancelled(_)) => {}
            Err(error) => return Err(format!("cancelled Subagent did not settle: {error}")),
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let events = loop {
            let events = store
                .list_events("run-live-message", 0)
                .map_err(|error| error.to_string())?;
            let lifecycle = events
                .iter()
                .filter(|event| {
                    event
                        .payload
                        .get("command_id")
                        .and_then(serde_json::Value::as_str)
                        == Some("message-1")
                })
                .map(|event| event.event_type)
                .collect::<Vec<_>>();
            if lifecycle.contains(&RuntimeEventKind::SubagentGuidanceSettled) {
                break lifecycle;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("tracked Subagent lifecycle observer did not settle".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let event_types = events;
        assert_eq!(
            event_types,
            vec![
                RuntimeEventKind::SubagentGuidanceQueued,
                RuntimeEventKind::SubagentGuidanceMailboxAccepted,
                RuntimeEventKind::SubagentGuidanceDelivered,
                RuntimeEventKind::SubagentGuidanceDrained,
                RuntimeEventKind::SubagentGuidanceSettled,
            ]
        );
        let replay = existing_receipt(
            &store,
            &SubagentControlIdentity {
                run_id: "run-live-message".to_string(),
                task_id: "task-1".to_string(),
                execution_id: execution_id.to_string(),
                plan_revision: 1,
                attempt: 1,
                command_id: "message-1".to_string(),
            },
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "tracked receipt replay was missing".to_string())?;
        assert_eq!(replay.status, SubagentControlStatus::Settled);
        assert_eq!(replay.phase, SubagentControlPhase::TurnSettled);
        assert_eq!(replay.outcome, Some(SubagentControlOutcome::Completed));
        assert_eq!(replay.drained, Some(true));
        let states = command_states(
            &store
                .list_events("run-live-message", 0)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            states.get("message-1"),
            Some(&SubagentControlStatus::Settled)
        );
        Ok(())
    }

    #[test]
    fn terminal_before_drain_keeps_mailbox_phase_without_drain_fact() -> Result<(), String> {
        let store = store_with_plan("run-terminal-before-drain", &["task-1"])?;
        let identity = identity("run-terminal-before-drain", "task-1", 1, "message-1");
        store
            .commit_runtime_events(
                &identity.run_id,
                vec![
                    guidance_event(
                        &identity,
                        RuntimeEventKind::SubagentGuidanceMailboxAccepted,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Gui,
                        None,
                        serde_json::json!({ "framework_turn_id": "turn-1" }),
                    ),
                    guidance_event(
                        &identity,
                        RuntimeEventKind::SubagentGuidanceDelivered,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Gui,
                        None,
                        serde_json::json!({ "framework_turn_id": "turn-1" }),
                    ),
                    guidance_event(
                        &identity,
                        RuntimeEventKind::SubagentGuidanceSettled,
                        SubagentGuidanceKind::LiveMessage,
                        SubagentControlActorSource::Gui,
                        None,
                        serde_json::json!({
                            "framework_turn_id": "turn-1",
                            "outcome": "cancelled",
                            "drained": false,
                        }),
                    ),
                ],
            )
            .map_err(|error| error.to_string())?;
        let replay = existing_receipt(&store, &identity)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "terminal receipt replay was missing".to_string())?;
        assert_eq!(replay.status, SubagentControlStatus::Settled);
        assert_eq!(replay.phase, SubagentControlPhase::TurnSettled);
        assert_eq!(replay.outcome, Some(SubagentControlOutcome::Cancelled));
        assert_eq!(replay.drained, Some(false));
        let events = store
            .list_events(&identity.run_id, 0)
            .map_err(|error| error.to_string())?;
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::SubagentGuidanceDrained)
        );
        Ok(())
    }

    #[tokio::test]
    async fn exact_interrupt_routes_once_and_settles_durably() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::agent::subagent::{
            DispatchRequest, ExecutionMode as FrameworkExecutionMode, SubagentDefinition,
            SubagentStatus,
        };
        use echo_agent::testing::MockAgent;

        let store = store_with_plan("run-interrupt", &["task-1"])?;
        let registry = Arc::new(echo_agent::agent::subagent::SubagentRegistry::new());
        registry
            .register(
                SubagentDefinition::new("slow", "Slow Subagent"),
                Box::new(
                    MockAgent::new("slow")
                        .with_response("must be cancelled before completion")
                        .with_delay_ms(30_000),
                ),
            )
            .await;
        let executor = Arc::new(SubagentExecutor::new(
            registry,
            echo_agent::agent::subagent::SubagentExecutorConfig::default(),
        ));
        let execution_id = "run-interrupt:task-1:1:1:claim-1";
        let (_control_identity, framework_identity) =
            attempt_identity("run-interrupt", "task-1", execution_id, 1, 1)
                .map_err(|error| error.to_string())?;
        let _route = store
            .record_controlled_subagent_assigned(
                "run-interrupt",
                "task-1",
                execution_id,
                "slow",
                "Slow Subagent",
                1,
                1,
                true,
                false,
                executor.clone(),
            )
            .map_err(|error| error.to_string())?;
        let handle = executor
            .dispatch_background_attempt(
                DispatchRequest {
                    agent_name: "slow".to_string(),
                    task: "wait".to_string(),
                    mode_override: Some(FrameworkExecutionMode::Sync),
                    cancel: CancellationToken::new(),
                    parent_agent: "parent".to_string(),
                    parent_context: None,
                    delegation_policy: DispatchRequest::policy_from_depth(0),
                    runtime_context: None,
                    message: None,
                    prompt_payload: None,
                    constraints: Vec::new(),
                    background: false,
                },
                framework_identity,
            )
            .await
            .map_err(|error| error.to_string())?;
        let identity = SubagentControlIdentity {
            run_id: "run-interrupt".to_string(),
            task_id: "task-1".to_string(),
            execution_id: execution_id.to_string(),
            plan_revision: 1,
            attempt: 1,
            command_id: "interrupt-1".to_string(),
        };
        let service = SubagentControlService::new(store.clone());
        service.fail_next_settlements(2);
        let receipt = service
            .interrupt_subagent(identity.clone(), SubagentControlActorSource::Gui)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SubagentControlStatus::Settled);
        match handle.join().await {
            Ok(result) => assert_eq!(result.outcome.status, SubagentStatus::Cancelled),
            Err(echo_agent::error::ReactError::Agent(error))
                if matches!(*error, echo_agent::error::AgentError::Cancelled(_)) => {}
            Err(error) => return Err(format!("interrupted Subagent did not settle: {error}")),
        }

        let replay = service
            .interrupt_subagent(identity, SubagentControlActorSource::Gui)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.status, SubagentControlStatus::Settled);
        let events = store
            .list_events("run-interrupt", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::SubagentInterruptRequested)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::SubagentInterruptSettled)
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caller_abort_cannot_leave_interrupt_requested_without_settlement() -> Result<(), String>
    {
        use echo_agent::agent::CancellationToken;
        use echo_agent::agent::subagent::{
            DispatchRequest, ExecutionMode as FrameworkExecutionMode, SubagentDefinition,
            SubagentStatus,
        };
        use echo_agent::testing::MockAgent;

        let store = store_with_plan("run-abort-interrupt", &["task-1"])?;
        let registry = Arc::new(echo_agent::agent::subagent::SubagentRegistry::new());
        registry
            .register(
                SubagentDefinition::new("slow", "Slow Subagent"),
                Box::new(
                    MockAgent::new("slow")
                        .with_response("must be interrupted")
                        .with_delay_ms(30_000),
                ),
            )
            .await;
        let executor = Arc::new(SubagentExecutor::new(
            registry,
            echo_agent::agent::subagent::SubagentExecutorConfig::default(),
        ));
        let execution_id = "run-abort-interrupt:task-1:1:1:claim-1";
        let (_, framework_identity) =
            attempt_identity("run-abort-interrupt", "task-1", execution_id, 1, 1)
                .map_err(|error| error.to_string())?;
        let _route = store
            .record_controlled_subagent_assigned(
                "run-abort-interrupt",
                "task-1",
                execution_id,
                "slow",
                "Slow Subagent",
                1,
                1,
                true,
                false,
                executor.clone(),
            )
            .map_err(|error| error.to_string())?;
        let handle = executor
            .dispatch_background_attempt(
                DispatchRequest {
                    agent_name: "slow".to_string(),
                    task: "wait".to_string(),
                    mode_override: Some(FrameworkExecutionMode::Sync),
                    cancel: CancellationToken::new(),
                    parent_agent: "parent".to_string(),
                    parent_context: None,
                    delegation_policy: DispatchRequest::policy_from_depth(0),
                    runtime_context: None,
                    message: None,
                    prompt_payload: None,
                    constraints: Vec::new(),
                    background: false,
                },
                framework_identity,
            )
            .await
            .map_err(|error| error.to_string())?;
        let identity = SubagentControlIdentity {
            run_id: "run-abort-interrupt".to_string(),
            task_id: "task-1".to_string(),
            execution_id: execution_id.to_string(),
            plan_revision: 1,
            attempt: 1,
            command_id: "interrupt-aborted-caller".to_string(),
        };
        let barrier = Arc::new(SubagentControlTestBarrier {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let service =
            SubagentControlService::new(store.clone()).with_command_test_barrier(barrier.clone());
        let caller_service = service.clone();
        let caller = tokio::spawn(async move {
            caller_service
                .interrupt_subagent(identity, SubagentControlActorSource::Gui)
                .await
        });
        barrier.entered.notified().await;
        caller.abort();
        let _ = caller.await;
        let before_release = store
            .list_events("run-abort-interrupt", 0)
            .map_err(|error| error.to_string())?;
        if !before_release
            .iter()
            .any(|event| event.event_type == RuntimeEventKind::SubagentInterruptRequested)
            || before_release
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::SubagentInterruptSettled)
        {
            return Err("interrupt did not pause at the durable requested boundary".to_string());
        }
        let shutdown_store = store.clone();
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_operations().await });
        tokio::task::yield_now().await;
        if shutdown.is_finished() {
            return Err("operation shutdown crossed an unsettled interrupt".to_string());
        }
        barrier.release.notify_one();
        shutdown
            .await
            .map_err(|error| format!("operation shutdown failed to join: {error}"))??;
        match handle.join().await {
            Ok(result) if result.outcome.status == SubagentStatus::Cancelled => {}
            Ok(result) => {
                return Err(format!(
                    "unexpected interrupted status: {:?}",
                    result.outcome.status
                ));
            }
            Err(echo_agent::error::ReactError::Agent(error))
                if matches!(*error, echo_agent::error::AgentError::Cancelled(_)) => {}
            Err(error) => return Err(format!("interrupted Subagent did not settle: {error}")),
        }
        let settled = store
            .list_events("run-abort-interrupt", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::SubagentInterruptSettled)
            .count();
        if settled != 1 {
            return Err(format!(
                "expected one interrupt settlement, found {settled}"
            ));
        }
        Ok(())
    }
}
