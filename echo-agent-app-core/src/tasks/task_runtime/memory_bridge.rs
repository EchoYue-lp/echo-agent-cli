//! Memory bridge — sinks TaskRuntime completion events into long-term memory.
//!
//! Per the plan (§984-1017): all long-term memory writes must go through the
//! single chokepoint `MemoryLayerManager::write_memory`. This module turns
//! run/task lifecycle events into memory candidates and writes them through
//! that API, reusing the `MemoryLayerManager` that `ReviewIntegration`
//! already constructs for the primary agent (so there is exactly one write
//! path, not a parallel one).
//!
//! What gets written:
//! - run completed (success) → a verified-fix / decision memory
//! - task failed review repeatedly → a repeated-bug-pattern memory
//! - run cancelled by user → a preference memory (what the user rejected)
//!
//! Writes are best-effort: a memory failure must never break a run. Every
//! call logs and swallows the error.

use std::sync::Arc;

use echo_agent::evolution::MemoryLayerManager;
use echo_agent::prelude::{MemoryMeta, MemorySource, MemoryType};

use super::store::TaskRuntimeStore;
use super::types::*;

/// How a run's terminal memory write should be performed (B5.1).
///
/// `execute_run` takes one of these so each caller can pick the delivery
/// guarantee that matches its UX:
/// - `None` — never write (cron / DAG / execute_plan tool: no recall closure
///   needed today; their results surface via other channels).
/// - `FireAndForget` — write in a detached task, return immediately (the two
///   GUI Tauri commands `resume_task_run` / `execute_task_run`: behavior
///   unchanged from before B5.1).
/// - `Blocking` — `await` the write before returning, so the caller knows the
///   memory is durable by the time it sees "completed" (`create_complex_task` /
///   `drive_run_async`: eliminates the recall race — a run that returns
///   Completed has its `taskrun:completed:{run_id}` memory landed before any
///   follow-up question can fire).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryPolicy {
    /// Never write a memory candidate for this run.
    None,
    /// (Default) Write in a detached `tokio::spawn`; the run returns before the
    /// write is guaranteed durable. Matches pre-B5.1 behavior.
    #[default]
    FireAndForget,
    /// `await` the write inside `execute_run` so it is durable before the run
    /// returns. Use for callers whose completion triggers a recall the user
    /// might immediately act on.
    Blocking,
}

impl MemoryPolicy {
    /// True when this policy actually writes anything (i.e. not `None`).
    pub fn writes(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// The set of events that may produce a memory candidate. Modeled as a small
/// enum rather than reusing `RuntimeEventKind` because only a few kinds are
/// memory-worthy and we want explicit, reviewable rules.
#[derive(Debug, Clone)]
pub enum MemoryEvent {
    /// A run finished successfully — record what was accomplished.
    RunCompleted { run_id: String, goal: String },
    /// A task failed review with the same fingerprint repeatedly — record
    /// the bug pattern so future runs can avoid it.
    RepeatedTaskFailure {
        run_id: String,
        task_title: String,
        fingerprint: String,
    },
    /// The user cancelled a run — record their rejection as a preference.
    RunCancelledByUser { run_id: String, goal: String },
    /// A review found an issue — record the issue class for future avoidance (plan §991).
    ReviewFoundIssue {
        run_id: String,
        task_title: String,
        issue_category: String,
        issue_message: String,
    },
    /// The user corrected or rejected an approval — record as a preference (plan §993).
    UserCorrection {
        run_id: String,
        conversation_id: String,
        correction: String,
    },
    /// An approval was rejected — record what was rejected (plan §994).
    ApprovalRejected {
        run_id: String,
        tool_name: String,
        reason: String,
    },
}

/// Write a memory candidate through the canonical `MemoryLayerManager`.
/// Fire-and-forget: writes the memory candidate in a detached task so a slow
/// MemoryLayerManager (embedding, IO, network) does NOT block the run's
/// completion path. No-op (logged) if `layer_manager` is `None`.
pub fn write_memory_candidate(
    layer_manager: Option<&Arc<MemoryLayerManager>>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    let Some(lm) = layer_manager else {
        tracing::debug!(event = ?event, "memory layer manager unavailable; skipping memory write");
        return;
    };
    let lm = lm.clone();
    let store = store.clone();
    tokio::spawn(async move {
        write_memory_candidate_inner(&lm, &store, event).await;
    });
}

/// Blocking variant of [`write_memory_candidate`] (B5.1): `await`s the write so
/// the caller knows the memory is durable by the time it returns. Used by
/// `MemoryPolicy::Blocking` callers (`create_complex_task` /
/// `drive_run_async`) to eliminate the recall race — a Completed run has its
/// memory landed before any follow-up question can fire.
///
/// No-op (logged) if `layer_manager` is `None`, matching the fire-and-forget
/// variant. Best-effort: a write failure is logged and swallowed (a memory
/// failure must never break a run).
pub async fn write_memory_candidate_blocking(
    layer_manager: Option<&Arc<MemoryLayerManager>>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    let Some(lm) = layer_manager else {
        tracing::debug!(event = ?event, "memory layer manager unavailable; skipping blocking memory write");
        return;
    };
    write_memory_candidate_inner(lm, store, event).await;
}

/// Dispatch a memory candidate write according to [`MemoryPolicy`] (B5.1).
/// Used by `execute_run`'s terminal branches so each caller's delivery
/// guarantee is honored from one place:
/// - `None` → no write (return immediately).
/// - `FireAndForget` → spawn + return (the original `write_memory_candidate`).
/// - `Blocking` → `await` (`write_memory_candidate_blocking`).
///
/// `layer_manager == None` short-circuits to a no-op regardless of policy (a
/// caller without a memory layer can't write — e.g. the autonomous path before
/// B5.1 wired `layer_manager` into `RunPayload`).
pub async fn write_memory_candidate_dispatch(
    policy: MemoryPolicy,
    layer_manager: Option<&Arc<MemoryLayerManager>>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    match policy {
        MemoryPolicy::None => {}
        MemoryPolicy::FireAndForget => {
            write_memory_candidate(layer_manager, store, event);
        }
        MemoryPolicy::Blocking => {
            write_memory_candidate_blocking(layer_manager, store, event).await;
        }
    }
}

async fn write_memory_candidate_inner(
    lm: &Arc<MemoryLayerManager>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    let candidates = build_candidates(store, &event).await;
    for candidate in candidates {
        let category = candidate.category.clone();
        let meta = MemoryMeta::new(candidate.memory_type, candidate.source, candidate.category)
            .with_confidence(candidate.confidence);

        match lm
            .write_memory(&candidate.key, &candidate.content, meta)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    key = %candidate.key,
                    category = %category,
                    "memory candidate written from TaskRuntime event"
                );
            }
            Err(e) => {
                tracing::warn!(
                    key = %candidate.key,
                    error = %e,
                    "memory write failed (non-fatal); skipping"
                );
            }
        }
    }
}

struct MemoryCandidate {
    key: String,
    content: String,
    memory_type: MemoryType,
    source: MemorySource,
    category: String,
    confidence: f32,
}

/// Build one or more memory candidates from an event. Pulls extra context
/// (artifacts, todos) from the store so the memory is grounded in what
/// actually happened, not just the event payload.
async fn build_candidates(
    store: &Arc<TaskRuntimeStore>,
    event: &MemoryEvent,
) -> Vec<MemoryCandidate> {
    match event {
        MemoryEvent::RunCompleted { run_id, goal } => {
            // Summarize completed todos into a decision/fix memory.
            let todos = store.list_todos(run_id).unwrap_or_default();
            let completed: Vec<&TodoItem> = todos
                .iter()
                .filter(|t| t.status == TodoStatus::Completed)
                .collect();
            if completed.is_empty() {
                return Vec::new();
            }
            let body = completed
                .iter()
                .map(|t| {
                    let summary = t.summary.as_deref().unwrap_or("(no summary)");
                    format!("- {}: {}", t.title, summary)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let content = format!("Completed complex task.\nGoal: {goal}\nAccomplished:\n{body}");
            vec![MemoryCandidate {
                key: format!("taskrun:completed:{run_id}"),
                content,
                memory_type: MemoryType::ArchitectureDecision, // verified fix / decision
                source: MemorySource::AutoExtracted,
                category: "task_completion".to_string(),
                confidence: 0.8,
            }]
        }
        MemoryEvent::RepeatedTaskFailure {
            run_id,
            task_title,
            fingerprint,
        } => {
            let content = format!(
                "Repeated task failure pattern.\nTask: {task_title}\nRun: {run_id}\n\
                 Failure fingerprint: {fingerprint}\n\
                 This bug pattern recurred across review retries — consider it a \
                 known pitfall for similar future work."
            );
            vec![MemoryCandidate {
                key: format!("taskrun:failure:{fingerprint}"),
                content,
                memory_type: MemoryType::DebuggingLesson,
                source: MemorySource::ErrorResolution,
                category: "repeated_failure".to_string(),
                confidence: 0.7,
            }]
        }
        MemoryEvent::RunCancelledByUser { run_id, goal } => {
            let content = format!(
                "User cancelled a task run.\nGoal: {goal}\nRun: {run_id}\n\
                 The user chose not to proceed with this goal — treat as a \
                 preference signal for similar requests."
            );
            vec![MemoryCandidate {
                key: format!("taskrun:cancelled:{run_id}"),
                content,
                memory_type: MemoryType::UserPreference,
                source: MemorySource::UserCorrection,
                category: "user_rejection".to_string(),
                confidence: 0.6,
            }]
        }
        MemoryEvent::ReviewFoundIssue {
            run_id,
            task_title,
            issue_category,
            issue_message,
        } => {
            vec![MemoryCandidate {
                key: format!("taskrun:review_issue:{issue_category}"),
                content: format!(
                    "Review found issue.\nTask: {task_title}\nRun: {run_id}\n\
                     Category: {issue_category}\nIssue: {issue_message}\n\
                     This issue class appeared in review — watch for it in similar future work."
                ),
                memory_type: MemoryType::DebuggingLesson,
                source: MemorySource::AutoExtracted,
                category: format!("review_issue:{issue_category}"),
                confidence: 0.7,
            }]
        }
        MemoryEvent::UserCorrection {
            run_id,
            conversation_id,
            correction,
        } => {
            vec![MemoryCandidate {
                key: format!("taskrun:user_correction:{run_id}"),
                content: format!(
                    "User correction during task.\nRun: {run_id}\nConversation: {conversation_id}\n\
                     Correction: {correction}\n\
                     Treat as a preference signal for future similar work."
                ),
                memory_type: MemoryType::UserPreference,
                source: MemorySource::UserCorrection,
                category: "user_correction".to_string(),
                confidence: 0.8,
            }]
        }
        MemoryEvent::ApprovalRejected {
            run_id,
            tool_name,
            reason,
        } => {
            vec![MemoryCandidate {
                key: format!("taskrun:approval_rejected:{run_id}:{tool_name}"),
                content: format!(
                    "User rejected an approval.\nRun: {run_id}\nTool: {tool_name}\nReason: {reason}\n\
                     The user chose not to allow this — respect in future requests."
                ),
                memory_type: MemoryType::UserPreference,
                source: MemorySource::UserCorrection,
                category: "approval_rejected".to_string(),
                confidence: 0.7,
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_store() -> Arc<TaskRuntimeStore> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "Review runtime",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::AiCoding,
            goal: "Review runtime".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review chat.rs".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        store.attach_plan(&plan).unwrap();
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("code_reviewer"),
                Some("found gap"),
            )
            .unwrap();
        store
    }

    #[tokio::test]
    async fn build_candidates_for_completed_run_summarizes_todos() {
        let store = seeded_store();
        let cands = build_candidates(
            &store,
            &MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "Review runtime".into(),
            },
        )
        .await;
        assert_eq!(cands.len(), 1);
        assert!(cands[0].content.contains("Review chat.rs"));
        assert!(cands[0].content.contains("found gap"));
        assert!(cands[0].key.starts_with("taskrun:completed:"));
        assert_eq!(cands[0].source, MemorySource::AutoExtracted);
    }

    #[tokio::test]
    async fn build_candidates_for_repeated_failure_carries_fingerprint() {
        let store = seeded_store();
        let cands = build_candidates(
            &store,
            &MemoryEvent::RepeatedTaskFailure {
                run_id: "r1".into(),
                task_title: "Apply fix".into(),
                fingerprint: "missing-test".into(),
            },
        )
        .await;
        assert_eq!(cands.len(), 1);
        assert!(cands[0].content.contains("missing-test"));
        assert_eq!(cands[0].memory_type, MemoryType::DebuggingLesson);
        assert_eq!(cands[0].source, MemorySource::ErrorResolution);
    }

    #[tokio::test]
    async fn build_candidates_for_cancelled_run_is_a_preference() {
        let store = seeded_store();
        let cands = build_candidates(
            &store,
            &MemoryEvent::RunCancelledByUser {
                run_id: "r1".into(),
                goal: "Refactor everything".into(),
            },
        )
        .await;
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].memory_type, MemoryType::UserPreference);
        assert_eq!(cands[0].source, MemorySource::UserCorrection);
    }

    #[tokio::test]
    async fn write_memory_candidate_is_a_noop_without_layer_manager() {
        let store = seeded_store();
        // None for layer_manager → no panic, no error. Now sync (fire-and-forget).
        write_memory_candidate(
            None,
            &store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "x".into(),
            },
        );
    }

    #[tokio::test]
    async fn memory_policy_writes_predicate() {
        // B5.1: writes() is the quick guard callers can use to skip building an
        // event entirely when the policy is None.
        assert!(!MemoryPolicy::None.writes());
        assert!(MemoryPolicy::FireAndForget.writes());
        assert!(MemoryPolicy::Blocking.writes());
    }

    #[tokio::test]
    async fn dispatch_none_policy_is_a_noop_even_with_no_layer_manager() {
        // B5.1: MemoryPolicy::None short-circuits before touching the layer
        // manager, so a None layer_manager is fine (no panic, returns fast).
        let store = seeded_store();
        write_memory_candidate_dispatch(
            MemoryPolicy::None,
            None,
            &store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "x".into(),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_blocking_with_no_layer_manager_returns_without_panic() {
        // B5.1: Blocking + None layer manager → no-op (a caller without a memory
        // subsystem can't write). Must NOT block forever or panic — this is the
        // autonomous path's fallback when no layer manager is wired (TUI/channel).
        let store = seeded_store();
        write_memory_candidate_dispatch(
            MemoryPolicy::Blocking,
            None,
            &store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "x".into(),
            },
        )
        .await;
    }
}
