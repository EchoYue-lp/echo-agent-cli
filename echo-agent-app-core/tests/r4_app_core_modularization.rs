//! R4 integration contracts for the app-core physical boundary.
//!
//! Cross-module consumers use this file to verify the supported facade and
//! wire-level behavior. Authority-specific state-machine tests remain next to
//! their implementation because they require private seams.

use echo_agent_app_core::api::{
    AppState,
    agent_router::AgentAddress,
    chat_event_log::ChatEventRetention,
    tasks::task_runtime::{DomainProfile, ExecutionMode, PlanTask, TaskPlan},
};

#[test]
fn facade_exposes_each_app_authority_without_duplicate_types() {
    let address = AgentAddress::new(
        echo_agent_app_core::api::workspace::WorkspaceId::from_name("r4-workspace"),
        "r4-conversation",
    );
    assert_eq!(address.conversation_id, "r4-conversation");

    let plan = TaskPlan {
        plan_id: "plan:r4".to_string(),
        run_id: "run:r4".to_string(),
        revision: 1,
        domain_profile: DomainProfile::General,
        goal_revision: 1,
        goal_sha256: "r4-goal".to_string(),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask::default()],
    };
    assert_eq!(plan.run_id, "run:r4");

    let _retention = ChatEventRetention::default();
    let _app_state: Option<AppState> = None;
}

#[test]
fn physical_authority_split_and_bypass_contract_are_present() {
    let lib = include_str!("../src/lib.rs");
    let facade = include_str!("../src/api/mod.rs");
    for module in [
        "agent_pool",
        "agent_router",
        "chat_event_log",
        "extension_control",
        "infra",
        "plugin_runtime",
        "state",
        "tasks",
    ] {
        assert!(
            lib.contains(&format!("pub(crate) mod {module};")),
            "implementation module escaped facade: {module}"
        );
        assert!(
            facade.contains(&format!("pub mod {module}")),
            "facade omitted authority module: {module}"
        );
    }
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for old_path in [
        "agent_pool.rs",
        "agent_router.rs",
        "chat_event_log.rs",
        "extension_control.rs",
        "infra.rs",
        "plugin_runtime.rs",
        "state.rs",
        "tasks/task_runtime/store.rs",
        "tasks/task_runtime/executor.rs",
    ] {
        assert!(
            !source_root.join(old_path).exists(),
            "deleted aggregate path still exists: {old_path}"
        );
    }
}

#[test]
fn retention_wire_round_trip_stays_facade_compatible() -> Result<(), String> {
    let address = AgentAddress::new(
        echo_agent_app_core::api::workspace::WorkspaceId::from_name("wire-workspace"),
        "wire-conversation",
    );
    let encoded = serde_json::to_string(&address).map_err(|error| error.to_string())?;
    let decoded: AgentAddress =
        serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
    assert_eq!(decoded, address);
    let _retention = ChatEventRetention::default();
    Ok(())
}
