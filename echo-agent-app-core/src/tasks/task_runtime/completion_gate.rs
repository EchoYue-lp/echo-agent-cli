//! EKO's single Requirement/Evidence completion projection.
//!
//! The framework owns the revisioned task graph. This module owns only EKO's
//! product rule for deciding whether the current Goal has enough durable,
//! verifiable evidence to complete.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::review::requires_review;
use super::store::{StoreError, TaskRuntimeStore};
use super::types::*;

pub fn requirements_for_plan(plan: &TaskPlan) -> Vec<GoalRequirement> {
    plan.tasks
        .iter()
        .map(|task| {
            requirement_from_parts(
                plan.goal_revision,
                plan.revision,
                &task.id,
                &task.title,
                &task.description,
                task.kind,
                &task.required_artifacts,
                &task.execution_checks,
                &task.acceptance_criteria,
            )
        })
        .collect()
}

pub(crate) fn requirements_for_revision(plan: &PlanRevision) -> Vec<GoalRequirement> {
    plan.tasks
        .iter()
        .map(|task| {
            requirement_from_parts(
                plan.goal_revision,
                plan.revision,
                &task.id,
                &task.title,
                &task.description,
                task.kind,
                &task.required_artifacts,
                &task.execution_checks,
                &task.acceptance_criteria,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn requirement_from_parts(
    goal_revision: u64,
    plan_revision: u64,
    task_id: &str,
    title: &str,
    description: &str,
    kind: PlanTaskKind,
    required_artifacts: &[String],
    execution_checks: &[String],
    acceptance_criteria: &[String],
) -> GoalRequirement {
    let identity_sha = sha256_bytes(task_id.as_bytes());
    let requirement_id = format!("req:{}", identity_sha.chars().take(24).collect::<String>());
    let content = serde_json::to_vec(&serde_json::json!({
        "task_id": task_id,
        "title": title,
        "description": description,
        "kind": kind.as_str(),
        "required_artifacts": required_artifacts,
        "execution_checks": execution_checks,
        "acceptance_criteria": acceptance_criteria,
    }))
    .unwrap_or_default();
    GoalRequirement {
        requirement_id,
        goal_revision,
        plan_revision,
        task_id: task_id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        requirement_sha256: sha256_bytes(&content),
        required_artifacts: required_artifacts.to_vec(),
        execution_checks: execution_checks.to_vec(),
        acceptance_criteria: acceptance_criteria.to_vec(),
    }
}

impl TaskRuntimeStore {
    /// Fold the current event stream into the one completion report consumed by
    /// executor, GUI, TUI, CLI, and channel adapters.
    pub fn completion_gate_report(&self, run_id: &str) -> Result<CompletionGateReport, StoreError> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let Some(plan) = self.get_plan(run_id)? else {
            return Ok(CompletionGateReport {
                run_id: run_id.to_string(),
                goal_revision: run.goal_revision,
                plan_revision: 0,
                ready: false,
                requirements: Vec::new(),
                blockers: vec![blocker(
                    CompletionBlockerCode::NoPlan,
                    None,
                    None,
                    "run has no committed plan",
                )],
            });
        };
        // Audit allowlist: completion reviews all Requirement/Evidence and
        // artifact history; the hot runtime snapshot intentionally omits it.
        let events = self.list_events(run_id, 0)?;
        let requirements = requirements_for_plan(&plan);
        let mut blockers = Vec::new();
        let mut assessments = Vec::with_capacity(requirements.len());

        if plan.goal_revision != run.goal_revision || plan.goal_sha256 != run.goal_sha256 {
            blockers.push(blocker(
                CompletionBlockerCode::PlanGoalMismatch,
                None,
                None,
                format!(
                    "plan revision {} targets Goal revision {}, current Goal revision is {}",
                    plan.revision, plan.goal_revision, run.goal_revision
                ),
            ));
        }
        if requirements.is_empty() {
            blockers.push(blocker(
                CompletionBlockerCode::EmptyPlan,
                None,
                None,
                "run plan has no Goal requirements",
            ));
        }

        for requirement in requirements {
            let Some(task) = plan
                .tasks
                .iter()
                .find(|task| task.id == requirement.task_id)
            else {
                blockers.push(blocker(
                    CompletionBlockerCode::RequirementUncovered,
                    Some(&requirement),
                    None,
                    "requirement is not linked to a PlanTask",
                ));
                assessments.push(RequirementAssessment {
                    requirement,
                    status: RequirementStatus::Pending,
                    evidence: Vec::new(),
                });
                continue;
            };
            assessments.push(assess_requirement(
                &run,
                &plan,
                task,
                requirement,
                &events,
                &mut blockers,
            ));
        }

        match self.active_subagent_boundaries(run_id) {
            Ok(active) => {
                for boundary in active {
                    blockers.push(blocker(
                        CompletionBlockerCode::ActiveSubagent,
                        None,
                        Some(&boundary.task_id),
                        format!(
                            "Subagent execution '{}' is still active",
                            boundary.execution_id
                        ),
                    ));
                }
            }
            Err(error) => blockers.push(blocker(
                CompletionBlockerCode::StoreReadFailed,
                None,
                None,
                format!("active Subagent boundaries could not be read: {error}"),
            )),
        }
        match self.list_background_cells(run_id) {
            Ok(cells) => {
                for cell in cells.into_iter().filter(BackgroundCellState::is_active) {
                    blockers.push(blocker(
                        CompletionBlockerCode::ActiveCommandCell,
                        None,
                        None,
                        format!(
                            "command cell '{}' ({}) is still active",
                            cell.name, cell.cell_id
                        ),
                    ));
                }
            }
            Err(error) => blockers.push(blocker(
                CompletionBlockerCode::StoreReadFailed,
                None,
                None,
                format!("command cells could not be read: {error}"),
            )),
        }
        match self.list_recovery_blockers(run_id) {
            Ok(recovery) => {
                for item in recovery {
                    blockers.push(blocker(
                        CompletionBlockerCode::RecoveryBlocker,
                        None,
                        Some(&item.task_id),
                        item.reason,
                    ));
                }
            }
            Err(error) => blockers.push(blocker(
                CompletionBlockerCode::StoreReadFailed,
                None,
                None,
                format!("recovery blockers could not be read: {error}"),
            )),
        }

        Ok(CompletionGateReport {
            run_id: run_id.to_string(),
            goal_revision: run.goal_revision,
            plan_revision: plan.revision,
            ready: blockers.is_empty(),
            requirements: assessments,
            blockers,
        })
    }
}

fn assess_requirement(
    run: &TaskRun,
    plan: &TaskPlan,
    task: &PlanTask,
    requirement: GoalRequirement,
    events: &[RuntimeTaskEvent],
    blockers: &mut Vec<RunCompletionBlocker>,
) -> RequirementAssessment {
    let mut evidence = Vec::new();
    if let Some(skip) = events.iter().rev().find(|event| {
        event.event_type == RuntimeEventKind::RequirementSkipped
            && event
                .payload
                .get("requirement_id")
                .and_then(|value| value.as_str())
                == Some(requirement.requirement_id.as_str())
            && event
                .payload
                .get("requirement_sha256")
                .and_then(|value| value.as_str())
                == Some(requirement.requirement_sha256.as_str())
            && event
                .payload
                .get("goal_revision")
                .and_then(serde_json::Value::as_u64)
                == Some(run.goal_revision)
    }) {
        evidence.push(evidence_from_event(
            &requirement,
            RequirementEvidenceKind::UserSkip,
            RequirementEvidenceStatus::Passed,
            skip,
            EvidencePayload {
                producer_identity: skip
                    .payload
                    .get("actor_user_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                subject: "user-confirmed skip".to_string(),
                sha256: None,
                details: skip
                    .payload
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            },
        ));
        if task.status == TodoStatus::Skipped {
            return RequirementAssessment {
                requirement,
                status: RequirementStatus::Skipped,
                evidence,
            };
        }
    }

    let mut status = RequirementStatus::Accepted;
    if task.status != TodoStatus::Completed {
        blockers.push(blocker(
            if task.status == TodoStatus::Skipped {
                CompletionBlockerCode::RequirementUncovered
            } else {
                CompletionBlockerCode::TaskNotComplete
            },
            Some(&requirement),
            Some(&task.id),
            if task.status == TodoStatus::Skipped {
                "task is skipped without an explicit user-confirmed requirement Skip".to_string()
            } else {
                format!("task is {}", task.status.as_str())
            },
        ));
        status = RequirementStatus::Pending;
    }

    let Some(summary_event) = latest_summary_event(events, &task.id) else {
        blockers.push(blocker(
            CompletionBlockerCode::RequirementEvidenceMissing,
            Some(&requirement),
            Some(&task.id),
            "task has no structured execution result",
        ));
        return RequirementAssessment {
            requirement,
            status: RequirementStatus::Pending,
            evidence,
        };
    };
    let Some(summary) = summary_event
        .payload
        .get("summary")
        .cloned()
        .and_then(|value| serde_json::from_value::<TaskExecutionSummary>(value).ok())
    else {
        blockers.push(blocker(
            CompletionBlockerCode::RequirementEvidenceMissing,
            Some(&requirement),
            Some(&task.id),
            "task execution result is malformed",
        ));
        return RequirementAssessment {
            requirement,
            status: RequirementStatus::Failed,
            evidence,
        };
    };
    let (source_goal_revision, source_plan_revision) =
        event_binding(events, summary_event.seq).unwrap_or_default();
    let fresh = source_goal_revision == run.goal_revision
        || has_revalidation(events, summary_event.seq, &requirement, run.goal_revision);
    let execution_passed = summary.result.status == SubagentRunStatus::Completed
        && !summary.result.summary.trim().is_empty()
        && summary.result.remaining_work.is_empty();
    evidence.push(evidence_from_event(
        &requirement,
        RequirementEvidenceKind::TaskExecution,
        if !fresh {
            RequirementEvidenceStatus::Stale
        } else if execution_passed {
            RequirementEvidenceStatus::Passed
        } else {
            RequirementEvidenceStatus::Failed
        },
        summary_event,
        EvidencePayload {
            producer_identity: Some(summary.subagent_name.clone()),
            subject: "structured task execution result".to_string(),
            sha256: None,
            details: Some(format!(
                "source Goal revision {source_goal_revision}, Plan revision {source_plan_revision}"
            )),
        },
    ));
    if !fresh {
        blockers.push(blocker(
            CompletionBlockerCode::StaleEvidence,
            Some(&requirement),
            Some(&task.id),
            format!(
                "execution evidence belongs to Goal revision {source_goal_revision} and was not revalidated"
            ),
        ));
        status = RequirementStatus::Stale;
    } else if !execution_passed {
        blockers.push(blocker(
            CompletionBlockerCode::RequirementEvidenceMissing,
            Some(&requirement),
            Some(&task.id),
            "task result is not a completed, non-empty result with no remaining work",
        ));
        status = RequirementStatus::Failed;
    }

    for required in &requirement.execution_checks {
        let matched = summary.result.verification.iter().find(|item| {
            item.source == SubagentVerificationSource::Observed
                && verification_matches(required, &item.check)
        });
        let passed = matched.is_some_and(|item| item.status == SubagentVerificationStatus::Passed);
        evidence.push(evidence_from_event(
            &requirement,
            RequirementEvidenceKind::Test,
            if !fresh {
                RequirementEvidenceStatus::Stale
            } else if passed {
                RequirementEvidenceStatus::Passed
            } else {
                RequirementEvidenceStatus::Failed
            },
            summary_event,
            EvidencePayload {
                producer_identity: Some(summary.subagent_name.clone()),
                subject: required.clone(),
                sha256: None,
                details: matched.map(|item| item.details.clone()),
            },
        ));
        if fresh && !passed {
            blockers.push(blocker(
                CompletionBlockerCode::TestFailed,
                Some(&requirement),
                Some(&task.id),
                format!("required observed check did not pass: {required}"),
            ));
            status = RequirementStatus::Failed;
        }
    }

    for required in &requirement.required_artifacts {
        let artifact = summary
            .result
            .artifacts
            .iter()
            .find(|item| artifact_matches(required, &item.path));
        let verification = artifact.map(verify_artifact);
        let (artifact_status, code, detail, producer, digest) =
            match (fresh, artifact, verification) {
                (false, item, _) => (
                    RequirementEvidenceStatus::Stale,
                    None,
                    "artifact evidence is stale".to_string(),
                    item.and_then(|value| value.producer_execution_id.clone()),
                    item.and_then(|value| value.sha256.clone()),
                ),
                (true, Some(item), Some(Ok(actual))) => (
                    RequirementEvidenceStatus::Passed,
                    None,
                    "artifact exists and its SHA-256 matches".to_string(),
                    item.producer_execution_id.clone(),
                    Some(actual),
                ),
                (
                    true,
                    Some(item),
                    Some(Err(ArtifactVerificationError::HashMismatch { actual })),
                ) => (
                    RequirementEvidenceStatus::Failed,
                    Some(CompletionBlockerCode::ArtifactHashMismatch),
                    format!("recorded and current SHA-256 differ for '{}'", item.path),
                    item.producer_execution_id.clone(),
                    Some(actual),
                ),
                (true, Some(item), Some(Err(ArtifactVerificationError::Unavailable(detail)))) => (
                    RequirementEvidenceStatus::Failed,
                    Some(CompletionBlockerCode::ArtifactMissing),
                    detail,
                    item.producer_execution_id.clone(),
                    item.sha256.clone(),
                ),
                (true, None, _) => (
                    RequirementEvidenceStatus::Failed,
                    Some(CompletionBlockerCode::ArtifactMissing),
                    format!("required artifact was not reported: {required}"),
                    None,
                    None,
                ),
                (true, Some(_), None) => (
                    RequirementEvidenceStatus::Failed,
                    Some(CompletionBlockerCode::ArtifactMissing),
                    format!("required artifact could not be verified: {required}"),
                    None,
                    None,
                ),
            };
        evidence.push(evidence_from_event(
            &requirement,
            RequirementEvidenceKind::Artifact,
            artifact_status,
            summary_event,
            EvidencePayload {
                producer_identity: producer,
                subject: required.clone(),
                sha256: digest,
                details: Some(detail.clone()),
            },
        ));
        if let Some(code) = code {
            blockers.push(blocker(code, Some(&requirement), Some(&task.id), detail));
            status = RequirementStatus::Failed;
        }
    }

    if requires_review(task) {
        let review = events.iter().rev().find(|event| {
            event.seq > summary_event.seq
                && event.task_id.as_deref() == Some(task.id.as_str())
                && matches!(
                    event.event_type,
                    RuntimeEventKind::ReviewPassed
                        | RuntimeEventKind::ReviewNeedsFix
                        | RuntimeEventKind::ReviewBlocked
                )
        });
        if let Some(review) = review {
            let review_fresh = event_binding(events, review.seq)
                .is_some_and(|binding| binding.0 == run.goal_revision)
                || has_revalidation(events, review.seq, &requirement, run.goal_revision);
            let passed = review.event_type == RuntimeEventKind::ReviewPassed;
            evidence.push(evidence_from_event(
                &requirement,
                RequirementEvidenceKind::Review,
                if !review_fresh {
                    RequirementEvidenceStatus::Stale
                } else if passed {
                    RequirementEvidenceStatus::Passed
                } else {
                    RequirementEvidenceStatus::Failed
                },
                review,
                EvidencePayload {
                    producer_identity: review
                        .payload
                        .get("reviewer")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    subject: "semantic acceptance review".to_string(),
                    sha256: None,
                    details: review
                        .payload
                        .get("outcome")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                },
            ));
            if !review_fresh {
                blockers.push(blocker(
                    CompletionBlockerCode::StaleEvidence,
                    Some(&requirement),
                    Some(&task.id),
                    "semantic review belongs to an older Goal revision",
                ));
                status = RequirementStatus::Stale;
            } else if !passed {
                blockers.push(blocker(
                    CompletionBlockerCode::ReviewFailed,
                    Some(&requirement),
                    Some(&task.id),
                    "latest semantic review did not pass",
                ));
                status = RequirementStatus::Failed;
            }
        } else {
            blockers.push(blocker(
                CompletionBlockerCode::ReviewMissing,
                Some(&requirement),
                Some(&task.id),
                "required semantic review is missing",
            ));
            status = RequirementStatus::Pending;
        }
    }

    if status == RequirementStatus::Accepted
        && evidence
            .iter()
            .any(|item| item.status != RequirementEvidenceStatus::Passed)
    {
        status = RequirementStatus::Failed;
    }
    if plan.goal_revision != run.goal_revision && status == RequirementStatus::Accepted {
        status = RequirementStatus::Stale;
    }
    RequirementAssessment {
        requirement,
        status,
        evidence,
    }
}

fn latest_summary_event<'a>(
    events: &'a [RuntimeTaskEvent],
    task_id: &str,
) -> Option<&'a RuntimeTaskEvent> {
    events.iter().rev().find(|event| {
        event.event_type == RuntimeEventKind::Note
            && event.task_id.as_deref() == Some(task_id)
            && event.payload.get("kind").and_then(|value| value.as_str())
                == Some("summary_persisted")
    })
}

fn event_binding(events: &[RuntimeTaskEvent], through_seq: i64) -> Option<(u64, u64)> {
    events
        .iter()
        .rev()
        .filter(|event| event.seq <= through_seq)
        .find_map(|event| {
            if event.event_type != RuntimeEventKind::PlanRevisionCommitted {
                return None;
            }
            event
                .payload
                .get("plan")
                .cloned()
                .and_then(|value| serde_json::from_value::<PlanRevision>(value).ok())
                .map(|plan| (plan.goal_revision, plan.revision))
        })
}

fn has_revalidation(
    events: &[RuntimeTaskEvent],
    source_seq: i64,
    requirement: &GoalRequirement,
    current_goal_revision: u64,
) -> bool {
    events.iter().any(|event| {
        event.seq > source_seq
            && event.event_type == RuntimeEventKind::RequirementEvidenceRevalidated
            && event
                .payload
                .get("requirement_id")
                .and_then(|value| value.as_str())
                == Some(requirement.requirement_id.as_str())
            && event
                .payload
                .get("requirement_sha256")
                .and_then(|value| value.as_str())
                == Some(requirement.requirement_sha256.as_str())
            && event
                .payload
                .get("new_goal_revision")
                .and_then(serde_json::Value::as_u64)
                == Some(current_goal_revision)
    })
}

struct EvidencePayload {
    producer_identity: Option<String>,
    subject: String,
    sha256: Option<String>,
    details: Option<String>,
}

fn evidence_from_event(
    requirement: &GoalRequirement,
    kind: RequirementEvidenceKind,
    status: RequirementEvidenceStatus,
    event: &RuntimeTaskEvent,
    payload: EvidencePayload,
) -> RequirementEvidence {
    let identity = format!(
        "{}:{}:{kind:?}:{}:{}",
        requirement.requirement_id, event.seq, payload.subject, requirement.requirement_sha256
    );
    RequirementEvidence {
        evidence_id: format!(
            "ev:{}",
            sha256_bytes(identity.as_bytes())
                .chars()
                .take(24)
                .collect::<String>()
        ),
        requirement_id: requirement.requirement_id.clone(),
        goal_revision: requirement.goal_revision,
        plan_revision: requirement.plan_revision,
        task_id: requirement.task_id.clone(),
        kind,
        source_event_seq: event.seq.to_string(),
        status,
        producer_identity: payload.producer_identity,
        subject: payload.subject,
        sha256: payload.sha256,
        details: payload.details,
    }
}

fn blocker(
    code: CompletionBlockerCode,
    requirement: Option<&GoalRequirement>,
    task_id: Option<&str>,
    detail: impl Into<String>,
) -> RunCompletionBlocker {
    RunCompletionBlocker {
        code,
        requirement_id: requirement.map(|item| item.requirement_id.clone()),
        task_id: task_id
            .map(str::to_string)
            .or_else(|| requirement.map(|item| item.task_id.clone())),
        detail: detail.into(),
    }
}

#[derive(Debug)]
enum ArtifactVerificationError {
    HashMismatch { actual: String },
    Unavailable(String),
}

fn verify_artifact(artifact: &SubagentArtifactResult) -> Result<String, ArtifactVerificationError> {
    if !artifact.available {
        return Err(ArtifactVerificationError::Unavailable(format!(
            "artifact was reported unavailable: {}",
            artifact.path
        )));
    }
    let Some(recorded) = artifact.sha256.as_deref() else {
        return Err(ArtifactVerificationError::Unavailable(format!(
            "artifact has no recorded SHA-256: {}",
            artifact.path
        )));
    };
    if recorded.chars().count() != 64 {
        return Err(ArtifactVerificationError::Unavailable(format!(
            "artifact has an invalid recorded SHA-256: {}",
            artifact.path
        )));
    }
    if artifact
        .producer_execution_id
        .as_deref()
        .is_none_or(|identity| identity.trim().is_empty())
    {
        return Err(ArtifactVerificationError::Unavailable(format!(
            "artifact has no producer identity: {}",
            artifact.path
        )));
    }
    let actual = sha256_file(Path::new(&artifact.path)).map_err(|error| {
        ArtifactVerificationError::Unavailable(format!(
            "artifact '{}' could not be read: {error}",
            artifact.path
        ))
    })?;
    if !actual.eq_ignore_ascii_case(recorded) {
        return Err(ArtifactVerificationError::HashMismatch { actual });
    }
    Ok(actual)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..read) {
            digest.update(chunk);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn verification_matches(required: &str, observed: &str) -> bool {
    let required = required.split_whitespace().collect::<Vec<_>>().join(" ");
    let observed = observed.split_whitespace().collect::<Vec<_>>().join(" ");
    !required.is_empty() && required.eq_ignore_ascii_case(&observed)
}

pub(crate) fn artifact_matches(required: &str, actual: &str) -> bool {
    let required = required.trim().replace('\\', "/");
    let actual = actual.trim().replace('\\', "/");
    !required.is_empty()
        && (actual == required
            || actual.ends_with(&format!("/{required}"))
            || Path::new(&actual)
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == required))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn fixture(
        artifact_bytes: &[u8],
    ) -> Result<(tempfile::TempDir, TaskRuntimeStore, String), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let artifact_path = temp.path().join("result.txt");
        std::fs::write(&artifact_path, artifact_bytes)?;
        let artifact_path = artifact_path.to_string_lossy().to_string();
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("shadow"))?;
        let run = store.create_run(
            "run",
            "workspace",
            "conversation",
            "root",
            DomainProfile::General,
            "produce verified output",
            "test",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "plan".to_string(),
            run_id: run.run_id,
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("produce verified output"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "task".to_string(),
                title: "Produce output".to_string(),
                description: "Write and verify the output".to_string(),
                required_artifacts: vec![artifact_path.clone()],
                execution_checks: vec!["cargo test -p sample".to_string()],
                ..PlanTask::default()
            }],
        })?;
        Ok((temp, store, artifact_path))
    }

    fn persist_passed_evidence(
        store: &TaskRuntimeStore,
        artifact_path: &str,
        artifact_bytes: &[u8],
    ) -> Result<(), StoreError> {
        store.put_summary(&TaskExecutionSummary {
            run_id: "run".to_string(),
            task_id: "task".to_string(),
            subagent_name: "test_subagent".to_string(),
            result: SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "output produced".to_string(),
                artifacts: vec![SubagentArtifactResult {
                    path: artifact_path.to_string(),
                    kind: "file".to_string(),
                    bytes: None,
                    sha256: Some(sha256_bytes(artifact_bytes)),
                    producer_execution_id: Some("execution-1".to_string()),
                    available: true,
                }],
                evidence: Vec::new(),
                verification: vec![SubagentVerificationResult {
                    check: "cargo   test -p sample".to_string(),
                    status: SubagentVerificationStatus::Passed,
                    details: "passed".to_string(),
                    source: SubagentVerificationSource::Observed,
                }],
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        })?;
        store.set_task_status("run", "task", TodoStatus::Completed, None, Some("verified"))
    }

    #[test]
    fn completion_gate_rehashes_required_artifacts() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = b"verified output";
        let (_temp, store, artifact_path) = fixture(bytes)?;
        persist_passed_evidence(&store, &artifact_path, bytes)?;
        assert!(store.completion_gate_report("run")?.ready);

        std::fs::write(&artifact_path, b"modified after verification")?;
        let report = store.completion_gate_report("run")?;
        assert!(!report.ready);
        assert!(report.blockers.iter().any(|item| {
            item.code == CompletionBlockerCode::ArtifactHashMismatch
                && item.task_id.as_deref() == Some("task")
        }));
        Ok(())
    }

    #[test]
    fn unchanged_requirement_is_revalidated_after_goal_update()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = b"verified output";
        let (_temp, store, artifact_path) = fixture(bytes)?;
        persist_passed_evidence(&store, &artifact_path, bytes)?;
        store.transition_run("run", TaskRunStatus::Running)?;
        store.transition_run("run", TaskRunStatus::Paused)?;
        store.update_run_goal(
            "run",
            1,
            "produce verified output with clarified wording",
            "clarify wording only",
            RunGoalActorSource::Tui,
        )?;
        let stale = store.completion_gate_report("run")?;
        assert!(stale.blockers.iter().any(|item| {
            matches!(
                item.code,
                CompletionBlockerCode::PlanGoalMismatch | CompletionBlockerCode::StaleEvidence
            )
        }));

        let rebound = store.apply_task_patch_for_test(
            "run",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "bind unchanged requirement to revised Goal".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("task".to_string()),
                    task: PlanTask {
                        id: "goal-binding-review".to_string(),
                        title: "Review clarified Goal".to_string(),
                        description: "Check the clarified wording".to_string(),
                        depends_on: vec!["task".to_string()],
                        sort_order: 1,
                        ..PlanTask::default()
                    }
                    .spec(),
                }],
            },
        )?;
        assert_eq!(rebound.goal_revision, 2);
        let report = store.completion_gate_report("run")?;
        assert_eq!(
            report
                .requirements
                .iter()
                .find(|item| item.requirement.task_id == "task")
                .map(|item| item.status),
            Some(RequirementStatus::Accepted)
        );
        assert!(
            store.list_events("run", 0)?.iter().any(|event| {
                event.event_type == RuntimeEventKind::RequirementEvidenceRevalidated
            })
        );
        Ok(())
    }

    #[test]
    fn skipped_task_needs_explicit_user_requirement_skip() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp, store, _artifact_path) = fixture(b"unused")?;
        let plan = store.apply_task_patch_for_test(
            "run",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "user chose not to execute this task".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "task".to_string(),
                }],
            },
        )?;
        let requirement = requirements_for_plan(&plan)
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::InvalidPlan("fixture requirement is missing".to_string()))?;
        let before = store.completion_gate_report("run")?;
        assert!(!before.ready);
        let after = store.skip_goal_requirement(
            "run",
            1,
            &requirement.requirement_id,
            "not applicable to this Goal",
            RunGoalActorSource::Cli,
        )?;
        assert!(after.ready, "blockers: {:?}", after.blockers);
        assert_eq!(
            after.requirements.first().map(|item| item.status),
            Some(RequirementStatus::Skipped)
        );
        Ok(())
    }
}
