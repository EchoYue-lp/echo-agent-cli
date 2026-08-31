//! Memory bridge — sinks TaskRuntime completion events into long-term memory.
//!
//! Per the plan (§984-1017): all long-term memory writes must go through the
//! single chokepoint `MemoryLayerManager::write_memory`. This module turns
//! run/task lifecycle events into memory candidates and writes them through
//! that API. The retained `ReviewGenerationLease` returns the generation's
//! shared manager and settles its hot-memory projection after each successful
//! mutation batch, so no caller can pair a manager with the wrong generation.
//!
//! What gets written:
//! - run completed (success) → a verified-fix / decision memory
//! - task failed review repeatedly → a repeated-bug-pattern memory
//! - run cancelled by user → a preference memory (what the user rejected)
//!
//! Writes are best-effort: a memory failure must never break a run. Every
//! call logs and swallows the error.

use std::sync::Arc;

use echo_agent::prelude::{MemoryMeta, MemorySource, MemoryType};

use super::executor::TaskRuntimeOperation;
use super::store::TaskRuntimeStore;

/// How a run's terminal memory write should be performed (B5.1).
///
/// `execute_run` takes one of these so each caller can pick the delivery
/// guarantee that matches its UX:
/// - `None` — never write (cron / DAG / task_execute tool: no recall closure
///   needed today; their results surface via other channels).
/// - `BestEffortSettled` — await the write inside the owned TaskRun driver.
///   Write errors are logged and never replace the business terminal outcome,
///   but no detached memory task can outlive driver settlement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryPolicy {
    /// Never write a memory candidate for this run.
    None,
    /// (Default) Await best-effort memory IO before the driver settles.
    #[default]
    BestEffortSettled,
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
}

/// Await a memory candidate through the canonical `MemoryLayerManager` while
/// the caller's generation receipt remains alive. This is best-effort: write
/// failures are logged and swallowed, so memory cannot replace the TaskRun's
/// business terminal outcome.
pub async fn write_memory_candidate_settled(
    memory_generation: Option<&crate::evolution::ReviewGenerationLease>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    let Some(memory_generation) = memory_generation else {
        tracing::debug!(event = ?event, "memory generation unavailable; skipping settled memory write");
        return;
    };
    let layer_manager = match memory_generation.layer_manager() {
        Ok(manager) => manager,
        Err(error) => {
            tracing::warn!(%error, "generation-bound memory manager unavailable");
            return;
        }
    };
    write_memory_candidate_inner(&layer_manager, memory_generation, store, event).await;
}

/// Dispatch a memory candidate write according to [`MemoryPolicy`] (B5.1).
/// Used by `execute_run`'s terminal branches so each caller's delivery
/// guarantee is honored from one place:
/// - `None` → no write (return immediately).
/// - `BestEffortSettled` → await IO, while swallowing/logging memory errors.
///
/// A missing generation lease short-circuits to a no-op regardless of policy.
/// The manager is always resolved from that lease.
pub async fn write_memory_candidate_dispatch(
    policy: MemoryPolicy,
    memory_generation: Option<&crate::evolution::ReviewGenerationLease>,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    match policy {
        MemoryPolicy::None => {}
        MemoryPolicy::BestEffortSettled => {
            write_memory_candidate_settled(memory_generation, store, event).await;
        }
    }
}

async fn write_memory_candidate_inner(
    layer_manager: &echo_agent::evolution::MemoryLayerManager,
    memory_generation: &crate::evolution::ReviewGenerationLease,
    store: &Arc<TaskRuntimeStore>,
    event: MemoryEvent,
) {
    let candidates = match build_candidates(store, &event).await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(%error, "TaskRuntime memory candidate context unavailable");
            return;
        }
    };
    let mut mutated = false;
    for candidate in candidates {
        let category = candidate.category.clone();
        let meta = MemoryMeta::new(candidate.memory_type, candidate.source, candidate.category)
            .with_confidence(candidate.confidence);

        match layer_manager
            .write_memory(&candidate.key, &candidate.content, meta)
            .await
        {
            Ok(_) => {
                mutated = true;
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
    if mutated {
        let receipt = memory_generation.settle_hot_memory_projection().await;
        if matches!(
            receipt.status,
            crate::evolution::review_integration::MemoryProjectionSettlementStatus::Degraded
        ) {
            tracing::warn!(
                authority_scope = %receipt.authority_scope,
                workspace_generation = %receipt.workspace_generation,
                revision = receipt.revision,
                status = ?receipt.status,
                pending_revision = ?receipt.pending_revision,
                error = ?receipt.error,
                "hot-memory projection settlement degraded"
            );
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
) -> Result<Vec<MemoryCandidate>, String> {
    match event {
        MemoryEvent::RunCompleted { run_id, goal } => {
            // Summarize canonical completed tasks into a decision/fix memory.
            let load_run_id = run_id.clone();
            let completed = TaskRuntimeOperation::new(store.clone())
                .run("load memory candidate tasks", move |store| {
                    let tasks = store
                        .get_plan(&load_run_id)?
                        .map(|plan| plan.tasks)
                        .unwrap_or_default();
                    let fallback_summaries = store
                        .list_todos(&load_run_id)?
                        .into_iter()
                        .filter_map(|todo| todo.summary.map(|summary| (todo.task_id, summary)))
                        .collect::<std::collections::HashMap<_, _>>();
                    tasks
                        .into_iter()
                        .filter(|task| task.status == echo_agent::tasks::TaskStatus::Completed)
                        .map(|task| {
                            let summary = store
                                .get_summary(&load_run_id, &task.id)?
                                .map(|summary| summary.outcome.summary)
                                .filter(|summary| !summary.trim().is_empty())
                                .or_else(|| fallback_summaries.get(&task.id).cloned())
                                .unwrap_or_else(|| "(no summary)".to_string());
                            Ok((task.title, summary))
                        })
                        .collect::<Result<Vec<_>, super::store::StoreError>>()
                })
                .await
                .map_err(|error| error.to_string())?;
            if completed.is_empty() {
                return Ok(Vec::new());
            }
            let body = completed
                .iter()
                .map(|(title, summary)| format!("- {title}: {summary}"))
                .collect::<Vec<_>>()
                .join("\n");
            let content = format!("Completed complex task.\nGoal: {goal}\nAccomplished:\n{body}");
            Ok(vec![MemoryCandidate {
                key: format!("taskrun:completed:{run_id}"),
                content,
                memory_type: MemoryType::ArchitectureDecision, // verified fix / decision
                source: MemorySource::AutoExtracted,
                category: "task_completion".to_string(),
                confidence: 0.8,
            }])
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
            Ok(vec![MemoryCandidate {
                key: format!("taskrun:failure:{fingerprint}"),
                content,
                memory_type: MemoryType::DebuggingLesson,
                source: MemorySource::ErrorResolution,
                category: "repeated_failure".to_string(),
                confidence: 0.7,
            }])
        }
        MemoryEvent::RunCancelledByUser { run_id, goal } => {
            let content = format!(
                "User cancelled a task run.\nGoal: {goal}\nRun: {run_id}\n\
                 The user chose not to proceed with this goal — treat as a \
                 preference signal for similar requests."
            );
            Ok(vec![MemoryCandidate {
                key: format!("taskrun:cancelled:{run_id}"),
                content,
                memory_type: MemoryType::UserPreference,
                source: MemorySource::UserCorrection,
                category: "user_rejection".to_string(),
                confidence: 0.6,
            }])
        }
        MemoryEvent::ReviewFoundIssue {
            run_id,
            task_title,
            issue_category,
            issue_message,
        } => Ok(vec![MemoryCandidate {
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
        }]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPlan, TaskRunStatus,
    };

    fn seeded_store() -> Result<Arc<TaskRuntimeStore>, String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
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
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("Review runtime"),
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
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Completed,
                Some("code_reviewer"),
                Some("found gap"),
            )
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    #[tokio::test]
    async fn build_candidates_for_completed_run_summarizes_todos() -> Result<(), String> {
        let store = seeded_store()?;
        let cands = build_candidates(
            &store,
            &MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "Review runtime".into(),
            },
        )
        .await?;
        assert_eq!(cands.len(), 1);
        let candidate = cands
            .first()
            .ok_or_else(|| "completed memory candidate missing".to_string())?;
        assert!(candidate.content.contains("Review chat.rs"));
        assert!(candidate.content.contains("found gap"));
        assert!(candidate.key.starts_with("taskrun:completed:"));
        assert_eq!(candidate.source, MemorySource::AutoExtracted);
        Ok(())
    }

    #[tokio::test]
    async fn build_candidates_for_repeated_failure_carries_fingerprint() -> Result<(), String> {
        let store = seeded_store()?;
        let cands = build_candidates(
            &store,
            &MemoryEvent::RepeatedTaskFailure {
                run_id: "r1".into(),
                task_title: "Apply fix".into(),
                fingerprint: "missing-test".into(),
            },
        )
        .await?;
        assert_eq!(cands.len(), 1);
        let candidate = cands
            .first()
            .ok_or_else(|| "failure memory candidate missing".to_string())?;
        assert!(candidate.content.contains("missing-test"));
        assert_eq!(candidate.memory_type, MemoryType::DebuggingLesson);
        assert_eq!(candidate.source, MemorySource::ErrorResolution);
        Ok(())
    }

    #[tokio::test]
    async fn build_candidates_for_cancelled_run_is_a_preference() -> Result<(), String> {
        let store = seeded_store()?;
        let cands = build_candidates(
            &store,
            &MemoryEvent::RunCancelledByUser {
                run_id: "r1".into(),
                goal: "Refactor everything".into(),
            },
        )
        .await?;
        assert_eq!(cands.len(), 1);
        let candidate = cands
            .first()
            .ok_or_else(|| "cancel memory candidate missing".to_string())?;
        assert_eq!(candidate.memory_type, MemoryType::UserPreference);
        assert_eq!(candidate.source, MemorySource::UserCorrection);
        Ok(())
    }

    #[tokio::test]
    async fn write_memory_candidate_is_a_noop_without_generation() -> Result<(), String> {
        let store = seeded_store()?;
        write_memory_candidate_settled(
            None,
            &store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "x".into(),
            },
        )
        .await;
        Ok(())
    }

    #[tokio::test]
    async fn memory_policy_writes_predicate() {
        // B5.1: writes() is the quick guard callers can use to skip building an
        // event entirely when the policy is None.
        assert!(!MemoryPolicy::None.writes());
        assert!(MemoryPolicy::BestEffortSettled.writes());
    }

    #[tokio::test]
    async fn dispatch_none_policy_is_a_noop_even_with_no_generation() -> Result<(), String> {
        // B5.1: MemoryPolicy::None short-circuits before touching the lease.
        let store = seeded_store()?;
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
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_settled_with_no_generation_returns_without_panic() -> Result<(), String> {
        let store = seeded_store()?;
        write_memory_candidate_dispatch(
            MemoryPolicy::BestEffortSettled,
            None,
            &store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "x".into(),
            },
        )
        .await;
        Ok(())
    }

    /// B5.5 recall-closure e2e: a Completed run's memory (written via the
    /// settled path that `drive_run_async` uses) is durable + findable by
    /// recall BEFORE the run returns — so a follow-up question can hit it.
    ///
    /// This is the closure that `MemoryPolicy::BestEffortSettled` guarantees. It
    /// exercises the real MemoryLayerManager + InMemoryStore (no LLM, no
    /// Postgres, no embeddings — the write path only serializes+stores, and
    /// recall falls back to keyword search). The precondition: the run must
    /// have ≥1 Completed todo (else build_candidates returns nothing).
    #[tokio::test]
    async fn completed_run_memory_is_recallable_after_settlement() -> Result<(), String> {
        use echo_agent::evolution::{MemoryRecaller, ReviewConfig};
        use echo_agent::memory::InMemoryStore;
        use echo_agent::memory::store::Store;

        // Real backing store; keep a clone for direct recall.
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let dir = temp.path().to_path_buf();
        let integration =
            crate::evolution::ReviewIntegration::new(ReviewConfig::default(), dir, store.clone());
        let memory_generation = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let lm = memory_generation
            .layer_manager()
            .map_err(|error| error.to_string())?;

        // Seeded TaskRuntimeStore: run "r1", goal "Review runtime", one
        // Completed todo ("Review chat.rs" / "found gap").
        let rt_store = seeded_store()?;

        // The write the settled policy performs on Completion (the path
        // drive_run_async → execute_run → write_memory_candidate_dispatch uses).
        write_memory_candidate_settled(
            Some(&memory_generation),
            &rt_store,
            MemoryEvent::RunCompleted {
                run_id: "r1".into(),
                goal: "Review runtime".into(),
            },
        )
        .await;

        // (1) Exact-key lookup — strongest, no ranking luck. The key the write
        // path uses is `taskrun:completed:{run_id}`.
        let located = lm.locate("taskrun:completed:r1").await;
        assert!(
            located.is_some(),
            "RunCompleted memory must be located by exact key"
        );
        let (_, entry) = located.ok_or_else(|| "completed memory was not located".to_string())?;
        assert!(
            entry.content.contains("Review runtime"),
            "memory content must carry the goal; got: {}",
            entry.content
        );
        assert!(
            entry.content.contains("Review chat.rs"),
            "memory content must summarize the completed todo title; got: {}",
            entry.content
        );

        // (2) Keyword recall via the manager (exercises the layered search path
        // the frontend/agent recall would use).
        let hits = lm
            .search_layered("Review", 10)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            hits.iter()
                .any(|(_, e)| e.content.contains("Review runtime")),
            "search_layered should find the completed-run memory by keyword"
        );

        // (3) True ReactAgent recall path over the same store (what a follow-up
        // question actually does). Confirms the write landed in a recallable
        // namespace, not just an internal log.
        let recalled = MemoryRecaller::new(store.clone())
            .recall("Review", 5)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            recalled.iter().any(|i| i.key == "taskrun:completed:r1"),
            "MemoryRecaller (the real follow-up-question path) must find the completed-run memory"
        );
        Ok(())
    }
}
