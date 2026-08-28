//! F5 characterization for exact TaskSubagent control boundaries.
//!
//! These tests exercise the existing AgentControl/SubagentControl/TaskRuntime
//! authorities. They intentionally add no routing, storage, or lifecycle
//! implementation.

use std::path::Path;
use std::sync::Arc;

use echo_agent_app_core::agent_control::{
    AgentControlError, AgentControlService, AgentMessageDelivery, AgentMessageRequest, AgentTarget,
    TaskSubagentTarget,
};
use echo_agent_app_core::agent_router::AgentRouter;
use echo_agent_app_core::subagent_loader::{SubagentCatalogSnapshot, discover_subagents};
use echo_agent_app_core::tasks::task_runtime::task_tools::TaskCapabilityCatalog;
use echo_agent_app_core::tasks::task_runtime::{
    AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPatch, TaskPlan,
    TaskRunStatus, TaskRuntimeStore, TaskUpdateOperation, TaskUpdateRequest, apply_eko_task_update,
    build_eko_task_revision_service, commit_eko_task_plan, task_goal_sha256,
};
use echo_agent_app_core::workspace::registry::WorkspaceRegistry;

fn task_plan(run_id: &str) -> TaskPlan {
    TaskPlan {
        plan_id: format!("plan:{run_id}"),
        run_id: run_id.to_string(),
        revision: 0,
        domain_profile: DomainProfile::General,
        goal_revision: 1,
        goal_sha256: task_goal_sha256("exact Subagent control"),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask {
            id: "task-a".to_string(),
            title: "Exact Subagent attempt".to_string(),
            description: "Characterize exact-attempt control".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "general-purpose".to_string(),
            domain_profile: DomainProfile::General,
            ..PlanTask::default()
        }],
    }
}

async fn fixture(
    root: &Path,
    run_id: &str,
) -> Result<(Arc<TaskRuntimeStore>, AgentControlService), String> {
    let store = Arc::new(
        TaskRuntimeStore::open_for_workspace(root.join("tasks"), "global")
            .map_err(|error| error.to_string())?,
    );
    store
        .create_run(
            run_id,
            "global",
            "conversation-a",
            "message-a",
            DomainProfile::General,
            "exact Subagent control",
            "task",
            AttendedMode::Attended,
        )
        .map_err(|error| error.to_string())?;
    commit_eko_task_plan(store.clone(), task_plan(run_id))
        .await
        .map_err(|error| error.to_string())?;
    let router = Arc::new(AgentRouter::new(root.join("router")));
    let registry = Arc::new(
        WorkspaceRegistry::with_base_dir(root.join("workspaces"))
            .map_err(|error| error.to_string())?,
    );
    Ok((
        store.clone(),
        AgentControlService::new(router, store, registry),
    ))
}

fn target(
    run_id: &str,
    task_id: &str,
    plan_revision: u64,
    execution_id: &str,
    attempt: u32,
) -> AgentTarget {
    AgentTarget::TaskSubagent {
        target: TaskSubagentTarget {
            workspace_id: "global".to_string(),
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            plan_revision,
            execution_id: execution_id.to_string(),
            attempt,
            workspace_generation: None,
        },
    }
}

fn next_target(run_id: &str, plan_revision: u64, attempt: u32) -> AgentTarget {
    target(
        run_id,
        "task-a",
        plan_revision,
        &format!("pending:{run_id}:task-a:{plan_revision}:{attempt}"),
        attempt,
    )
}

fn message_request(
    target: AgentTarget,
    text: &str,
    command_id: &str,
    delivery: AgentMessageDelivery,
) -> AgentMessageRequest {
    AgentMessageRequest {
        target,
        text: text.to_string(),
        command_id: Some(command_id.to_string()),
        message_id: None,
        correlation_id: None,
        delivery,
        from: None,
    }
}

fn revision_service(store: Arc<TaskRuntimeStore>) -> Arc<echo_agent::tasks::TaskRevisionService> {
    let definitions = discover_subagents(None, None);
    let catalog = Arc::new(SubagentCatalogSnapshot::from_definitions(&definitions));
    let capabilities = Arc::new(TaskCapabilityCatalog::new(catalog, Vec::<String>::new()));
    build_eko_task_revision_service(store, capabilities)
}

#[tokio::test]
async fn task_subagent_target_is_exact_and_late_live_message_fails_closed() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (store, service) = fixture(root.path(), "run-exact").await?;
    let execution_id = "run-exact:task-a:1:1:claim-a";
    store
        .record_subagent_assigned(
            "run-exact",
            "task-a",
            execution_id,
            "general-purpose",
            "Exact Subagent attempt",
            1,
            1,
            true,
            false,
        )
        .map_err(|error| error.to_string())?;

    let exact = target("run-exact", "task-a", 1, execution_id, 1);
    let inspected = service
        .inspect(exact.clone())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(inspected.status, "running");
    assert_eq!(inspected.attempt, Some(1));
    let encoded = serde_json::to_value(&exact).map_err(|error| error.to_string())?;
    for field in [
        "type",
        "workspace_id",
        "run_id",
        "task_id",
        "plan_revision",
        "execution_id",
        "attempt",
    ] {
        assert!(
            encoded.get(field).is_some(),
            "target field missing: {field}"
        );
    }

    let missing_run = service
        .inspect(target("missing-run", "task-a", 1, execution_id, 1))
        .await
        .err()
        .ok_or_else(|| "unknown TaskRun target was accepted".to_string())?;
    assert!(matches!(missing_run, AgentControlError::RunNotFound { .. }));

    let wrong_task = service
        .inspect(target("run-exact", "task-b", 1, execution_id, 1))
        .await
        .err()
        .ok_or_else(|| "wrong task target was accepted".to_string())?;
    assert!(matches!(wrong_task, AgentControlError::StaleAttempt { .. }));

    let wrong_attempt = service
        .inspect(target("run-exact", "task-a", 1, execution_id, 2))
        .await
        .err()
        .ok_or_else(|| "wrong attempt target was accepted".to_string())?;
    assert!(matches!(
        wrong_attempt,
        AgentControlError::StaleAttempt { .. }
    ));

    store
        .transition_run("run-exact", TaskRunStatus::Running)
        .map_err(|error| error.to_string())?;
    store
        .transition_run("run-exact", TaskRunStatus::Completed)
        .map_err(|error| error.to_string())?;
    let event_count_before_late_message = store
        .list_events("run-exact", 0)
        .map_err(|error| error.to_string())?
        .len();
    let late = service
        .message(message_request(
            exact,
            "late guidance must not execute",
            "late-command",
            AgentMessageDelivery::Live,
        ))
        .await
        .err()
        .ok_or_else(|| "late live guidance crossed a terminal TaskRun".to_string())?;
    assert!(matches!(late, AgentControlError::TargetUnavailable(_)));
    assert_eq!(
        store
            .list_events("run-exact", 0)
            .map_err(|error| error.to_string())?
            .len(),
        event_count_before_late_message,
        "fail-closed late guidance must not append a command"
    );
    Ok(())
}

#[tokio::test]
async fn next_attempt_guidance_is_revision_bound_and_task_update_is_cas_bound() -> Result<(), String>
{
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (store, service) = fixture(root.path(), "run-revision").await?;

    let first = service
        .message(message_request(
            next_target("run-revision", 1, 1),
            "guidance for revision one",
            "command-revision-one",
            AgentMessageDelivery::NextAttempt,
        ))
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(first.status, "pending");
    assert!(!first.duplicate);

    let revision_service = revision_service(store.clone());
    let updated = apply_eko_task_update(
        &revision_service,
        store.clone(),
        "run-revision",
        TaskUpdateRequest {
            base_revision: 1,
            reason: "rename the exact-attempt fixture".to_string(),
            operations: vec![TaskUpdateOperation::Update {
                task_id: "task-a".to_string(),
                patch: TaskPatch {
                    title: Some("Updated exact-attempt fixture".to_string()),
                    ..TaskPatch::default()
                },
            }],
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(updated.revision, 2);

    let stale_guidance = service
        .message(message_request(
            next_target("run-revision", 1, 1),
            "must stay on the old plan revision",
            "command-stale-revision",
            AgentMessageDelivery::NextAttempt,
        ))
        .await
        .err()
        .ok_or_else(|| "stale revision guidance was accepted".to_string())?;
    assert!(matches!(
        stale_guidance,
        AgentControlError::WrongRevision { .. }
    ));

    let second = service
        .message(message_request(
            next_target("run-revision", 2, 1),
            "guidance for revision two",
            "command-revision-two",
            AgentMessageDelivery::NextAttempt,
        ))
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(second.status, "pending");
    assert!(!second.duplicate);

    let stale_update = apply_eko_task_update(
        &revision_service,
        store.clone(),
        "run-revision",
        TaskUpdateRequest {
            base_revision: 1,
            reason: "must lose the stale CAS".to_string(),
            operations: vec![TaskUpdateOperation::Update {
                task_id: "task-a".to_string(),
                patch: TaskPatch {
                    description: Some("stale description".to_string()),
                    ..TaskPatch::default()
                },
            }],
        },
    )
    .await;
    assert!(
        stale_update.is_err(),
        "task_update accepted stale base_revision"
    );
    assert_eq!(
        store
            .get_plan("run-revision")
            .map_err(|error| error.to_string())?
            .map(|plan| plan.revision),
        Some(2)
    );
    Ok(())
}
