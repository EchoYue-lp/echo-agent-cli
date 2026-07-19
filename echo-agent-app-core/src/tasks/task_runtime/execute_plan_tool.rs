//! plan_execute 工具: L1 主 agent 把拆好的 plan 交给 L2 run_dag 并行执行。
//!
//! # 设计意图 (spec §3.1.1)
//!
//! 主 agent 通过 plan_create 拆完 plan 后显式调用本工具, 触发 execute_run
//! (L2 wave 调度)。这是 L1→L2 的显式衔接, 对齐 Claude Code "拆完 plan 再执行"
//! 两阶段模型, 避免边拆边跑退化串行。
//!
//! # 铁律 (spec §10)
//!
//! - **§10.1**: `execute` 必须 `.await` `execute_run` 返回的 `RunOutcome`,
//!   不得 fire-and-forget。`cancel` 从 task_local `CURRENT_CANCEL` 透传进
//!   `execute_run`。
//! - **§10.2**: 本工具只注册在主 agent, subagent 绝不注册 (物理上防止 L3 子 agent
//!   回流 L2 造成死锁)。
//! - **§10.5**: ComplexRuntime 路径下, 首次调用先 transition `Paused` 并 await
//!   `Notify` (由 resume_run 触发), 恢复后才调用 `execute_run`。

use std::sync::{Arc, LazyLock};

use dashmap::DashMap;
use echo_agent::error;
use echo_agent::tools::{Tool, ToolParameters, ToolResult};
use futures::future::BoxFuture;
use tokio::sync::Mutex as TokioMutex;

use super::executor::{RunOutcome, execute_run, preflight_unattended_plan};
use super::store::TaskRuntimeStore;
use super::types::{
    AttendedMode, TaskExecutionSummary, TaskRunStatus, TodoItem, TodoStatus, UnattendedWriteMode,
};
use crate::agent_handle::AgentHandle;

/// One active execute_run driver per run_id.
///
/// Duplicate plan_execute calls can be emitted by the model or a resumed turn.
/// Serializing per run keeps one authoritative DAG driver and lets later calls
/// re-read terminal task state instead of dispatching completed nodes again.
static RUN_EXECUTION_LOCKS: LazyLock<DashMap<String, Arc<TokioMutex<()>>>> =
    LazyLock::new(DashMap::new);

/// RAII guard: 持有 run 的执行锁, Drop 时同时从 `RUN_EXECUTION_LOCKS` 删除该 entry。
///
/// 修复 P1-1: 此前 entry 只 insert 不 remove, 每个唯一 run_id 永久占内存,
/// Tauri 长期运行数月后累积数千无用 entry。用 guard 封装保证无论从哪条路径
/// 返回 (提前 ? / 正常 return), lock 释放的同时 entry 被清理。
///
/// 用 `OwnedMutexGuard` (来自 `Arc<TokioMutex>::lock_owned`) 而非 `MutexGuard`,
/// 这样 guard 不借用任何外部引用, 可自由移动、放入结构体, 无自引用 / 生命周期问题。
/// Drop 顺序由字段声明顺序保证: Rust 按声明逆序 drop, 即先 drop `_guard` (释放锁),
/// 再 drop `_lock_owned`(map 删除由显式 Drop impl 完成)。
struct RunExecutionGuard {
    /// Owned guard 不借外部引用, 持有它即持有锁。Option 包裹以便 Drop 里 take。
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    run_id: String,
}

impl Drop for RunExecutionGuard {
    fn drop(&mut self) {
        // 必须先释放锁再删 entry, 否则在"entry 已删 + 锁仍持有"的窗口内,
        // 另一个 acquire 会建新 lock 并进入临界区, 破坏 per-run 互斥语义。
        // take() 出 guard 显式 drop → 释放锁, 然后才删 map entry。
        if let Some(g) = self._guard.take() {
            drop(g);
        }
        let _ = RUN_EXECUTION_LOCKS.remove(&self.run_id);
    }
}

/// 获取 (并等待) 某个 run 的执行锁, 返回 RAII guard 负责释放锁 + 清理 entry。
async fn acquire_run_execution_lock(run_id: &str) -> RunExecutionGuard {
    let lock = RUN_EXECUTION_LOCKS
        .entry(run_id.to_string())
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone();
    // lock_owned 需要 Arc<TokioMutex>, 返回 OwnedMutexGuard (不绑引用, 可自由移动)。
    let guard = lock.lock_owned().await;
    RunExecutionGuard {
        _guard: Some(guard),
        run_id: run_id.to_string(),
    }
}

/// L1→L2 桥接工具: 把 plan 提交给 run_dag 并行调度器。
///
/// 字段说明:
/// - `store`: TaskRuntimeStore (用来读/写 run 状态)
/// - `primary_agent`: AgentHandle (传给 execute_run 做 subagent 调度)
pub struct ExecutePlanTool {
    store: Arc<TaskRuntimeStore>,
    primary_agent: AgentHandle,
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
            write_mode,
        }
    }
}

impl Tool for ExecutePlanTool {
    fn name(&self) -> &str {
        "plan_execute"
    }

    fn description(&self) -> &str {
        "Execute the current persisted PlanTask DAG with subagents. First create every node with one plan_create call per task, then call task_list. Pass the exact returned task count as expected_task_count. The runtime rejects missing or partial plans. For one ad-hoc isolated subtask use agent_tool instead; plan_execute never accepts an inline task."
    }

    /// plan_execute 派 subagent 跑独立 ReAct(延迟远高于普通文件/shell 工具)。
    /// 豁免并行批次总超时,避免它占满批次预算导致同批其他工具被提前取消;
    /// execute_run 内部有信号量 + subagent 600s per-dispatch 超时兜底。
    fn exempt_from_batch_timeout(&self) -> bool {
        true
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expected_task_count": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Exact Tasks (N) count returned by task_list after every plan_create call has completed. Execution is rejected when the persisted plan count differs."
                }
            },
            "required": ["expected_task_count"]
        })
    }

    fn execute<'a>(&'a self, params: ToolParameters) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match super::task_tools::require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            tracing::info!(
                run_id = %run_id,
                param_keys = ?params.keys().cloned().collect::<Vec<_>>(),
                "plan_execute: start"
            );

            if params.contains_key("task") {
                return Ok(ToolResult::error(
                    "plan_execute no longer accepts an inline task. Use agent_tool for one isolated subtask, or create every formal PlanTask with plan_create before executing the DAG.",
                ));
            }
            let expected_task_count = params
                .get("expected_task_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
                .filter(|count| *count > 0);
            let Some(expected_task_count) = expected_task_count else {
                return Ok(ToolResult::error(
                    "plan_execute requires expected_task_count from the latest task_list result.",
                ));
            };
            let materialized_plan = match self.store.get_plan(&run_id) {
                Ok(Some(plan)) if !plan.tasks.is_empty() => plan,
                Ok(_) => {
                    return Ok(ToolResult::error(
                        "plan_execute requires a non-empty persisted plan. Create one concrete PlanTask per intended subagent with plan_create, then call task_list.",
                    ));
                }
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "Failed to read the persisted plan before execution: {error}"
                    )));
                }
            };
            let plan_task_count = materialized_plan.tasks.len();
            if plan_task_count != expected_task_count {
                return Ok(ToolResult::error(format!(
                    "Plan task count mismatch: task_list declared {expected_task_count}, but the persisted plan contains {plan_task_count}. Finish all plan_create calls, run task_list again, and only then call plan_execute."
                )));
            }

            // ── §10.1: cancel 透传 ──
            let cancel = super::task_tools::CURRENT_CANCEL
                .try_with(|c| c.clone())
                .unwrap_or_else(|_| tokio_util::sync::CancellationToken::new());

            // ── Read attended mode once for unattended preflight ──
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
                && let Err(rejection) =
                    preflight_unattended_plan(&materialized_plan.tasks, self.write_mode)
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

            // Route is diagnostic policy metadata. Plan review/approval is not
            // a TaskRun state transition; risky tools remain governed by the
            // shared permission/HITL contract during execution.
            let route_str = self
                .store
                .get_run_route(&run_id)
                .unwrap_or_default()
                .unwrap_or_default();

            // ── Read trace_sink from task_local ──
            // (stage4 P4.1) cache_user_id read from single source inside
            // execute_run/review_task — no longer threaded.
            let trace_sink = super::task_tools::CURRENT_TRACE_SINK
                .try_with(|s| s.clone())
                .ok()
                .flatten();
            tracing::info!(
                run_id = %run_id,
                task_count = plan_task_count,
                route = %route_str,
                attended_mode = %attended_mode.as_str(),
                has_trace_sink = trace_sink.is_some(),
                write_mode = ?self.write_mode,
                "plan_execute: dispatching run_dag"
            );
            tracing::info!(run_id = %run_id, "plan_execute: waiting for run execution lock");
            // RAII guard: 持锁 + Drop 时清理 RUN_EXECUTION_LOCKS entry (P1-1 修复)。
            let _run_guard = acquire_run_execution_lock(&run_id).await;
            tracing::info!(run_id = %run_id, "plan_execute: acquired run execution lock");
            if self
                .store
                .get_run(&run_id)
                .ok()
                .flatten()
                .is_some_and(|run| run.status == TaskRunStatus::Completed)
                && !has_unresolved_tasks(&self.store, &run_id)
            {
                let summaries = build_run_summaries(&self.store, &run_id);
                tracing::info!(
                    run_id = %run_id,
                    summary_chars = summaries.chars().count(),
                    "plan_execute: run already completed after waiting for lock"
                );
                return Ok(ToolResult::success(plan_execute_outcome_text(
                    &RunOutcome::Completed,
                    &summaries,
                )));
            }

            // ── §10.1: 必须 await RunOutcome, 不得 fire-and-forget ──
            // G3 fix: read run_store from the primary agent instead of passing
            // None. execute_run uses it to persist trace Run records (token
            // usage, status). Without it, the plan_execute path silently drops
            // trace persistence (event-wiring #1残留).
            let run_store = self.primary_agent.read(|a| a.run_store.clone()).await;
            // D7 stage 2: scope the write mode into a task-local so CP B
            // preflight in `execute_task` (deep inside execute_run → run_dag)
            // can read it without threading the mode through every signature.
            let write_mode = self.write_mode;
            let _cancel_registration = match self
                .store
                .register_run_cancellation(&run_id, cancel.clone())
            {
                Ok(registration) => registration,
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "Failed to register run cancellation: {error}"
                    )));
                }
            };
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
                        // B5.1: plan_execute tool drives an existing run's plan;
                        // memory write is owned by the outer run's caller
                        // (drive_run_async / resume_task_run), not this tool.
                        super::memory_bridge::MemoryPolicy::None,
                    )
                    .await
                })
                .await;

            match outcome {
                Ok(RunOutcome::Completed) => {
                    // 把各 subagent 的 summary 拼进返回文本,给主 agent 写最终答案的
                    // 素材(否则主 agent 只拿到一句"计划执行完成",无法产出实质答案)。
                    let summaries = build_run_summaries(&self.store, &run_id);
                    tracing::info!(
                        run_id = %run_id,
                        summary_chars = summaries.chars().count(),
                        "plan_execute: completed"
                    );
                    Ok(ToolResult::success(plan_execute_outcome_text(
                        &RunOutcome::Completed,
                        &summaries,
                    )))
                }
                Ok(RunOutcome::Cancelled) => {
                    tracing::info!(run_id = %run_id, "plan_execute: cancelled");
                    Ok(ToolResult::success(plan_execute_outcome_text(
                        &RunOutcome::Cancelled,
                        "",
                    )))
                }
                Ok(RunOutcome::Failed {
                    failed_task_id,
                    error,
                }) => {
                    tracing::warn!(
                        run_id = %run_id,
                        failed_task_id = %failed_task_id,
                        error = %error,
                        "plan_execute: failed"
                    );
                    Ok(ToolResult::success(plan_execute_outcome_text(
                        &RunOutcome::Failed {
                            failed_task_id,
                            error,
                        },
                        "",
                    )))
                }
                Ok(RunOutcome::Paused {
                    failed_task_id,
                    error,
                }) => {
                    tracing::warn!(
                        run_id = %run_id,
                        failed_task_id = %failed_task_id,
                        error = %error,
                        "plan_execute: paused"
                    );
                    Ok(ToolResult::success(plan_execute_outcome_text(
                        &RunOutcome::Paused {
                            failed_task_id,
                            error,
                        },
                        "",
                    )))
                }
                Err(e) => {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "plan_execute: executor error"
                    );
                    Ok(ToolResult::error(format!("plan_execute 失败: {e}")))
                }
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

fn plan_execute_outcome_text(outcome: &RunOutcome, summaries: &str) -> String {
    match outcome {
        RunOutcome::Completed => format!(
            "计划执行完成。各 subagent 的产出如下,请基于这些内容撰写最终答案:\n\n{summaries}"
        ),
        RunOutcome::Cancelled => "计划执行被取消。".to_string(),
        RunOutcome::Failed {
            failed_task_id,
            error,
        } => format!("计划执行失败 (任务 {failed_task_id}): {error}。可调整计划后重试。"),
        RunOutcome::Paused {
            failed_task_id,
            error,
        } => format!("计划因任务 {failed_task_id} 失败而暂停: {error}。"),
    }
}

fn build_run_summaries(store: &TaskRuntimeStore, run_id: &str) -> String {
    let todos = store.list_todos(run_id).unwrap_or_default();
    let tasks = store
        .get_plan(run_id)
        .ok()
        .flatten()
        .map(|p| p.tasks)
        .unwrap_or_default();

    let mut sections = Vec::new();
    for task in tasks {
        let todo = todos.iter().find(|t| t.task_id == task.id);
        let owner = todo
            .and_then(|t| t.owner_agent.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or(task.agent_role.as_str());
        let body = store
            .get_summary(run_id, &task.id)
            .ok()
            .flatten()
            .map(|summary| format_execution_summary(&summary))
            .or_else(|| todo.and_then(todo_summary))
            .unwrap_or_else(|| "subagent completed but no summary was recorded".to_string());
        sections.push(format!("## {} ({})\n{}", task.title, owner, body));
    }

    if sections.is_empty() {
        return "未找到已执行的 subagent 产出。".to_string();
    }
    sections.join("\n\n")
}

fn has_unresolved_tasks(store: &TaskRuntimeStore, run_id: &str) -> bool {
    store
        .get_plan(run_id)
        .ok()
        .flatten()
        .map(|plan| {
            plan.tasks.iter().any(|task| {
                !matches!(
                    task.status,
                    TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                )
            })
        })
        .unwrap_or(false)
}

fn todo_summary(todo: &TodoItem) -> Option<String> {
    todo.summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn format_execution_summary(summary: &TaskExecutionSummary) -> String {
    let mut parts = Vec::new();
    if !summary.result.summary.trim().is_empty() {
        parts.push(format!("完成: {}", summary.result.summary));
    }
    if !summary.result.touched_files.read.is_empty() {
        parts.push(format!(
            "读取: {}",
            summary.result.touched_files.read.join(", ")
        ));
    }
    if !summary.result.touched_files.written.is_empty() {
        parts.push(format!(
            "修改: {}",
            summary.result.touched_files.written.join(", ")
        ));
    }
    if !summary.decisions.is_empty() {
        parts.push(format!("决策: {}", summary.decisions.join("; ")));
    }
    if !summary.result.remaining_work.is_empty() {
        parts.push(format!(
            "未完成: {}",
            summary.result.remaining_work.join("; ")
        ));
    }
    if !summary.result.verification.is_empty() {
        parts.push(format!(
            "验证: {}",
            summary
                .result
                .verification
                .iter()
                .map(|item| format!("{}: {:?}", item.check, item.status))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if !summary.next_implications.is_empty() {
        parts.push(format!(
            "后续影响: {}",
            summary.next_implications.join("; ")
        ));
    }
    if !summary.suggested_tasks.is_empty() {
        let titles = summary
            .suggested_tasks
            .iter()
            .map(|task| task.title.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!("建议新增任务: {titles}"));
    }
    if parts.is_empty() {
        "subagent summary persisted without details".to_string()
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::task_tools;
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, SubagentRunStatus,
        SubagentTaskResult, SubagentTouchedFiles, TaskPlan,
    };
    use echo_agent::prelude::*;
    use echo_agent::tools::ToolParameters;

    fn test_tool(store: Arc<TaskRuntimeStore>) -> std::result::Result<ExecutePlanTool, String> {
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for plan_execute tool")
            .build()
            .map_err(|error| error.to_string())?;
        Ok(ExecutePlanTool::new(
            store,
            crate::agent_handle::AgentHandle::new(agent),
        ))
    }

    fn one_task_plan(run_id: &str) -> TaskPlan {
        TaskPlan {
            plan_id: format!("plan_{run_id}"),
            run_id: run_id.to_string(),
            domain_profile: DomainProfile::General,
            goal: "分析项目架构".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "task_1".to_string(),
                title: "项目结构".to_string(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "explorer".to_string(),
                ..Default::default()
            }],
        }
    }

    /// 验证无 task_local run_id 时 plan_execute 返回 error。
    #[tokio::test]
    async fn plan_execute_requires_run_id() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let result = tool
            .execute(ToolParameters::default())
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            !result.success,
            "expected error but got success: {}",
            result.output
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_execute_rejects_inline_task_in_every_mode() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let mut params = ToolParameters::new();
        params.insert(
            "task".to_string(),
            serde_json::json!({
                "agent_role": "explorer",
                "description": "分析当前项目结构"
            }),
        );
        params.insert("expected_task_count".to_string(), serde_json::json!(1));
        let result = task_tools::with_run_id("msg1".to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("no longer accepts an inline task"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn plan_execute_schema_requires_persisted_task_count() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let schema = tool.parameters();
        assert!(
            schema
                .get("properties")
                .and_then(|props| props.get("task"))
                .is_none(),
            "plan_execute schema must not expose inline task: {schema}"
        );
        assert!(
            schema
                .get("properties")
                .and_then(|props| props.get("expected_task_count"))
                .is_some(),
            "plan_execute schema must expose expected_task_count: {schema}"
        );
        assert_eq!(
            schema.get("required"),
            Some(&serde_json::json!(["expected_task_count"]))
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_execute_requires_non_empty_materialized_plan() -> std::result::Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let mut params = ToolParameters::new();
        params.insert("expected_task_count".to_string(), serde_json::json!(1));
        let result = task_tools::with_run_id("run_without_plan".to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("non-empty persisted plan")
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_execute_rejects_task_count_mismatch() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = "run_count_mismatch";
        store
            .create_run(
                run_id,
                "default",
                "conversation:count",
                "message:count",
                DomainProfile::General,
                "分析项目架构",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan(&one_task_plan(run_id))
            .map_err(|error| error.to_string())?;
        let tool = test_tool(store)?;
        let mut params = ToolParameters::new();
        params.insert("expected_task_count".to_string(), serde_json::json!(6));
        let result = task_tools::with_run_id(run_id.to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("declared 6"), "unexpected error: {error}");
        assert!(error.contains("contains 1"), "unexpected error: {error}");
        Ok(())
    }

    #[test]
    fn build_run_summaries_uses_persisted_task_summary() -> std::result::Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?;
        store
            .create_run(
                "r1",
                "default",
                "c1",
                "m1",
                DomainProfile::General,
                "分析项目架构",
                "chat_turn",
                AttendedMode::Attended,
            )
            .map_err(|e| e.to_string())?;
        let task = PlanTask {
            id: "t1".to_string(),
            title: "核心运行时".to_string(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "explorer".to_string(),
            ..Default::default()
        };
        store
            .attach_plan(&TaskPlan {
                plan_id: "p1".to_string(),
                run_id: "r1".to_string(),
                domain_profile: DomainProfile::General,
                goal: "分析项目架构".to_string(),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Parallel,
                tasks: vec![task],
            })
            .map_err(|e| e.to_string())?;
        store
            .put_summary(&TaskExecutionSummary {
                run_id: "r1".to_string(),
                task_id: "t1".to_string(),
                subagent_name: "explorer".to_string(),
                result: SubagentTaskResult {
                    contract_version: 1,
                    status: SubagentRunStatus::Completed,
                    summary: "梳理 runtime、agent_pool、task_runtime 的职责".to_string(),
                    artifacts: Vec::new(),
                    verification: Vec::new(),
                    remaining_work: Vec::new(),
                    touched_files: SubagentTouchedFiles {
                        read: vec!["echo-agent-app-core/src/runtime.rs".to_string()],
                        written: Vec::new(),
                    },
                },
                decisions: vec!["core 层负责应用编排, framework 层负责 agent 能力".to_string()],
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            })
            .map_err(|e| e.to_string())?;

        let text = build_run_summaries(&store, "r1");
        assert!(text.contains("核心运行时"));
        assert!(text.contains("梳理 runtime"));
        assert!(text.contains("runtime.rs"));
        Ok(())
    }

    #[test]
    fn every_plan_execute_outcome_omits_runtime_recovery_marker() -> std::result::Result<(), String>
    {
        let outcomes = [
            RunOutcome::Completed,
            RunOutcome::Cancelled,
            RunOutcome::Failed {
                failed_task_id: "failed-task".to_string(),
                error: "failed".to_string(),
            },
            RunOutcome::Paused {
                failed_task_id: "paused-task".to_string(),
                error: "paused".to_string(),
            },
        ];

        for outcome in &outcomes {
            let text = plan_execute_outcome_text(outcome, "subagent summary");
            if text.contains(super::super::compact_context::RUNTIME_RECOVERY_MARKER) {
                return Err(format!(
                    "plan_execute outcome must be ordinary status text: {outcome:?}"
                ));
            }
        }
        Ok(())
    }

    /// 验证 tool 的 name/description/parameters 基本属性。
    #[test]
    fn basic_properties() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        assert_eq!(tool.name(), "plan_execute");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters().is_object());
        Ok(())
    }
}
