//! Interruption-first classification of success evidence.
//!
//! Cancellation and pause must win over evidence that was produced by work the
//! user already interrupted. Two different situations can deliver such
//! evidence, and callers must be able to tell them apart:
//!
//! - **Late** evidence belongs to an attempt or run epoch that was already
//!   settled by an interruption (or by a newer attempt). It must be rejected
//!   without rewriting the durable terminal truth.
//! - **Duplicate** evidence repeats an attempt that already committed the same
//!   successful result. Accepting it again is a no-op, not a state change.
//!
//! The classifier is deliberately pure: store mutations pass the durable run
//! status plus the canonical framework execution, so the decision is
//! deterministic and can be unit-tested without timers or background tasks.

use serde::{Deserialize, Serialize};

use super::types::TaskRunStatus;

/// What happened to one attempt to publish successful task evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessEvidenceSettlement {
    /// The evidence matched a live claim and was committed.
    Committed,
    /// The exact attempt already committed a successful terminal result.
    Duplicate,
    /// Interruption or a newer attempt settled first; the evidence was
    /// rejected and durable truth was preserved.
    Late(LateSuccessEvidence),
}

impl SuccessEvidenceSettlement {
    pub const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed)
    }

    pub const fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate)
    }

    pub const fn is_late(&self) -> bool {
        matches!(self, Self::Late(_))
    }

    /// Stable wire label for journal payloads and logs.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Duplicate => "duplicate",
            Self::Late(_) => "late",
        }
    }
}

/// Final acceptance of one attempt's execution-success evidence.
///
/// [`SuccessEvidenceSettlement`] only classifies the claim against durable run
/// state *before* the canonical settlement runs, so it cannot express the last
/// word on whether the evidence was published. Two real paths reach a
/// settlement that refuses evidence while the classifier still reports
/// [`SuccessEvidenceSettlement::Committed`]:
///
/// - a durable claim interruption requested by an active driver keeps the run
///   in `Running`, yet [`echo_agent::tasks::settle_runtime_resolution`] commits
///   [`echo_agent::tasks::RuntimeTaskResolution::Interrupted`];
/// - a non-success attempt (review `NeedsFix`/`Blocked`) finishes its execution
///   after the run/claim was interrupted while the review gate was in flight.
///
/// Live projections and the journal must be driven by this acceptance fact
/// instead of [`SuccessEvidenceSettlement::is_late`]; otherwise the live
/// stream advertises artifacts/verification the journal never accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuccessEvidenceAcceptance {
    /// The canonical settlement accepted the attempt's evidence.
    Accepted,
    /// The attempt settled through an interruption or a non-success
    /// resolution; none of its execution-success evidence may be published.
    Rejected,
}

impl SuccessEvidenceAcceptance {
    pub const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }

    pub const fn is_rejected(self) -> bool {
        !self.is_accepted()
    }

    /// Stable wire label for journal payloads and logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

/// Why a piece of success evidence was classified as late.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LateSuccessEvidence {
    /// The run was cancelled or paused before the evidence arrived.
    RunInterrupted { run_status: TaskRunStatus },
    /// The run already reached a different terminal status.
    RunTerminal { run_status: TaskRunStatus },
    /// The task already settled to a different terminal status.
    TaskTerminal {
        task_status: echo_agent::tasks::TaskStatus,
    },
    /// A retry or re-dispatch moved the canonical execution past this attempt.
    AttemptSuperseded { attempted: u32, current: u32 },
    /// The evidence claims an attempt the canonical execution never admitted.
    AttemptNotAdmitted { attempted: u32, current: u32 },
    /// The claim that produced the evidence is no longer the current claim.
    ClaimNotCurrent {
        attempted: u32,
        current_task_attempt: Option<u32>,
    },
}

impl LateSuccessEvidence {
    /// Stable wire label for journal payloads and logs.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::RunInterrupted { .. } => "run_interrupted",
            Self::RunTerminal { .. } => "run_terminal",
            Self::TaskTerminal { .. } => "task_terminal",
            Self::AttemptSuperseded { .. } => "attempt_superseded",
            Self::AttemptNotAdmitted { .. } => "attempt_not_admitted",
            Self::ClaimNotCurrent { .. } => "claim_not_current",
        }
    }
}

/// Classify one attempt to publish successful task evidence.
///
/// `attempted_attempt` is the framework attempt number of the producing claim
/// (`retry_count + 1`). `execution` is the canonical framework execution read
/// under the run lock.
pub fn classify_success_evidence(
    run_status: TaskRunStatus,
    attempted_attempt: u32,
    execution: &echo_agent::tasks::TaskExecution,
) -> SuccessEvidenceSettlement {
    let current_attempt = execution.retry_count.saturating_add(1);
    let settled_successfully = matches!(
        execution.status,
        echo_agent::tasks::TaskStatus::Completed | echo_agent::tasks::TaskStatus::Skipped
    );
    // A repeated delivery of the same attempt's committed success is a
    // duplicate even when the run later moved on (pause/cancel/terminal).
    if settled_successfully && attempted_attempt <= current_attempt {
        return SuccessEvidenceSettlement::Duplicate;
    }
    if matches!(run_status, TaskRunStatus::Cancelled | TaskRunStatus::Paused) {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunInterrupted { run_status });
    }
    if run_status == TaskRunStatus::Completed {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunTerminal { run_status });
    }
    if run_status == TaskRunStatus::Failed {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunTerminal { run_status });
    }
    if execution.status.is_terminal() {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::TaskTerminal {
            task_status: execution.status.clone(),
        });
    }
    if attempted_attempt == 0 {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::AttemptNotAdmitted {
            attempted: 0,
            current: current_attempt,
        });
    }
    if attempted_attempt < current_attempt {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::AttemptSuperseded {
            attempted: attempted_attempt,
            current: current_attempt,
        });
    }
    if attempted_attempt > current_attempt {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::AttemptNotAdmitted {
            attempted: attempted_attempt,
            current: current_attempt,
        });
    }
    let Some(claim) = execution.claim.as_ref() else {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::ClaimNotCurrent {
            attempted: attempted_attempt,
            current_task_attempt: None,
        });
    };
    if claim.attempt != attempted_attempt {
        return SuccessEvidenceSettlement::Late(LateSuccessEvidence::ClaimNotCurrent {
            attempted: attempted_attempt,
            current_task_attempt: Some(claim.attempt),
        });
    }
    SuccessEvidenceSettlement::Committed
}

/// Classify an attempt to mark the whole run completed.
///
/// `complete_run_if_quiescent` already rejects non-running runs; this helper
/// classifies the same durable truth for callers that must report *why* a late
/// completion was refused.
pub fn classify_run_completion(run_status: TaskRunStatus) -> SuccessEvidenceSettlement {
    match run_status {
        TaskRunStatus::Completed => SuccessEvidenceSettlement::Duplicate,
        TaskRunStatus::Cancelled | TaskRunStatus::Paused => {
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunInterrupted { run_status })
        }
        TaskRunStatus::Failed => {
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunTerminal { run_status })
        }
        TaskRunStatus::Pending | TaskRunStatus::Running => SuccessEvidenceSettlement::Committed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tasks::{TaskClaim, TaskExecution, TaskStatus};

    fn running_execution(retry_count: u32, attempt: u32) -> TaskExecution {
        let mut execution = TaskExecution::pending("t1");
        execution.status = TaskStatus::Running;
        execution.retry_count = retry_count;
        execution.claim = Some(TaskClaim::new(1, attempt, "spec-hash".to_string()));
        execution
    }

    #[test]
    fn live_attempt_commits() {
        let execution = running_execution(0, 1);
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Running, 1, &execution),
            SuccessEvidenceSettlement::Committed
        );
    }

    #[test]
    fn run_interruption_wins_over_live_attempt() {
        let execution = running_execution(0, 1);
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Cancelled, 1, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunInterrupted {
                run_status: TaskRunStatus::Cancelled
            })
        );
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Paused, 1, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::RunInterrupted {
                run_status: TaskRunStatus::Paused
            })
        );
    }

    #[test]
    fn repeated_success_for_settled_attempt_is_duplicate_not_late() {
        let mut execution = running_execution(0, 1);
        execution.status = TaskStatus::Completed;
        execution.claim = None;
        // The run may already be paused or cancelled; the same attempt's
        // committed success is still a duplicate re-delivery.
        for run_status in [
            TaskRunStatus::Running,
            TaskRunStatus::Paused,
            TaskRunStatus::Cancelled,
        ] {
            assert_eq!(
                classify_success_evidence(run_status, 1, &execution),
                SuccessEvidenceSettlement::Duplicate
            );
        }
    }

    #[test]
    fn newer_attempt_supersedes_old_evidence() {
        let execution = running_execution(1, 2);
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Running, 1, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::AttemptSuperseded {
                attempted: 1,
                current: 2
            })
        );
    }

    #[test]
    fn evidence_from_the_future_is_not_admitted() {
        let execution = running_execution(0, 1);
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Running, 3, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::AttemptNotAdmitted {
                attempted: 3,
                current: 1
            })
        );
    }

    #[test]
    fn released_claim_rejects_evidence() {
        let mut execution = running_execution(0, 1);
        execution.claim = None;
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Running, 1, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::ClaimNotCurrent {
                attempted: 1,
                current_task_attempt: None
            })
        );
    }

    #[test]
    fn terminal_task_blocks_late_success() {
        let mut execution = running_execution(0, 1);
        execution.status = TaskStatus::Failed("boom".to_string());
        assert_eq!(
            classify_success_evidence(TaskRunStatus::Running, 1, &execution),
            SuccessEvidenceSettlement::Late(LateSuccessEvidence::TaskTerminal {
                task_status: TaskStatus::Failed("boom".to_string())
            })
        );
    }

    #[test]
    fn run_completion_classification_matches_terminal_truth() {
        assert_eq!(
            classify_run_completion(TaskRunStatus::Running),
            SuccessEvidenceSettlement::Committed
        );
        assert_eq!(
            classify_run_completion(TaskRunStatus::Completed),
            SuccessEvidenceSettlement::Duplicate
        );
        assert!(classify_run_completion(TaskRunStatus::Cancelled).is_late());
        assert!(classify_run_completion(TaskRunStatus::Paused).is_late());
        assert!(classify_run_completion(TaskRunStatus::Failed).is_late());
    }
}
