//! execute_plan 工具: L1 主 agent 把拆好的 plan 交给 L2 run_dag 并行执行。
//!
//! # 设计意图 (spec §3.1.1)
//!
//! 主 agent 通过 task_create 拆完 plan 后显式调用本工具, 触发 execute_run
//! (L2 wave 调度)。这是 L1→L2 的显式衔接, 对齐 Claude Code "拆完 plan 再执行"
//! 两阶段模型, 避免边拆边跑退化串行。
//!
//! # 铁律 (spec §10)
//!
//! - **§10.1**: `execute` 必须 `.await` `execute_run` 返回的 `RunOutcome`,
//!   不得 fire-and-forget。`cancel` 从 task_local `CURRENT_CANCEL` 透传进
//!   `execute_run`。
//! - **§10.2**: 本工具只注册在主 agent, worker 绝不注册 (物理上防止 L3 子 agent
//!   回流 L2 造成死锁)。
//! - **§10.5**: ComplexRuntime 路径下, 首次调用先 transition `Paused` 并 await
//!   `Notify` (由 resume_run 触发), 恢复后才调用 `execute_run`。

use std::sync::Arc;

use echo_agent::error;
use echo_agent::tools::{Tool, ToolParameters, ToolResult};
use futures::future::BoxFuture;

use super::executor::{RunOutcome, execute_run};
use super::router::TaskRouteKind;
use super::store::TaskRuntimeStore;
use super::types::TaskRunStatus;
use crate::agent_handle::AgentHandle;

/// L1→L2 桥接工具: 把 plan 提交给 run_dag 并行调度器。
///
/// 字段说明:
/// - `store`: TaskRuntimeStore (用来读/写 run 状态)
/// - `primary_agent`: AgentHandle (传给 execute_run 做 worker 调度)
/// - `route`: TaskRouteKind, 决定是否走 ComplexRuntime 审批闭环 (§10.5)
/// - `approval_signal`: ComplexRuntime 模式下等 resume_run 唤醒的 channel
pub struct ExecutePlanTool {
    store: Arc<TaskRuntimeStore>,
    primary_agent: AgentHandle,
    route: TaskRouteKind,
    /// ComplexRuntime 审批唤醒通道 (spec §10.5)。
    /// 首次调用时若 `route == ComplexRuntime`, 工具 transition `Paused`
    /// 并等待此 signal; 外部调用 `notify_one()` 恢复。
    approval_signal: Arc<tokio::sync::Notify>,
}

impl ExecutePlanTool {
    pub fn new(
        store: Arc<TaskRuntimeStore>,
        primary_agent: AgentHandle,
        route: TaskRouteKind,
    ) -> Self {
        Self {
            store,
            primary_agent,
            route,
            approval_signal: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Expose the approval signal so the frontend or an orchestration layer
    /// can call `notify_one()` to resume a ComplexRuntime run after the user
    /// has approved the plan.
    pub fn approval_signal(&self) -> Arc<tokio::sync::Notify> {
        self.approval_signal.clone()
    }
}

impl Tool for ExecutePlanTool {
    fn name(&self) -> &str {
        "execute_plan"
    }

    fn description(&self) -> &str {
        "把你用 task_create 拆好的计划交给并行执行引擎 (run_dag) 执行。\
         引擎会按任务的依赖关系 (depends_on) 自动并行/串行调度。\
         调用此工具后, 等待返回的执行结果, 再据此写最终答案给用户。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn execute<'a>(&'a self, _params: ToolParameters) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            // ── 从 task_local 读取 run_id (§10.1) ──
            let run_id = match super::task_tools::require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };

            // ── §10.1: cancel 透传 ──
            let cancel = super::task_tools::CURRENT_CANCEL
                .try_with(|c| c.clone())
                .unwrap_or_else(|_| tokio_util::sync::CancellationToken::new());

            // ── §10.5: ComplexRuntime 审批闭环 ──
            if self.route == TaskRouteKind::ComplexRuntime {
                if let Err(e) = self.store.transition_run(&run_id, TaskRunStatus::Paused) {
                    return Ok(ToolResult::error(format!("Failed to pause run: {e}")));
                }
                // Register the signal so resume_task_run can find it.
                super::task_tools::register_approval_signal(&run_id, self.approval_signal.clone());
                // 等待 resume_run 通过 approval_signal 唤醒
                self.approval_signal.notified().await;
                // Remove the signal -- the run has been woken.
                super::task_tools::remove_approval_signal(&run_id);
                if let Err(e) = self.store.transition_run(&run_id, TaskRunStatus::Running) {
                    return Ok(ToolResult::error(format!("Failed to resume run: {e}")));
                }
            }

            // ── §10.1: 必须 await RunOutcome, 不得 fire-and-forget ──
            let outcome = execute_run(
                self.store.clone(),
                Some(self.primary_agent.clone()),
                None, // reviewer_llm — 暂时 None, 后续由上层配置
                None, // layer_manager — 暂时 None
                None, // run_store — 暂时 None
                None, // trace_sink — L2 内部按需建
                &run_id,
                String::new(), // cache_user_id — 暂时空, 后续从 agent config 读取
                cancel,
            )
            .await;

            match outcome {
                Ok(RunOutcome::Completed) => Ok(ToolResult::success("计划执行完成。")),
                Ok(RunOutcome::Cancelled) => Ok(ToolResult::success("计划执行被取消。")),
                Ok(RunOutcome::Failed {
                    failed_task_id,
                    error,
                }) => Ok(ToolResult::success(format!(
                    "计划执行失败 (任务 {failed_task_id}): {error}。可调整计划后重试。"
                ))),
                Ok(RunOutcome::Paused {
                    failed_task_id,
                    error,
                }) => Ok(ToolResult::success(format!(
                    "计划因任务 {failed_task_id} 失败而暂停: {error}。"
                ))),
                Err(e) => Ok(ToolResult::error(format!("execute_plan 失败: {e}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::prelude::*;
    use echo_agent::tools::ToolParameters;

    /// 验证无 task_local run_id 时 execute_plan 返回 error。
    #[tokio::test]
    async fn execute_plan_requires_run_id() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for execute_plan tool")
            .build()
            .expect("Failed to create test agent");
        let handle = crate::agent_handle::AgentHandle::new(agent);
        let tool = ExecutePlanTool::new(store, handle, TaskRouteKind::ParallelReadonlyDelegation);
        let result = tool.execute(ToolParameters::default()).await.unwrap();
        assert!(
            !result.success,
            "expected error but got success: {}",
            result.output
        );
    }

    /// 验证 tool 的 name/description/parameters 基本属性。
    #[test]
    fn basic_properties() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for execute_plan tool")
            .build()
            .expect("Failed to create test agent");
        let handle = crate::agent_handle::AgentHandle::new(agent);
        let tool = ExecutePlanTool::new(store, handle, TaskRouteKind::ParallelReadonlyDelegation);
        assert_eq!(tool.name(), "execute_plan");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters().is_object());
    }
}
