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

use super::executor::{ExecEvent, RunOutcome, execute_run, preflight_unattended_plan};
use super::router::TaskRouteKind;
use super::store::TaskRuntimeStore;
use super::types::{
    AttendedMode, DomainProfile, ExecutionMode, InteractionMode, PlanTask, PlanTaskKind,
    TaskExecutionSummary, TaskPlan, TaskRunStatus, TodoItem, TodoStatus, UnattendedWriteMode,
};
use crate::agent_handle::AgentHandle;

/// One active execute_run driver per run_id.
///
/// Inline plan_execute calls can be emitted by the model as a parallel tool
/// batch. Without this guard, every call appends one task and then starts a
/// full DAG driver over the same run, so earlier tasks are dispatched multiple
/// times. Serializing per run lets the first call execute what is ready; later
/// calls re-read the persisted plan and skip tasks already marked Completed.
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

/// ComplexRuntime plan-approval gate 的最长等待时间。
///
/// 设计依据(调研 Claude Code / Codex / Cursor / Devin, 2026-07-03):
/// - Claude Code / Cursor: 交互式审批默认**无限等待** (fail-closed 挂起),
///   靠权限模式前置消除"需要问"的场景, 不依赖定时器。
/// - Codex: 人类审批无超时配置; 机器审批 (Guardian) 90s 硬超时 + fail-closed。
/// - Devin: plan-gate 默认 30s 超时后 fail-open 继续 (异步协作产品)。
///
/// EKO 是本地、用户在场的同步工具 (对齐 Cursor / Claude Code 阵营),
/// 故取 **fail-closed**: 超时后 run 留在 `Paused`, 不擅自执行写操作,
/// 用户回来点 resume 即可继续 (是"暂停"而非"失败")。
///
/// 时长 5 分钟与 `hitl/dispatcher.rs` 的 per-provider 超时一致, 让两条
/// 审批路径 (工具级 + run 级) 等待上限统一。本场景 (本地个人助理) 下仍
/// 需要这一上限: 防止框架自身在"用户走开/切到别的窗口"时无限占用信号量
/// 与 subagent slot, 符合 AGENTS.md "防止框架自身 bug/僵死造成破坏" 的加防护准则。
const APPROVAL_GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// 在给定时限内等待 plan-approval 信号; 超时则 fail-closed 返回 `Err`。
///
/// 抽成独立函数便于单测超时/正常两条路径 —— 无需构造完整 Tool/run/plan 链路。
/// (run 级审批门控此前为裸 `notified().await`, 无上限; 见 F3-1/F5-1。)
async fn await_approval_with_timeout(
    signal: &tokio::sync::Notify,
    timeout: std::time::Duration,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, signal.notified()).await
}

/// L1→L2 桥接工具: 把 plan 提交给 run_dag 并行调度器。
///
/// 字段说明:
/// - `store`: TaskRuntimeStore (用来读/写 run 状态)
/// - `primary_agent`: AgentHandle (传给 execute_run 做 subagent 调度)
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
        "plan_execute"
    }

    fn description(&self) -> &str {
        "派 subagent 执行任务并返回结果。两种用法:\n\
         \n\
         1. 单步派发 (传 task): 临时调研/分析/审查,直接派一个只读 subagent。\
         subagent 在独立上下文跑 ReAct,只回传结论摘要,保持主会话干净 (防 context 污染)。\n\
         可用 agent_role: explorer(探索代码库/数据/文档)、reviewer(审查 bug/方法/证据)、\
         planner(规划验证/测试路径)、summarizer(汇总多源发现)。\n\
         \n\
         2. 多 subagent 编排 (不传 task,先用 plan_create 拆 plan): 有依赖关系的多步任务,\
         先 plan_create 拆成带 depends_on 的子任务,再调本工具。\
         引擎 (run_dag) 按依赖自动并行/串行调度,统一收集 token 统计。\n\
         \n\
         适用场景: 大范围代码库检索、多文件架构梳理、冗长日志分析、多源调研综合等高噪声任务。\
         简单问答/单文件小改直接回复即可,不要调用本工具。"
    }

    /// plan_execute 派 subagent 跑独立 ReAct(延迟远高于普通文件/shell 工具)。
    /// 豁免并行批次总超时,避免它占满批次预算导致同批其他工具被提前取消;
    /// execute_run 内部有信号量 + subagent 600s per-dispatch 超时兜底。
    fn exempt_from_batch_timeout(&self) -> bool {
        true
    }

    fn parameters(&self) -> serde_json::Value {
        if !inline_task_schema_available() {
            return serde_json::json!({
                "type": "object",
                "properties": {}
            });
        }
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "object",
                    "description": "可选: 内联单步只读任务。提供时直接派一个 subagent (无需先 plan_create),不提供时执行当前 formal plan。",
                    "properties": {
                        "agent_role": {
                            "type": "string",
                            "enum": ["explorer", "reviewer", "planner", "summarizer"],
                            "description": "subagent 角色名,必须从 enum 中精确选择。"
                        },
                        "description": {
                            "type": "string",
                            "description": "给 subagent 的任务 prompt,含相关路径/范围/约束/需要的结果格式。"
                        }
                    },
                    "required": ["agent_role", "description"]
                }
            }
        })
    }

    fn execute<'a>(&'a self, params: ToolParameters) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            // ── 从 task_local 读取 run_id (§10.1) ──
            // drive_chat 已为普通 chat 轮次 scope 了 run_id (用 root_message_id),
            // 故这里总能拿到值。
            //
            // P1.0: `mut` — inline 分支会重新赋值为独立的 inline_<uuid> run_id,
            // 让后续 route_str/lock/execute_run/build_run_summaries 全部用 inline
            // run(而非共享的 root_message_id)。之前的 `let` shadow 只在 if 块内
            // 有效,块外的代码仍读旧 run_id → task_count=0 → subagent 不跑。
            let mut run_id = match super::task_tools::require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            let root_message_id = run_id.clone();
            tracing::info!(
                run_id = %run_id,
                has_inline_task = params.contains_key("task"),
                "plan_execute: start"
            );

            // ── §10.1: cancel 透传 ──
            let cancel = super::task_tools::CURRENT_CANCEL
                .try_with(|c| c.clone())
                .unwrap_or_else(|_| tokio_util::sync::CancellationToken::new());

            // ── inline task 路径 (吸收原 delegate_readonly 语义) ──
            // 当 LLM 传入 task 对象时,组装单任务 plan 直接走 execute_run,
            // 继承全部可见性 (started/completed/usage) + 调度 + 统计。
            // 普通 chat 轮次的 run_id (root_message_id) 没在 store 建过 run,
            // 故需先建 ad-hoc run (create_run + transition Running)。
            if params.contains_key("task") {
                if crate::chat_resources::current_chat_resources()
                    .is_some_and(|res| res.interaction_mode == super::types::InteractionMode::Task)
                {
                    return Ok(ToolResult::error(
                        "Task 模式不允许 plan_execute({task}) 单步派发。请先用 plan_create 拆分 \
                         PlanTask，然后调用 plan_execute() 执行整个 DAG。"
                            .to_string(),
                    ));
                }
                let (role, task_desc) = match parse_inline_task_params(&params) {
                    Ok(task) => task,
                    Err(reason) => {
                        tracing::warn!(
                            run_id = %run_id,
                            reason = %reason,
                            param_keys = ?params.keys().cloned().collect::<Vec<_>>(),
                            "plan_execute: invalid inline task params"
                        );
                        return Ok(ToolResult::error(reason));
                    }
                };
                if role.is_empty() || task_desc.is_empty() {
                    tracing::warn!(
                        run_id = %run_id,
                        role_empty = role.is_empty(),
                        description_empty = task_desc.is_empty(),
                        "plan_execute: inline task missing required fields"
                    );
                    return Ok(ToolResult::error(
                        "inline task 的 agent_role 和 description 不能为空",
                    ));
                }
                // conv_id 既用于建 run,也用于 RunStarted 事件通知前端激活面板,
                // 故提到 if 外层(事件发射处也要用)。
                let conv = crate::chat_resources::current_chat_resources()
                    .and_then(|r| r.conv_id.clone())
                    .unwrap_or_else(|| format!("message:{run_id}"));
                // P1.0: 每个 inline 派发用**独立 run_id**,不共享本轮 root_message_id。
                // 原因:模型在同 turn 并发发多个 inline plan_execute(ReAct join_all),
                // 若共享 run_id,第 1 个跑完把 run 推到 Completed(terminal,不可复活),
                // 后续 inline 的 task 虽 insert 成功但 execute_run 被 NotRunning 拒绝
                // (日志已确认此死锁)。独立 run_id 让每个 inline subagent 有
                // 自己的生命周期(started/running/completed 各自闭合),对标 Claude Code
                // 的"独立 subagent 实例"。下游所有 run_id 引用(insert/lock/execute_run/
                // summary)都用 reassign 后的这个值(外层 `mut run_id`,不是 let shadow,
                // 否则 if 块外仍读旧 run_id)。
                run_id = format!("inline_{}", uuid::Uuid::new_v4().as_simple());
                // 建 ad-hoc run(create_run 幂等性在此不再关键,因 run_id 已唯一)。
                let run = match self.store.create_run(
                    &run_id,
                    "default",
                    &conv,
                    &root_message_id,
                    DomainProfile::General,
                    &task_desc,
                    "agent_inline_task",
                    AttendedMode::Attended,
                ) {
                    Ok(run) => run,
                    Err(e) => {
                        tracing::warn!(
                            run_id = %run_id,
                            error = %e,
                            "plan_execute: failed to create ad-hoc inline run"
                        );
                        return Ok(ToolResult::error(format!(
                            "plan_execute: 建 ad-hoc run 失败: {e}"
                        )));
                    }
                };
                if run.status == TaskRunStatus::Pending {
                    if let Err(e) = self.store.transition_run(&run_id, TaskRunStatus::Running) {
                        tracing::warn!(
                            run_id = %run_id,
                            error = %e,
                            "plan_execute: failed to transition ad-hoc inline run to Running"
                        );
                        return Ok(ToolResult::error(format!(
                            "plan_execute: run 转 Running 失败: {e}"
                        )));
                    }
                }
                // 组装单任务 (用 LLM 传入的 role/desc,kind=ReadOnlyReview)。
                // 用 insert_task(追加语义)而非 attach_plan(覆盖语义):
                // 多次 inline 调用时 attach_plan 的 PlanGenerated 会覆盖整个
                // task 列表(event_rebuild.rs 赋值非追加),导致前面的 inline
                // task 在重建后丢失 → execute_task 报 task not found。
                // insert_task 追加到现有 plan,每个 inline task 都存活。
                let task_id = format!("inline_{}", uuid::Uuid::new_v4().as_simple());
                let title: String = task_desc.chars().take(80).collect();
                let task = PlanTask {
                    id: task_id.clone(),
                    title,
                    description: task_desc.to_string(),
                    kind: PlanTaskKind::ReadOnlyReview,
                    agent_role: role.to_string(),
                    ..Default::default()
                };
                if let Err(e) = self.store.insert_task(&run_id, None, task) {
                    tracing::warn!(
                        run_id = %run_id,
                        role = %role,
                        error = %e,
                        "plan_execute: failed to insert inline task"
                    );
                    return Ok(ToolResult::error(format!(
                        "plan_execute: insert inline task 失败: {e}"
                    )));
                }
                // 通知前端 run 已激活(send_chat_message 返回值不带 run_id,
                // 前端靠此 RunStarted 事件触发 loadByConversation → 激活
                // activeRun,否则 subagent 卡片/任务进度/Token 面板全空)。
                let inline_trace_sink = super::task_tools::CURRENT_TRACE_SINK
                    .try_with(|s| s.clone())
                    .ok()
                    .flatten();
                if let Some(sink) = inline_trace_sink.as_ref() {
                    sink(ExecEvent::run(
                        run_id.clone(),
                        "run_started",
                        serde_json::json!({
                            "conversation_id": conv,
                            "message_id": root_message_id,
                            "mode": "inline_task",
                            "run_id": run_id.clone(),
                            "route": "inline",
                            "goal": task_desc,
                        }),
                    ));
                }
                // plan 已就绪,落到下面的公共 execute_run 路径。
            } else {
                // ── 兜底: 若主 agent 跳过了 plan_create 直接调 plan_execute ──
                // LLM 可能不遵守 system prompt 的两阶段顺序。若 plan 为空,
                // 从 run goal 动态生成一个单 task plan,保证执行始终经过 run_dag
                // (有 wave 调度 + 信号量限流 + 失败传播保护)。
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
                        agent_role: "explorer".to_string(),
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
                            "plan_execute: 自动生成 plan 失败: {e}"
                        )));
                    }
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
                    // 等待 resume_run 通过 approval_signal 唤醒, 带超时上限。
                    // 超时 = fail-closed: run 保持 Paused (上面已 transition),
                    // 不擅自执行写操作; 用户回来点 resume 即可重新进入审批流。
                    let approval_result =
                        await_approval_with_timeout(&self.approval_signal, APPROVAL_GATE_TIMEOUT)
                            .await;
                    // Remove the signal -- 无论超时还是唤醒都要清理, 避免泄漏
                    // (G6/APPROVAL_NOTIFIES 未消费时泄漏, 见 code-review-2026-07-03 P1-3)。
                    super::task_tools::remove_approval_signal(&run_id);
                    if let Err(_elapsed) = approval_result {
                        let _ = self.store.note(
                            &run_id,
                            None,
                            &format!(
                                "plan approval timed out after {}s; run left Paused, \
                                 resume manually to retry",
                                APPROVAL_GATE_TIMEOUT.as_secs()
                            ),
                        );
                        return Ok(ToolResult::error(format!(
                            "plan approval timed out after {}s. The run is paused — \
                             call resume_run (or re-approve in the UI) to continue, \
                             or cancel it to free the slot.",
                            APPROVAL_GATE_TIMEOUT.as_secs()
                        )));
                    }
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
            let plan_task_count = self
                .store
                .get_plan(&run_id)
                .ok()
                .flatten()
                .map(|p| p.tasks.len())
                .unwrap_or(0);
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
                return Ok(ToolResult::success(format!(
                    "计划执行完成。各 subagent 的产出如下,请基于这些内容撰写最终答案:\n\n{summaries}"
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
                    Ok(ToolResult::success(format!(
                        "计划执行完成。各 subagent 的产出如下,请基于这些内容撰写最终答案:\n\n{summaries}"
                    )))
                }
                Ok(RunOutcome::Cancelled) => {
                    tracing::info!(run_id = %run_id, "plan_execute: cancelled");
                    Ok(ToolResult::success("计划执行被取消。"))
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
                    Ok(ToolResult::success(format!(
                        "计划执行失败 (任务 {failed_task_id}): {error}。可调整计划后重试。"
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
                    Ok(ToolResult::success(format!(
                        "计划因任务 {failed_task_id} 失败而暂停: {error}。"
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
    if !summary.completed_work.is_empty() {
        parts.push(format!("完成: {}", summary.completed_work.join("; ")));
    }
    if !summary.files_read.is_empty() {
        parts.push(format!("读取: {}", summary.files_read.join(", ")));
    }
    if !summary.files_changed.is_empty() {
        parts.push(format!("修改: {}", summary.files_changed.join(", ")));
    }
    if !summary.decisions.is_empty() {
        parts.push(format!("决策: {}", summary.decisions.join("; ")));
    }
    if !summary.failures.is_empty() {
        parts.push(format!("问题: {}", summary.failures.join("; ")));
    }
    if !summary.verification.is_empty() {
        parts.push(format!("验证: {}", summary.verification.join("; ")));
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

fn inline_task_schema_available() -> bool {
    !crate::chat_resources::current_chat_resources()
        .is_some_and(|res| res.interaction_mode == InteractionMode::Task)
}

fn parse_inline_task_params(
    params: &ToolParameters,
) -> std::result::Result<(String, String), String> {
    let task_value = params
        .get("task")
        .ok_or_else(|| "inline task 参数缺少 task 字段".to_string())?;

    let task_json = if let Some(s) = task_value.as_str() {
        serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::json!({ "description": s }))
    } else {
        task_value.clone()
    };

    let role = string_field(&task_json, &["agent_role", "role", "agent"])
        .or_else(|| string_param(params, &["agent_role", "role", "agent"]))
        .unwrap_or_else(|| "explorer".to_string());
    let description = string_field(
        &task_json,
        &["description", "prompt", "task", "query", "goal"],
    )
    .or_else(|| string_param(params, &["description", "prompt", "query", "goal"]))
    .unwrap_or_default();

    let description = description.trim().to_string();
    if description.is_empty() {
        return Err("inline task 的 description/prompt/task/query/goal 不能为空".to_string());
    }
    Ok((role.trim().to_string(), description))
}

fn string_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = value.get(*key).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn string_param(params: &ToolParameters, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = params.get(*key).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_driver::ChatSink;
    use crate::tasks::task_runtime::task_tools;
    use echo_agent::prelude::*;
    use echo_agent::tools::ToolParameters;

    struct NoopChatSink;

    impl ChatSink for NoopChatSink {
        fn on_agent_event(&self, _event: AgentEvent) -> bool {
            true
        }
    }

    /// 验证无 task_local run_id 时 plan_execute 返回 error。
    #[tokio::test]
    async fn plan_execute_requires_run_id() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for plan_execute tool")
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

    #[tokio::test]
    async fn task_mode_rejects_inline_plan_execute_task() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for plan_execute tool")
            .build()
            .expect("Failed to create test agent");
        let handle = crate::agent_handle::AgentHandle::new(agent);
        let tool = ExecutePlanTool::new(store.clone(), handle);
        let mut params = ToolParameters::new();
        params.insert(
            "task".to_string(),
            serde_json::json!({
                "agent_role": "explorer",
                "description": "分析当前项目结构"
            }),
        );
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store),
            sink: Arc::new(NoopChatSink),
            conv_id: Some("conv1".to_string()),
            root_message_id: "msg1".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            mode_hint: None,
            interaction_mode: InteractionMode::Task,
            layer_manager: None,
        });
        let result = crate::chat_resources::with_chat_resources(resources, async {
            task_tools::with_run_context(
                "msg1".to_string(),
                tokio_util::sync::CancellationToken::new(),
                None,
                tool.execute(params),
            )
            .await
        })
        .await
        .expect("plan_execute should return a ToolResult");
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("Task 模式不允许 plan_execute({task})"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn task_mode_plan_execute_schema_hides_inline_task() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for plan_execute tool")
            .build()
            .expect("Failed to create test agent");
        let handle = crate::agent_handle::AgentHandle::new(agent);
        let tool = ExecutePlanTool::new(store.clone(), handle);
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store),
            sink: Arc::new(NoopChatSink),
            conv_id: Some("conv1".to_string()),
            root_message_id: "msg1".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            mode_hint: None,
            interaction_mode: InteractionMode::Task,
            layer_manager: None,
        });
        let schema =
            crate::chat_resources::with_chat_resources(resources, async { tool.parameters() })
                .await;
        assert!(
            schema
                .get("properties")
                .and_then(|props| props.get("task"))
                .is_none(),
            "Task mode plan_execute schema must not expose inline task: {schema}"
        );
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
                worker_agent: "explorer".to_string(),
                completed_work: vec!["梳理 runtime、agent_pool、task_runtime 的职责".to_string()],
                files_read: vec!["echo-agent-app-core/src/runtime.rs".to_string()],
                files_changed: Vec::new(),
                decisions: vec!["core 层负责应用编排, framework 层负责 agent 能力".to_string()],
                failures: Vec::new(),
                verification: Vec::new(),
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
    fn parse_inline_task_accepts_object_task() -> std::result::Result<(), String> {
        let mut params = ToolParameters::new();
        params.insert(
            "task".to_string(),
            serde_json::json!({
                "agent_role": "reviewer",
                "description": "审查 subagent 执行链路"
            }),
        );
        let (role, desc) = parse_inline_task_params(&params)?;
        assert_eq!(role, "reviewer");
        assert_eq!(desc, "审查 subagent 执行链路");
        Ok(())
    }

    #[test]
    fn parse_inline_task_accepts_string_task_with_root_role() -> std::result::Result<(), String> {
        let mut params = ToolParameters::new();
        params.insert(
            "task".to_string(),
            serde_json::Value::String("分析当前项目架构".to_string()),
        );
        params.insert(
            "agent_role".to_string(),
            serde_json::Value::String("planner".to_string()),
        );
        let (role, desc) = parse_inline_task_params(&params)?;
        assert_eq!(role, "planner");
        assert_eq!(desc, "分析当前项目架构");
        Ok(())
    }

    #[test]
    fn parse_inline_task_defaults_role_to_explorer() -> std::result::Result<(), String> {
        let mut params = ToolParameters::new();
        params.insert(
            "task".to_string(),
            serde_json::json!({
                "prompt": "梳理仓库模块边界"
            }),
        );
        let (role, desc) = parse_inline_task_params(&params)?;
        assert_eq!(role, "explorer");
        assert_eq!(desc, "梳理仓库模块边界");
        Ok(())
    }

    /// 验证 tool 的 name/description/parameters 基本属性。
    #[test]
    fn basic_properties() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test agent for plan_execute tool")
            .build()
            .expect("Failed to create agent");
        let handle = crate::agent_handle::AgentHandle::new(agent);
        let tool = ExecutePlanTool::new(store, handle);
        assert_eq!(tool.name(), "plan_execute");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters().is_object());
    }

    /// F3-1/F5-1 回归: approval gate 超时后 fail-closed。
    ///
    /// `await_approval_with_timeout` 在无人 notify 时应在时限后返回 `Err`,
    /// 主管线据此保持 run 在 Paused 并返回工具错误 (而非无限等待)。
    /// 这里用极短超时避免单测拖慢 CI。
    #[tokio::test]
    async fn approval_gate_times_out_fail_closed() {
        let signal = Arc::new(tokio::sync::Notify::new());
        let started = tokio::time::Instant::now();
        let result =
            await_approval_with_timeout(&signal, std::time::Duration::from_millis(50)).await;
        assert!(result.is_err(), "expected timeout, got {:?}", result);
        // 至少等满了 50ms (容许调度抖动, 不卡上界)。
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(45),
            "elapsed {:?} < 45ms, timeout not honored",
            started.elapsed()
        );
    }

    /// F3-1 正路径: approval gate 在超时前被 resume_run 唤醒 → 返回 Ok。
    ///
    /// 验证 notify_one 能及时解除等待, 不影响正常审批流。
    #[tokio::test]
    async fn approval_gate_resumes_on_notify() {
        let signal = Arc::new(tokio::sync::Notify::new());
        let signal_clone = signal.clone();
        // 立即 notify (模拟用户瞬间批准)。Notify 记录一个 permit,
        // 后续 notified() 立即消费返回。
        signal_clone.notify_one();
        let result = await_approval_with_timeout(&signal, std::time::Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected immediate resume, got {:?}",
            result
        );
    }
}
