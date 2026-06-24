//! delegate_readonly 工具:让主 agent 在 ReAct 循环里派只读 worker。
//!
//! 设计:工具持 AgentHandle,execute 时 read_async 调
//! delegate_to_agent_with_parent_and_cancel。run_id 和 cancel 从 task_local 读取。
//!
//! 参考:echo-agent 的 AgentDispatchTool(hold Arc<SubagentExecutor> + cancel handle)。

use echo_agent::agent::AgentHandle;
use echo_agent::error;
use echo_agent::tools::{Tool, ToolParameters, ToolResult};
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

/// 让主 agent 在 ReAct 循环里派只读 worker 跑独立 ReAct,返回 summary。
pub struct DelegateReadonlyTool {
    pub agent_handle: AgentHandle,
}

impl DelegateReadonlyTool {
    pub fn new(agent_handle: AgentHandle) -> Self {
        Self { agent_handle }
    }
}

impl Tool for DelegateReadonlyTool {
    fn name(&self) -> &str {
        "delegate_readonly"
    }

    fn description(&self) -> &str {
        "派一个只读 worker(独立 ReAct agent)执行任务并返回 summary。\
         用于调研/审查/分析类子任务。worker 跑独立 ReAct,不修改文件,返回结论给你。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_role": {
                    "type": "string",
                    "description": "worker 角色(如 project_explorer, summary_writer, code_reviewer, test_planner)"
                },
                "task": {
                    "type": "string",
                    "description": "给 worker 的任务 prompt"
                }
            },
            "required": ["agent_role", "task"]
        })
    }

    fn execute<'a>(&'a self, params: ToolParameters) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            // 从 task_local 拿 run_id(与 task_* 工具一致)
            let run_id = match super::task_tools::require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };

            let role = params
                .get("agent_role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task = params
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if role.is_empty() || task.is_empty() {
                return Ok(ToolResult::error("agent_role 和 task 不能为空"));
            }

            // 从 task_local 拿 cancel token
            let cancel = super::task_tools::CURRENT_CANCEL
                .try_with(|c| c.clone())
                .unwrap_or_else(|_| CancellationToken::new());

            // Read delegate depth from task_local and increment by 1 for
            // the next delegation level. The framework's MAX_DELEGATE_DEPTH=3
            // guards against runaway recursion (see executor.rs).
            let depth = super::task_tools::CURRENT_DELEGATE_DEPTH
                .try_with(|d| d.get() + 1)
                .unwrap_or(0);

            let handle = self.agent_handle.clone();
            let result = handle
                .read_async(|a| {
                    Box::pin(async move {
                        a.delegate_to_agent_with_parent_and_cancel(
                            &role, &task, &run_id, cancel, depth,
                        )
                        .await
                    })
                })
                .await;

            match result {
                Ok(subagent_result) => Ok(ToolResult::success(subagent_result.output)),
                Err(e) => Ok(ToolResult::error(format!("delegate_readonly 失败: {e}"))),
            }
        })
    }
}

/// Register `delegate_readonly` tool on an agent via its handle.
/// Call this AFTER the agent is wrapped in an AgentHandle and task_runtime_store
/// is available (run_id/cancel via task_local).
pub async fn register_delegate_readonly_on_handle(handle: &AgentHandle) {
    let tool = DelegateReadonlyTool::new(handle.clone());
    handle
        .write(|a| {
            a.add_tool(Box::new(tool));
        })
        .await;
    tracing::info!("Registered delegate_readonly tool on agent");
}
