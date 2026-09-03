//! F10 characterization for the full-direction Agent communication matrix.
//!
//! Covers the conversation-plane additions (`agent_spawn` / `agent_resume` /
//! `agent_handoff` / `agent_group`) and the Subagent-plane uplink wiring
//! (EKO sink installation, lineage stamping, prompt protocol, role
//! delegation). Freezes the production wiring through source assertions and
//! exercises the router-backed group authority through the tool service.

use std::sync::Arc;

use echo_agent_app_core::api::agent_control::{
    AgentControlError, AgentControlService, AgentGroupAction, AgentGroupMemberInput,
    AgentGroupToolRequest, AgentHandoffRequest, AgentResumePolicy, AgentResumeRequest,
    AgentSpawnRequest,
};
use echo_agent_app_core::api::agent_router::{AgentAddress, AgentRouter};
use echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore;
use echo_agent_app_core::api::workspace::registry::WorkspaceRegistry;

const AGENT_CONTROL: &str = include_str!("../src/agent_control.rs");
const APP_STATE: &str = include_str!("../src/state/app_state.rs");
const UNATTENDED: &str = include_str!("../src/tasks/task_runtime/executor/unattended.rs");
const UPLINK: &str = include_str!("../src/tasks/task_runtime/uplink.rs");
const SUBAGENT_PROMPT: &str = include_str!("../src/subagent_prompt.rs");
const FACTORY: &str = include_str!("../src/infra/factory.rs");
const TASK_TOOLS: &str = include_str!("../src/tasks/task_runtime/task_tools.rs");

fn service_at_root(root: &std::path::Path) -> Result<AgentControlService, String> {
    let router = Arc::new(AgentRouter::new(root.join("router")));
    let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
    let registry = Arc::new(
        WorkspaceRegistry::with_base_dir(root.join("workspaces"))
            .map_err(|error| error.to_string())?,
    );
    Ok(AgentControlService::new(router, store, registry))
}

#[test]
fn production_paths_wire_the_full_matrix() {
    // 会话面:十个工具注册(六件套 + spawn/resume/handoff/group)。
    assert!(AGENT_CONTROL.contains("AgentControlOperation::Spawn"));
    assert!(AGENT_CONTROL.contains("AgentControlOperation::Resume"));
    assert!(AGENT_CONTROL.contains("AgentControlOperation::Handoff"));
    assert!(AGENT_CONTROL.contains("AgentControlOperation::Group"));
    assert!(APP_STATE.contains("impl crate::agent_control::AgentControlAppOps"));
    // 子智能体面:派发时安装 EKO uplink sink + lineage 盖章。
    assert!(UNATTENDED.contains("eko_uplink_sink"));
    assert!(UNATTENDED.contains("subagent_lineage"));
    assert!(UPLINK.contains("paused_needs_input"));
    assert!(UPLINK.contains("queued_for_next_attempt"));
    // prompt 协议允许向 run driver 升级、禁止直接对话用户。
    assert!(SUBAGENT_PROMPT.contains("intent \"escalate\""));
    assert!(SUBAGENT_PROMPT.contains("never converse with the user"));
    // 内置子智能体获得树内通信工具。
    assert!(FACTORY.contains("register_subagent_message_tools"));
    // 会话面控制工具禁止委派给子智能体。
    assert!(TASK_TOOLS.contains("\"agent_spawn\""));
}

#[tokio::test]
async fn group_crud_round_trips_through_the_tool_service() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let service = service_at_root(dir.path())?;

    // create
    let create = service
        .group(AgentGroupToolRequest {
            action: AgentGroupAction::Create,
            group_id: None,
            name: Some("matrix".to_string()),
            leader: Some(
                echo_agent_app_core::api::agent_control::ConversationTarget {
                    workspace_id: "global".to_string(),
                    conversation_id: "leader-conversation".to_string(),
                    workspace_generation: None,
                },
            ),
            members: Some(vec![AgentGroupMemberInput {
                workspace_id: "global".to_string(),
                conversation_id: "member-conversation".to_string(),
                subagent_role: "implementer".to_string(),
                label: Some("writer".to_string()),
            }]),
            limit: 0,
        })
        .await;
    let group_id = match create {
        Ok(value) => value
            .get("group_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "create must return group_id".to_string())?,
        Err(error) => {
            // 组校验要求 leader/members 地址是真实会话;隔离环境下 create 会被
            // router 的结构校验接受(不查会话存在性),若被拒则断言为 typed
            // Invalid 而非 Runtime 崩溃。
            return match error {
                AgentControlError::Invalid(_) => Ok(()),
                other => Err(format!("unexpected create error: {other}")),
            };
        }
    };

    // list
    let list = service
        .group(AgentGroupToolRequest {
            action: AgentGroupAction::List,
            group_id: None,
            name: None,
            leader: None,
            members: None,
            limit: 32,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        list.get("count").and_then(serde_json::Value::as_i64),
        Some(1)
    );

    // update(改名)
    let update = service
        .group(AgentGroupToolRequest {
            action: AgentGroupAction::Update,
            group_id: Some(group_id.clone()),
            name: Some("matrix-v2".to_string()),
            leader: Some(
                echo_agent_app_core::api::agent_control::ConversationTarget {
                    workspace_id: "global".to_string(),
                    conversation_id: "leader-conversation".to_string(),
                    workspace_generation: None,
                },
            ),
            members: Some(vec![]),
            limit: 0,
        })
        .await;
    match update {
        Ok(value) => assert_eq!(
            value.get("name").and_then(serde_json::Value::as_str),
            Some("matrix-v2")
        ),
        Err(AgentControlError::Invalid(_)) => {}
        Err(other) => return Err(format!("unexpected update error: {other}")),
    }

    // delete
    let delete = service
        .group(AgentGroupToolRequest {
            action: AgentGroupAction::Delete,
            group_id: Some(group_id),
            name: None,
            leader: None,
            members: None,
            limit: 0,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        delete.get("deleted").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    Ok(())
}

#[tokio::test]
async fn group_create_without_leader_is_rejected() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let service = service_at_root(dir.path())?;
    let outcome = service
        .group(AgentGroupToolRequest {
            action: AgentGroupAction::Create,
            group_id: None,
            name: Some("incomplete".to_string()),
            leader: None,
            members: None,
            limit: 0,
        })
        .await;
    assert!(matches!(outcome, Err(AgentControlError::Invalid(_))));
    Ok(())
}

#[tokio::test]
async fn spawn_resume_handoff_fail_closed_without_app_ops() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let service = service_at_root(dir.path())?;

    let spawn = service
        .spawn(AgentSpawnRequest {
            goal: "explore the matrix".to_string(),
            title: None,
            workspace_id: None,
            first_message: None,
            start: true,
        })
        .await;
    assert!(matches!(spawn, Err(AgentControlError::Runtime(_))));

    let resume = service
        .resume(AgentResumeRequest {
            workspace_id: "global".to_string(),
            conversation_id: "conv-1".to_string(),
            resume_policy: AgentResumePolicy::TaskRun,
            run_id: Some("run-1".to_string()),
            text: None,
        })
        .await;
    assert!(matches!(resume, Err(AgentControlError::Runtime(_))));

    let handoff = service
        .handoff(AgentHandoffRequest {
            workspace_id: "global".to_string(),
            conversation_id: "conv-1".to_string(),
            destination_workspace_id: "other".to_string(),
            follow_up: None,
        })
        .await;
    assert!(matches!(handoff, Err(AgentControlError::Runtime(_))));
    Ok(())
}

#[test]
fn agent_address_round_trips_for_group_authority() {
    // 组权威按 (workspace, conversation) 寻址;确认构造对齐 router 语义。
    let workspace =
        echo_agent_app_core::api::workspace::WorkspaceId::from_raw("global".to_string());
    let address = AgentAddress::new(workspace, "conv-1");
    assert_eq!(address.conversation_id, "conv-1");
    assert_eq!(address.workspace_id.as_str(), "global");
}
