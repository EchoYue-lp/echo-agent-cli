//! Register the full task-management tool suite on an existing agent.
//!
//! Shared by GUI (`echo-agent-cli/src/tauri/desktop.rs`) and TUI
//! (`echo-agent-cli/src/main.rs`): the primary agent is created before the
//! `TaskRuntimeStore` exists (bootstrap builds it later), so task tools can't
//! be injected at construction time — they're registered post-hoc once the
//! store is ready. Pooled agents instead get the tools via
//! `SharedResources.task_runtime_store`.
//!
//! Registers: TaskCreate/Update/Complete/Skip/List + CreateComplexTask /
//! CheckRunStatus / CancelRun (8 task tools) + `ExecutePlanTool`.
//!
//! TUI/GUI functional parity (AGENTS.md): both entry points call this so the
//! primary agent can drive complex tasks (plan / worker / run lifecycle) via
//! `drive_chat` — TUI is a full Agent, not a lightweight chat.

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::tasks::task_runtime::execute_plan_tool::ExecutePlanTool;
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::task_tools::{
    CancelRunTool, CheckRunStatusTool, CreateComplexTaskTool, TaskCompleteTool, TaskCreateTool,
    TaskListTool, TaskSkipTool, TaskUpdateTool,
};

/// Register the task-management tools + `execute_plan` (with store) on
/// `agent_handle`. See module docs for why this is post-hoc.
pub async fn register_task_tools_on_agent(
    agent_handle: &AgentHandle,
    store: Arc<TaskRuntimeStore>,
) {
    let added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(TaskCreateTool {
                store: store.clone(),
            }));
            agent.add_tool(Box::new(TaskUpdateTool {
                store: store.clone(),
            }));
            agent.add_tool(Box::new(TaskCompleteTool {
                store: store.clone(),
            }));
            agent.add_tool(Box::new(TaskSkipTool {
                store: store.clone(),
            }));
            agent.add_tool(Box::new(TaskListTool {
                store: store.clone(),
            }));
            // Phase B3: agent-autonomous complex-task tools. These read
            // pool/store/sink from the chat turn's task_local
            // (current_chat_resources), so no store injection is needed.
            agent.add_tool(Box::new(CreateComplexTaskTool));
            agent.add_tool(Box::new(CheckRunStatusTool));
            agent.add_tool(Box::new(CancelRunTool));
            true
        })
        .await;
    if added {
        tracing::info!("Registered 8 task-management tools on primary agent");
    } else {
        tracing::warn!(
            "Failed to register task-management tools on primary agent (write lock poisoned)"
        );
    }

    // Also register execute_plan tool (only on main agent per §10.2).
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
        tracing::info!("Registered execute_plan tool on primary agent");
    } else {
        tracing::warn!(
            "Failed to register execute_plan tool on primary agent (write lock poisoned)"
        );
    }

    // (delegate_readonly 工具已删除,其单步派发能力由 execute_plan 的 inline task
    // 参数吸收。无需在此 re-register。)
}
