//! Lossless GUI projections of original scheduler receipts. This module never
//! admits/retries occurrences, parses callback result strings, or creates runs.

use echo_agent::scheduler::{
    SchedulerCommitOutcome, SchedulerEditError, SchedulerHistoryReadError, SchedulerManualCause,
    SchedulerManualError, SchedulerManualReceipt, SchedulerOccurrenceRef,
};
use echo_agent::state::journal::{
    ApplyBatchReceipt, ApplyReceipt, CheckpointApplyStatus, JournalBatchCommitStatus,
};
use serde_json::{Value, json};

fn commit_name(commit: JournalBatchCommitStatus) -> &'static str {
    match commit {
        JournalBatchCommitStatus::Committed => "committed",
        JournalBatchCommitStatus::AlreadyCommitted => "already_committed",
    }
}
fn checkpoint_view(checkpoint: &CheckpointApplyStatus) -> Value {
    match checkpoint {
        CheckpointApplyStatus::NotDue => json!({"status":"not_due"}),
        CheckpointApplyStatus::Saved => json!({"status":"saved"}),
        CheckpointApplyStatus::Degraded { error } => json!({"status":"degraded", "error":error}),
    }
}
pub fn apply_receipt_view(receipt: &ApplyReceipt) -> Value {
    json!({"batch_id":receipt.batch_id,"sequence":receipt.sequence.to_string(),
        "journal":receipt.journal,"commit":commit_name(receipt.commit),
        "checkpoint":checkpoint_view(&receipt.checkpoint)})
}
fn batch_receipt_view(receipt: &ApplyBatchReceipt) -> Value {
    json!({"batch_id":receipt.batch_id,"first_sequence":receipt.first_sequence.to_string(),
        "last_sequence":receipt.last_sequence.to_string(),"record_count":receipt.record_count.to_string(),
        "journal":receipt.journal,"commit":commit_name(receipt.commit),
        "checkpoint":checkpoint_view(&receipt.checkpoint)})
}

pub fn edit_error_view(error: SchedulerEditError) -> Value {
    let cause = match &error {
        SchedulerEditError::InvalidCron => "invalid_cron",
        SchedulerEditError::NotFound => "not_found",
        SchedulerEditError::DefinitionConflict => "definition_conflict",
        SchedulerEditError::RevisionExhausted => "revision_exhausted",
        SchedulerEditError::Closed => "closed",
        SchedulerEditError::AuthorityUnavailable(_) => "authority_unavailable",
        SchedulerEditError::NotCommitted(_) => "not_committed",
        SchedulerEditError::CommitOutcomeUnknown(_) => "commit_outcome_unknown",
        _ => "unknown_failure",
    };
    json!({"success":false,"cause":cause,"error":error.to_string(),
        "outcome_unknown": matches!(error, SchedulerEditError::CommitOutcomeUnknown(_) | SchedulerEditError::AuthorityUnavailable(_)),
        "task":Value::Null})
}

pub fn manual_receipt_view(result: Result<SchedulerManualReceipt, SchedulerManualError>) -> Value {
    match result {
        Ok(receipt) => json!({"success":true,"cause":Value::Null,"outcome_unknown":false,
            "occurrence":receipt.occurrence,"attempt_id":receipt.attempt_id,"attempt":receipt.attempt,
            "result":receipt.result,"error":Value::Null,
            "terminal_receipt":apply_receipt_view(&receipt.terminal_receipt)}),
        Err(error) => {
            let cause = match error.cause.as_ref() {
                SchedulerManualCause::NotFound => "not_found",
                SchedulerManualCause::Disabled => "disabled",
                SchedulerManualCause::Closed => "closed",
                SchedulerManualCause::Authority(_) => "authority_unavailable",
                SchedulerManualCause::Callback(_) => "callback_failed",
                SchedulerManualCause::PhysicalSettlement(_) => "physical_settlement_unknown",
                SchedulerManualCause::Projection(_) => "projection_failed",
                SchedulerManualCause::Journal(_) => "journal_failed",
                SchedulerManualCause::Internal(_) => "internal",
                _ => "unknown_failure",
            };
            let journal = match error.cause.as_ref() {
                SchedulerManualCause::Journal(failure) => {
                    let outcome = match failure.outcome {
                        SchedulerCommitOutcome::NotCommitted => "not_committed",
                        SchedulerCommitOutcome::Committed => "committed",
                        SchedulerCommitOutcome::Unknown => "unknown",
                    };
                    json!({"outcome":outcome,"batch_id":failure.batch_id,
                        "retry_safe":failure.retry_safe,"requires_reopen":failure.requires_reopen,
                        "receipt":failure.receipt.as_deref().map(apply_receipt_view),
                        "batch_receipt":failure.batch_receipt.as_deref().map(batch_receipt_view)})
                }
                _ => Value::Null,
            };
            let unknown = matches!(
                error.cause.as_ref(),
                SchedulerManualCause::PhysicalSettlement(_) | SchedulerManualCause::Authority(_)
            ) || matches!(error.cause.as_ref(), SchedulerManualCause::Journal(failure) if failure.outcome == SchedulerCommitOutcome::Unknown);
            json!({"success":false,"cause":cause,"outcome_unknown":unknown,
                "occurrence":error.occurrence,"attempt_id":error.attempt_id,"attempt":error.attempt,
                "result":error.callback_result,"error":error.to_string(),
                "callback_error":error.callback_error.as_ref().map(ToString::to_string),
                "projection_error":error.projection_error.as_ref().map(ToString::to_string),
                "terminal_receipt":error.terminal_receipt.as_deref().map(apply_receipt_view),
                "journal_failure":journal})
        }
    }
}

pub fn history_error_view(error: SchedulerHistoryReadError) -> Value {
    match error {
        SchedulerHistoryReadError::InvalidLimit { limit } => {
            json!({"success":false,"cause":"invalid_limit","limit":limit})
        }
        SchedulerHistoryReadError::GenerationMismatch { current } => {
            json!({"success":false,"cause":"generation_mismatch","current_journal_identity":current})
        }
        SchedulerHistoryReadError::RetainedFloorChanged { retained_floor } => {
            json!({"success":false,"cause":"retained_floor_changed","retained_floor":retained_floor.to_string()})
        }
        SchedulerHistoryReadError::InvalidCursor => {
            json!({"success":false,"cause":"invalid_cursor"})
        }
        SchedulerHistoryReadError::Journal(error) => {
            json!({"success":false,"cause":"journal_unavailable","error":error.to_string()})
        }
    }
}

/// Resolve only a run already present in the scheduler's original TaskRuntime
/// store. The callback result is opaque; selected GUI workspace never retargets it.
pub fn occurrence_run_view(
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
    occurrence: &SchedulerOccurrenceRef,
) -> Value {
    let Some(store) = store else {
        return json!({"run":Value::Null,"lookup":"store_unavailable"});
    };
    let conversation = super::cron_conversation_id(&occurrence.task_id, &occurrence.occurrence_id);
    match store.list_runs_for_conversation(&conversation) {
        Ok(runs) if runs.len() == 1 => {
            let run = runs.into_iter().next();
            json!({"run":run.map(|run| json!({"run_id":run.run_id,"workspace_id":run.workspace_id,
                "conversation_id":run.conversation_id,"status":run.status})),"lookup":"found"})
        }
        Ok(runs) if runs.is_empty() => json!({"run":Value::Null,"lookup":"not_created"}),
        Ok(_) => json!({"run":Value::Null,"lookup":"ambiguous"}),
        Err(error) => json!({"run":Value::Null,"lookup":"unavailable","error":error.to_string()}),
    }
}

/// Cursor bytes preserve the original u64 authority across JavaScript's number
/// boundary. No cursor store or reinterpretation of journal facts is introduced.
pub fn history_page_view(
    page: echo_agent::scheduler::SchedulerHistoryPage,
    runs: std::collections::BTreeMap<String, Value>,
) -> Value {
    let cursor = page
        .next_cursor
        .as_ref()
        .map(serde_json::to_string)
        .transpose();
    match cursor {
        Ok(next_cursor) => json!({"success":true,"runs":runs,"page":{
            "journal_identity":page.journal_identity,"retained_floor":page.retained_floor.to_string(),
            "snapshot_end_sequence":page.snapshot_end_sequence.to_string(),
            "events":page.events.into_iter().map(|record| json!({"batch_id":record.batch_id,
                "sequence":record.sequence.to_string(),"event":record.event})).collect::<Vec<_>>(),
            "next_cursor":next_cursor}}),
        Err(error) => {
            json!({"success":false,"cause":"cursor_serialization_failed","error":error.to_string()})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_projection_keeps_unknown_commit_and_opaque_callback_result() -> anyhow::Result<()> {
        use echo_agent::scheduler::SchedulerJournalFailure;
        for outcome in [
            SchedulerCommitOutcome::NotCommitted,
            SchedulerCommitOutcome::Committed,
            SchedulerCommitOutcome::Unknown,
        ] {
            let view = manual_receipt_view(Err(SchedulerManualError {
                occurrence: None,
                attempt_id: Some("original-attempt".into()),
                attempt: Some(2),
                callback_result: Some("TaskRun: looks-like-a-run-id".into()),
                callback_error: None,
                projection_error: None,
                terminal_receipt: None,
                cause: Box::new(SchedulerManualCause::Journal(SchedulerJournalFailure {
                    outcome,
                    batch_id: Some("original-batch".into()),
                    receipt: None,
                    batch_receipt: None,
                    retry_safe: false,
                    requires_reopen: Some(true),
                    cause: Box::new(std::io::Error::other("original journal failure")),
                })),
            }));
            assert_eq!(view.get("success").and_then(Value::as_bool), Some(false));
            assert_eq!(
                view.get("outcome_unknown").and_then(Value::as_bool),
                Some(outcome == SchedulerCommitOutcome::Unknown)
            );
            assert_eq!(
                view.pointer("/journal_failure/requires_reopen")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert_eq!(
                view.pointer("/journal_failure/retry_safe")
                    .and_then(Value::as_bool),
                Some(false)
            );
            assert_eq!(
                view.get("result").and_then(Value::as_str),
                Some("TaskRun: looks-like-a-run-id")
            );
            assert!(view.get("run_id").is_none());
        }
        Ok(())
    }

    #[test]
    fn history_cursor_retains_u64_identity_without_javascript_rounding() -> anyhow::Result<()> {
        use echo_agent::scheduler::{SchedulerHistoryCursor, SchedulerHistoryPage};
        let journal = echo_agent::state::journal::JournalIdentity::new();
        let cursor = SchedulerHistoryCursor {
            journal_identity: journal.clone(),
            retained_floor: 1,
            snapshot_end_sequence: u64::MAX,
            after_sequence: u64::MAX.saturating_sub(1),
        };
        let value = history_page_view(
            SchedulerHistoryPage {
                journal_identity: journal,
                retained_floor: 1,
                snapshot_end_sequence: u64::MAX,
                events: Vec::new(),
                next_cursor: Some(cursor.clone()),
            },
            Default::default(),
        );
        let token = value
            .pointer("/page/next_cursor")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("opaque cursor missing"))?;
        let restored: SchedulerHistoryCursor = serde_json::from_str(token)?;
        assert_eq!(restored, cursor);
        assert_eq!(
            value
                .pointer("/page/snapshot_end_sequence")
                .and_then(Value::as_str),
            Some(u64::MAX.to_string().as_str())
        );
        Ok(())
    }
}
