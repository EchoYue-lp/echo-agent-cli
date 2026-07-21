//! Agent tools for managing the task plan during execution.
//!
//! These let the main agent autonomously create / update / complete / skip /
//! list tasks, mirroring Claude Code's TaskCreate / TaskUpdate model.
//!
//! Each tool reads `run_id` from a `tokio::task_local!` scoped by the executor
//! around task execution, and operates on the [`TaskRuntimeStore`] injected at
//! construction time.
//!
//! Why task_local (not thread_local): tokio uses a work-stealing scheduler that
//! may move a task across OS threads at any `.await` point. A `thread_local!`
//! value would be lost or silently swapped with another run's value after a
//! thread hop. `task_local!` is bound to the logical async task and survives
//! `.await` across threads — correct for this use case.

use echo_agent::prelude::*;
use echo_agent::tools::{Tool, ToolResult};
use std::collections::HashSet;
use std::sync::Arc;

use super::executor::{ExecEvent, RunPlanPolicy};
use super::profiles::{ProfileTemplate, default_subagent_for, subagent_catalog_prompt};
use super::store::TaskRuntimeStore;
use super::types::{
    AttendedMode, DomainProfile, ExecutionMode, PlanPatchOperation, PlanPatchRequest, PlanTask,
    PlanTaskKind, TaskExecution, TaskPlan, TaskRunStatus, TodoStatus,
};

#[derive(Debug, Clone, Default)]
pub struct PlanCapabilityCatalog {
    subagents: HashSet<String>,
    tools: HashSet<String>,
}

impl PlanCapabilityCatalog {
    const PLAN_CONTROL_TOOLS: [&'static str; 4] =
        ["plan_create", "plan_patch", "task_list", "plan_execute"];

    pub fn new(
        subagents: impl IntoIterator<Item = String>,
        tools: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            subagents: subagents.into_iter().collect(),
            tools: tools.into_iter().collect(),
        }
    }

    fn validate_task(&self, task: &PlanTask) -> std::result::Result<(), String> {
        if !self.subagents.contains(&task.agent_role) {
            let mut available = self.subagents.iter().cloned().collect::<Vec<_>>();
            available.sort();
            return Err(format!(
                "unknown Subagent '{}'; available: {}",
                task.agent_role,
                available.join(", ")
            ));
        }
        for tool in &task.allowed_tools {
            if Self::PLAN_CONTROL_TOOLS.contains(&tool.as_str()) {
                return Err(format!(
                    "task '{}' cannot delegate plan-control tool '{}' to a Subagent",
                    task.id, tool
                ));
            }
            if !self.tools.contains(tool) {
                return Err(format!(
                    "task '{}' declares unknown tool '{}'",
                    task.id, tool
                ));
            }
        }
        Ok(())
    }

    fn validate_patch_operation(
        &self,
        operation: &PlanPatchOperation,
    ) -> std::result::Result<(), String> {
        match operation {
            PlanPatchOperation::Insert { task, .. } => self.validate_task(&PlanTask::from_parts(
                task.clone(),
                TaskExecution::pending(task.id.clone()),
            )),
            PlanPatchOperation::Update { patch, .. } => {
                if let Some(role) = &patch.agent_role
                    && !self.subagents.contains(role)
                {
                    return Err(format!("unknown Subagent '{role}'"));
                }
                if let Some(tools) = &patch.allowed_tools {
                    for tool in tools {
                        if Self::PLAN_CONTROL_TOOLS.contains(&tool.as_str()) {
                            return Err(format!(
                                "plan-control tool '{tool}' cannot be delegated to a Subagent"
                            ));
                        }
                        if !self.tools.contains(tool) {
                            return Err(format!("unknown tool '{tool}'"));
                        }
                    }
                }
                Ok(())
            }
            PlanPatchOperation::Skip { .. } | PlanPatchOperation::Reorder { .. } => Ok(()),
        }
    }
}

/// Convenience alias for a trace-sink callback that forwards execution-flow
/// events out of the task-runtime executor. Same shape as
/// [`super::executor::ExecSink`]; kept as a distinct alias because the
/// `CURRENT_TRACE_SINK` task_local predates the `ExecSink` name and several
/// call sites still spell it as `TraceSink`.
pub type TraceSink = Arc<dyn Fn(ExecEvent) + Send + Sync>;

// ── Task-local run_id injection (async-safe) ──────────────────────────────

tokio::task_local! {
    /// The run_id of the currently executing task run. Set by the executor via
    /// [`with_run_context`] around the subagent dispatch so tools can read it.
    pub static CURRENT_RUN_ID: String;
    /// The cancel token for the currently executing task run. Set alongside
    /// CURRENT_RUN_ID so plan_execute and other tools can read it.
    pub static CURRENT_CANCEL: tokio_util::sync::CancellationToken;
    /// Delegate nesting depth — incremented each time a subagent is delegated
    /// during tool execution. Used by Task 6 (L3 nesting) to prevent runaway
    /// recursion and to route subagent tool calls correctly.
    pub static CURRENT_DELEGATE_DEPTH: std::cell::Cell<u32>;
    /// An optional trace-sink that forwards [`ExecEvent`] items out of the
    /// executor so the frontend can render real-time execution-flow views. Set
    /// alongside `CURRENT_RUN_ID` by [`with_run_context`].
    pub static CURRENT_TRACE_SINK: Option<TraceSink>;
    /// The unattended write mode for the currently executing run (D7 stage 2).
    /// Set by `ExecutePlanTool::execute` so CP B preflight in `execute_task`
    /// can read it without threading the mode through `execute_run` →
    /// `run_dag` → `execute_task`. Defaults to `Disabled` when no scope is
    /// active (e.g. tests, attended runs).
    pub static CURRENT_UNATTENDED_WRITE_MODE: super::types::UnattendedWriteMode;
}

/// Run `f` with run_id, cancel, delegate_depth, and trace_sink available to all
/// task tools.
///
/// Called by the executor before dispatching task work. Replaces the old
/// [`with_run_id`] which only scoped the run_id. Delegate depth starts at 0
/// and is incremented by the L3 nesting layer (Task 6).
///
/// (stage4 P4.1) `cache_user_id` is no longer threaded here — tools/LLM calls
/// read the single source via `infra::load_or_create_cache_user_id()` instead.
pub async fn with_run_context<F, R>(
    run_id: String,
    cancel: tokio_util::sync::CancellationToken,
    trace_sink: Option<TraceSink>,
    f: F,
) -> R
where
    F: std::future::Future<Output = R>,
{
    let cell_cancel = cancel.clone();
    CURRENT_RUN_ID
        .scope(
            run_id,
            CURRENT_CANCEL.scope(
                cell_cancel,
                CURRENT_DELEGATE_DEPTH.scope(
                    std::cell::Cell::new(0),
                    CURRENT_TRACE_SINK.scope(trace_sink, f),
                ),
            ),
        )
        .await
}

/// Legacy wrapper — keeps old callers compiling. Prefer [`with_run_context`].
pub async fn with_run_id<F, R>(run_id: String, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let cancel = tokio_util::sync::CancellationToken::new();
    with_run_context(run_id, cancel, None, f).await
}

pub(crate) fn current_run_id() -> Option<String> {
    CURRENT_RUN_ID.try_with(|cell| cell.clone()).ok()
}

/// Read the current unattended write mode from the task-local scope (D7
/// stage 2). Returns `Disabled` when no scope is active (attended runs,
/// tests) — matching stage-1 behaviour as the safe default.
pub fn current_unattended_write_mode() -> super::types::UnattendedWriteMode {
    CURRENT_UNATTENDED_WRITE_MODE
        .try_with(|m| *m)
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn require_run_id() -> std::result::Result<String, ToolResult> {
    current_run_id().ok_or_else(|| ToolResult::error("no active run — run_id not set in context"))
}

pub(crate) fn formal_run_id_for_turn(turn_id: &str) -> String {
    format!("taskrun:{turn_id}")
}

/// 从 ToolContext 优先读 run_id(跨 spawn 安全),回退 task_local(主 agent scope)。
///
/// 这是根治 task_local 跨 tokio::spawn 断裂的关键:subagent 在框架层 dispatch_fork
/// 的 spawn 里执行,task_local 全部丢失;但 ToolContext 是值传递(经 dispatch_fork
/// → set_external_context → pipeline 填入),跨 spawn 安全。工具 override
/// `execute_with_context` 后用此 helper 读 run_id,主 agent 和 subagent 都能拿到。
#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn run_id_from_ctx_or_local(
    ctx: &echo_core::tools::ToolContext,
) -> std::result::Result<String, ToolResult> {
    ctx.run_id
        .clone()
        .or_else(|| ctx.turn_id.as_deref().map(formal_run_id_for_turn))
        .or_else(current_run_id)
        .ok_or_else(|| ToolResult::error("no active run — run_id not in ToolContext or task_local"))
}

fn trace_sink_from_tool_context(ctx: &echo_core::tools::ToolContext) -> Option<TraceSink> {
    ctx.trace_sink.as_ref().map(|sink| {
        let sink = sink.clone();
        Arc::new(move |event: ExecEvent| {
            if let Ok(value) = serde_json::to_value(event) {
                sink(value);
            }
        }) as TraceSink
    })
}

/// 在 ToolContext.run_id/trace_sink(若有)的 task_local 覆盖作用域内执行 f。
///
/// 这样工具既有的 `execute`(读 task_local 的 require_run_id)无需改动即可在
/// subagent 场景工作:execute_with_context 调本函数包住原 execute,ctx.run_id 被
/// 临时注入 task_local,require_run_id 读到的是 ToolContext 的值(跨 spawn 安全)。
/// ctx.run_id 为 None 时直接执行 f(回退原 task_local,主 agent 场景)。
///
/// 同时覆盖 run_id / cancel / trace_sink(若有),让 require_run_id /
/// CURRENT_CANCEL / CURRENT_TRACE_SINK 在框架 spawn 的工具执行场景读到
/// ToolContext 的值(跨 spawn 安全)。`ctx.trace_sink` 是框架 Value 形式,
/// 这里反序列化回 `ExecEvent` 包装成 `TraceSink` 注入 task_local。
pub(crate) async fn scoped_with_ctx_run_id<F, Fut, R>(
    ctx: &echo_core::tools::ToolContext,
    f: F,
) -> R
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let scoped_run_id = ctx
        .run_id
        .clone()
        .or_else(|| ctx.turn_id.as_deref().map(formal_run_id_for_turn));
    match scoped_run_id {
        Some(rid) => {
            let cancel = ctx
                .cancel
                .as_ref()
                .map(|c| (**c).clone())
                .unwrap_or_default();
            let trace_sink = trace_sink_from_tool_context(ctx);
            CURRENT_CANCEL
                .scope(
                    cancel,
                    CURRENT_RUN_ID.scope(rid, CURRENT_TRACE_SINK.scope(trace_sink, f())),
                )
                .await
        }
        None => f().await,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // tool impls below are production code; reordering is pure churn
mod tests {
    use super::*;

    /// task_local must survive `.await` across tokio thread hops.
    /// (thread_local would fail this: a work-stealing scheduler can move the
    /// task to another OS thread after `yield_now`, dropping the thread_local
    /// value or returning another run's.)
    #[tokio::test]
    async fn run_id_survives_await_across_threads() {
        // Force a yield point so the scheduler has the opportunity to move us
        // to a different Tokio scheduler thread.
        let captured = with_run_id("run-xyz".to_string(), async {
            tokio::task::yield_now().await;
            // After yield, we may be on a different thread — task_local must
            // still hold the value.
            current_run_id()
        })
        .await;
        assert_eq!(captured.as_deref(), Some("run-xyz"));
    }

    /// Outside a `with_run_id` scope, `current_run_id` must be None.
    #[tokio::test]
    async fn run_id_absent_outside_scope() {
        // Use a multi-thread runtime to make any latent thread_local bug
        // surface; task_local must still correctly return None.
        assert_eq!(current_run_id(), None);
    }

    /// Nested scopes must shadow correctly (inner wins, outer restored).
    #[tokio::test]
    async fn nested_scopes_shadow_and_restore() {
        let inner = with_run_id("outer".to_string(), async {
            with_run_id("inner".to_string(), async { current_run_id() }).await
        })
        .await;
        assert_eq!(inner.as_deref(), Some("inner"));
    }

    #[tokio::test]
    async fn tool_context_scopes_run_and_cancellation_together() -> std::result::Result<(), String>
    {
        let parent_cancel = tokio_util::sync::CancellationToken::new();
        let ctx = echo_core::tools::ToolContext {
            run_id: Some("run-context".to_string()),
            cancel: Some(Arc::new(parent_cancel.clone())),
            ..Default::default()
        };

        let (run_id, scoped_cancel) = scoped_with_ctx_run_id(&ctx, || async {
            let run_id = current_run_id();
            let cancel = CURRENT_CANCEL
                .try_with(Clone::clone)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((run_id, cancel))
        })
        .await?;

        assert_eq!(run_id.as_deref(), Some("run-context"));
        assert!(!scoped_cancel.is_cancelled());
        parent_cancel.cancel();
        assert!(scoped_cancel.is_cancelled());
        Ok(())
    }

    // ── stage4 P4.1: cache_user_id single-source ────────────────────────────
    // with_run_context no longer threads a cache_user_id param — tools that
    // need the id read it from config / load_or_create_cache_user_id() instead.
    // This compile-time assertion guards the signature change.
    #[tokio::test]
    async fn with_run_context_drops_cache_user_id_param() {
        let result: i32 = with_run_context(
            "r1".to_string(),
            tokio_util::sync::CancellationToken::new(),
            None, // trace_sink
            async { 42 },
        )
        .await;
        assert_eq!(result, 42);
    }
}

fn parse_kind(s: &str) -> std::result::Result<PlanTaskKind, String> {
    PlanTaskKind::from_str(s).ok_or_else(|| format!("unknown task kind '{s}'"))
}

fn string_array_from(params: &ToolParameters, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn string_array_in(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_plan_task(
    value: &serde_json::Value,
    index: usize,
    domain_profile: DomainProfile,
) -> std::result::Result<PlanTask, String> {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("tasks[{index}].{key} is required"))
    };
    let id = field("id")?;
    let title = field("title")?;
    let description = field("description")?;
    let kind_name = field("kind")?;
    let kind = parse_kind(&kind_name)?;
    let agent_role = value
        .get("subagent")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_subagent_for(domain_profile, kind).to_string());
    let max_retries = value
        .get("max_retries")
        .and_then(|item| item.as_u64())
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(3);
    let sort_order = i64::try_from(index).unwrap_or(i64::MAX);
    Ok(PlanTask {
        id,
        title,
        description,
        kind,
        agent_role,
        domain_profile,
        depends_on: string_array_in(value, "depends_on"),
        parallel_group: value
            .get("parallel_group")
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string),
        files: string_array_in(value, "files"),
        allowed_tools: string_array_in(value, "allowed_tools"),
        required_artifacts: string_array_in(value, "required_artifacts"),
        execution_checks: string_array_in(value, "execution_checks"),
        acceptance_criteria: string_array_in(value, "acceptance_criteria"),
        retry_count: 0,
        max_retries,
        failure_fingerprint: None,
        status: TodoStatus::Pending,
        sort_order,
    })
}

fn plan_task_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string", "description": "Stable task id unique within this run" },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "kind": { "type": "string", "enum": ["implementation","debugging","verification","review","investigation","test_plan","summary","read_only_review"] },
            "subagent": { "type": "string", "description": "Registered Subagent role; omit for the domain default" },
            "depends_on": { "type": "array", "items": { "type": "string" } },
            "parallel_group": { "type": "string" },
            "files": { "type": "array", "items": { "type": "string" } },
            "allowed_tools": { "type": "array", "items": { "type": "string" } },
            "required_artifacts": { "type": "array", "items": { "type": "string" } },
            "execution_checks": { "type": "array", "items": { "type": "string" } },
            "acceptance_criteria": { "type": "array", "items": { "type": "string" } },
            "max_retries": { "type": "integer", "minimum": 0, "maximum": 10 }
        },
        "required": ["id", "title", "description", "kind"]
    })
}

fn plan_patch_operations_from(
    value: &serde_json::Value,
    domain_profile: DomainProfile,
) -> std::result::Result<Vec<PlanPatchOperation>, String> {
    let operations = value
        .as_array()
        .ok_or_else(|| "plan_patch operations must be an array".to_string())?;
    let mut parsed = Vec::with_capacity(operations.len());
    for (index, operation) in operations.iter().enumerate() {
        let op = operation
            .get("op")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("operations[{index}].op is required"))?;
        let parsed_operation = match op {
            "insert" => {
                let task = operation
                    .get("task")
                    .ok_or_else(|| format!("operations[{index}].task is required"))?;
                PlanPatchOperation::Insert {
                    after_task_id: operation
                        .get("after_task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    task: parse_plan_task(task, index, domain_profile)?.spec(),
                }
            }
            "update" => PlanPatchOperation::Update {
                task_id: operation
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|task_id| !task_id.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| format!("operations[{index}].task_id is required"))?,
                patch: operation
                    .get("patch")
                    .cloned()
                    .ok_or_else(|| format!("operations[{index}].patch is required"))
                    .and_then(|patch| {
                        serde_json::from_value(patch)
                            .map_err(|error| format!("operations[{index}].patch: {error}"))
                    })?,
            },
            "skip" => PlanPatchOperation::Skip {
                task_id: operation
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|task_id| !task_id.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| format!("operations[{index}].task_id is required"))?,
            },
            "reorder" => PlanPatchOperation::Reorder {
                task_ids: string_array_in(operation, "task_ids"),
            },
            other => return Err(format!("operations[{index}] has unknown op '{other}'")),
        };
        parsed.push(parsed_operation);
    }
    Ok(parsed)
}

// ── plan_create ───────────────────────────────────────────────────────────

pub struct TaskCreateTool {
    pub store: Arc<TaskRuntimeStore>,
    pub capabilities: Arc<PlanCapabilityCatalog>,
}

impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "plan_create"
    }

    fn description(&self) -> &str {
        "Atomically create the complete formal PlanTask DAG as revision 1. Submit every intended task in one call with stable ids and explicit dependencies. The TaskRun already represents the user goal, so do not create a wrapper task."
    }

    fn parameters(&self) -> serde_json::Value {
        let task_schema = plan_task_input_schema();
        serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The complete DAG. All dependency ids must refer to tasks in this array.",
                    "items": task_schema
                },
                "assumptions": { "type": "array", "items": { "type": "string" } },
                "risks": { "type": "array", "items": { "type": "string" } },
                "execution_mode": { "type": "string", "enum": ["parallel", "sequential"] }
            },
            "required": ["tasks"]
        })
    }

    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            self.create_task(run_id, params, None).await
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match run_id_from_ctx_or_local(ctx) {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            self.create_task(run_id, params, Some(ctx)).await
        })
    }
}

impl TaskCreateTool {
    async fn create_task(
        &self,
        run_id: String,
        params: ToolParameters,
        bootstrap_ctx: Option<&echo_core::tools::ToolContext>,
    ) -> echo_agent::error::Result<ToolResult> {
        let Some(raw_tasks) = params.get("tasks").and_then(|value| value.as_array()) else {
            return Ok(ToolResult::error(
                "plan_create requires a non-empty tasks array",
            ));
        };
        let Some(first) = raw_tasks.first() else {
            return Ok(ToolResult::error("plan_create requires at least one task"));
        };
        let bootstrap_title = first
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or("Complex task");
        let bootstrap_description = first
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or(bootstrap_title);
        if let Err(e) = self.ensure_run_exists(
            &run_id,
            bootstrap_title,
            bootstrap_description,
            bootstrap_ctx,
        ) {
            return Ok(e);
        }
        let run = match self.store.get_run(&run_id) {
            Ok(Some(run)) => run,
            Ok(None) => {
                return Ok(ToolResult::error(
                    "Task run disappeared after plan_create bootstrap",
                ));
            }
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "Failed to read task run domain: {error}"
                )));
            }
        };

        let mut tasks = Vec::with_capacity(raw_tasks.len());
        for (index, value) in raw_tasks.iter().enumerate() {
            let task = match parse_plan_task(value, index, run.domain_profile) {
                Ok(task) => task,
                Err(error) => return Ok(ToolResult::error(error)),
            };
            if let Err(error) = self.capabilities.validate_task(&task) {
                return Ok(ToolResult::error(error));
            }
            tasks.push(task);
        }
        let execution_mode = params
            .get("execution_mode")
            .and_then(|value| value.as_str())
            .and_then(ExecutionMode::from_str)
            .unwrap_or(ExecutionMode::Parallel);
        let plan = TaskPlan {
            plan_id: format!("plan_{}", uuid::Uuid::new_v4().as_simple()),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: run.domain_profile,
            goal: run.goal,
            assumptions: string_array_from(&params, "assumptions"),
            risks: string_array_from(&params, "risks"),
            execution_mode,
            tasks,
        };
        match self.store.attach_plan(&plan) {
            Ok(()) => Ok(ToolResult::success(format!(
                "Created plan revision 1 with {} task(s). Call plan_execute with plan_revision=1.",
                plan.tasks.len()
            ))),
            Err(error) => Ok(ToolResult::error(format!("Failed to create plan: {error}"))),
        }
    }

    #[allow(clippy::result_large_err)] // ToolResult is the framework error type used by Tool::execute
    fn ensure_run_exists(
        &self,
        run_id: &str,
        title: &str,
        description: &str,
        bootstrap_ctx: Option<&echo_core::tools::ToolContext>,
    ) -> std::result::Result<(), ToolResult> {
        match self.store.get_run(run_id) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => {
                return Err(ToolResult::error(format!(
                    "Failed to inspect task run before creating task: {e}"
                )));
            }
        }

        let chat_resources = crate::chat_resources::current_chat_resources();
        let conversation_id = bootstrap_ctx
            .and_then(|ctx| ctx.conversation_id.clone())
            .or_else(|| chat_resources.as_ref().and_then(|res| res.conv_id.clone()));
        let root_message_id = bootstrap_ctx
            .and_then(|ctx| ctx.message_id.clone().or_else(|| ctx.turn_id.clone()))
            .or_else(|| {
                chat_resources
                    .as_ref()
                    .map(|res| res.root_message_id.clone())
            })
            .unwrap_or_else(|| run_id.to_string());
        let attachments = chat_resources
            .as_ref()
            .map(|res| res.attachments.clone())
            .unwrap_or_default();
        let trace_sink = bootstrap_ctx
            .and_then(trace_sink_from_tool_context)
            .or_else(|| {
                chat_resources
                    .as_ref()
                    .map(|res| crate::chat_driver::subagent_trace_sink_for(&res.sink))
            });
        let conversation_id = conversation_id.unwrap_or_else(|| format!("message:{run_id}"));
        let goal = task_goal(title, description);

        if let Err(e) = self.store.create_run(
            run_id,
            "default",
            &conversation_id,
            &root_message_id,
            DomainProfile::General,
            &goal,
            "agent_task_plan",
            AttendedMode::Attended,
        ) {
            return Err(ToolResult::error(format!(
                "Failed to create task run before creating task: {e}"
            )));
        }
        #[allow(clippy::collapsible_if)]
        // outer guard + inner if-let-Err reads clearer than a let-chain
        if !attachments.is_empty() {
            if let Err(e) = self.store.set_run_attachments(run_id, &attachments) {
                tracing::warn!(run_id, error = %e, "failed to bind attachments to task run");
            }
        }
        if let Err(e) = self.store.transition_run(run_id, TaskRunStatus::Running) {
            return Err(ToolResult::error(format!(
                "Failed to start task run before creating task: {e}"
            )));
        }
        if let Some(sink) = trace_sink {
            sink(ExecEvent::run(
                run_id.to_string(),
                "run_started",
                serde_json::json!({
                    "goal": goal,
                    "route": "agent_task_plan",
                    "source": "plan_create",
                }),
            ));
        }
        Ok(())
    }
}

// ── plan_patch ────────────────────────────────────────────────────────────

pub struct PlanPatchTool {
    pub store: Arc<TaskRuntimeStore>,
    pub capabilities: Arc<PlanCapabilityCatalog>,
}

impl Tool for PlanPatchTool {
    fn name(&self) -> &str {
        "plan_patch"
    }

    fn description(&self) -> &str {
        "Atomically revise the current formal plan using optimistic concurrency. Only pending or blocked task specifications may change while a run is active."
    }

    fn parameters(&self) -> serde_json::Value {
        let task_schema = plan_task_input_schema();
        serde_json::json!({
            "type": "object",
            "properties": {
                "base_revision": { "type": "integer", "minimum": 1 },
                "reason": { "type": "string", "description": "Why runtime evidence requires this revision" },
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "op": { "const": "insert" },
                                    "after_task_id": { "type": ["string", "null"] },
                                    "task": task_schema
                                },
                                "required": ["op", "task"]
                            },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "op": { "const": "update" },
                                    "task_id": { "type": "string" },
                                    "patch": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": {
                                            "title": { "type": "string" },
                                            "description": { "type": "string" },
                                            "kind": { "type": "string", "enum": ["implementation","debugging","verification","review","investigation","test_plan","summary","read_only_review"] },
                                            "agent_role": { "type": "string" },
                                            "depends_on": { "type": "array", "items": { "type": "string" } },
                                            "files": { "type": "array", "items": { "type": "string" } },
                                            "allowed_tools": { "type": "array", "items": { "type": "string" } },
                                            "required_artifacts": { "type": "array", "items": { "type": "string" } },
                                            "execution_checks": { "type": "array", "items": { "type": "string" } },
                                            "acceptance_criteria": { "type": "array", "items": { "type": "string" } },
                                            "max_retries": { "type": "integer", "minimum": 0, "maximum": 10 }
                                        }
                                    }
                                },
                                "required": ["op", "task_id", "patch"]
                            },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "op": { "const": "skip" },
                                    "task_id": { "type": "string" }
                                },
                                "required": ["op", "task_id"]
                            },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "op": { "const": "reorder" },
                                    "task_ids": { "type": "array", "items": { "type": "string" } }
                                },
                                "required": ["op", "task_ids"]
                            }
                        ]
                    }
                }
            },
            "required": ["base_revision", "reason", "operations"]
        })
    }

    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(error) => return Ok(error),
            };
            let Some(base_revision) = params.get("base_revision").and_then(|value| value.as_u64())
            else {
                return Ok(ToolResult::error("plan_patch requires base_revision"));
            };
            let reason = params
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let current_plan = match self.store.get_plan(&run_id) {
                Ok(Some(plan)) => plan,
                Ok(None) => return Ok(ToolResult::error("plan_patch requires an existing plan")),
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "Failed to read current plan: {error}"
                    )));
                }
            };
            let Some(raw_operations) = params.get("operations") else {
                return Ok(ToolResult::error("plan_patch requires operations"));
            };
            let operations =
                match plan_patch_operations_from(raw_operations, current_plan.domain_profile) {
                    Ok(operations) => operations,
                    Err(error) => return Ok(ToolResult::error(error)),
                };
            for operation in &operations {
                if let Err(error) = self.capabilities.validate_patch_operation(operation) {
                    return Ok(ToolResult::error(error));
                }
            }
            let request = PlanPatchRequest {
                base_revision,
                reason,
                operations,
            };
            match self.store.patch_plan(&run_id, &request) {
                Ok(plan) => Ok(ToolResult::success(format!(
                    "Committed plan revision {} with {} task(s)",
                    plan.revision,
                    plan.tasks.len()
                ))),
                Err(error) => Ok(ToolResult::error(format!("Failed to patch plan: {error}"))),
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match run_id_from_ctx_or_local(ctx) {
                Ok(id) => id,
                Err(error) => return Ok(error),
            };
            with_run_id(run_id, self.execute(params)).await
        })
    }
}

fn task_goal(title: &str, description: &str) -> String {
    if !description.trim().is_empty() {
        description.to_string()
    } else if !title.trim().is_empty() {
        title.to_string()
    } else {
        "Agent task plan".to_string()
    }
}

fn complex_run_prompt(
    user_goal: &str,
    reason: &str,
    domain: DomainProfile,
    plan_mode: &str,
    initial_plan: &[String],
) -> String {
    let template = ProfileTemplate::for_profile(domain);
    let plan_contract = if plan_mode == "direct_execute" {
        "Complete the goal directly with ordinary tools when that remains the lightest reliable path. Do not create a placeholder plan merely for ceremony. If execution reveals real dependencies, parallel work, or separately verifiable outcomes, upgrade by submitting the complete DAG in one plan_create call and execute its returned revision."
    } else {
        "This run requires a formal, reviewable DAG. The TaskRun already represents the overall goal, so do not create a wrapper, placeholder, or prose-only summary PlanTask for it. Submit every executable node together in one plan_create call with stable ids and explicit dependencies. Assign an appropriate Subagent to every node and declare artifacts, files, executable checks, and semantic acceptance criteria. A Subagent completing is not the PlanTask completing — tasks blocked on acceptance pause the run and wait for explicit retry. Execute exactly the committed revision returned by plan_create or plan_patch."
    };
    let initial = if initial_plan.is_empty() {
        "None supplied; derive the smallest complete decomposition from evidence.".to_string()
    } else {
        initial_plan
            .iter()
            .map(|step| format!("- {step}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "[complex_run]\nUser goal: {user_goal}\nComplexity rationale: {reason}\nDomain profile: {} ({})\nPlan mode: {plan_mode}\n\nRun contract:\n{plan_contract}\n\nDomain planning methodology:\n{}\n\nDomain execution standard:\n{}\n\nPreferred Subagents for this domain: {}\nAvailable builtin Subagents:\n{}\n\nInitial decomposition brief:\n{initial}\n[/complex_run]",
        template.key,
        template.label,
        template.prompt_suffix,
        template.execution_guidance,
        template.default_subagent_roles.join(", "),
        subagent_catalog_prompt(),
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // complex-task tool impls below are production code; moving them is churn
mod plan_create_tests {
    use super::super::types::RuntimeEventKind;
    use super::*;

    fn test_capabilities() -> Arc<PlanCapabilityCatalog> {
        Arc::new(PlanCapabilityCatalog::new(
            [
                "explorer".to_string(),
                "analyst".to_string(),
                "data-shaper".to_string(),
            ],
            Vec::<String>::new(),
        ))
    }

    fn one_task_params(task: serde_json::Value) -> ToolParameters {
        let mut params = ToolParameters::new();
        params.insert("tasks".to_string(), serde_json::json!([task]));
        params
    }

    #[tokio::test]
    async fn plan_create_bootstraps_run_before_plan_events() -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|e| e.to_string())?,
        );
        let tool = TaskCreateTool {
            store: store.clone(),
            capabilities: test_capabilities(),
        };
        let run_id = "run_plan_create_bootstrap";
        let params = one_task_params(serde_json::json!({
            "id": "architecture-review",
            "title": "分析当前项目架构",
            "description": "并行分析当前项目架构并汇总结果",
            "kind": "read_only_review"
        }));

        let result = with_run_id(run_id.to_string(), tool.execute(params))
            .await
            .map_err(|e| e.to_string())?;
        if !result.success {
            return Err(format!("plan_create failed: {:?}", result.error));
        }
        if result
            .output
            .contains(super::super::compact_context::RUNTIME_RECOVERY_MARKER)
        {
            return Err(
                "plan_create result must not embed the runtime recovery capsule".to_string(),
            );
        }
        if !result
            .output
            .contains("Created plan revision 1 with 1 task(s)")
        {
            return Err(format!(
                "plan_create must report the materialized task count: {}",
                result.output
            ));
        }

        let events = store.list_events(run_id, 0).map_err(|e| e.to_string())?;
        let first = events
            .first()
            .ok_or_else(|| "expected at least one runtime event".to_string())?;
        assert_eq!(first.event_type, RuntimeEventKind::RunCreated);

        let run = store.get_run(run_id).map_err(|e| e.to_string())?;
        assert!(run.is_some());
        let plan = store
            .get_plan(run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "expected bootstrapped plan".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].agent_role, "explorer");
        Ok(())
    }

    #[tokio::test]
    async fn plan_create_bootstrap_preserves_chat_identity_from_tool_context()
    -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let tool = TaskCreateTool {
            store: store.clone(),
            capabilities: test_capabilities(),
        };
        let run_id = "taskrun:message-identity";
        let params = one_task_params(serde_json::json!({
            "id": "parallel-review",
            "title": "并行架构分析",
            "description": "由多个 Subagent 分析当前项目",
            "kind": "read_only_review"
        }));
        let ctx = echo_core::tools::ToolContext {
            conversation_id: Some("conversation-identity".to_string()),
            run_id: Some(run_id.to_string()),
            turn_id: Some("turn-identity".to_string()),
            message_id: Some("assistant-message-identity".to_string()),
            ..Default::default()
        };

        let result = tool
            .execute_with_context(params, &ctx)
            .await
            .map_err(|error| error.to_string())?;
        if !result.success {
            return Err(format!("plan_create failed: {:?}", result.error));
        }

        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "expected bootstrapped run".to_string())?;
        assert_eq!(run.conversation_id, "conversation-identity");
        assert_eq!(run.root_message_id, "assistant-message-identity");
        Ok(())
    }

    #[tokio::test]
    async fn plan_create_inherits_domain_and_routes_data_subagents()
    -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let run_id = "run_data_profile";
        store
            .create_run(
                run_id,
                "default",
                "conversation:data",
                "message:data",
                DomainProfile::DataAnalysis,
                "清洗并分析销售数据",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let tool = TaskCreateTool {
            store: store.clone(),
            capabilities: test_capabilities(),
        };
        let mut params = ToolParameters::new();
        params.insert(
            "tasks".to_string(),
            serde_json::json!([
                {
                    "id": "analyze-metrics",
                    "title": "分析指标",
                    "description": "计算核心指标并验证不确定性",
                    "kind": "implementation"
                },
                {
                    "id": "shape-data",
                    "title": "清洗数据",
                    "description": "画像 schema 并导出清洗结果",
                    "kind": "implementation",
                    "subagent": "data-shaper"
                }
            ]),
        );
        let result = with_run_id(run_id.to_string(), tool.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        if !result.success {
            return Err(format!(
                "atomic data plan_create failed: {:?}",
                result.error
            ));
        }
        if !result
            .output
            .contains("Created plan revision 1 with 2 task(s)")
        {
            return Err(format!(
                "second plan_create must report two materialized tasks: {}",
                result.output
            ));
        }

        let plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "expected data plan".to_string())?;
        if plan.tasks.len() != 2 {
            return Err(format!("expected 2 tasks, got {}", plan.tasks.len()));
        }
        assert!(
            plan.tasks
                .iter()
                .all(|task| task.domain_profile == DomainProfile::DataAnalysis)
        );
        assert!(plan.tasks.iter().any(|task| task.agent_role == "analyst"));
        assert!(
            plan.tasks
                .iter()
                .any(|task| task.agent_role == "data-shaper")
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_patch_insert_accepts_plan_create_task_shape() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let capabilities = test_capabilities();
        let create = TaskCreateTool {
            store: store.clone(),
            capabilities: capabilities.clone(),
        };
        let run_id = "run_patch_shape";
        let created = with_run_id(
            run_id.to_string(),
            create.execute(one_task_params(serde_json::json!({
                "id": "inspect",
                "title": "Inspect runtime",
                "description": "Inspect the current runtime evidence",
                "kind": "investigation",
                "subagent": "explorer"
            }))),
        )
        .await
        .map_err(|error| error.to_string())?;
        if !created.success {
            return Err(format!("plan_create failed: {:?}", created.error));
        }

        let patch = PlanPatchTool {
            store: store.clone(),
            capabilities,
        };
        let mut params = ToolParameters::new();
        params.insert("base_revision".to_string(), serde_json::json!(1));
        params.insert(
            "reason".to_string(),
            serde_json::json!("inspection found a required verification"),
        );
        params.insert(
            "operations".to_string(),
            serde_json::json!([{
                "op": "insert",
                "after_task_id": "inspect",
                "task": {
                    "id": "verify",
                    "title": "Verify runtime",
                    "description": "Verify the discovered runtime contract",
                    "kind": "verification",
                    "subagent": "explorer",
                    "depends_on": ["inspect"]
                }
            }]),
        );
        let result = with_run_id(run_id.to_string(), patch.execute(params))
            .await
            .map_err(|error| error.to_string())?;
        if !result.success {
            return Err(format!("plan_patch failed: {:?}", result.error));
        }
        let plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "patched plan missing".to_string())?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[1].agent_role, "explorer");
        Ok(())
    }

    #[tokio::test]
    async fn plan_create_rejects_plan_control_tools_in_subagent_allowlist()
    -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = TaskCreateTool {
            store,
            capabilities: test_capabilities(),
        };
        let result = with_run_id(
            "run_forbidden_tool".to_string(),
            tool.execute(one_task_params(serde_json::json!({
                "id": "bad-tools",
                "title": "Bad tools",
                "description": "Attempt to delegate plan control",
                "kind": "investigation",
                "subagent": "explorer",
                "allowed_tools": ["plan_patch"]
            }))),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("cannot delegate plan-control tool")
        );
        Ok(())
    }

    #[test]
    fn complex_run_prompt_encodes_domain_and_plan_policy() {
        let prompt = complex_run_prompt(
            "分析临床研究",
            "multi_source synthesis",
            DomainProfile::MedicalResearch,
            "plan_then_execute",
            &["检索指南: 形成证据表".to_string()],
        );
        assert!(prompt.contains("medical_research"));
        assert!(prompt.contains("PICO"));
        assert!(prompt.contains("formal, reviewable DAG"));
        assert!(prompt.contains("do not create a wrapper"));
        assert!(prompt.contains("Available builtin Subagents"));
        assert!(prompt.contains("检索指南: 形成证据表"));
    }
}

// ── create_complex_task (Phase B3) ────────────────────────────────────────
//
// The "nuclear button": lets the main agent autonomously spin up a background
// Run when it judges a task complex. Reads pool/store/sink from the chat
// turn's task_local (`current_chat_resources`, scoped by `drive_chat`), so it
// only works during an active chat turn. `reason` is required as a CoT
// anti-misfire gate (spec §4.2). foreground blocks the turn + streams subagent
// events (Claude Code Task style); background spawns + returns run_id (spec §6
// 主从异步). Default background (spec §4.1 Priority Trap).

/// Create a background orchestrated Run for a complex multi-step task.
pub struct CreateComplexTaskTool;

impl Tool for CreateComplexTaskTool {
    fn name(&self) -> &str {
        "create_complex_task"
    }

    fn description(&self) -> &str {
        r#"Create an independent orchestrated Run for work that should outlive the current chat turn or needs substantial multi-step coordination. Use it only when a material complexity signal applies: expensive dependent steps, multi-file/architectural implementation, long-lived state, or multi-source synthesis. Use direct work or a single subagent for narrower requests. `reason` must name the signals and why ordinary in-turn execution is insufficient. Prefer background so the user can continue; choose foreground only when the current reply depends on a prompt result."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["user_goal", "reason", "domain_profile", "plan_mode"],
            "properties": {
                "user_goal": { "type": "string", "description": "The user's full goal (verbatim or distilled), as the Run's goal." },
                "reason": { "type": "string", "description": "Why an independent Run is warranted. Name the material signals: dependent multi_step work, multi_file/architectural change, long_running/cross_turn state, or multi_source synthesis." },
                "domain_profile": { "type": "string", "enum": ["general","ai_coding","data_analysis","academic_research","medical_research"], "description": "Best-fit evidence and review profile. Cross-domain tasks should choose the profile that governs the final claim or artifact." },
                "plan_mode": { "type": "string", "enum": ["plan_then_execute","direct_execute"], "description": "Use plan_then_execute when the work benefits from an explicit reviewable DAG; use direct_execute only when autonomous ReAct is sufficient and a formal plan adds no value." },
                "initial_plan": { "type": "array", "items": { "type": "object", "properties": { "step_name": {"type":"string"}, "expected_outcome": {"type":"string"} }, "required": ["step_name"] }, "description": "Optional coarse decomposition (>=2 steps) as a brief. Not the PlanTask DAG — the Run's agent refines via plan_create." },
                "priority": { "type": "string", "enum": ["foreground","background"], "default": "background", "description": "Default background (returns run_id immediately, non-blocking). foreground only for <1min tasks where you need the result in this turn (blocks the UI until done)." }
            }
        })
    }

    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { self.do_create(params).await })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { self.do_create(params).await })
    }
}

impl CreateComplexTaskTool {
    async fn do_create(&self, params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task can only be used during an active chat turn (no chat resources available)",
                ));
            }
        };
        let pool = match res.pool.clone() {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task requires an AgentPool; none is available in this context",
                ));
            }
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task requires a TaskRuntimeStore; none is available in this context",
                ));
            }
        };

        let user_goal = params
            .get("user_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if user_goal.is_empty() {
            return Ok(ToolResult::error("user_goal is required"));
        }
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if reason.is_empty() {
            return Ok(ToolResult::error(
                "reason is required — list the complexity signals (multi_step/needs_research/needs_code_gen/long_running/multi_file) that justify this task",
            ));
        }
        let domain_profile_str = params
            .get("domain_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("general");
        let domain = super::types::DomainProfile::from_str(domain_profile_str)
            .unwrap_or(super::types::DomainProfile::General);
        let plan_mode = params
            .get("plan_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("plan_then_execute");
        let plan_policy = if plan_mode == "direct_execute" {
            RunPlanPolicy::AllowDirect
        } else {
            RunPlanPolicy::RequirePlan
        };
        let priority = params
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("background");
        let initial_plan: Vec<String> = params
            .get("initial_plan")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        let step_name = v
                            .get("step_name")
                            .and_then(|s| s.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())?;
                        let expected = v
                            .get("expected_outcome")
                            .and_then(|s| s.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty());
                        Some(match expected {
                            Some(expected) => format!("{step_name}: {expected}"),
                            None => step_name.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let goal = if initial_plan.is_empty() {
            user_goal.clone()
        } else {
            format!(
                "{user_goal}\n\nInitial plan:\n{}",
                initial_plan
                    .iter()
                    .map(|s| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let run_prompt = complex_run_prompt(&user_goal, &reason, domain, plan_mode, &initial_plan);

        let run_id = uuid::Uuid::new_v4().to_string();
        let conv = res
            .conv_id
            .clone()
            .unwrap_or_else(|| format!("message:{run_id}"));
        let attended = super::types::AttendedMode::Attended;
        if let Err(e) = store.create_run(
            &run_id,
            "default",
            &conv,
            &res.root_message_id,
            domain,
            &goal,
            "agent_autonomous",
            attended,
        ) {
            return Ok(ToolResult::error(format!("Failed to create run: {e}")));
        }
        #[allow(clippy::collapsible_if)]
        // outer guard + inner if-let-Err reads clearer than a let-chain
        if !res.attachments.is_empty() {
            if let Err(e) = store.set_run_attachments(&run_id, &res.attachments) {
                tracing::warn!(error = %e, "failed to bind attachments to run");
            }
        }
        if let Err(e) = store.transition_run(&run_id, super::types::TaskRunStatus::Running) {
            return Ok(ToolResult::error(format!(
                "Failed to transition run to Running: {e}"
            )));
        }
        // Independent cancel token: background runs must not reuse the chat
        // turn's token. `drive_run_async` registers it in the single runtime
        // cancellation registry before acquiring the isolated agent.
        let run_cancel = echo_agent::agent::CancellationToken::new();

        let trace_sink = if priority == "foreground" {
            Some(crate::chat_driver::subagent_trace_sink_for(&res.sink))
        } else {
            None
        };
        let payload = crate::run_driver::RunPayload {
            run_id: run_id.clone(),
            pool,
            store: store.clone(),
            cancel: run_cancel,
            // B5.1: forward the chat turn's memory layer so the run's Blocking
            // memory write (drive_run_async → execute_run) actually lands the
            // taskrun:completed:{run_id} memory. None when no memory subsystem
            // is wired (then the Blocking write is a no-op).
            layer_manager: res.layer_manager.clone(),
            trace_sink,
            prompt: run_prompt,
            plan_policy,
        };

        if priority == "foreground" {
            // Block the turn: drive_run_async streams subagent events to the chat
            // sink (via trace_sink), returns the terminal RunOutcome so the
            // agent can use the result in-turn (Claude Code Task style).
            match crate::run_driver::drive_run_async(payload).await {
                Ok(outcome) => {
                    use super::executor::RunOutcome;
                    let terminal = match outcome {
                        RunOutcome::Completed => "completed",
                        RunOutcome::Failed { .. } => "failed",
                        RunOutcome::Cancelled => "cancelled",
                        RunOutcome::Paused { .. } => "paused",
                    };
                    Ok(ToolResult::success(
                        serde_json::json!({"status":"completed","run_id":run_id,"terminal":terminal})
                            .to_string(),
                    ))
                }
                Err(e) => Ok(ToolResult::error(format!("Run failed: {e}"))),
            }
        } else {
            // Background: spawn + return immediately (decoupled, spec §6).
            tokio::spawn(crate::run_driver::drive_run_async(payload));
            Ok(ToolResult::success(
                serde_json::json!({"status":"accepted","run_id":run_id}).to_string(),
            ))
        }
    }
}

// ── check_run_status / cancel_run (Phase B3) ──────────────────────────────

/// Check the status of a Run created by `create_complex_task`.
pub struct CheckRunStatusTool;

impl Tool for CheckRunStatusTool {
    fn name(&self) -> &str {
        "check_run_status"
    }
    fn description(&self) -> &str {
        "Check the status of a background Run created by create_complex_task. Returns {status, goal}."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type":"object",
            "properties":{"run_id":{"type":"string","description":"The run_id returned by create_complex_task"}},
            "required":["run_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_check(params).await })
    }
    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_check(params).await })
    }
}

impl CheckRunStatusTool {
    async fn do_check(params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let run_id = match params.get("run_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("run_id is required")),
        };
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => {
                return Ok(ToolResult::error(
                    "check_run_status requires an active chat turn",
                ));
            }
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => return Ok(ToolResult::error("no TaskRuntimeStore available")),
        };
        match store.get_run(&run_id) {
            Ok(Some(run)) => Ok(ToolResult::success(
                serde_json::json!({"status": format!("{:?}", run.status), "goal": run.goal})
                    .to_string(),
            )),
            Ok(None) => Ok(ToolResult::error(format!("Run {run_id} not found"))),
            Err(e) => Ok(ToolResult::error(format!("Failed to read run: {e}"))),
        }
    }
}

/// Cancel a background Run by run_id.
pub struct CancelRunTool;

impl Tool for CancelRunTool {
    fn name(&self) -> &str {
        "cancel_run"
    }
    fn description(&self) -> &str {
        "Cancel a background Run by run_id. Use when a task is no longer needed or going wrong."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type":"object",
            "properties":{"run_id":{"type":"string"}},
            "required":["run_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_cancel(params).await })
    }
    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_cancel(params).await })
    }
}

impl CancelRunTool {
    async fn do_cancel(params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let run_id = match params.get("run_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("run_id is required")),
        };
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => return Ok(ToolResult::error("cancel_run requires an active chat turn")),
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => return Ok(ToolResult::error("no TaskRuntimeStore available")),
        };
        match store.request_cancel(&run_id) {
            Ok(cancelled) => Ok(ToolResult::success(
                serde_json::json!({"run_id": run_id, "cancelled": cancelled}).to_string(),
            )),
            Err(error) => Ok(ToolResult::error(format!(
                "Failed to cancel run {run_id}: {error}"
            ))),
        }
    }
}

// ── task_list ─────────────────────────────────────────────────────────────

pub struct TaskListTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }
    fn description(&self) -> &str {
        "List all tasks in the current plan with their status."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn execute<'a>(
        &'a self,
        _params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            match (self.store.get_plan(&run_id), self.store.list_todos(&run_id)) {
                (Ok(Some(plan)), Ok(todos)) => {
                    let lines: Vec<String> = todos
                        .iter()
                        .map(|t| format!("[{}] {} — {}", t.status.as_str(), t.task_id, t.title))
                        .collect();
                    Ok(ToolResult::success(format!(
                        "Plan revision {} — Tasks ({}):\n{}",
                        plan.revision,
                        todos.len(),
                        lines.join("\n")
                    )))
                }
                (Ok(None), _) => Ok(ToolResult::error("No committed plan")),
                (Err(error), _) | (_, Err(error)) => {
                    Ok(ToolResult::error(format!("Failed: {error}")))
                }
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { scoped_with_ctx_run_id(ctx, || self.execute(params)).await })
    }
}
