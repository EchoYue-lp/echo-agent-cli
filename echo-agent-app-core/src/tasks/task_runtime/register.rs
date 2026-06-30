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
//! CheckRunStatus / CancelRun (8 task tools) + `ExecutePlanTool`, and
//! re-registers `delegate_readonly` WITH the store (replacing the store-less
//! one from bootstrap, so the "plan exists → refuse, tell LLM to use
//! execute_plan" interception becomes effective).
//!
//! TUI/GUI functional parity (AGENTS.md): both entry points call this so the
//! primary agent can drive complex tasks (plan / worker / run lifecycle) via
//! `drive_chat` — TUI is a full Agent, not a lightweight chat.

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::tasks::task_runtime::delegate_readonly_tool::DelegateReadonlyTool;
use crate::tasks::task_runtime::execute_plan_tool::ExecutePlanTool;
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::task_tools::{
    CancelRunTool, CheckRunStatusTool, CreateComplexTaskTool, TaskCompleteTool, TaskCreateTool,
    TaskListTool, TaskSkipTool, TaskUpdateTool,
};

/// Register the task-management tools + `execute_plan` + `delegate_readonly`
/// (with store) on `agent_handle`. See module docs for why this is post-hoc.
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

    // Re-register delegate_readonly WITH the store. At bootstrap (runtime.rs)
    // the store didn't exist yet, so delegate_readonly was registered with
    // store=None — which disables the "plan exists → refuse, tell LLM to use
    // execute_plan" interception. Replacing it here (after the store exists)
    // makes the interception effective, so the main agent is forced down the
    // execute_plan path when it has a plan. (根因①修复)
    let removed = agent_handle
        .write(|agent| agent.remove_tool("delegate_readonly").is_some())
        .await;
    if removed {
        tracing::debug!("Removed store-less delegate_readonly from primary agent");
    }
    let dr_tool = DelegateReadonlyTool::new(agent_handle.clone()).with_store(store.clone());
    let dr_added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(dr_tool));
            true
        })
        .await;
    if dr_added {
        tracing::info!("Re-registered delegate_readonly WITH store on primary agent");
    } else {
        tracing::warn!("Failed to re-register delegate_readonly with store");
    }
}
