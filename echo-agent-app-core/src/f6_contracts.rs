//! Executable F6 closure contracts for durable cursors, cold recovery, and
//! surface-independent terminal facts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::memory::{ConversationStore, FileConversationStore, NewConversation};

use crate::agent_control::{
    AgentControlError, AgentControlService, AgentMessageDelivery, AgentMessageRequest, AgentTarget,
    AgentWaitRequest, AgentWaitStatus, ConversationTarget, TaskSubagentTarget,
};
use crate::agent_router::{
    AgentAddress, AgentDeliveryOutcome, AgentDeliveryPhase, AgentMessage, AgentRouter,
};
use crate::chat_driver::ChatDriverEvent;
use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
use crate::tasks::task_runtime::store::SubagentReleaseRecord;
use crate::tasks::task_runtime::{
    AttendedMode, DomainProfile, ExecutionMode, PlanTask, TaskPlan, TaskRunBootReconciler,
    TaskRunStatus, TaskRuntimeStore, task_goal_sha256,
};
use crate::workspace::WorkspaceId;
use crate::workspace::registry::WorkspaceRegistry;

struct DurableControlFixture {
    router_root: PathBuf,
    task_root: PathBuf,
    conversation_root: PathBuf,
    workspace_root: PathBuf,
    workspace_id: String,
}

#[test]
fn interactive_surfaces_replay_one_canonical_fixture_without_terminal_inference()
-> Result<(), String> {
    const INTERACTIVE_SURFACES: [&str; 5] = ["gui", "tui", "cli", "jsonl", "channel"];
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log_root = root.path().join("chat-events");
    let log = ChatEventLog::open(&log_root, ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    for status in ["running", "completed"] {
        log.append(
            "global",
            Some("surface-fixture"),
            "turn-fixture",
            ChatDriverEvent::TurnStatus {
                status: status.to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
    }
    drop(log);

    let reopened = ChatEventLog::open(log_root, ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    let replay = reopened
        .replay("global", Some("surface-fixture"), "turn-fixture", 0)
        .map_err(|error| error.to_string())?;
    assert!(!replay.truncated);
    assert_eq!(replay.latest_cursor, 2);

    let canonical = replay
        .events
        .iter()
        .map(|envelope| {
            let ChatDriverEvent::TurnStatus { status } = &envelope.payload else {
                return Err("surface fixture contained a non-status event".to_string());
            };
            Ok((envelope.sequence, status.clone()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for surface in INTERACTIVE_SURFACES {
        let consumed = canonical.clone();
        assert_eq!(
            consumed,
            vec![(1, "running".to_string()), (2, "completed".to_string())],
            "{surface} diverged from the canonical fixture"
        );
        assert_eq!(
            consumed
                .iter()
                .filter(|(_, status)| status == "completed")
                .count(),
            1,
            "{surface} inferred or duplicated a terminal"
        );
    }
    Ok(())
}

impl DurableControlFixture {
    fn new(root: &Path, workspace_id: &str) -> Self {
        Self {
            router_root: root.join("router"),
            task_root: root.join("tasks"),
            conversation_root: root.join("conversations"),
            workspace_root: root.join("workspaces"),
            workspace_id: workspace_id.to_string(),
        }
    }

    fn open(
        &self,
    ) -> Result<
        (
            AgentControlService,
            Arc<TaskRuntimeStore>,
            Arc<WorkspaceRegistry>,
        ),
        String,
    > {
        let router = Arc::new(AgentRouter::new(self.router_root.clone()));
        let task_runtime = Arc::new(
            TaskRuntimeStore::open_for_workspace(&self.task_root, &self.workspace_id)
                .map_err(|error| error.to_string())?,
        );
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(self.workspace_root.clone())
                .map_err(|error| error.to_string())?,
        );
        let conversations: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(&self.conversation_root)
                .map_err(|error| error.to_string())?,
        );
        let service = AgentControlService::new(
            Arc::clone(&router),
            Arc::clone(&task_runtime),
            Arc::clone(&registry),
        )
        .with_conversation_store(conversations, self.workspace_id.clone());
        Ok((service, task_runtime, registry))
    }
}

fn conversation_target(
    workspace_id: &str,
    conversation_id: &str,
    workspace_generation: Option<String>,
) -> AgentTarget {
    AgentTarget::Conversation {
        target: ConversationTarget {
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
            workspace_generation,
        },
    }
}

fn task_target(run_id: &str) -> AgentTarget {
    AgentTarget::TaskSubagent {
        target: TaskSubagentTarget {
            workspace_id: "global".to_string(),
            run_id: run_id.to_string(),
            task_id: "task-a".to_string(),
            plan_revision: 1,
            execution_id: "execution-a".to_string(),
            attempt: 1,
            workspace_generation: Some("global".to_string()),
        },
    }
}

async fn ensure_conversation(
    fixture: &DurableControlFixture,
    conversation_id: &str,
) -> Result<(), String> {
    let store = FileConversationStore::new(&fixture.conversation_root)
        .map_err(|error| error.to_string())?;
    store
        .ensure_conversation(NewConversation {
            conversation_id: conversation_id.to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F6 durable cursor".to_string()),
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn seed_task(store: &TaskRuntimeStore, run_id: &str, assign_subagent: bool) -> Result<(), String> {
    store
        .create_run(
            run_id,
            "global",
            "conversation-task",
            "root-message",
            DomainProfile::General,
            "close F6",
            "task",
            AttendedMode::Attended,
        )
        .map_err(|error| error.to_string())?;
    store
        .attach_plan_for_test(&TaskPlan {
            plan_id: format!("plan:{run_id}"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("close F6"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "task-a".to_string(),
                title: "F6 cursor recovery".to_string(),
                ..PlanTask::default()
            }],
        })
        .map_err(|error| error.to_string())?;
    if assign_subagent {
        store
            .record_subagent_assigned(
                run_id,
                "task-a",
                "execution-a",
                "reviewer",
                "F6 cursor recovery",
                1,
                1,
                true,
                false,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tokio::test]
async fn conversation_cursor_and_terminal_survive_router_restart_exactly_once() -> Result<(), String>
{
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = DurableControlFixture::new(root.path(), "global");
    let (first, _, _) = fixture.open()?;
    ensure_conversation(&fixture, "conversation-restart").await?;
    let target = conversation_target("global", "conversation-restart", Some("global".to_string()));
    let address = AgentAddress::new(
        WorkspaceId::from_raw("global".to_string()),
        "conversation-restart",
    );
    let mut message = AgentMessage::agent_text(None, address.clone(), "persist across restart");
    message.message_id = "f6-conversation-message".to_string();
    let turn_id = message.delivery_turn_id();
    first
        .router()
        .enqueue(message.clone())
        .await
        .map_err(|error| error.to_string())?;
    let claim = first
        .router()
        .claim_next(&address)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "F6 Conversation claim is missing".to_string())?;
    first
        .router()
        .begin_injection(&claim, turn_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    first
        .router()
        .mailbox_accepted(&claim, turn_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    first
        .router()
        .drained(&claim, turn_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    first
        .router()
        .turn_settled(
            &claim,
            Some(turn_id),
            AgentDeliveryOutcome::Completed,
            true,
            None,
            false,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    let before_restart = first
        .inspect(target.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(before_restart.status, "turn_settled");
    drop(first);

    let (restarted, _, _) = fixture.open()?;
    let after_restart = restarted
        .inspect(target.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(after_restart.cursor, before_restart.cursor);
    assert_eq!(after_restart.status, "turn_settled");
    let unchanged = restarted
        .wait(
            AgentWaitRequest {
                targets: vec![target.clone()],
                after_cursor: Some(before_restart.cursor.clone()),
                timeout_ms: 0,
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(unchanged.status, AgentWaitStatus::Timeout);

    let duplicate = restarted
        .message(AgentMessageRequest {
            target,
            text: "persist across restart".to_string(),
            command_id: None,
            message_id: Some(message.message_id.clone()),
            correlation_id: None,
            delivery: AgentMessageDelivery::Live,
            from: None,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.phase, "turn_settled");
    let phases = restarted
        .router()
        .event_phases_for_test(&address, &message.message_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        phases
            .iter()
            .filter(|phase| **phase == AgentDeliveryPhase::TurnSettled.as_str())
            .count(),
        1
    );
    assert!(
        restarted
            .router()
            .in_flight_claim(&address)
            .await
            .map_err(|error| error.to_string())?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn task_subagent_cursor_survives_store_restart_without_stranded_boundaries()
-> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = DurableControlFixture::new(root.path(), "global");
    let (first, first_store, _) = fixture.open()?;
    seed_task(&first_store, "run-restart", true)?;
    let target = task_target("run-restart");
    let before_restart = first
        .inspect(target.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(before_restart.status, "running");
    drop(first);
    drop(first_store);

    let (restarted, restarted_store, _) = fixture.open()?;
    let recovered = restarted
        .inspect(target.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(recovered.cursor, before_restart.cursor);
    restarted_store
        .record_subagent_released(SubagentReleaseRecord {
            run_id: "run-restart",
            task_id: "task-a",
            execution_id: "execution-a",
            agent_name: "reviewer",
            task_subject: "F6 cursor recovery",
            plan_revision: 1,
            attempt: 1,
            status: "completed",
            outcome: None,
            full_output: None,
            usage: None,
            dispatch_hook: false,
        })
        .map_err(|error| error.to_string())?;
    let changed = restarted
        .wait(
            AgentWaitRequest {
                targets: vec![target.clone()],
                after_cursor: Some(before_restart.cursor),
                timeout_ms: 0,
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(changed.status, AgentWaitStatus::Changed);
    assert_eq!(changed.events.len(), 1);
    assert_eq!(
        changed.events.first().map(|event| event.kind.as_str()),
        Some("subagent_released")
    );
    let next_cursor = changed
        .next_cursor
        .ok_or_else(|| "F6 TaskSubagent next cursor is missing".to_string())?;
    let replay = restarted
        .wait(
            AgentWaitRequest {
                targets: vec![target],
                after_cursor: Some(next_cursor),
                timeout_ms: 0,
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(replay.status, AgentWaitStatus::Timeout);
    assert_eq!(
        restarted_store
            .list_events("run-restart", 0)
            .map_err(|error| error.to_string())?
            .iter()
            .filter(|event| {
                event.event_type == crate::tasks::task_runtime::RuntimeEventKind::SubagentReleased
            })
            .count(),
        1
    );
    assert!(
        restarted_store
            .active_subagent_boundaries("run-restart")
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(restarted_store.active_run_driver_receipt_count()?, 0);
    assert_eq!(restarted_store.active_operation_count(), 0);
    Ok(())
}

#[tokio::test]
async fn cold_address_and_workspace_recreation_fail_closed_by_generation() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = DurableControlFixture::new(root.path(), "workspace-a");
    let (service, _, registry) = fixture.open()?;
    let workspace = registry
        .create("workspace-a", crate::workspace::WorkspaceKind::General)
        .map_err(|error| error.to_string())?;
    ensure_conversation(&fixture, "cold-conversation").await?;
    let generation = workspace.opaque_product_data_generation();
    let cold = conversation_target(
        workspace.id.as_str(),
        "cold-conversation",
        Some(generation.clone()),
    );
    let inspected = service
        .inspect(cold.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(inspected.status, "idle");
    assert!(inspected.cursor.ends_with(":0"));

    let workspace_b = registry
        .create("workspace-b", crate::workspace::WorkspaceKind::General)
        .map_err(|error| error.to_string())?;
    let foreign = conversation_target(
        workspace_b.id.as_str(),
        "cold-conversation",
        Some(workspace_b.opaque_product_data_generation()),
    );
    assert!(matches!(
        service.inspect(foreign).await,
        Err(AgentControlError::TargetUnavailable(_))
    ));

    registry
        .delete(&workspace.id)
        .map_err(|error| error.to_string())?;
    let recreated = registry
        .create("workspace-a", crate::workspace::WorkspaceKind::General)
        .map_err(|error| error.to_string())?;
    assert_ne!(generation, recreated.opaque_product_data_generation());
    assert!(matches!(
        service.inspect(cold).await,
        Err(AgentControlError::WrongWorkspaceGeneration { .. })
    ));
    let recreated_target = conversation_target(
        recreated.id.as_str(),
        "cold-conversation",
        Some(recreated.opaque_product_data_generation()),
    );
    let recreated_inspect = service
        .inspect(recreated_target)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(recreated_inspect.status, "idle");
    Ok(())
}

#[tokio::test]
async fn disk_boot_reconcile_is_once_only_across_process_generations() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let task_root = root.path().join("tasks");
    let first = Arc::new(
        TaskRuntimeStore::open_for_workspace(&task_root, "global")
            .map_err(|error| error.to_string())?,
    );
    seed_task(&first, "run-boot", false)?;
    first
        .transition_run("run-boot", TaskRunStatus::Running)
        .map_err(|error| error.to_string())?;
    first
        .configure_run_continuation("run-boot", true, true, None, None)
        .map_err(|error| error.to_string())?;
    drop(first);

    let restarted = Arc::new(
        TaskRuntimeStore::open_for_workspace(&task_root, "global")
            .map_err(|error| error.to_string())?,
    );
    let reconciler = TaskRunBootReconciler::for_store(&restarted);
    assert_eq!(reconciler.recover_once().await?, 1);
    assert_eq!(reconciler.recover_once().await?, 1);
    assert_eq!(
        restarted
            .get_run("run-boot")
            .map_err(|error| error.to_string())?
            .map(|run| run.status),
        Some(TaskRunStatus::Paused)
    );
    drop(reconciler);
    drop(restarted);

    let second_restart = Arc::new(
        TaskRuntimeStore::open_for_workspace(&task_root, "global")
            .map_err(|error| error.to_string())?,
    );
    assert_eq!(
        TaskRunBootReconciler::for_store(&second_restart)
            .recover_once()
            .await?,
        0
    );
    assert!(
        second_restart
            .active_subagent_boundaries("run-boot")
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(second_restart.active_run_driver_receipt_count()?, 0);
    assert_eq!(second_restart.active_operation_count(), 0);
    Ok(())
}
