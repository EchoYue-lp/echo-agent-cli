//! Stable scheduler-occurrence identity and idempotent TaskRun admission.
//!
//! T3 (scheduled task runs) receives a durable framework occurrence identity
//! for every scheduler delivery attempt. The scheduler may retry one
//! occurrence after an owner loss, so the application must map *slot* identity
//! — not delivery time, attempt id, or claim time — onto exactly one TaskRun.
//!
//! [`OccurrenceIdentity`] is that slot identity. It is serializable and
//! persistable so the scheduler journal can store it next to the occurrence,
//! and it derives a deterministic, filesystem-safe [`OccurrenceIdentity::run_id`]
//! from the scheduling slot. [`TaskRuntimeStore::get_or_create_occurrence_run`]
//! turns that id into an idempotent `(TaskRun, created)` admission: the first
//! delivery creates the run, every later delivery of the same slot returns the
//! existing run without touching its status and without triggering a resume.
//!
//! Existing `create_run*`, `transition_run`, `boot_auto_resume_decision`, and
//! `resume_task_run_after_boot` keep their current semantics; this module is a
//! pure addition on top of them.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::store::{StoreError, TaskRuntimeStore};
use super::types::{AttendedMode, DomainProfile, TaskRun, TaskRunExecutionProfile};

/// Version tag folded into the run id digest. Bump only together with an
/// intentional migration of existing scheduled runs.
const OCCURRENCE_RUN_ID_VERSION: u8 = 1;

/// Stable identity of one scheduled occurrence slot.
///
/// The identity is decided by the scheduler slot (which task fired, for which
/// scheduled time) and never by the delivery moment. `SchedulerInvocation`'s
/// `attempt_id`, `attempt`, and `recovered_after_owner_loss` are deliberately
/// excluded: every physical retry of one slot must map to the same run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OccurrenceIdentity {
    /// Workspace that owns the slot. Runs never migrate across workspaces.
    pub workspace_id: String,
    /// Durable scheduler occurrence id
    /// (`echo_agent::scheduler::SchedulerInvocation::occurrence_id`), stable
    /// across callback retries of the same slot.
    pub occurrence_id: String,
    /// Scheduler task id that produced the slot.
    pub scheduler_task_id: String,
    /// The scheduled slot time (or the request time for a manual one-off).
    /// Never the delivery/claim time.
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub scheduled_at: DateTime<Utc>,
}

impl OccurrenceIdentity {
    /// Build a slot identity from a durable scheduler invocation.
    pub fn from_scheduler_invocation(
        workspace_id: impl Into<String>,
        invocation: &echo_agent::scheduler::SchedulerInvocation,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            occurrence_id: invocation.occurrence_id.clone(),
            scheduler_task_id: invocation.task.id.clone(),
            scheduled_at: invocation.scheduled_at,
        }
    }

    /// Canonical digest over every slot-defining field.
    pub fn digest(&self) -> String {
        let scheduled_at = self
            .scheduled_at
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
        let mut hasher = Sha256::new();
        hasher.update([OCCURRENCE_RUN_ID_VERSION]);
        for part in [
            self.workspace_id.as_str(),
            self.occurrence_id.as_str(),
            self.scheduler_task_id.as_str(),
            scheduled_at.as_str(),
        ] {
            // Length-prefix each field so adjacent fields cannot alias.
            hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(part.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Deterministic, filesystem-safe TaskRun id for this slot.
    ///
    /// The id is the single idempotency key: re-delivering the same slot reads
    /// the run created by the first delivery, and a genuine slot collision
    /// cannot happen without a SHA-256 collision.
    pub fn run_id(&self) -> String {
        format!("occ-{}", self.digest())
    }

    /// Reject identities that cannot safely address a run directory.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.workspace_id.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "occurrence identity requires a workspace_id".to_string(),
            ));
        }
        if self.occurrence_id.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "occurrence identity requires an occurrence_id".to_string(),
            ));
        }
        if self.scheduler_task_id.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "occurrence identity requires a scheduler_task_id".to_string(),
            ));
        }
        Ok(())
    }
}

/// Immutable run fields captured by the first delivery of an occurrence.
///
/// Later deliveries of the same slot ignore these values except for the
/// identity check performed by `create_run*`: a mismatch is a programming
/// error, not a silent retarget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceRunRequest {
    pub identity: OccurrenceIdentity,
    pub conversation_id: String,
    pub root_message_id: String,
    pub domain_profile: DomainProfile,
    pub goal: String,
    pub route: String,
    pub attended_mode: AttendedMode,
    pub execution_profile: TaskRunExecutionProfile,
}

impl OccurrenceRunRequest {
    /// Deterministic run id for the requested slot.
    pub fn run_id(&self) -> String {
        self.identity.run_id()
    }
}

impl TaskRuntimeStore {
    /// Read-only lookup of the run bound to one occurrence slot.
    ///
    /// This path never changes run status and never triggers a resume; it only
    /// projects the already-committed run.
    pub fn find_occurrence_run(
        &self,
        identity: &OccurrenceIdentity,
    ) -> Result<Option<TaskRun>, StoreError> {
        identity.validate()?;
        self.get_run(&identity.run_id())
    }

    /// Idempotent occurrence admission: return the run already bound to the
    /// slot, or atomically create exactly one `Pending` run.
    ///
    /// `created` is `true` only for the delivery that committed `RunCreated`.
    /// Re-delivery of the same slot (owner-loss retry, duplicate scheduler
    /// delivery) returns `(existing_run, false)` with the run untouched: no
    /// status change, no resume, no event. Immutable run fields are validated
    /// against the existing run, so a slot can never be silently retargeted.
    pub fn get_or_create_occurrence_run(
        &self,
        request: &OccurrenceRunRequest,
    ) -> Result<(TaskRun, bool), StoreError> {
        request.identity.validate()?;
        let run_id = request.identity.run_id();
        // `create_run_with_profile_result` takes the run lock itself; do not
        // hold it here (the lock is not reentrant).
        self.create_run_with_profile_result(
            &run_id,
            &request.identity.workspace_id,
            &request.conversation_id,
            &request.root_message_id,
            request.domain_profile,
            &request.goal,
            &request.route,
            request.attended_mode,
            request.execution_profile,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{RunPlanPolicy, TaskRunStatus};
    use super::*;
    use chrono::TimeZone;

    fn identity(scheduled_at: DateTime<Utc>) -> OccurrenceIdentity {
        OccurrenceIdentity {
            workspace_id: "ws".to_string(),
            occurrence_id: "cron-scheduled:abc".to_string(),
            scheduler_task_id: "task-1".to_string(),
            scheduled_at,
        }
    }

    fn slot() -> DateTime<Utc> {
        match Utc.with_ymd_and_hms(2026, 10, 22, 3, 0, 0) {
            chrono::LocalResult::Single(value) => value,
            _ => Utc::now(),
        }
    }

    #[test]
    fn run_id_is_stable_for_one_slot_and_filesystem_safe() {
        let first = identity(slot());
        let second = first.clone();
        assert_eq!(first.run_id(), second.run_id());
        assert!(first.run_id().starts_with("occ-"));
        let run_id = first.run_id();
        assert!(!run_id.contains('/'));
        assert!(!run_id.contains('.'));
        assert_eq!(run_id.len(), "occ-".len().saturating_add(64));
    }

    #[test]
    fn delivery_attempt_fields_do_not_change_the_slot_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        // The identity has no attempt/delivery fields at all: reconstructing
        // it from the same slot always yields the same run id.
        let identity = identity(slot());
        let json = serde_json::to_value(&identity)?;
        let restored: OccurrenceIdentity = serde_json::from_value(json.clone())?;
        assert_eq!(restored, identity);
        assert_eq!(restored.run_id(), identity.run_id());
        assert!(json.get("attempt_id").is_none());
        assert!(json.get("occurrence_id").is_some());
        Ok(())
    }

    #[test]
    fn different_slots_produce_different_runs() {
        let base = identity(slot());
        let other_slot = identity(slot() + chrono::Duration::minutes(1));
        let other_task = OccurrenceIdentity {
            scheduler_task_id: "task-2".to_string(),
            ..base.clone()
        };
        let other_workspace = OccurrenceIdentity {
            workspace_id: "ws-2".to_string(),
            ..base.clone()
        };
        assert_ne!(base.run_id(), other_slot.run_id());
        assert_ne!(base.run_id(), other_task.run_id());
        assert_ne!(base.run_id(), other_workspace.run_id());
    }

    #[test]
    fn identity_rejects_blank_slot_fields() {
        let blank = OccurrenceIdentity {
            occurrence_id: "  ".to_string(),
            ..identity(slot())
        };
        assert!(blank.validate().is_err());
    }

    fn request() -> OccurrenceRunRequest {
        OccurrenceRunRequest {
            identity: identity(slot()),
            conversation_id: "cron:task-1".to_string(),
            root_message_id: String::new(),
            domain_profile: DomainProfile::General,
            goal: "summarize the inbox".to_string(),
            route: "scheduled".to_string(),
            attended_mode: AttendedMode::Unattended,
            execution_profile: TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
        }
    }

    #[test]
    fn duplicate_delivery_admits_exactly_one_run() -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        let (first, created) = store.get_or_create_occurrence_run(&request)?;
        assert!(created, "first delivery must create the run");
        assert_eq!(first.run_id, request.run_id());
        assert_eq!(first.status, TaskRunStatus::Pending);

        let (second, created_again) = store.get_or_create_occurrence_run(&request)?;
        assert!(!created_again, "re-delivery must reuse the run");
        assert_eq!(second.run_id, first.run_id);
        assert_eq!(second.status, TaskRunStatus::Pending);

        let runs = store.list_runs_in(&[TaskRunStatus::Pending])?;
        assert_eq!(runs.len(), 1, "exactly one run for one occurrence slot");
        Ok(())
    }

    #[test]
    fn re_delivery_does_not_mutate_status_or_resume() -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        store.get_or_create_occurrence_run(&request)?;
        let run_id = request.run_id();
        store.transition_run(&run_id, TaskRunStatus::Running)?;
        assert!(store.request_pause(&run_id)?);
        let before = store
            .get_run_state(&run_id)?
            .ok_or("run state missing after pause")?;
        assert_eq!(before.run.status, TaskRunStatus::Paused);

        let (delivered, created) = store.get_or_create_occurrence_run(&request)?;
        assert!(!created);
        assert_eq!(delivered.status, TaskRunStatus::Paused);
        let after = store
            .get_run_state(&run_id)?
            .ok_or("run state missing after re-delivery")?;
        assert_eq!(
            after.journal_sequence, before.journal_sequence,
            "re-delivery must not append journal facts"
        );
        assert_eq!(
            store
                .find_occurrence_run(&request.identity)?
                .map(|run| run.status),
            Some(TaskRunStatus::Paused),
            "read-only lookup must not resume the run"
        );
        Ok(())
    }

    #[test]
    fn immutable_identity_mismatch_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let store = TaskRuntimeStore::new_in_memory()?;
        let request = request();
        store.get_or_create_occurrence_run(&request)?;
        let conflicting = OccurrenceRunRequest {
            conversation_id: "cron:other".to_string(),
            ..request
        };
        assert!(store.get_or_create_occurrence_run(&conflicting).is_err());
        Ok(())
    }
}
