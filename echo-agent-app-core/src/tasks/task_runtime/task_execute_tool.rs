//! `task_execute` submits one committed task-graph revision to the framework
//! runtime DAG executor.
//!
//! # 设计意图 (spec §3.1.1)
//!
//! The main Agent creates one or more related tasks through `task_create`, then
//! explicitly invokes this tool to trigger `execute_run` and its L2 wave
//! scheduling. Atomic task batches keep parallel work from degrading into
//! create-one/run-one serialization.
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

tokio::task_local! {
    static CURRENT_EXECUTION_CONVERSATION_ID: Option<String>;
}

/// One active execute_run driver per run_id.
///
/// Duplicate task_execute calls can be emitted by the model or a resumed turn.
/// Serializing per run keeps one authoritative DAG driver and lets later calls
/// re-read terminal task state instead of dispatching completed nodes again.
///
/// Stored as `Weak<TokioMutex>`: when the last guard drops, the Weak
/// automatically fails to upgrade, so the next acquire builds a fresh Arc.
/// This avoids the remove-on-drop race that the prior `Arc` + `Drop::remove`
/// implementation had: between `drop(guard)` and `map.remove()`, a racing
/// `acquire` could clone the about-to-be-removed Arc, while a third caller
/// arriving after `remove` built a brand-new Arc — two live Mutexes for the
/// same run, mutual exclusion lost. Weak eliminates the window entirely:
/// there is no remove step, and stale entries are reclaimed lazily on the
/// next `acquire` (entry is replaced when upgrade fails).
static RUN_EXECUTION_LOCKS: LazyLock<DashMap<String, std::sync::Weak<TokioMutex<()>>>> =
    LazyLock::new(DashMap::new);

/// RAII guard holding a per-run execution lock. Releasing the guard drops
/// the strong reference; the `DashMap` entry (Weak) then becomes inert and
/// is replaced by the next `acquire_run_execution_lock` caller.
struct RunExecutionGuard {
    /// Owned guard from `Arc<TokioMutex>::lock_owned`. Does not borrow any
    /// external reference, so it can be moved freely. Held in `Option` so
    /// the explicit `Drop` impl can `take()` it before the rest of `self`
    /// is torn down.
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    /// Strong reference kept alive for the guard's lifetime so the DashMap
    /// Weak keeps upgrading successfully for any racing caller that cloned
    /// this same Arc before we dropped the guard.
    _keep_alive: Arc<TokioMutex<()>>,
}

impl Drop for RunExecutionGuard {
    fn drop(&mut self) {
        // Drop order: _guard first (release the mutex), then _keep_alive
        // (decrement strong count). After this the DashMap entry's Weak can
        // no longer upgrade — the next acquire replaces it with a fresh Arc.
        if let Some(g) = self._guard.take() {
            drop(g);
        }
    }
}

/// Acquire (and wait on) the per-run execution lock. Returns an RAII guard
/// that releases the lock on drop. Concurrent callers for the same run_id
/// share the same `Arc<TokioMutex>` (one waits while the other holds it).
async fn acquire_run_execution_lock(run_id: &str) -> RunExecutionGuard {
    // Fast path: an entry exists and its Weak still upgrades — share it.
    if let Some(arc) = RUN_EXECUTION_LOCKS
        .get(run_id)
        .and_then(|weak| weak.upgrade())
    {
        let guard = arc.clone().lock_owned().await;
        return RunExecutionGuard {
            _guard: Some(guard),
            _keep_alive: arc,
        };
        // Weak was dead (previous holder fully dropped). Fall through to
        // replace the stale entry.
    }
    // Build a fresh Arc and try to install it. Entry API makes this
    // race-safe: if two callers race here, the loser sees the winner's
    // entry via Occupied and shares that Arc instead.
    let new_arc = Arc::new(TokioMutex::new(()));
    let arc = match RUN_EXECUTION_LOCKS.entry(run_id.to_string()) {
        dashmap::mapref::entry::Entry::Occupied(mut occ) => {
            if let Some(existing_arc) = occ.get().upgrade() {
                // Another caller installed a live entry between our get()
                // and entry() — share theirs instead of replacing.
                existing_arc
            } else {
                // Stale entry; replace with our fresh Weak.
                occ.insert(Arc::downgrade(&new_arc));
                new_arc
            }
        }
        dashmap::mapref::entry::Entry::Vacant(vac) => {
            vac.insert(Arc::downgrade(&new_arc));
            new_arc
        }
    };
    let guard = arc.clone().lock_owned().await;
    RunExecutionGuard {
        _guard: Some(guard),
        _keep_alive: arc,
    }
}

/// L1→L2 bridge: submit the committed task graph to the shared DAG executor.
///
/// 字段说明:
/// - `store`: TaskRuntimeStore (用来读/写 run 状态)
/// - `primary_agent`: AgentHandle (传给 execute_run 做 subagent 调度)
pub struct ExecuteTaskTool {
    store: Arc<TaskRuntimeStore>,
    primary_agent: AgentHandle,
    agent_pool: Option<std::sync::Weak<crate::agent_pool::AgentPool>>,
    /// D7 stage 2: unattended write mode for this tool's runs. Determines
    /// whether the CP A preflight loosens its write ban (Worktree/InPlace)
    /// or keeps stage-1 rejection (Disabled). Also scoped into a task-local
    /// so CP B preflight in `execute_task` can read it.
    write_mode: UnattendedWriteMode,
}

impl ExecuteTaskTool {
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
            agent_pool: None,
            write_mode,
        }
    }

    /// Resolve shared task execution against the AgentHandle that owns the
    /// current conversation. The Weak avoids a ToolManager ↔ AgentPool cycle.
    pub fn with_agent_pool(
        mut self,
        agent_pool: std::sync::Weak<crate::agent_pool::AgentPool>,
    ) -> Self {
        self.agent_pool = Some(agent_pool);
        self
    }

    async fn execution_agent(&self) -> AgentHandle {
        let conversation_id = CURRENT_EXECUTION_CONVERSATION_ID
            .try_with(Clone::clone)
            .ok()
            .flatten();
        if let (Some(pool), Some(conversation_id)) = (
            self.agent_pool.as_ref().and_then(std::sync::Weak::upgrade),
            conversation_id,
        ) && let Some(agent) = pool.get(&conversation_id).await
        {
            return agent;
        }
        self.primary_agent.clone()
    }

    #[cfg(test)]
    pub(crate) async fn execution_agent_for_test(
        &self,
        conversation_id: Option<String>,
    ) -> AgentHandle {
        CURRENT_EXECUTION_CONVERSATION_ID
            .scope(conversation_id, self.execution_agent())
            .await
    }
}

impl Tool for ExecuteTaskTool {
    fn name(&self) -> &str {
        "task_execute"
    }

    fn description(&self) -> &str {
        "Execute one exact committed task-graph revision with Subagents. The graph may contain one task or a dependency DAG; stale or missing revisions are rejected."
    }

    /// task_execute 派 subagent 跑独立 ReAct(延迟远高于普通文件/shell 工具)。
    /// 豁免并行批次总超时,避免它占满批次预算导致同批其他工具被提前取消;
    /// execute_run 内部有信号量 + subagent 600s per-dispatch 超时兜底。
    fn exempt_from_batch_timeout(&self) -> bool {
        true
    }

    fn allows_parallel_batch_execution(&self) -> bool {
        false
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "revision": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Exact committed task-graph revision returned by task_create, task_update, or task_list."
                }
            },
            "required": ["revision"]
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
                "task_execute: start"
            );
            let execution_agent = self.execution_agent().await;

            if params.contains_key("task") {
                return Ok(ToolResult::error(
                    "task_execute accepts only a committed revision; call task_create first.",
                ));
            }

            let Some(revision) = params
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .filter(|revision| *revision > 0)
            else {
                return Ok(ToolResult::error(
                    "task_execute requires the committed revision.",
                ));
            };
            let materialized_plan = match self.store.get_plan(&run_id) {
                Ok(Some(plan)) if !plan.tasks.is_empty() => plan,
                Ok(_) => {
                    return Ok(ToolResult::error(
                        "task_execute requires at least one persisted task. Call task_create, then refresh with task_list.",
                    ));
                }
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "Failed to read the persisted task graph before execution: {error}"
                    )));
                }
            };
            if materialized_plan.revision != revision {
                return Ok(ToolResult::error(format!(
                    "Task graph revision mismatch: requested {revision}, but the latest committed revision is {}. Refresh task_list and execute the latest revision.",
                    materialized_plan.revision
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
                task_count = materialized_plan.tasks.len(),
                route = %route_str,
                attended_mode = %attended_mode.as_str(),
                has_trace_sink = trace_sink.is_some(),
                write_mode = ?self.write_mode,
                "task_execute: dispatching runtime DAG executor"
            );
            tracing::info!(run_id = %run_id, "task_execute: waiting for run execution lock");
            // RAII guard: 持锁 + Drop 时清理 RUN_EXECUTION_LOCKS entry (P1-1 修复)。
            let _run_guard = acquire_run_execution_lock(&run_id).await;
            tracing::info!(run_id = %run_id, "task_execute: acquired run execution lock");
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
                    "task_execute: run already completed after waiting for lock"
                );
                return Ok(ToolResult::success(task_execute_outcome_text(
                    &RunOutcome::Completed,
                    &summaries,
                )));
            }

            // ── §10.1: 必须 await RunOutcome, 不得 fire-and-forget ──
            // G3 fix: read run_store from the primary agent instead of passing
            // None. execute_run uses it to persist trace Run records (token
            // usage, status). Without it, the task_execute path silently drops
            // trace persistence (event-wiring #1残留).
            let run_store = execution_agent.read(|a| a.run_store.clone()).await;
            // D7 stage 2: scope the write mode into a task-local so CP B
            // preflight in `execute_task` (inside execute_run's EKO controller)
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
            // Wire the reviewer LLM from the conversation's execution agent.
            // Without this the
            // review gate falls through to Skipped, which (per M7) must NOT
            // auto-pass — tasks with acceptance_criteria would block forever.
            // Review calls are sequential per task and bounded by
            // max_parallel_llm_calls.
            let reviewer_llm = execution_agent
                .read(|agent| agent.llm_client().cloned())
                .await;
            let outcome = super::task_tools::CURRENT_UNATTENDED_WRITE_MODE
                .scope(write_mode, async {
                    execute_run(
                        self.store.clone(),
                        Some(execution_agent),
                        reviewer_llm,
                        None, // layer_manager — 暂时 None
                        run_store,
                        trace_sink,
                        &run_id,
                        cancel,
                        // B5.1: task_execute tool drives an existing run's plan;
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
                        "task_execute: completed"
                    );
                    Ok(ToolResult::success(task_execute_outcome_text(
                        &RunOutcome::Completed,
                        &summaries,
                    )))
                }
                Ok(RunOutcome::Cancelled) => {
                    tracing::info!(run_id = %run_id, "task_execute: cancelled");
                    Ok(ToolResult::success(task_execute_outcome_text(
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
                        "task_execute: failed"
                    );
                    Ok(ToolResult::success(task_execute_outcome_text(
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
                        "task_execute: paused"
                    );
                    Ok(ToolResult::success(task_execute_outcome_text(
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
                        "task_execute: executor error"
                    );
                    Ok(ToolResult::error(format!("task_execute 失败: {e}")))
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
            let conversation_id = ctx.conversation_id.clone();
            let result = super::task_tools::scoped_with_ctx_run_id(ctx, || {
                CURRENT_EXECUTION_CONVERSATION_ID.scope(conversation_id, self.execute(params))
            })
            .await?;
            compact_completed_task_result(ctx, result)
        })
    }
}

fn compact_completed_task_result(
    ctx: &echo_core::tools::ToolContext,
    mut result: ToolResult,
) -> echo_agent::error::Result<ToolResult> {
    if !result.success || !result.output.contains("各 subagent 的产出如下") {
        return Ok(result);
    }
    let Some(mut config) = ctx.output_artifacts.clone() else {
        return Ok(result);
    };
    config.threshold_bytes = 1;
    let full_output = result.output.clone();
    let artifact = match echo_core::tools::artifact::persist_tool_output(
        config,
        echo_core::tools::artifact::ToolOutputArtifactIdentity::from_context(ctx, "task_execute"),
        &full_output,
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            tracing::warn!(%error, "task_execute output artifact write failed");
            result
                .metadata
                .insert("artifact_status".to_string(), "write_failed".to_string());
            result
                .metadata
                .insert("artifact_error".to_string(), error.to_string());
            return Ok(result);
        }
    };
    let Some(artifact) = artifact else {
        return Ok(result);
    };
    let subagent_count = full_output
        .lines()
        .filter(|line| line.starts_with("## "))
        .count();
    result.output = format!(
        "计划执行完成。已汇总 {subagent_count} 个 Subagent 的结果。完整汇总已保存到 artifact: {} (sha256 {})。请用 read_artifact 分页读取后撰写最终答案。",
        artifact.path.display(),
        artifact.sha256
    );
    result.data = None;
    artifact.extend_metadata(&mut result.metadata);
    result
        .metadata
        .insert("output_handling".to_string(), "spilled".to_string());
    result
        .metadata
        .insert("original_bytes".to_string(), full_output.len().to_string());
    result.metadata.insert(
        "returned_bytes".to_string(),
        result.output.len().to_string(),
    );
    result
        .metadata
        .insert("subagent_count".to_string(), subagent_count.to_string());
    Ok(result)
}

fn task_execute_outcome_text(outcome: &RunOutcome, summaries: &str) -> String {
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

    fn test_tool(store: Arc<TaskRuntimeStore>) -> std::result::Result<ExecuteTaskTool, String> {
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for task_execute tool")
            .build()
            .map_err(|error| error.to_string())?;
        Ok(ExecuteTaskTool::new(
            store,
            crate::agent_handle::AgentHandle::new(agent),
        ))
    }

    fn one_task_plan(run_id: &str) -> TaskPlan {
        TaskPlan {
            plan_id: format!("plan_{run_id}"),
            run_id: run_id.to_string(),
            revision: 1,
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

    /// 验证无 task_local run_id 时 task_execute 返回 error。
    #[tokio::test]
    async fn task_execute_requires_run_id() -> std::result::Result<(), String> {
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
    async fn task_execute_rejects_inline_task_in_every_mode() -> std::result::Result<(), String> {
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
        params.insert("revision".to_string(), serde_json::json!(1));
        let result = task_tools::with_run_id("msg1".to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("accepts only a committed revision"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn task_execute_schema_requires_committed_revision() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let schema = tool.parameters();
        assert!(
            schema
                .get("properties")
                .and_then(|props| props.get("task"))
                .is_none(),
            "task_execute schema must not expose inline task: {schema}"
        );
        assert!(
            schema
                .get("properties")
                .and_then(|props| props.get("revision"))
                .is_some(),
            "task_execute schema must expose revision: {schema}"
        );
        assert_eq!(
            schema.get("required"),
            Some(&serde_json::json!(["revision"]))
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_execute_requires_materialized_tasks() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = test_tool(store)?;
        let mut params = ToolParameters::new();
        params.insert("revision".to_string(), serde_json::json!(1));
        let result = task_tools::with_run_id("run_without_plan".to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("at least one persisted task")
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_execute_rejects_stale_revision() -> std::result::Result<(), String> {
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
            .attach_plan_for_test(&one_task_plan(run_id))
            .map_err(|error| error.to_string())?;
        let tool = test_tool(store)?;
        let mut params = ToolParameters::new();
        params.insert("revision".to_string(), serde_json::json!(6));
        let result = task_tools::with_run_id(run_id.to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("requested 6"), "unexpected error: {error}");
        assert!(error.contains("revision is 1"), "unexpected error: {error}");
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
            .attach_plan_for_test(&TaskPlan {
                plan_id: "p1".to_string(),
                run_id: "r1".to_string(),
                revision: 1,
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
    fn every_task_execute_outcome_omits_runtime_recovery_marker() -> std::result::Result<(), String>
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
            let text = task_execute_outcome_text(outcome, "subagent summary");
            if text.contains(super::super::compact_context::RUNTIME_RECOVERY_MARKER) {
                return Err(format!(
                    "task_execute outcome must be ordinary status text: {outcome:?}"
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
        assert_eq!(tool.name(), "task_execute");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters().is_object());
        assert!(!tool.allows_parallel_batch_execution());
        Ok(())
    }
}
