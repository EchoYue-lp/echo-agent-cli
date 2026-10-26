//! EKO's frozen cron destination and thin adapter onto the TaskRun journal.
//!
//! The framework owns delivery transitions. The typed journal preserves the
//! original physical authority's batch identities, sequences and receipts.
//! Only the discardable projection checkpoint is held in memory.

use std::sync::Arc;

#[cfg(any(test, feature = "channels"))]
use echo_agent::delivery::{DeliveryClaim, DeliveryEnvelope, DeliveryOutcome, DeliverySettlement};
use echo_agent::delivery::{
    DeliveryEvent, DeliveryLedger, DeliveryLedgerConfig, DeliveryLedgerProjection, DeliveryRecord,
    DeliveryRoute,
};
#[cfg(any(test, feature = "channels"))]
use echo_agent::state::journal::JournalDurabilityStatus;
use echo_agent::state::journal::{
    CheckpointStore, EventJournal, EventReducer, JournalBatchAppendResult, JournalBatchLookup,
    JournalIdentity, JournalRecord, MemoryCheckpointStore, PreparedJournalBatch,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "channels"))]
use sha2::{Digest, Sha256};

use super::occurrence::OccurrenceIdentity;
use super::store::{RuntimeEventQuery, StoreError, TaskRuntimeStore};
use super::types::{PlanTaskKind, RuntimeEventKind, TaskExecutionSummary, TaskRunStatus};

/// A reference to the exact account/context selected at occurrence admission.
/// `context_ref` is an opaque serialized provider reference, never a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinDeliveryTarget {
    pub account_id: String,
    pub credential_generation: String,
    pub peer_id: String,
    pub context_ref: String,
    pub workspace_id: String,
    pub channel_conversation: String,
    pub definition_id: String,
}

impl WeixinDeliveryTarget {
    pub fn validate_for_occurrence(&self, identity: &OccurrenceIdentity) -> Result<(), StoreError> {
        self.validate()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        if self.workspace_id != identity.workspace_id {
            return Err(StoreError::InvalidPlan(
                "Weixin destination belongs to another occurrence workspace".to_string(),
            ));
        }
        Ok(())
    }
}

impl DeliveryRoute for WeixinDeliveryTarget {
    fn validate(&self) -> echo_agent::error::Result<()> {
        if [
            &self.account_id,
            &self.credential_generation,
            &self.peer_id,
            &self.context_ref,
            &self.workspace_id,
            &self.channel_conversation,
            &self.definition_id,
        ]
        .iter()
        .any(|field| field.trim().is_empty())
        {
            return Err(echo_agent::error::ReactError::Other(
                "Weixin notification destination is incomplete".to_string(),
            ));
        }
        Ok(())
    }
}

/// ChannelRuntime supplies account/context ownership; it must not look up a
/// newer context or credential when sending an already-frozen destination.
pub trait WeixinCronDelivery: Send + Sync {
    fn freeze_target<'a>(
        &'a self,
        invocation: &'a echo_agent::scheduler::SchedulerInvocation,
        workspace_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WeixinDeliveryTarget>, String>>;

    /// Err is an authoritative, sanitized local refusal BEFORE HTTP-effect
    /// admission (validation or queue admission). Once the producer accepts
    /// the effect, every ambiguous observation MUST be Ok(Unknown), including
    /// lost receipts, cancellation, timeout and transport failure.
    #[cfg(feature = "channels")]
    fn send<'a>(
        &'a self,
        target: &'a WeixinDeliveryTarget,
        message_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<echo_agent::channels::weixin::WeixinSendOutcome, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinCompletionPayload {
    pub run_id: String,
    pub occurrence_id: String,
    /// Complete durable result values and artifact references, never the
    /// scheduler's bounded last_result or the webhook's finished label.
    pub text: String,
}

pub(crate) type WeixinEvent = DeliveryEvent<WeixinDeliveryTarget, WeixinCompletionPayload>;
type ReplayJournal = WeixinJournal;
type Ledger = DeliveryLedger<WeixinJournal, WeixinDeliveryTarget, WeixinCompletionPayload>;
pub type WeixinNotificationRecord = DeliveryRecord<WeixinDeliveryTarget, WeixinCompletionPayload>;

/// A typed view of the SAME TaskRuntime physical authority. These fields do
/// not allocate a new journal generation, write authority or delivery store.
struct WeixinJournal {
    authority: Arc<super::run_authority::RunAuthority>,
    identity: JournalIdentity,
    snapshot_head: u64,
}

impl EventJournal<WeixinEvent> for WeixinJournal {
    fn journal_identity(&self) -> &JournalIdentity {
        &self.identity
    }
    fn append_batch(
        &self,
        batch: PreparedJournalBatch<WeixinEvent>,
    ) -> JournalBatchAppendResult<WeixinEvent> {
        self.authority.append_weixin_prepared(batch)
    }
    fn lookup_batch(
        &self,
        batch: &PreparedJournalBatch<WeixinEvent>,
    ) -> echo_agent::error::Result<JournalBatchLookup<WeixinEvent>> {
        self.authority.lookup_weixin_prepared(batch)
    }
    fn next_sequence(&self) -> u64 {
        self.last_sequence().saturating_add(1)
    }
    fn last_sequence(&self) -> u64 {
        // A closed authority cannot append; the frozen real head is only a
        // read fallback for this infallible trait method, never a commit fact.
        self.authority
            .weixin_journal_head()
            .unwrap_or(self.snapshot_head)
    }
    fn replay_after(
        &self,
        after: u64,
        limit: usize,
    ) -> echo_agent::error::Result<Vec<JournalRecord<WeixinEvent>>> {
        self.authority.replay_weixin_after(after, limit)
    }
}

fn store_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidPlan(format!("Weixin notification journal: {error}"))
}

#[cfg(any(test, feature = "channels"))]
fn notification_id(run_id: &str, target: &WeixinDeliveryTarget) -> Result<String, StoreError> {
    let canonical = serde_json::to_vec(&(1_u8, run_id, target, "completed"))?;
    Ok(format!("weixin-cron-{:x}", Sha256::digest(canonical)))
}

/// Read-only recovery accounting. Pending entries remain backed by the
/// original RunCreated/ledger facts, including when no new frame can append.
#[derive(Debug, Default)]
pub struct WeixinNotificationRecoveryReport {
    pub scanned: usize,
    pub recovered: usize,
    pub settled: usize,
    pub pending: Vec<String>,
}

impl WeixinNotificationRecoveryReport {
    #[cfg(feature = "channels")]
    pub(crate) fn merge(&mut self, other: Self) {
        self.scanned = self.scanned.saturating_add(other.scanned);
        self.recovered = self.recovered.saturating_add(other.recovered);
        self.settled = self.settled.saturating_add(other.settled);
        self.pending.extend(other.pending);
    }
}

#[cfg(feature = "channels")]
struct WeixinRecoveryWorkspaceReceipt {
    _lifetime: crate::workspace::runtime::WorkspaceIoLease,
}

#[cfg(feature = "channels")]
impl super::store::RunDriverExecutionReceipt for WeixinRecoveryWorkspaceReceipt {
    fn release(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            drop(self);
        })
    }
}

impl TaskRuntimeStore {
    /// Read only the original admission. Missing historical bindings remain
    /// None; callers must never fill them from today's GUI configuration.
    pub fn frozen_weixin_notification(
        &self,
        run_id: &str,
    ) -> Result<Option<WeixinDeliveryTarget>, StoreError> {
        let events = self.query_events_bounded(
            run_id,
            RuntimeEventQuery::new(0, 1).with_event_types(vec![RuntimeEventKind::RunCreated]),
        )?;
        let value = events
            .first()
            .and_then(|event| event.payload.get("weixin_notification"));
        match value {
            Some(value) if !value.is_null() => Ok(Some(serde_json::from_value(value.clone())?)),
            _ => Ok(None),
        }
    }

    /// Original durable notification debt. Full occurrence identity shares
    /// the admission frame with its target, so recovery never parses a GUI
    /// conversation string or reads mutable scheduler/config definitions.
    pub fn frozen_weixin_occurrence(
        &self,
        run_id: &str,
    ) -> Result<Option<OccurrenceIdentity>, StoreError> {
        let events = self.query_events_bounded(
            run_id,
            RuntimeEventQuery::new(0, 1).with_event_types(vec![RuntimeEventKind::RunCreated]),
        )?;
        let Some(event) = events.first() else {
            return Ok(None);
        };
        if event
            .payload
            .get("weixin_notification")
            .is_none_or(serde_json::Value::is_null)
        {
            return Ok(None);
        }
        let value = event.payload.get("weixin_occurrence")
            .filter(|value| !value.is_null()).ok_or_else(|| store_error(
                "frozen notification is pending but its original occurrence identity is missing",
            ))?;
        let identity: OccurrenceIdentity = serde_json::from_value(value.clone())?;
        identity.validate()?;
        let target = self
            .frozen_weixin_notification(run_id)?
            .ok_or_else(|| store_error("original notification target missing"))?;
        target.validate_for_occurrence(&identity)?;
        if identity.run_id() != run_id {
            return Err(store_error("frozen occurrence belongs to another TaskRun"));
        }
        Ok(Some(identity))
    }

    /// Reconcile the original completed-run obligations independently of
    /// scheduler occurrence completion. A journal refusal before enqueue,
    /// claim or EffectStarted leaves the same admission as durable debt.
    /// This path never launches an Agent or changes a completed run's status.
    #[cfg(feature = "channels")]
    pub async fn recover_weixin_notifications(
        self: &Arc<Self>,
        sender: Arc<dyn WeixinCronDelivery>,
        cancel: &echo_agent::agent::CancellationToken,
    ) -> WeixinNotificationRecoveryReport {
        self.recover_weixin_notifications_with_workspace(sender, cancel, None)
            .await
    }

    #[cfg(feature = "channels")]
    pub(crate) async fn recover_weixin_notifications_with_workspace(
        self: &Arc<Self>,
        sender: Arc<dyn WeixinCronDelivery>,
        cancel: &echo_agent::agent::CancellationToken,
        workspace_lifetime: Option<crate::workspace::runtime::WorkspaceIoLease>,
    ) -> WeixinNotificationRecoveryReport {
        let mut report = WeixinNotificationRecoveryReport::default();
        let runs = match super::TaskRuntimeOperation::new(self.clone())
            .run_store(
                "list original completed Weixin notification obligations",
                |store| store.list_runs_in(&[TaskRunStatus::Completed]),
            )
            .await
        {
            Ok(runs) => runs,
            Err(error) => {
                report.pending.push(error.to_string());
                return report;
            }
        };
        for run in runs {
            if cancel.is_cancelled() {
                report.pending.push(
                    "original notification recovery cancelled; durable admissions remain pending"
                        .to_string(),
                );
                break;
            }
            let identity = match self.frozen_weixin_occurrence(&run.run_id) {
                Ok(Some(identity)) => identity,
                Ok(None) => continue,
                Err(error) => {
                    report.pending.push(format!("{}: {error}", run.run_id));
                    continue;
                }
            };
            report.scanned = report.scanned.saturating_add(1);
            if identity.workspace_id != self.active_workspace_id()
                || identity.workspace_id != run.workspace_id
            {
                report.pending.push(format!(
                    "{}: original notification workspace does not match this physical store",
                    run.run_id
                ));
                continue;
            }
            match self.weixin_notification_status(&run.run_id) {
                Ok(Some(receipt)) if receipt.terminal => {
                    report.settled = report.settled.saturating_add(1);
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    report.pending.push(format!("{}: {error}", run.run_id));
                    continue;
                }
            }
            let admission =
                match self.reserve_run_driver_admission(run.run_id.clone(), cancel.child_token()) {
                    Ok(admission) => admission,
                    Err(error) => {
                        report.pending.push(format!("{}: {error}", run.run_id));
                        continue;
                    }
                };
            let generation = match self.lease_active_workspace_generation() {
                Ok(generation) => generation,
                Err(error) => {
                    report.pending.push(format!("{}: {error}", run.run_id));
                    continue;
                }
            };
            if self.active_workspace_id() != identity.workspace_id {
                report.pending.push(format!(
                    "{}: original workspace changed before generation admission",
                    run.run_id
                ));
                continue;
            }
            let registration = match self
                .register_observed_run_driver::<Result<(), String>>(admission, generation)
            {
                Ok(registration) => registration,
                Err(error) => {
                    report.pending.push(format!("{}: {error}", run.run_id));
                    continue;
                }
            };
            let original = self.clone();
            let original_sender = sender.clone();
            let original_workspace = workspace_lifetime.clone();
            let waiter = registration.start(move |mut receipt_owner| async move {
                if let Some(lifetime) = original_workspace {
                    receipt_owner.retain(WeixinRecoveryWorkspaceReceipt {
                        _lifetime: lifetime,
                    })?;
                }
                // Return delivery refusal as data. The original run-driver
                // owner still joins the physical producer, but its terminal
                // settlement observes Completed and cannot fail the prompt.
                Ok::<_, super::store::RunDriverFailure>(
                    original
                        .deliver_weixin_completion(&identity, original_sender.as_ref())
                        .await
                        .map_err(|error| error.to_string()),
                )
            });
            match waiter.await {
                Ok(Ok(Ok(()))) => report.recovered = report.recovered.saturating_add(1),
                Ok(Ok(Err(error))) => report.pending.push(format!("{}: {error}", run.run_id)),
                Ok(Err(error)) => report.pending.push(format!("{}: {error}", run.run_id)),
                Err(error) => report.pending.push(format!(
                    "{}: original recovery owner did not settle: {error}",
                    run.run_id
                )),
            }
        }
        report
    }

    #[cfg(all(test, feature = "channels"))]
    pub(crate) fn fail_weixin_phase_before_append_for_test(
        run_id: &str,
        phase: echo_agent::delivery::DeliveryPhase,
    ) {
        super::run_authority::RunAuthority::fail_weixin_phase_before_append_for_test(run_id, phase);
    }

    /// Extract actual completion data from the original revisioned graph.
    /// No missing result is replaced with a generic success notification.
    pub fn weixin_completed_output(&self, run_id: &str) -> Result<String, StoreError> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        if run.status != TaskRunStatus::Completed {
            return Err(store_error("notification requires a completed TaskRun"));
        }
        let plan = self
            .get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        let mut results = Vec::new();
        for task in plan.tasks {
            if task.status != echo_agent::tasks::TaskStatus::Completed {
                continue;
            }
            let summary = self.get_summary(run_id, &task.id)?;
            let result = match task.result {
                Some(result) => result,
                None => summary
                    .as_ref()
                    .map(|summary| serde_json::to_value(&summary.outcome))
                    .transpose()?
                    .ok_or_else(|| store_error("completed task has no durable execution result"))?,
            };
            let full_output = self.weixin_task_full_output(
                run_id,
                &task.id,
                summary.as_ref(),
                task.kind == PlanTaskKind::Summary && task.agent_role == "primary-agent",
            )?;
            results.push(serde_json::json!({
                "task_id": task.id,
                "title": task.title,
                "result": result,
                "full_output": full_output,
            }));
        }
        if results.is_empty() {
            return Err(store_error("completed TaskRun has no durable task result"));
        }
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "run_id": run.run_id,
            "results": results,
        }))?)
    }

    /// Resolve raw output only through the canonical summary's exact physical
    /// claim. A late older Subagent release cannot supply another attempt's
    /// answer. Direct completion's original TaskCompleted carries its full
    /// answer; it has no Subagent claim by design.
    fn weixin_task_full_output(
        &self,
        run_id: &str,
        task_id: &str,
        summary: Option<&TaskExecutionSummary>,
        direct: bool,
    ) -> Result<Option<String>, StoreError> {
        let Some(summary) = summary else {
            return Ok(None);
        };
        let mut after = 0;
        let mut exact_claim = None;
        let mut direct_output = None;
        loop {
            let events = self.query_events_bounded(
                run_id,
                RuntimeEventQuery::new(after, 64).with_event_types(vec![
                    RuntimeEventKind::Note,
                    RuntimeEventKind::TaskCompleted,
                ]),
            )?;
            if events.is_empty() {
                break;
            }
            for event in events {
                after = event.seq;
                if event.task_id.as_deref() != Some(task_id) {
                    continue;
                }
                if direct && event.event_type == RuntimeEventKind::TaskCompleted {
                    direct_output = event
                        .payload
                        .get("summary")
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_string);
                }
                if event.event_type != RuntimeEventKind::Note
                    || event
                        .payload
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        != Some("summary_persisted")
                {
                    continue;
                }
                let Some(value) = event.payload.get("summary") else {
                    continue;
                };
                let candidate: TaskExecutionSummary = serde_json::from_value(value.clone())?;
                if candidate.created_at != summary.created_at
                    || candidate.task_id != summary.task_id
                    || candidate.run_id != summary.run_id
                {
                    continue;
                }
                if let (Some(claim_id), Some(revision), Some(attempt)) = (
                    event
                        .payload
                        .get("claim_id")
                        .and_then(serde_json::Value::as_str),
                    event
                        .payload
                        .get("plan_revision")
                        .and_then(serde_json::Value::as_u64),
                    event
                        .payload
                        .get("attempt")
                        .and_then(serde_json::Value::as_u64),
                ) {
                    exact_claim = Some(format!(
                        "{run_id}:{task_id}:{revision}:{attempt}:{claim_id}"
                    ));
                }
            }
        }
        if direct {
            return Ok(direct_output);
        }
        let Some(execution_id) = exact_claim else {
            return Ok(None);
        };
        let events = self.query_events_bounded(
            run_id,
            RuntimeEventQuery::new(0, 1)
                .for_execution(execution_id)
                .with_event_types(vec![RuntimeEventKind::SubagentReleased]),
        )?;
        let output = events
            .first()
            .and_then(|event| {
                (event.task_id.as_deref() == Some(task_id)
                    && event
                        .payload
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        == Some("completed"))
                .then(|| {
                    event
                        .payload
                        .get("full_output")
                        .and_then(serde_json::Value::as_str)
                })
                .flatten()
            })
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string);
        output
            .map(Some)
            .ok_or_else(|| store_error("exact completed Subagent raw output is unavailable"))
    }

    fn with_weixin_ledger<R>(
        &self,
        run_id: &str,
        operation: impl FnOnce(&Ledger, &Arc<ReplayJournal>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        self.with_weixin_delivery_lock(run_id, || {
            let authority = self.weixin_delivery_authority(run_id)?;
            let (identity, physical_head, records) = authority.weixin_journal_snapshot()?;
            let config = DeliveryLedgerConfig {
                terminal_retention: 1,
                terminal_retention_bytes: usize::MAX,
            };
            let mut projection =
                DeliveryLedgerProjection::<WeixinDeliveryTarget, WeixinCompletionPayload>::default(
                );
            for record in records {
                projection.apply_record(&record);
            }
            projection.validate(config).map_err(store_error)?;
            let checkpoints = Arc::new(MemoryCheckpointStore::new());
            // The original journal multiplexes other runtime facts. Folding
            // its typed records and checkpointing at the SAME physical read
            // head accounts for those unrelated facts without renumbering any
            // delivery receipt. No fabricated no-op events enter the ledger.
            checkpoints
                .save(&identity, &projection, physical_head)
                .map_err(store_error)?;
            let journal = Arc::new(WeixinJournal {
                authority,
                identity,
                snapshot_head: physical_head,
            });
            let ledger = Ledger::new(Arc::clone(&journal), checkpoints, config, 0);
            ledger.recover().map_err(store_error)?;
            operation(&ledger, &journal)
        })
    }

    #[cfg(any(test, feature = "channels"))]
    fn apply_weixin_event(
        &self,
        _run_id: &str,
        ledger: &Ledger,
        journal: &Arc<ReplayJournal>,
        event: WeixinEvent,
    ) -> Result<(), StoreError> {
        let prepared = PreparedJournalBatch::new(vec![event]).map_err(store_error)?;
        let receipt = ledger
            .apply_prepared_with(prepared, |batch| journal.append_batch(batch))
            .map_err(store_error)?;
        if receipt.journal != JournalDurabilityStatus::Confirmed {
            return Err(store_error(
                "physical TaskRun delivery durability is unconfirmed; effect refused",
            ));
        }
        Ok(())
    }

    /// Enqueue, claim and durably start at most one original completion effect.
    /// Any existing started effect is reconciled as Unknown, never re-claimed.
    #[cfg(any(test, feature = "channels"))]
    pub(crate) fn claim_weixin_completion(
        &self,
        run_id: &str,
        identity: &OccurrenceIdentity,
    ) -> Result<Option<DeliveryClaim<WeixinDeliveryTarget, WeixinCompletionPayload>>, StoreError>
    {
        self.with_weixin_ledger(run_id, |ledger, journal| {
            let run = self.get_run(run_id)?.ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status != TaskRunStatus::Completed {
                return Err(store_error("notification requires a completed TaskRun"));
            }
            let Some(target) = self.frozen_weixin_notification(run_id)? else { return Ok(None); };
            target.validate_for_occurrence(identity)?;
            let message_id = notification_id(run_id, &target)?;
            let existing = ledger.with_projection(|state| state.record(&message_id).cloned());
            if let Some(record) = existing {
                if record.terminal { return Ok(None); }
                if record.effect_started_at.is_some() {
                    let claim = claim_from_record(&record)?;
                    let event = ledger.prepare_settle(&claim, DeliverySettlement::terminal(
                        record.turn_id, DeliveryOutcome::OutcomeUnknown, None,
                        Some("original Weixin effect started without durable terminal receipt; replay refused".to_string()), None,
                    )).map_err(store_error)?;
                    self.apply_weixin_event(run_id, ledger, journal, event)?;
                    return Ok(None);
                }
            } else {
                let payload = WeixinCompletionPayload {
                    run_id: run_id.to_string(), occurrence_id: identity.occurrence_id.clone(),
                    text: self.weixin_completed_output(run_id)?,
                };
                let event = ledger.prepare_enqueue(DeliveryEnvelope::new(message_id, target, payload)).map_err(store_error)?;
                self.apply_weixin_event(run_id, ledger, journal, event)?;
            }
            let Some(draft) = ledger.prepare_claim_next().map_err(store_error)? else { return Ok(None); };
            self.apply_weixin_event(run_id, ledger, journal, draft.event)?;
            let event = ledger.prepare_begin_effect(&draft.claim, format!("weixin:{}", draft.claim.message_id)).map_err(store_error)?;
            self.apply_weixin_event(run_id, ledger, journal, event)?;
            Ok(Some(draft.claim))
        })
    }

    pub fn weixin_notification_status(
        &self,
        run_id: &str,
    ) -> Result<Option<WeixinNotificationRecord>, StoreError> {
        self.with_weixin_ledger(run_id, |ledger, _| {
            Ok(ledger.with_projection(|state| state.records().next().cloned()))
        })
    }

    #[cfg(feature = "channels")]
    fn settle_weixin_completion(
        &self,
        run_id: &str,
        claim: &DeliveryClaim<WeixinDeliveryTarget, WeixinCompletionPayload>,
        outcome: Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
    ) -> Result<(), StoreError> {
        use echo_agent::channels::weixin::WeixinSendOutcome;
        let (outcome, reason, reply) = match outcome {
            Ok(WeixinSendOutcome::Accepted { message_id, .. }) => {
                (DeliveryOutcome::Completed, None, message_id)
            }
            Ok(WeixinSendOutcome::Rejected { code }) => (
                DeliveryOutcome::Failed,
                Some(format!("Weixin rejected send: {code}")),
                None,
            ),
            Ok(WeixinSendOutcome::Unknown { reason, .. }) => {
                (DeliveryOutcome::OutcomeUnknown, Some(reason), None)
            }
            // EffectStarted records the durable permission boundary, not
            // proof that the HTTP producer accepted a request. The send
            // contract can still authoritatively refuse local admission.
            Err(reason) => (
                DeliveryOutcome::Failed,
                Some(format!("(local not sent) {reason}")),
                None,
            ),
        };
        self.with_weixin_ledger(run_id, |ledger, journal| {
            let event = ledger
                .prepare_settle(
                    claim,
                    DeliverySettlement::terminal(
                        Some(format!("weixin:{}", claim.message_id)),
                        outcome,
                        None,
                        reason,
                        reply,
                    ),
                )
                .map_err(store_error)?;
            self.apply_weixin_event(run_id, ledger, journal, event)
        })
    }

    /// A notification failure is reported independently and cannot turn a
    /// completed TaskRun into Failed or cause its prompt to execute again.
    #[cfg(feature = "channels")]
    pub async fn deliver_weixin_completion(
        &self,
        identity: &OccurrenceIdentity,
        sender: &dyn WeixinCronDelivery,
    ) -> Result<(), StoreError> {
        let run_id = identity.run_id();
        let Some(claim) = self.claim_weixin_completion(&run_id, identity)? else {
            return Ok(());
        };
        let outcome = sender
            .send(&claim.route, &claim.message_id, &claim.payload.text)
            .await;
        self.settle_weixin_completion(&run_id, &claim, outcome)
    }
}

#[cfg(any(test, feature = "channels"))]
fn claim_from_record(
    record: &WeixinNotificationRecord,
) -> Result<DeliveryClaim<WeixinDeliveryTarget, WeixinCompletionPayload>, StoreError> {
    Ok(DeliveryClaim {
        message_id: record.message_id.clone(),
        route: record.route.clone(),
        payload: record.payload.clone(),
        attempt_id: record
            .attempt_id
            .clone()
            .ok_or_else(|| store_error("started notification has no attempt identity"))?,
        attempt: record.attempt,
        claimed_at: record
            .claimed_at
            .ok_or_else(|| store_error("started notification has no original claim time"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::{
        AttendedMode, DomainProfile, OccurrenceRunRequest, RunPlanPolicy, TaskRunExecutionProfile,
    };
    use chrono::Utc;

    fn request() -> OccurrenceRunRequest {
        OccurrenceRunRequest {
            identity: OccurrenceIdentity {
                workspace_id: "test".to_string(),
                occurrence_id: "scheduled:one".to_string(),
                scheduler_task_id: "cron-one".to_string(),
                scheduled_at: Utc::now(),
            },
            conversation_id: "cron-one-conversation".to_string(),
            root_message_id: String::new(),
            domain_profile: DomainProfile::General,
            goal: "deliver a real result".to_string(),
            route: "cron".to_string(),
            attended_mode: AttendedMode::Unattended,
            execution_profile: TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
        }
    }

    fn target() -> WeixinDeliveryTarget {
        WeixinDeliveryTarget {
            account_id: "original-account".to_string(),
            credential_generation: "generation-uuid".to_string(),
            peer_id: "original-peer".to_string(),
            context_ref: "opaque-original-context-reference".to_string(),
            workspace_id: "test".to_string(),
            channel_conversation: "original-conversation".to_string(),
            definition_id: "original-definition".to_string(),
        }
    }

    #[test]
    fn first_admission_freezes_target_and_legacy_none_is_not_backfilled()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let original = target();
        let (_, created) =
            store.get_or_create_occurrence_run_with_weixin(&request, Some(&original))?;
        assert!(created);
        let mut changed = original.clone();
        changed.account_id = "replacement-account".to_string();
        changed.credential_generation = "replacement-generation".to_string();
        changed.peer_id = "replacement-peer".to_string();
        let (_, created) =
            store.get_or_create_occurrence_run_with_weixin(&request, Some(&changed))?;
        assert!(!created);
        assert_eq!(
            store.frozen_weixin_notification(&request.run_id())?,
            Some(original)
        );
        assert_eq!(
            store.frozen_weixin_occurrence(&request.run_id())?,
            Some(request.identity.clone())
        );
        let facts = store.list_events(&request.run_id(), 0)?;
        assert_eq!(
            facts.len(),
            1,
            "target and original admission must be one physical fact"
        );
        assert_eq!(
            facts.first().map(|event| event.event_type),
            Some(RuntimeEventKind::RunCreated)
        );

        let mut legacy = request.clone();
        legacy.identity.occurrence_id = "scheduled:legacy".to_string();
        store.get_or_create_occurrence_run(&legacy)?;
        store.get_or_create_occurrence_run_with_weixin(&legacy, Some(&changed))?;
        assert_eq!(store.frozen_weixin_notification(&legacy.run_id())?, None);
        assert_eq!(store.list_events(&legacy.run_id(), 0)?.len(), 1);
        Ok(())
    }

    fn enqueue_completed_fixture(
        store: &TaskRuntimeStore,
        request: &OccurrenceRunRequest,
    ) -> Result<(), StoreError> {
        let target = target();
        let run_id = request.run_id();
        store.get_or_create_occurrence_run_with_weixin(request, Some(&target))?;
        store.transition_run(&run_id, TaskRunStatus::Running)?;
        store.transition_run(&run_id, TaskRunStatus::Completed)?;
        store.with_weixin_ledger(&run_id, |ledger, journal| {
            let envelope = DeliveryEnvelope::new(
                notification_id(&run_id, &target)?,
                target,
                WeixinCompletionPayload {
                    run_id: run_id.clone(),
                    occurrence_id: request.identity.occurrence_id.clone(),
                    text: "中文与 emoji 🌱: durable result fixture, artifact: output/report.md"
                        .to_string(),
                },
            );
            let event = ledger.prepare_enqueue(envelope).map_err(store_error)?;
            store.apply_weixin_event(&run_id, ledger, journal, event)
        })
    }

    #[test]
    fn started_effect_reopens_as_unknown_without_reclaiming_or_reexecuting()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let request = request();
        let run_id = request.run_id();
        {
            let store = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
            enqueue_completed_fixture(&store, &request)?;
            let claim = store.claim_weixin_completion(&run_id, &request.identity)?;
            assert!(claim.is_some());
            let record = store
                .weixin_notification_status(&run_id)?
                .ok_or("missing started receipt")?;
            assert!(record.effect_started_at.is_some());
            assert_eq!(record.route, target());
        }
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
        assert!(
            reopened
                .claim_weixin_completion(&run_id, &request.identity)?
                .is_none()
        );
        let record = reopened
            .weixin_notification_status(&run_id)?
            .ok_or("missing recovery receipt")?;
        assert_eq!(record.outcome, Some(DeliveryOutcome::OutcomeUnknown));
        assert!(record.terminal);
        assert!(
            reopened
                .claim_weixin_completion(&run_id, &request.identity)?
                .is_none()
        );
        assert_eq!(
            reopened.get_run(&run_id)?.ok_or("run missing")?.status,
            TaskRunStatus::Completed
        );
        let original = reopened.list_events(&run_id, 0)?;
        assert_eq!(
            original
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunCreated)
                .count(),
            1
        );
        assert!(
            original
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::WeixinDeliveryRecorded)
                .count()
                >= 4
        );
        Ok(())
    }

    #[test]
    fn unstarted_claim_recovers_with_same_frozen_payload_and_new_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        enqueue_completed_fixture(&store, &request)?;
        let first = store.with_weixin_ledger(&run_id, |ledger, journal| {
            let draft = ledger
                .prepare_claim_next()
                .map_err(store_error)?
                .ok_or_else(|| store_error("no claim"))?;
            store.apply_weixin_event(&run_id, ledger, journal, draft.event)?;
            Ok(draft.claim)
        })?;
        let recovered = store
            .claim_weixin_completion(&run_id, &request.identity)?
            .ok_or("no recovered claim")?;
        assert_eq!(first.message_id, recovered.message_id);
        assert_eq!(first.payload, recovered.payload);
        assert_eq!(first.route, recovered.route);
        assert_eq!(recovered.attempt, first.attempt.saturating_add(1));
        assert_ne!(first.attempt_id, recovered.attempt_id);
        Ok(())
    }

    #[test]
    fn accepted_receipt_replays_without_a_second_effect() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        enqueue_completed_fixture(&store, &request)?;
        let claim = store
            .claim_weixin_completion(&run_id, &request.identity)?
            .ok_or("missing claim")?;
        // Ordinary runtime facts share this exact physical journal and must
        // not force delivery to invent another sequence namespace.
        store.note(
            &run_id,
            None,
            "ordinary completion bookkeeping between send phases",
        )?;
        store.with_weixin_ledger(&run_id, |ledger, journal| {
            let event = ledger
                .prepare_settle(
                    &claim,
                    DeliverySettlement::terminal(
                        Some(format!("weixin:{}", claim.message_id)),
                        DeliveryOutcome::Completed,
                        None,
                        None,
                        Some("server-accepted-identity".to_string()),
                    ),
                )
                .map_err(store_error)?;
            store.apply_weixin_event(&run_id, ledger, journal, event)
        })?;
        assert!(
            store
                .claim_weixin_completion(&run_id, &request.identity)?
                .is_none()
        );
        let status = store
            .weixin_notification_status(&run_id)?
            .ok_or("missing accepted receipt")?;
        assert_eq!(
            status.reply_message_id.as_deref(),
            Some("server-accepted-identity")
        );
        assert_eq!(status.outcome, Some(DeliveryOutcome::Completed));
        assert_eq!(status.attempt, 1);
        Ok(())
    }

    #[test]
    fn a_finished_label_without_durable_result_never_creates_a_notification()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        store.get_or_create_occurrence_run_with_weixin(&request, Some(&target()))?;
        store.transition_run(&run_id, TaskRunStatus::Running)?;
        store.transition_run(&run_id, TaskRunStatus::Completed)?;
        assert!(
            store
                .claim_weixin_completion(&run_id, &request.identity)
                .is_err()
        );
        assert!(store.weixin_notification_status(&run_id)?.is_none());
        assert_eq!(
            store.get_run(&run_id)?.ok_or("run missing")?.status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn unconfirmed_effect_frame_refuses_effect_and_recovers_unknown()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        enqueue_completed_fixture(&store, &request)?;
        let result = store.with_weixin_ledger(&run_id, |ledger, journal| {
            let draft = ledger
                .prepare_claim_next()
                .map_err(store_error)?
                .ok_or_else(|| store_error("no claim"))?;
            store.apply_weixin_event(&run_id, ledger, journal, draft.event)?;
            store.shadow.fail_next_append_durability_for_test(&run_id)?;
            let event = ledger
                .prepare_begin_effect(&draft.claim, "original-attempt")
                .map_err(store_error)?;
            store.apply_weixin_event(&run_id, ledger, journal, event)?;
            Ok(draft.claim)
        });
        assert!(
            result.is_err(),
            "no send claim may escape an unconfirmed physical EffectStarted"
        );
        assert!(
            store
                .claim_weixin_completion(&run_id, &request.identity)?
                .is_none()
        );
        let receipt = store
            .weixin_notification_status(&run_id)?
            .ok_or("missing reconciled receipt")?;
        assert_eq!(receipt.outcome, Some(DeliveryOutcome::OutcomeUnknown));
        assert_eq!(
            store.get_run(&run_id)?.ok_or("run missing")?.status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn raw_answer_uses_exact_summary_claim_not_the_latest_late_release()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::tasks::task_runtime::types::{SubagentOutcome, SubagentStatus};
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        store.get_or_create_occurrence_run(&request)?;
        let task_id = "task-original";
        let exact = format!("{run_id}:{task_id}:2:1:accepted-claim");
        let wrong = format!("{run_id}:{task_id}:2:1:retired-claim");
        let full = format!("{}ORIGINAL-RAW-END", "中文🌱".repeat(600));
        for (execution_id, output) in [
            (&exact, full.as_str()),
            (&wrong, "late obsolete raw answer"),
        ] {
            store.shadow.append_event_line(
                &run_id,
                Some(task_id),
                Some(execution_id),
                RuntimeEventKind::SubagentReleased,
                serde_json::json!({"status":"completed", "full_output":output}),
            )?;
        }
        let summary = TaskExecutionSummary {
            run_id: run_id.clone(),
            task_id: task_id.to_string(),
            subagent_name: "general".to_string(),
            outcome: SubagentOutcome::terminal(
                SubagentStatus::Completed,
                "bounded canonical summary",
                Vec::new(),
            ),
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        };
        store.shadow.append_event_line(
            &run_id,
            Some(task_id),
            None,
            RuntimeEventKind::Note,
            serde_json::json!({"kind":"summary_persisted", "summary":summary, "plan_revision":2,
                "attempt":1, "claim_id":"accepted-claim"}),
        )?;
        assert_eq!(
            store.weixin_task_full_output(&run_id, task_id, Some(&summary), false)?,
            Some(full)
        );
        Ok(())
    }

    #[test]
    fn typed_adapter_preserves_original_physical_identity_sequence_and_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        use echo_agent::state::journal::JournalBatchCommitStatus;
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let run_id = request.run_id();
        store.get_or_create_occurrence_run_with_weixin(&request, Some(&target()))?;
        let prepared = PreparedJournalBatch::new(vec![DeliveryEvent::Persisted {
            envelope: DeliveryEnvelope::new(
                "physical-proof",
                target(),
                WeixinCompletionPayload {
                    run_id: run_id.clone(),
                    occurrence_id: request.identity.occurrence_id.clone(),
                    text: "真实原件".to_string(),
                },
            ),
            persisted_at: Utc::now(),
        }])?;
        let expected_batch_id = prepared.batch_id().to_string();
        store.with_weixin_ledger(&run_id, |_ledger, journal| {
            let (identity, before, _) = journal.authority.weixin_journal_snapshot()?;
            assert_eq!(
                before, 1,
                "original RunCreated must own physical sequence one"
            );
            let receipt = journal
                .append_batch(prepared.clone())
                .map_err(store_error)?;
            assert_eq!(receipt.journal_identity(), &identity);
            assert_eq!(receipt.batch_id(), expected_batch_id.as_str());
            assert_eq!(
                receipt.records().first().map(|record| record.sequence),
                Some(2)
            );
            assert_eq!(receipt.commit_status(), JournalBatchCommitStatus::Committed);
            assert!(prepared.matches_receipt(&receipt).map_err(store_error)?);
            let lookup = journal.lookup_batch(&prepared).map_err(store_error)?;
            let JournalBatchLookup::AlreadyCommitted(looked_up) = lookup else {
                return Err(store_error("original frame lookup lost"));
            };
            assert_eq!(looked_up.journal_identity(), receipt.journal_identity());
            assert_eq!(
                looked_up.records().first().map(|record| record.sequence),
                Some(2)
            );
            assert_eq!(
                looked_up.commit_status(),
                JournalBatchCommitStatus::AlreadyCommitted
            );
            assert_eq!(
                looked_up.durability(),
                &JournalDurabilityStatus::Unconfirmed,
                "read-only lookup cannot invent a durability barrier"
            );
            let duplicate = journal
                .append_batch(prepared.clone())
                .map_err(store_error)?;
            assert_eq!(
                duplicate.records().first().map(|record| record.sequence),
                Some(2)
            );
            assert_eq!(
                duplicate.commit_status(),
                JournalBatchCommitStatus::AlreadyCommitted
            );
            Ok(())
        })?;
        let original = store.list_events(&run_id, 0)?;
        assert_eq!(original.len(), 2);
        let event = original.get(1).ok_or("physical frame missing")?;
        assert_eq!(event.seq, 2);
        assert_eq!(event.event_type, RuntimeEventKind::WeixinDeliveryRecorded);
        let typed: WeixinEvent = serde_json::from_value(event.payload.clone())?;
        assert_eq!(
            prepared.events().first().map(|event| event.as_ref()),
            Some(&typed)
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct DiagnosticSender {
        local_refusal: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[cfg(feature = "channels")]
    impl WeixinCronDelivery for DiagnosticSender {
        fn freeze_target<'a>(
            &'a self,
            _invocation: &'a echo_agent::scheduler::SchedulerInvocation,
            _workspace_id: &'a str,
        ) -> BoxFuture<'a, Result<Option<WeixinDeliveryTarget>, String>> {
            Box::pin(async { Err("fixture must use the original frozen admission".to_string()) })
        }

        fn send<'a>(
            &'a self,
            _target: &'a WeixinDeliveryTarget,
            message_id: &'a str,
            _text: &'a str,
        ) -> BoxFuture<'a, Result<echo_agent::channels::weixin::WeixinSendOutcome, String>>
        {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.local_refusal {
                    Err("frozen context is missing for the original account generation".to_string())
                } else {
                    Ok(echo_agent::channels::weixin::WeixinSendOutcome::Unknown {
                        client_id: message_id.to_string(),
                        reason: "HTTP send timed out after effect admission".to_string(),
                    })
                }
            })
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn local_not_sent_and_ambiguous_send_preserve_distinct_diagnostics_without_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        for local_refusal in [true, false] {
            let store = TaskRuntimeStore::new_in_memory()?;
            let request = request();
            let run_id = request.run_id();
            enqueue_completed_fixture(&store, &request)?;
            let sender = DiagnosticSender {
                local_refusal,
                calls: std::sync::atomic::AtomicUsize::new(0),
            };
            store
                .deliver_weixin_completion(&request.identity, &sender)
                .await?;
            let receipt = store
                .weixin_notification_status(&run_id)?
                .ok_or("diagnostic receipt missing")?;
            if local_refusal {
                assert_eq!(receipt.outcome, Some(DeliveryOutcome::Failed));
                assert_eq!(
                    receipt.reason.as_deref(),
                    Some(
                        "(local not sent) frozen context is missing for the original account generation"
                    )
                );
            } else {
                assert_eq!(receipt.outcome, Some(DeliveryOutcome::OutcomeUnknown));
                assert_eq!(
                    receipt.reason.as_deref(),
                    Some("HTTP send timed out after effect admission")
                );
            }
            assert!(receipt.terminal);
            assert!(receipt.next_attempt_at.is_none());
            assert!(
                receipt.effect_started_at.is_some(),
                "durable intent does not prove the HTTP effect was admitted"
            );
            store
                .deliver_weixin_completion(&request.identity, &sender)
                .await?;
            assert_eq!(sender.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                store
                    .get_run(&run_id)?
                    .ok_or("original completed run missing")?
                    .status,
                TaskRunStatus::Completed
            );
        }
        Ok(())
    }
}
