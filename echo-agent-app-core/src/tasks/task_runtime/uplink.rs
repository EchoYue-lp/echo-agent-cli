//! EKO application uplink sink for dispatched Subagents.
//!
//! The framework installs a default uplink sink (event bus + shared control
//! plane). EKO replaces it — at TaskRuntime dispatch time — with this
//! application sink so every Subagent→parent / Subagent→sibling message is
//! routed into the product authorities:
//!
//! - Parent `report` → journaled as `SubagentEscalationRequested`
//!   (`blocking:false`); the run keeps scheduling.
//! - Parent `escalate` → journaled (`blocking:true`) and the run is paused
//!   with `RunPauseReason::NeedsInput`; the user answers through the existing
//!   exact-attempt guidance path (live steer into the SAME attempt).
//! - Sibling by `execution_id` → live message via `SubagentControlService`
//!   (queue-only steer of the active sibling attempt).
//! - Sibling by `task_id` → durable next-attempt guidance, delivered when the
//!   dispatcher admits that task's next attempt.
//!
//! The sink is fire-and-forget by contract: the sending attempt never waits,
//! so parent/child mutual waiting cannot deadlock a run.

use std::sync::Arc;

use echo_agent::tools::{
    SubagentUplinkFn, SubagentUplinkKind, SubagentUplinkMessage, SubagentUplinkReceipt,
    SubagentUplinkTarget,
};

use super::executor::TaskRuntimeOperation;
use super::run_authority::RuntimeJournalEvent;
use super::store::TaskRuntimeStore;
use super::subagent_control::SubagentControlService;
use super::types::{
    RunPauseReason, RuntimeEventKind, SubagentControlActorSource, SubagentControlIdentity,
};

/// Bound for the journaled escalation text, in Unicode scalar values.
const MAX_ESCALATION_CHARS: usize = 4_000;

/// Build the EKO uplink sink bound to one TaskRuntimeStore.
pub fn eko_uplink_sink(store: Arc<TaskRuntimeStore>) -> SubagentUplinkFn {
    Arc::new(move |message: SubagentUplinkMessage| {
        let store = Arc::clone(&store);
        Box::pin(async move {
            let Some(run_id) = message.from.run_id.clone() else {
                return uplink_receipt(false, "no_run_context", "uplink sender carries no run id");
            };
            match message.target {
                SubagentUplinkTarget::Parent { kind, text } => {
                    handle_parent(&store, &run_id, &message.from, kind, text).await
                }
                SubagentUplinkTarget::Sibling { to, text } => {
                    handle_sibling(&store, &run_id, &message.from, to, text).await
                }
            }
        })
    })
}

async fn handle_parent(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    from: &echo_agent::tools::SubagentLineage,
    kind: SubagentUplinkKind,
    text: String,
) -> SubagentUplinkReceipt {
    let blocking = TaskRuntimeOperation::new(Arc::clone(store));
    let run_id = run_id.to_string();
    let from = from.clone();
    let bounded: String = text.chars().take(MAX_ESCALATION_CHARS).collect();
    let is_blocking = kind == SubagentUplinkKind::Escalate;
    let detail_pause = format!(
        "Subagent '{}' (task {}) requested clarification: {}",
        from.agent_name.as_deref().unwrap_or("<unknown>"),
        from.task_id.as_deref().unwrap_or("<unknown>"),
        bounded.chars().take(200).collect::<String>(),
    );

    let pause_requested = is_blocking;
    let journal_run_id = run_id.clone();
    let journal_task = from.task_id.clone();
    let journal_execution = from.execution_id.clone();
    let journal_payload = escalation_payload(&from, is_blocking, &bounded);
    let journal_outcome = blocking
        .run_store("journal Subagent escalation", move |store| {
            store.commit_runtime_events(
                &journal_run_id,
                vec![RuntimeJournalEvent::for_append(
                    &journal_run_id,
                    journal_task.as_deref(),
                    journal_execution.as_deref(),
                    RuntimeEventKind::SubagentEscalationRequested,
                    journal_payload,
                )],
            )
        })
        .await;

    if let Err(error) = journal_outcome {
        return uplink_receipt(false, "journal_failed", &format!("{error}"));
    }

    if pause_requested {
        let pause_run_id = run_id.clone();
        let pause_detail = detail_pause.clone();
        let pause_outcome = blocking
            .run_store("pause run for Subagent escalation", move |store| {
                store.request_pause_with_reason(
                    &pause_run_id,
                    RunPauseReason::NeedsInput,
                    Some(&pause_detail),
                )
            })
            .await;
        return match pause_outcome {
            Ok(true) => uplink_receipt(
                true,
                "paused_needs_input",
                "run paused; answer via Subagent guidance to the same attempt",
            ),
            Ok(false) => uplink_receipt(
                true,
                "recorded",
                "run was not in a pausable state; escalation journaled only",
            ),
            Err(error) => uplink_receipt(
                true,
                "recorded",
                &format!("pause request failed ({error}); escalation journaled"),
            ),
        };
    }

    uplink_receipt(true, "recorded", "report journaled for the run driver")
}

async fn handle_sibling(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    from: &echo_agent::tools::SubagentLineage,
    to: echo_agent::tools::SubagentPeerAddress,
    text: String,
) -> SubagentUplinkReceipt {
    let bounded: String = text.chars().take(MAX_ESCALATION_CHARS).collect();
    let service = SubagentControlService::new(Arc::clone(store));
    match to {
        echo_agent::tools::SubagentPeerAddress::ByExecutionId(execution_id) => {
            let resolved = TaskRuntimeOperation::new(Arc::clone(store))
                .run_store("resolve sibling control identity", move |store| {
                    Ok(store.active_control_identity(&execution_id))
                })
                .await;
            let Ok(Some(identity)) = resolved else {
                return uplink_receipt(
                    false,
                    "sibling_not_active",
                    "no live attempt under the given execution id",
                );
            };
            let mut identity = redact_command(identity);
            identity.run_id = run_id.to_string();
            let outcome = service
                .send_message(identity, &bounded, SubagentControlActorSource::Peer)
                .await;
            match outcome {
                Ok(_) => uplink_receipt(true, "delivered_to_sibling", "live steer accepted"),
                Err(error) => uplink_receipt(false, "sibling_delivery_failed", &format!("{error}")),
            }
        }
        echo_agent::tools::SubagentPeerAddress::ByTaskId(task_id) => {
            let next_attempt = TaskRuntimeOperation::new(Arc::clone(store))
                .run_store("compute sibling next attempt", {
                    let run_id = run_id.to_string();
                    let probe_task_id = task_id.clone();
                    move |store| Ok(next_peer_attempt(&store, &run_id, &probe_task_id))
                })
                .await;
            let Ok(Some(attempt)) = next_attempt else {
                return uplink_receipt(
                    false,
                    "sibling_task_not_found",
                    "target task is unknown or has no schedulable next attempt",
                );
            };
            let identity = SubagentControlIdentity {
                run_id: run_id.to_string(),
                execution_id: format!("{run_id}:{task_id}:pending:{attempt}:peer"),
                plan_revision: from.plan_revision.unwrap_or(0),
                attempt,
                command_id: uuid::Uuid::new_v4().to_string(),
                task_id,
            };
            let outcome = service
                .queue_guidance(identity, &bounded, SubagentControlActorSource::Peer)
                .map_err(|error| error.to_string());
            match outcome {
                Ok(_) => uplink_receipt(
                    true,
                    "queued_for_next_attempt",
                    &format!("queued for attempt {attempt}"),
                ),
                Err(error) => uplink_receipt(false, "sibling_queue_failed", &error),
            }
        }
    }
}

/// Compute the next schedulable attempt for a task, mirroring the control
/// service's own validation (`max(latest attempt, retry_count) + 1`).
fn next_peer_attempt(store: &TaskRuntimeStore, run_id: &str, task_id: &str) -> Option<u32> {
    let plan = store.get_plan(run_id).ok().flatten()?;
    let task = plan.tasks.iter().find(|task| task.id == task_id)?;
    let latest_attempt = store
        .list_subagent_runs(run_id)
        .ok()
        .map(|runs| {
            runs.into_iter()
                .filter(|run| run.task_id == task_id)
                .map(|run| run.attempt)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    latest_attempt.max(task.retry_count).checked_add(1)
}

fn escalation_payload(
    from: &echo_agent::tools::SubagentLineage,
    blocking: bool,
    text: &str,
) -> serde_json::Value {
    serde_json::json!({
        "blocking": blocking,
        "text": text,
        "from_agent": from.agent_name,
        "from_execution_id": from.execution_id,
        "from_task_id": from.task_id,
        "from_attempt": from.attempt,
        "command_id": uuid::Uuid::new_v4().to_string(),
        "actor_source": SubagentControlActorSource::Peer.as_str(),
    })
}

/// Strip the resolved identity's command id so the caller mints a fresh one;
/// keep the attempt coordinates authoritative.
fn redact_command(mut identity: SubagentControlIdentity) -> SubagentControlIdentity {
    identity.command_id = uuid::Uuid::new_v4().to_string();
    identity
}

fn uplink_receipt(accepted: bool, status: &str, detail: &str) -> SubagentUplinkReceipt {
    SubagentUplinkReceipt {
        accepted,
        status: status.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, TaskPlan, TaskRunStatus,
        task_goal_sha256,
    };

    type TestResult = Result<(), String>;

    fn seed_run_with_tasks(
        run_id: &str,
        task_ids: &[&str],
    ) -> Result<Arc<TaskRuntimeStore>, String> {
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
        store
            .transition_run(run_id, TaskRunStatus::Running)
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

    fn sender_lineage(run_id: &str, task_id: &str) -> echo_agent::tools::SubagentLineage {
        echo_agent::tools::SubagentLineage {
            agent_name: Some("implementer".to_string()),
            execution_id: Some(format!("{run_id}:{task_id}:1:1:claim")),
            run_id: Some(run_id.to_string()),
            task_id: Some(task_id.to_string()),
            attempt: Some(1),
            plan_revision: Some(1),
            ..Default::default()
        }
    }

    fn run_status(store: &TaskRuntimeStore, run_id: &str) -> Result<TaskRunStatus, String> {
        store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .map(|run| run.status)
            .ok_or_else(|| format!("run {run_id} must exist"))
    }

    fn count_events(
        store: &TaskRuntimeStore,
        run_id: &str,
        kind: RuntimeEventKind,
        task_id: Option<&str>,
    ) -> Result<usize, String> {
        Ok(store
            .list_events(run_id, 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                event.event_type == kind
                    && task_id.is_none_or(|expected| event.task_id.as_deref() == Some(expected))
            })
            .count())
    }

    #[tokio::test]
    async fn report_is_journaled_without_pausing() -> TestResult {
        let store = seed_run_with_tasks("run-1", &["task-a"])?;
        let sink = eko_uplink_sink(Arc::clone(&store));
        let receipt = sink(SubagentUplinkMessage {
            from: sender_lineage("run-1", "task-a"),
            target: SubagentUplinkTarget::Parent {
                kind: SubagentUplinkKind::Report,
                text: "dependency output is ready".to_string(),
            },
        })
        .await;
        assert!(receipt.accepted);
        assert_eq!(receipt.status, "recorded");
        assert_eq!(
            count_events(
                &store,
                "run-1",
                RuntimeEventKind::SubagentEscalationRequested,
                None
            )?,
            1
        );
        assert_eq!(run_status(&store, "run-1")?, TaskRunStatus::Running);
        Ok(())
    }

    #[tokio::test]
    async fn blocking_escalation_journals_and_pauses_needs_input() -> TestResult {
        let store = seed_run_with_tasks("run-2", &["task-a"])?;
        let sink = eko_uplink_sink(Arc::clone(&store));
        let receipt = sink(SubagentUplinkMessage {
            from: sender_lineage("run-2", "task-a"),
            target: SubagentUplinkTarget::Parent {
                kind: SubagentUplinkKind::Escalate,
                text: "计划假设错误:目标约束不明确".to_string(),
            },
        })
        .await;
        assert!(receipt.accepted);
        assert_eq!(receipt.status, "paused_needs_input");
        assert_eq!(
            count_events(
                &store,
                "run-2",
                RuntimeEventKind::SubagentEscalationRequested,
                None
            )?,
            1
        );
        assert_eq!(run_status(&store, "run-2")?, TaskRunStatus::Paused);
        Ok(())
    }

    #[tokio::test]
    async fn sibling_by_task_id_queues_next_attempt_guidance() -> TestResult {
        let store = seed_run_with_tasks("run-3", &["task-a", "task-b"])?;
        let sink = eko_uplink_sink(Arc::clone(&store));
        let receipt = sink(SubagentUplinkMessage {
            from: sender_lineage("run-3", "task-a"),
            target: SubagentUplinkTarget::Sibling {
                to: echo_agent::tools::SubagentPeerAddress::ByTaskId("task-b".to_string()),
                text: "我的产物已就绪,你可以开始了".to_string(),
            },
        })
        .await;
        assert!(receipt.accepted);
        assert_eq!(receipt.status, "queued_for_next_attempt");
        assert_eq!(
            count_events(
                &store,
                "run-3",
                RuntimeEventKind::SubagentGuidanceQueued,
                Some("task-b")
            )?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn uplink_without_run_context_is_rejected() -> TestResult {
        let store = seed_run_with_tasks("run-4", &["task-a"])?;
        let sink = eko_uplink_sink(Arc::clone(&store));
        let receipt = sink(SubagentUplinkMessage {
            from: echo_agent::tools::SubagentLineage::default(),
            target: SubagentUplinkTarget::Parent {
                kind: SubagentUplinkKind::Report,
                text: "orphan".to_string(),
            },
        })
        .await;
        assert!(!receipt.accepted);
        assert_eq!(receipt.status, "no_run_context");
        Ok(())
    }
}
