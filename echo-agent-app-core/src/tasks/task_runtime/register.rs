//! Register the full task-management tool suite on an existing agent.
//!
//! Shared by GUI (`echo-agent-cli/src/tauri/desktop.rs`) and TUI
//! (`echo-agent-cli/src/main.rs`): the primary agent is created before the
//! `TaskRuntimeStore` exists (bootstrap builds it later), so task tools can't
//! be injected at construction time — they're registered post-hoc once the
//! store is ready. Pooled agents instead get the tools via
//! `SharedResources.task_runtime_store`.
//!
//! Registers the revisioned TaskCreate/TaskUpdate/TaskList contract plus
//! CreateComplexTask / CheckRunStatus / CancelRun and `task_execute`.
//!
//! TUI/GUI functional parity (AGENTS.md): both entry points call this so the
//! primary agent can drive complex tasks (plan / subagent / run lifecycle) via
//! `drive_chat` — TUI is a full Agent, not a lightweight chat.

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::task_execute_tool::ExecuteTaskTool;
use crate::tasks::task_runtime::task_tools::{
    CancelRunTool, CheckRunStatusTool, CreateComplexTaskTool, TaskCapabilityCatalog,
};

/// Build EKO's revision service from the live Agent capability catalog.
pub async fn task_revision_service_for_agent(
    agent_handle: &AgentHandle,
    store: Arc<TaskRuntimeStore>,
) -> Arc<echo_agent::tasks::TaskRevisionService> {
    let tool_names = agent_handle.read(|agent| agent.tool_names()).await;
    let registry = agent_handle
        .read(|agent| agent.subagent_registry().clone())
        .await;
    let registered_subagents = registry.list_available().await;
    let subagent_catalog = Arc::new(
        crate::subagent_loader::SubagentCatalogSnapshot::from_registered(&registered_subagents),
    );
    let capabilities = Arc::new(TaskCapabilityCatalog::new(subagent_catalog, tool_names));
    super::revisioned_adapter::build_eko_task_revision_service(store, capabilities)
}

/// Register the task-management tools + `task_execute` (with store) on
/// `agent_handle`. See module docs for why this is post-hoc.
pub async fn register_task_tools_on_agent(
    agent_handle: &AgentHandle,
    store: Arc<TaskRuntimeStore>,
) {
    let tool_names = agent_handle.read(|agent| agent.tool_names()).await;
    let registry = agent_handle
        .read(|agent| agent.subagent_registry().clone())
        .await;
    let registered_subagents = registry.list_available().await;
    let subagent_catalog = Arc::new(
        crate::subagent_loader::SubagentCatalogSnapshot::from_registered(&registered_subagents),
    );
    let capabilities = Arc::new(TaskCapabilityCatalog::new(
        subagent_catalog.clone(),
        tool_names,
    ));
    let revision_service =
        super::revisioned_adapter::build_eko_task_revision_service(store.clone(), capabilities);
    let added = agent_handle
        .write(|agent| {
            echo_agent::tasks::register_task_tools(agent, revision_service);
            // Phase B3: agent-autonomous complex-task tools. These read
            // pool/store/sink from the chat turn's task_local
            // (current_chat_resources), so no store injection is needed.
            agent.add_tool(Box::new(CreateComplexTaskTool {
                subagent_catalog: subagent_catalog.clone(),
            }));
            agent.add_tool(Box::new(CheckRunStatusTool));
            agent.add_tool(Box::new(CancelRunTool));
            true
        })
        .await;
    if added {
        tracing::info!("Registered revisioned task-management tools on primary agent");
    } else {
        tracing::warn!(
            "Failed to register task-management tools on primary agent (write lock poisoned)"
        );
    }

    // Also register task_execute tool (only on main agent per §10.2).
    // Use ParallelReadonlyDelegation as the default route; the route is
    // resolved per-run by the router at the orchestration layer.
    let tool = ExecuteTaskTool::new(store.clone(), agent_handle.clone());
    let ep_added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(tool));
            true
        })
        .await;
    if ep_added {
        tracing::info!("Registered task_execute tool on primary agent");
    } else {
        tracing::warn!(
            "Failed to register task_execute tool on primary agent (write lock poisoned)"
        );
    }

    // A one-node task graph and a dependency DAG share this execution path.
    // `agent_tool` remains only for ephemeral side work with no TaskRun.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registration_replaces_default_task_store_with_eko_task_store()
    -> std::result::Result<(), String> {
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
            .system_prompt("unified task api test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let before = handle.read(|agent| agent.tool_names()).await;
        for expected in ["task_create", "task_update", "task_list"] {
            assert!(before.iter().any(|name| name == expected));
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        register_task_tools_on_agent(&handle, store).await;

        let after = handle.read(|agent| agent.tool_names()).await;
        for expected in ["task_create", "task_update", "task_list", "task_execute"] {
            assert!(after.iter().any(|name| name == expected));
        }
        Ok(())
    }
}
