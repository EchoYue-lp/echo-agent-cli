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

use super::executor::{RunOutcome, execute_run, preflight_unattended_plan};
use super::router::TaskRouteKind;
use super::store::TaskRuntimeStore;
use super::types::{
    AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPlan, TaskRunStatus,
    UnattendedWriteMode,
};
use crate::agent_handle::AgentHandle;

/// L1→L2 桥接工具: 把 plan 提交给 run_dag 并行调度器。
///
/// 字段说明:
/// - `store`: TaskRuntimeStore (用来读/写 run 状态)
/// - `primary_agent`: AgentHandle (传给 execute_run 做 worker 调度)
/// - `approval_signal`: ComplexRuntime 模式下等 resume_run 唤醒的 channel
pub struct ExecutePlanTool {
    store: Arc<TaskRuntimeStore>,
    primary_agent: AgentHandle,
    /// ComplexRuntime 审批唤醒通道 (spec §10.5)。
    /// 首次调用时若 route == ComplexRuntime, 工具 transition `Paused`
    /// 并等待此 signal; 外部调用 `notify_one()` 恢复。
    approval_signal: Arc<tokio::sync::Notify>,
    /// D7 stage 2: unattended write mode for this tool's runs. Determines
    /// whether the CP A preflight loosens its write ban (Worktree/InPlace)
    /// or keeps stage-1 rejection (Disabled). Also scoped into a task-local
    /// so CP B preflight in `execute_task` can read it.
    write_mode: UnattendedWriteMode,
}

impl ExecutePlanTool {
    pub fn new(store: Arc<TaskRuntimeStore>, primary_agent: AgentHandle) -> Self {
        Self::with_write_mode(store, primary_agent, UnattendedWriteMode::default())
    }

    /// Construct with an explicit write mode (D7 stage 2). Production callers
    /// that have access to app config should use this to pass the configured
    /// mode; `new()` defaults to `Worktree` (the spec default).
    pub fn with_write_mode(
        store: Arc<TaskRuntimeStore>,
        primary_agent: AgentHandle,
        write_mode: UnattendedWriteMode,
    ) -> Self {
        Self {
            store,
            primary_agent,
            approval_signal: Arc::new(tokio::sync::Notify::new()),
            write_mode,
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

            // ── 兜底: 若主 agent 跳过了 task_create 直接调 execute_plan ──
            // LLM 可能不遵守 system prompt 的两阶段顺序。若 plan 为空,
            // 从 run goal 动态生成一个单 task plan,保证执行始终经过 run_dag
            // (有 wave 调度 + 信号量限流 + 失败传播保护),不会退化成裸 delegate_readonly。
            let plan_exists = self
                .store
                .get_plan(&run_id)
                .ok()
                .flatten()
                .map(|p| !p.tasks.is_empty())
                .unwrap_or(false);
            if !plan_exists {
                let goal = self
                    .store
                    .get_run(&run_id)
                    .ok()
                    .flatten()
                    .map(|r| r.goal)
                    .unwrap_or_default();
                let task_id = format!("auto_{}", uuid::Uuid::new_v4().as_simple());
                let task = PlanTask {
                    id: task_id.clone(),
                    title: goal.chars().take(80).collect(),
                    description: goal.clone(),
                    kind: PlanTaskKind::ReadOnlyReview,
                    agent_role: "project_explorer".to_string(),
                    ..Default::default()
                };
                let plan = TaskPlan {
                    plan_id: uuid::Uuid::new_v4().to_string(),
                    run_id: run_id.clone(),
                    domain_profile: DomainProfile::General,
                    goal: goal.clone(),
                    assumptions: Vec::new(),
                    risks: Vec::new(),
                    execution_mode: ExecutionMode::Parallel,
                    tasks: vec![task],
                };
                if let Err(e) = self.store.attach_plan(&plan) {
                    return Ok(ToolResult::error(format!(
                        "execute_plan: 自动生成 plan 失败: {e}"
                    )));
                }
            }

            // ── U1c phase-1: read attended_mode once for CP A + approval gate ──
            let attended_mode = self
                .store
                .get_run(&run_id)
                .ok()
                .flatten()
                .map(|r| r.attended_mode)
                .unwrap_or_default();

            // ── U1c phase-1 CP A: unattended preflight ──
            // Only when attended_mode=Unattended: scan the full plan for
            // write tasks / write tools / shell commands and terminal-fail
            // on violation. Chat runs (Attended) skip this entirely.
            if attended_mode == AttendedMode::Unattended
                && let Some(ref plan) = self.store.get_plan(&run_id).ok().flatten()
                && let Err(rejection) = preflight_unattended_plan(&plan.tasks, self.write_mode)
            {
                let _ = self.store.transition_run(&run_id, TaskRunStatus::Failed);
                let _ = self.store.note(
                    &run_id,
                    None,
                    &format!("CP A preflight rejected: {}", rejection.reason),
                );
                return Ok(ToolResult::error(format!(
                    "Unattended run rejected by preflight: {}. \
                     ReadOnlyPlanNoShell mode only allows read tasks, \
                     read tools, and no shell/test commands.",
                    rejection.reason
                )));
            }

            // ── §10.5: ComplexRuntime 审批闭环 ──
            // Route is read from the persisted run record so the tool struct
            // doesn't need it baked in at construction time.
            let route_str = self
                .store
                .get_run_route(&run_id)
                .unwrap_or_default()
                .unwrap_or_default();
            let route = TaskRouteKind::from_str(&route_str)
                .unwrap_or(TaskRouteKind::ParallelReadonlyDelegation);
            if route == TaskRouteKind::ComplexRuntime {
                // U1c phase-1: Skip approval for unattended runs.
                // Precise condition (spec §4.1 v2): Unattended + ReadOnlyPlanNoShell
                // + preflight passed (CP A above already returned Ok). In stage 1,
                // all unattended runs use ReadOnlyPlanNoShell, so the mode check
                // is sufficient. Without this skip, the run would deadlock waiting
                // for a human who isn't there.
                if attended_mode != AttendedMode::Unattended {
                    if let Err(e) = self.store.transition_run(&run_id, TaskRunStatus::Paused) {
                        return Ok(ToolResult::error(format!("Failed to pause run: {e}")));
                    }
                    // Register the signal so resume_task_run can find it.
                    super::task_tools::register_approval_signal(
                        &run_id,
                        self.approval_signal.clone(),
                    );
                    // 等待 resume_run 通过 approval_signal 唤醒
                    self.approval_signal.notified().await;
                    // Remove the signal -- the run has been woken.
                    super::task_tools::remove_approval_signal(&run_id);
                    if let Err(e) = self.store.transition_run(&run_id, TaskRunStatus::Running) {
                        return Ok(ToolResult::error(format!("Failed to resume run: {e}")));
                    }
                }
            }

            // ── Read trace_sink from task_local ──
            // (stage4 P4.1) cache_user_id read from single source inside
            // execute_run/review_task — no longer threaded.
            let trace_sink = super::task_tools::CURRENT_TRACE_SINK
                .try_with(|s| s.clone())
                .ok()
                .flatten();

            // ── §10.1: 必须 await RunOutcome, 不得 fire-and-forget ──
            // G3 fix: read run_store from the primary agent instead of passing
            // None. execute_run uses it to persist trace Run records (token
            // usage, status). Without it, the execute_plan path silently drops
            // trace persistence (event-wiring #1残留).
            let run_store = self.primary_agent.read(|a| a.run_store.clone()).await;
            // D7 stage 2: scope the write mode into a task-local so CP B
            // preflight in `execute_task` (deep inside execute_run → run_dag)
            // can read it without threading the mode through every signature.
            let write_mode = self.write_mode;
            let outcome = super::task_tools::CURRENT_UNATTENDED_WRITE_MODE
                .scope(write_mode, async {
                    execute_run(
                        self.store.clone(),
                        Some(self.primary_agent.clone()),
                        None, // reviewer_llm — 暂时 None, 后续由上层配置
                        None, // layer_manager — 暂时 None
                        run_store,
                        trace_sink,
                        &run_id,
                        cancel,
                    )
                    .await
                })
                .await;

            match outcome {
                Ok(RunOutcome::Completed) => {
                    // 把各 worker 的 summary 拼进返回文本,给主 agent 写最终答案的
                    // 素材(否则主 agent 只拿到一句"计划执行完成",无法产出实质答案)。
                    let summaries = self
                        .store
                        .list_todos(&run_id)
                        .map(|todos| {
                            todos
                                .into_iter()
                                .filter(|t| t.summary.as_deref().is_some_and(|s| !s.is_empty()))
                                .map(|t| {
                                    format!(
                                        "## {} ({})\n{}",
                                        t.title,
                                        t.owner_agent.as_deref().unwrap_or("worker"),
                                        t.summary.as_deref().unwrap_or("")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                        .unwrap_or_default();
                    Ok(ToolResult::success(format!(
                        "计划执行完成。各 worker 的产出如下,请基于这些内容撰写最终答案:\n\n{summaries}"
                    )))
                }
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

    fn execute_with_context<'a>(
        &'a self,
        params: echo_agent::tools::ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            super::task_tools::scoped_with_ctx_run_id(ctx, || self.execute(params)).await
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
        let tool = ExecutePlanTool::new(store, handle);
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
        let tool = ExecutePlanTool::new(store, handle);
        assert_eq!(tool.name(), "execute_plan");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters().is_object());
    }
}
