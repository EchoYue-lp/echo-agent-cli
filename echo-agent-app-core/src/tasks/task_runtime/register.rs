//! Register the full task-management tool suite on an existing agent.
//!
//! Shared by GUI (`echo-agent-cli/src/tauri/desktop.rs`) and TUI
//! (`echo-agent-cli/src/main.rs`): the primary agent is created before the
//! `TaskRuntimeStore` exists (bootstrap builds it later), so task tools can't
//! be injected at construction time — they're registered post-hoc once the
//! store is ready. Pooled agents instead get the tools via
//! `SharedResources.task_runtime_store`.
//!
//! Registers the revisioned PlanCreate/PlanPatch/TaskList contract plus
//! CreateComplexTask / CheckRunStatus / CancelRun and `plan_execute`.
//!
//! TUI/GUI functional parity (AGENTS.md): both entry points call this so the
//! primary agent can drive complex tasks (plan / subagent / run lifecycle) via
//! `drive_chat` — TUI is a full Agent, not a lightweight chat.

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::tasks::task_runtime::execute_plan_tool::ExecutePlanTool;
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::task_tools::{
    CancelRunTool, CheckRunStatusTool, CreateComplexTaskTool, PlanCapabilityCatalog, PlanPatchTool,
    TaskCreateTool, TaskListTool,
};

/// Register the task-management tools + `plan_execute` (with store) on
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
    let capabilities = Arc::new(PlanCapabilityCatalog::new(
        subagent_catalog.clone(),
        tool_names,
    ));
    let added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(TaskCreateTool {
                store: store.clone(),
                capabilities: capabilities.clone(),
            }));
            agent.add_tool(Box::new(PlanPatchTool {
                store: store.clone(),
                capabilities: capabilities.clone(),
            }));
            agent.add_tool(Box::new(TaskListTool {
                store: store.clone(),
            }));
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

    // Also register plan_execute tool (only on main agent per §10.2).
    // Use ParallelReadonlyDelegation as the default route; the route is
    // resolved per-run by the router at the orchestration layer.
    let tool = ExecutePlanTool::new(store.clone(), agent_handle.clone());
    let ep_added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(tool));
            true
        })
        .await;
    if ep_added {
        tracing::info!("Registered plan_execute tool on primary agent");
    } else {
        tracing::warn!(
            "Failed to register plan_execute tool on primary agent (write lock poisoned)"
        );
    }

    // 单个临时子任务由 agent_tool 负责;plan_execute 只执行已物化的正式 DAG。
}
