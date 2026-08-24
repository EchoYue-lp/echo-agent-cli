//! Durable EKO control commands for exact Subagent attempts.
//!
//! The framework owns live mailbox delivery and cancellation. This module
//! validates TaskRun identity, records commands in `events.jsonl`, and keeps a
//! process-only route to the exact framework executor currently dispatching an
//! attempt. It does not own a second mailbox, scheduler, or retry loop.

use std::collections::HashMap;
use std::sync::Arc;

use echo_agent::agent::subagent::{
    SubagentAttemptIdentity, SubagentControlPhase, SubagentExecutor,
};

use super::run_authority::RuntimeJournalEvent;
use super::store::{StoreError, TaskRuntimeStore};
use super::types::{
    RuntimeEventKind, SubagentControlActorSource, SubagentControlIdentity, SubagentControlReceipt,
    SubagentControlStatus, SubagentGuidanceKind, TaskRunStatus,
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
}

impl SubagentControlService {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        Self { store }
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

    /// Deliver one message to the existing safe point of an exact active
    /// attempt. The durable queued boundary is written before framework IO.
    pub async fn send_message(
        &self,
        identity: SubagentControlIdentity,
        instruction: &str,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        validate_instruction(instruction)?;
        let command_run_id = identity.run_id.clone();
        let begin = self.store.with_run_lock(&command_run_id, || {
            if let Some(receipt) = existing_receipt(&self.store, &identity)? {
                return Ok(ControlBegin::Existing(receipt));
            }
            validate_plan_target(&self.store, &identity)?;
            match exact_active_target(&self.store, &identity) {
                Ok(target) => {
                    append_guidance_event(
                        &self.store,
                        &identity,
                        RuntimeEventKind::SubagentGuidanceQueued,
                        SubagentGuidanceKind::LiveMessage,
                        actor_source,
                        Some(instruction),
                        serde_json::json!({}),
                    )?;
                    Ok(ControlBegin::New(target))
                }
                Err(error) => {
                    let detail = error.to_string();
                    self.store.shadow.append_event_batch(
                        &identity.run_id,
                        vec![
                            guidance_event(
                                &identity,
                                RuntimeEventKind::SubagentGuidanceQueued,
                                SubagentGuidanceKind::LiveMessage,
                                actor_source,
                                Some(instruction),
                                serde_json::json!({}),
                            ),
                            guidance_event(
                                &identity,
                                RuntimeEventKind::SubagentGuidanceRejected,
                                SubagentGuidanceKind::LiveMessage,
                                actor_source,
                                None,
                                serde_json::json!({ "reason": detail }),
                            ),
                        ],
                    )?;
                    Ok(ControlBegin::Existing(rejected_receipt(
                        identity.clone(),
                        detail,
                    )))
                }
            }
        })?;
        let ControlBegin::New(target) = begin else {
            return begin.into_receipt();
        };

        let delivery = target
            .executor
            .send_message(&identity.execution_id, identity.attempt, instruction)
            .await;
        let run_id = identity.run_id.clone();
        self.store.with_run_lock(&run_id, || match delivery {
            Ok(delivery) => {
                append_guidance_event(
                    &self.store,
                    &identity,
                    RuntimeEventKind::SubagentGuidanceDelivered,
                    SubagentGuidanceKind::LiveMessage,
                    actor_source,
                    None,
                    serde_json::json!({ "framework_turn_id": delivery.turn_id }),
                )?;
                Ok(SubagentControlReceipt {
                    identity,
                    status: SubagentControlStatus::Delivered,
                    detail: None,
                    framework_turn_id: Some(delivery.turn_id),
                })
            }
            Err(error) => {
                let detail = error.to_string();
                append_guidance_event(
                    &self.store,
                    &identity,
                    RuntimeEventKind::SubagentGuidanceRejected,
                    SubagentGuidanceKind::LiveMessage,
                    actor_source,
                    None,
                    serde_json::json!({ "reason": detail }),
                )?;
                Ok(rejected_receipt(identity, detail))
            }
        })
    }

    /// Interrupt one exact active attempt without pausing or cancelling its
    /// parent TaskRun. The framework waits for dispatch settlement.
    pub async fn interrupt_subagent(
        &self,
        identity: SubagentControlIdentity,
        actor_source: SubagentControlActorSource,
    ) -> Result<SubagentControlReceipt, StoreError> {
        let command_run_id = identity.run_id.clone();
        let begin = self.store.with_run_lock(&command_run_id, || {
            if let Some(receipt) = existing_receipt(&self.store, &identity)? {
                return Ok(ControlBegin::Existing(receipt));
            }
            validate_plan_target(&self.store, &identity)?;
            match exact_active_target(&self.store, &identity) {
                Ok(target) => {
                    append_interrupt_event(
                        &self.store,
                        &identity,
                        RuntimeEventKind::SubagentInterruptRequested,
                        actor_source,
                        serde_json::json!({}),
                    )?;
                    Ok(ControlBegin::New(target))
                }
                Err(error) => {
                    let detail = error.to_string();
                    self.store.shadow.append_event_batch(
                        &identity.run_id,
                        vec![
                            interrupt_event(
                                &identity,
                                RuntimeEventKind::SubagentInterruptRequested,
                                actor_source,
                                serde_json::json!({}),
                            ),
                            interrupt_event(
                                &identity,
                                RuntimeEventKind::SubagentInterruptSettled,
                                actor_source,
                                serde_json::json!({ "accepted": false, "reason": detail }),
                            ),
                        ],
                    )?;
                    Ok(ControlBegin::Existing(rejected_receipt(
                        identity.clone(),
                        detail,
                    )))
                }
            }
        })?;
        let ControlBegin::New(target) = begin else {
            return begin.into_receipt();
        };

        let outcome = target
            .executor
            .interrupt_subagent(&identity.execution_id, identity.attempt)
            .await;
        let run_id = identity.run_id.clone();
        self.store.with_run_lock(&run_id, || {
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
                Err(error) => {
                    let detail = error.to_string();
                    (
                        SubagentControlStatus::Rejected,
                        Some(detail.clone()),
                        serde_json::json!({ "accepted": false, "reason": detail }),
                    )
                }
            };
            append_interrupt_event(
                &self.store,
                &identity,
                RuntimeEventKind::SubagentInterruptSettled,
                actor_source,
                payload,
            )?;
            Ok(SubagentControlReceipt {
                identity,
                status,
                detail,
                framework_turn_id: None,
            })
        })
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
            let states = command_states(&events);
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
    // Audit allowlist: command receipt replay must compare every event sharing
    // the idempotency key and reject cross-identity reuse.
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
    let Some(first) = matches.first() else {
        return Ok(None);
    };
    if first.task_id.as_deref() != Some(identity.task_id.as_str())
        || payload_string(first, "execution_id").as_deref() != Some(identity.execution_id.as_str())
        || payload_u64(first, "plan_revision") != Some(identity.plan_revision)
        || payload_u64(first, "attempt") != Some(u64::from(identity.attempt))
    {
        return Err(StoreError::InvalidPlan(format!(
            "Subagent command id {} is already bound to another identity",
            identity.command_id
        )));
    }
    let Some(last) = matches.last() else {
        return Ok(None);
    };
    let accepted = last
        .payload
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let status = match last.event_type {
        RuntimeEventKind::SubagentGuidanceDelivered => SubagentControlStatus::Delivered,
        RuntimeEventKind::SubagentGuidanceRejected => SubagentControlStatus::Rejected,
        RuntimeEventKind::SubagentInterruptSettled if accepted => SubagentControlStatus::Settled,
        RuntimeEventKind::SubagentInterruptSettled => SubagentControlStatus::Rejected,
        _ => SubagentControlStatus::Pending,
    };
    Ok(Some(SubagentControlReceipt {
        identity: identity.clone(),
        status,
        detail: payload_string(last, "reason").or_else(|| payload_string(last, "terminal_status")),
        framework_turn_id: payload_string(last, "framework_turn_id"),
    }))
}

fn command_states(
    events: &[super::types::RuntimeTaskEvent],
) -> HashMap<String, SubagentControlStatus> {
    let mut states = HashMap::new();
    for event in events {
        let Some(command_id) = event
            .payload
            .get("command_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let status = match event.event_type {
            RuntimeEventKind::SubagentGuidanceQueued
            | RuntimeEventKind::SubagentInterruptRequested => SubagentControlStatus::Pending,
            RuntimeEventKind::SubagentGuidanceDelivered => SubagentControlStatus::Delivered,
            RuntimeEventKind::SubagentGuidanceRejected => SubagentControlStatus::Rejected,
            RuntimeEventKind::SubagentInterruptSettled => SubagentControlStatus::Settled,
            _ => continue,
        };
        states.insert(command_id.to_string(), status);
    }
    states
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
    store.shadow.append_event_batch(
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
    store.shadow.append_event_batch(
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

fn pending_receipt(identity: SubagentControlIdentity) -> SubagentControlReceipt {
    SubagentControlReceipt {
        identity,
        status: SubagentControlStatus::Pending,
        detail: None,
        framework_turn_id: None,
    }
}

fn rejected_receipt(identity: SubagentControlIdentity, detail: String) -> SubagentControlReceipt {
    SubagentControlReceipt {
        identity,
        status: SubagentControlStatus::Rejected,
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

fn phase_name(phase: SubagentControlPhase) -> &'static str {
    match phase {
        SubagentControlPhase::Starting => "starting",
        SubagentControlPhase::Running => "running",
        SubagentControlPhase::InterruptRequested => "interrupt_requested",
        SubagentControlPhase::Settled => "settled",
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
    fn queued_guidance_transfers_once_to_exact_framework_attempt() -> Result<(), String> {
        let store = store_with_plan("run-delivery", &["task-1"])?;
        let service = SubagentControlService::new(store.clone());
        service
            .queue_guidance(
                identity("run-delivery", "task-1", 1, "command-1"),
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
                    ..target
                },
                SubagentControlActorSource::Channel,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(interrupt.status, SubagentControlStatus::Rejected);
        assert_eq!(
            last_frame_event_types(&store, "run-late")?,
            ["subagent_interrupt_requested", "subagent_interrupt_settled"]
        );
        let events = store
            .list_events("run-late", 0)
            .map_err(|error| error.to_string())?;
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
                Box::new(MockAgent::new("slow").with_delay_ms(30_000)),
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
        let receipt = service
            .interrupt_subagent(identity.clone(), SubagentControlActorSource::Gui)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SubagentControlStatus::Settled);
        let result = handle.join().await.map_err(|error| error.to_string())?;
        assert_eq!(result.outcome.status, SubagentStatus::Cancelled);

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
}
