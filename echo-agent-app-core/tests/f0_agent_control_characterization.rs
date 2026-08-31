//! F0 Lane A characterization for the existing Agent control surfaces.
//!
//! This file intentionally freezes the current contract. It does not add
//! control tools or production routing. Runtime assertions use the public
//! TaskRuntime/SubagentControl APIs; source assertions document the portions
//! that are currently observable only through application tests and wiring.

use std::sync::Arc;

use echo_agent_app_core::api::agent_router::AgentAddress;
use echo_agent_app_core::api::tasks::task_runtime::{
    AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, SubagentControlActorSource,
    SubagentControlIdentity, SubagentControlStatus, TaskPlan, TaskRuntimeStore, commit_task_plan,
};

const AGENT_ROUTER_INBOX: &str = include_str!("../src/agent_router/inbox.rs");
const AGENT_ROUTER_ROUTER: &str = include_str!("../src/agent_router/router.rs");
const SUBAGENT_CONTROL: &str = include_str!("../src/tasks/task_runtime/subagent_control.rs");
const TASK_RUNTIME_EXECUTOR: &str =
    include_str!("../src/tasks/task_runtime/executor/unattended.rs");
const TASK_RUNTIME_STORE: &str = include_str!("../src/tasks/task_runtime/store/runtime.rs");
const TASK_RUNTIME_REGISTER: &str = include_str!("../src/tasks/task_runtime/register.rs");
const TASK_RUNTIME_TYPES: &str = include_str!("../src/tasks/task_runtime/types.rs");
const TOOL_EXPOSURE: &str = include_str!("../src/tool_exposure.rs");
const INFRA: &str = include_str!("../src/infra/factory.rs");
const STATE: &str = include_str!("../src/state/app_state.rs");
const STATE_TESTS: &str = include_str!("../src/state/tests.rs");
const FRAMEWORK_AGENT_TOOL: &str =
    include_str!("../../../echo-agent/src/tools/builtin/agent_dispatch.rs");
const FRAMEWORK_REACT: &str = include_str!("../../../echo-agent/src/agent/react/mod.rs");

fn plan_for(run_id: &str, task_id: &str) -> TaskPlan {
    TaskPlan {
        plan_id: format!("plan:{run_id}"),
        run_id: run_id.to_string(),
        revision: 0,
        domain_profile: DomainProfile::General,
        goal_revision: 1,
        goal_sha256: echo_agent_app_core::api::tasks::task_runtime::task_goal_sha256("goal"),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask {
            id: task_id.to_string(),
            title: "Inspect control contract".to_string(),
            description: "Record the current control boundary".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            ..PlanTask::default()
        }],
    }
}

async fn store_with_plan(
    run_id: &str,
    workspace_id: &str,
) -> Result<Arc<TaskRuntimeStore>, String> {
    let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
    store
        .create_run(
            run_id,
            workspace_id,
            "conversation-1",
            "root-message-1",
            DomainProfile::General,
            "goal",
            "agent_autonomous",
            AttendedMode::Attended,
        )
        .map_err(|error| error.to_string())?;
    commit_task_plan(store.clone(), plan_for(run_id, "task-1"))
        .await
        .map_err(|error| error.to_string())?;
    Ok(store)
}

#[test]
fn authority_definitions_register_and_reach_production_paths() {
    assert!(AGENT_ROUTER_INBOX.contains("pub struct AgentRouter"));
    assert!(AGENT_ROUTER_ROUTER.contains("pub async fn enqueue"));
    assert!(AGENT_ROUTER_ROUTER.contains("pub async fn pending"));
    assert!(AGENT_ROUTER_ROUTER.contains("pub async fn records"));

    assert!(SUBAGENT_CONTROL.contains("pub struct SubagentControlService"));
    assert!(SUBAGENT_CONTROL.contains("pub async fn send_message"));
    assert!(SUBAGENT_CONTROL.contains("pub fn queue_guidance"));
    assert!(SUBAGENT_CONTROL.contains("pub async fn interrupt_subagent"));
    assert!(TASK_RUNTIME_STORE.contains("active_subagent_controls"));
    assert!(SUBAGENT_CONTROL.contains("record_controlled_subagent_assigned"));
    assert!(SUBAGENT_CONTROL.contains("deliver_pending_subagent_guidance"));
    assert!(TASK_RUNTIME_EXECUTOR.contains("store.record_controlled_subagent_assigned"));
    assert!(TASK_RUNTIME_EXECUTOR.contains("store.deliver_pending_subagent_guidance"));

    assert!(FRAMEWORK_AGENT_TOOL.contains("pub struct AgentDispatchTool"));
    assert!(FRAMEWORK_AGENT_TOOL.contains("\"agent_tool\""));
    assert!(FRAMEWORK_REACT.contains("AgentDispatchTool::new"));
    assert!(FRAMEWORK_REACT.contains("config.register_agent_dispatch_tool"));
    assert!(INFRA.contains("register_agent_dispatch_tool"));
    assert!(TASK_RUNTIME_REGISTER.contains("register_task_tools_on_agent"));
    assert!(TASK_RUNTIME_REGISTER.contains("ExecuteTaskTool::new"));
    assert!(TOOL_EXPOSURE.contains("task_execute"));
    assert!(!TOOL_EXPOSURE.contains("agent_list"));
    assert!(!TOOL_EXPOSURE.contains("agent_message"));
    assert!(!TOOL_EXPOSURE.contains("agent_followup"));
    assert!(!TOOL_EXPOSURE.contains("agent_wait"));

    // The app has one router owner and one TaskRuntime control route. A future
    // control surface must call these authorities instead of adding a store.
    assert!(STATE.contains("self.agent_router.enqueue(message).await?"));
    assert!(STATE.contains("self.kick_agent_delivery(target)?"));
}

#[test]
fn conversation_address_and_task_subagent_identity_are_distinct_axes() -> Result<(), String> {
    let source = AgentAddress::new(
        echo_agent_app_core::api::workspace::WorkspaceId::from_name("workspace-a"),
        "conversation-1",
    );
    let same_conversation_other_workspace = AgentAddress::new(
        echo_agent_app_core::api::workspace::WorkspaceId::from_name("workspace-b"),
        "conversation-1",
    );
    assert_ne!(source, same_conversation_other_workspace);
    assert_eq!(source.workspace_id.as_str(), "workspace-a");
    assert_eq!(source.conversation_id, "conversation-1");

    let identity = SubagentControlIdentity {
        run_id: "run-1".to_string(),
        task_id: "task-1".to_string(),
        execution_id: "run-1:task-1:1:1:claim".to_string(),
        plan_revision: 1,
        attempt: 1,
        command_id: "command-1".to_string(),
    };
    let encoded = serde_json::to_value(&identity).map_err(|error| error.to_string())?;
    for field in [
        "run_id",
        "task_id",
        "execution_id",
        "plan_revision",
        "attempt",
        "command_id",
    ] {
        assert!(
            encoded.get(field).is_some(),
            "identity field missing: {field}"
        );
    }
    assert!(encoded.get("workspace_id").is_none());
    assert!(encoded.get("conversation_id").is_none());

    // This is the current schema gap: workspace/conversation routing is
    // represented by AgentAddress/TaskRun, while exact Subagent control is
    // keyed by run/task/execution/revision/attempt only.
    assert!(TASK_RUNTIME_TYPES.contains("pub workspace_id: String"));
    assert!(TASK_RUNTIME_TYPES.contains("pub conversation_id: String"));
    Ok(())
}

#[tokio::test]
async fn stale_revision_and_attempt_are_rejected_with_typed_store_errors() -> Result<(), String> {
    let store = store_with_plan("run-control-schema", "workspace-a").await?;
    let service = echo_agent_app_core::api::tasks::task_runtime::SubagentControlService::new(store);

    let stale_revision = SubagentControlIdentity {
        run_id: "run-control-schema".to_string(),
        task_id: "task-1".to_string(),
        execution_id: "run-control-schema:task-1:2:1:claim".to_string(),
        plan_revision: 2,
        attempt: 1,
        command_id: "revision-command".to_string(),
    };
    let revision_error = service
        .queue_guidance(
            stale_revision,
            "must be rejected",
            SubagentControlActorSource::Cli,
        )
        .err()
        .ok_or_else(|| "stale plan revision was accepted".to_string())?;
    assert!(matches!(
        revision_error,
        echo_agent_app_core::api::tasks::task_runtime::StoreError::PlanConflict { .. }
    ));

    let stale_attempt = SubagentControlIdentity {
        run_id: "run-control-schema".to_string(),
        task_id: "task-1".to_string(),
        execution_id: "run-control-schema:task-1:1:2:claim".to_string(),
        plan_revision: 1,
        attempt: 2,
        command_id: "attempt-command".to_string(),
    };
    let attempt_error = service
        .queue_guidance(
            stale_attempt,
            "must also be rejected",
            SubagentControlActorSource::Cli,
        )
        .err()
        .ok_or_else(|| "stale Subagent attempt was accepted".to_string())?;
    assert!(matches!(
        attempt_error,
        echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(_)
    ));
    Ok(())
}

#[test]
fn stale_workspace_rejection_is_at_conversation_boundary_and_not_control_identity() {
    assert!(STATE.contains("ConversationNotFound {"));
    assert!(STATE.contains("workspace '{workspace_id}' is not registered"));
    assert!(STATE.contains("async fn validate_agent_address("));
    assert!(!SUBAGENT_CONTROL.contains("workspace_id: String"));
    assert!(!SUBAGENT_CONTROL.contains("conversation_id: String"));
}

#[test]
fn cold_start_and_active_message_boundaries_are_currently_observable() {
    assert!(
        STATE_TESTS
            .contains("async fn agent_delivery_cold_starts_target_and_routes_correlated_reply")
    );
    assert!(STATE_TESTS.contains("live_message.message_id = \"live-steer\""));
    assert!(
        STATE_TESTS
            .contains("record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled")
    );
    assert!(
        STATE_TESTS
            .contains("assert_eq!(live_record.turn_id.as_deref(), Some(\"active-target-turn\"))")
    );

    assert!(SUBAGENT_CONTROL.contains("RuntimeEventKind::SubagentGuidanceQueued"));
    assert!(SUBAGENT_CONTROL.contains("framework_turn_id"));
    // The low-level exact-attempt owner deliberately has no cursor/wait API.
    // F6 cursor wait belongs to the AgentControlService adapter over the
    // TaskRuntime journal, so adding it here would create a second authority.
    assert!(!SUBAGENT_CONTROL.contains("cursor"));
    assert!(!SUBAGENT_CONTROL.contains("pub async fn wait"));
    assert!(!SUBAGENT_CONTROL.contains("pub async fn followup"));
}

#[test]
fn control_receipt_status_is_bounded_to_existing_variants() {
    assert_eq!(SubagentControlStatus::Pending.as_str(), "pending");
    assert_eq!(SubagentControlStatus::Accepted.as_str(), "accepted");
    assert_eq!(SubagentControlStatus::Rejected.as_str(), "rejected");
    assert_eq!(SubagentControlStatus::Settled.as_str(), "settled");
}
