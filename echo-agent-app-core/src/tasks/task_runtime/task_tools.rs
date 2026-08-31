//! EKO product tools and task-tool policy helpers.
//!
//! The framework owns `task_create`, `task_update`, and `task_list`. This
//! module keeps EKO's run-level tools, task-local execution context, and
//! Subagent/tool capability catalog.
//!
//! Each tool reads `run_id` from a `tokio::task_local!` scoped by the executor
//! around task execution, and operates on the
//! [`TaskRuntimeStore`](super::store::TaskRuntimeStore) injected at
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

use crate::subagent_loader::SubagentCatalogSnapshot;

use super::executor::{ExecEvent, RunOutcome, RunPlanPolicy};
use super::profiles::ProfileTemplate;
use super::types::{DomainProfile, EkoTaskExtension};

#[derive(Debug, Clone, Default)]
pub struct TaskCapabilityCatalog {
    subagents: Arc<SubagentCatalogSnapshot>,
    tools: HashSet<String>,
}

impl TaskCapabilityCatalog {
    const TASK_CONTROL_TOOLS: [&'static str; 4] =
        ["task_create", "task_update", "task_list", "task_execute"];

    pub fn new(
        subagents: Arc<SubagentCatalogSnapshot>,
        tools: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            subagents,
            tools: tools.into_iter().collect(),
        }
    }

    pub(crate) fn validate_task_spec(
        &self,
        task: &echo_agent::tasks::TaskSpec,
    ) -> std::result::Result<(), String> {
        let extension: EkoTaskExtension = task
            .extension_as()
            .map_err(|error| format!("task '{}' has invalid EKO extension: {error}", task.id))?;
        if !self.subagents.contains(&extension.agent_role) {
            let mut available = self
                .subagents
                .names()
                .map(str::to_string)
                .collect::<Vec<_>>();
            available.sort();
            return Err(format!(
                "unknown Subagent '{}'; available: {}",
                extension.agent_role,
                available.join(", ")
            ));
        }
        for tool in &extension.allowed_tools {
            if Self::TASK_CONTROL_TOOLS.contains(&tool.as_str()) {
                return Err(format!(
                    "task '{}' cannot delegate task-control tool '{}' to a Subagent",
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
    /// CURRENT_RUN_ID so task_execute and other tools can read it.
    pub static CURRENT_CANCEL: tokio_util::sync::CancellationToken;
    /// Delegate nesting depth — incremented each time a subagent is delegated
    /// during tool execution. Used by Task 6 (L3 nesting) to prevent runaway
    /// recursion and to route subagent tool calls correctly.
    pub static CURRENT_DELEGATE_DEPTH: std::cell::Cell<u32>;
    /// An optional trace-sink that forwards [`ExecEvent`] items out of the
    /// executor so the frontend can render real-time execution-flow views. Set
    /// alongside `CURRENT_RUN_ID` by [`with_run_context`].
    pub static CURRENT_TRACE_SINK: Option<TraceSink>;
    /// Exact EKO product-data root plus opaque framework lifetime guards.
    /// `task_execute` receives this from its value-carried ToolContext.
    pub static CURRENT_WORKSPACE_IO: Option<crate::state::WorkspaceIoInvocation>;
    /// The unattended write mode for the currently executing run (D7 stage 2).
    /// Set by `ExecuteTaskTool::execute` so CP B preflight in `execute_task`
    /// can read it without threading the mode through `execute_run` →
    /// runtime executor → EKO controller → `execute_task`. Defaults to `Disabled` when no scope is
    /// active (e.g. tests, attended runs).
    pub static CURRENT_UNATTENDED_WRITE_MODE: super::types::UnattendedWriteMode;
}

/// Run `f` with run_id, cancel, delegate_depth, and trace_sink available to all
/// task tools.
///
/// Called by the executor before dispatching task work. Delegate depth starts
/// at 0 and is incremented by the nested delegation layer.
///
/// `cache_user_id` is not threaded here; tools and LLM calls
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

/// Test helper for installing only a run id around one future.
#[cfg(test)]
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

pub(crate) fn current_workspace_io() -> Option<crate::state::WorkspaceIoInvocation> {
    CURRENT_WORKSPACE_IO.try_with(Clone::clone).ok().flatten()
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

pub(crate) fn trace_sink_from_tool_context(
    ctx: &echo_agent::tools::ToolContext,
) -> Option<TraceSink> {
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
    ctx: &echo_agent::tools::ToolContext,
    f: F,
) -> R
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let workspace_io = crate::state::WorkspaceIoInvocation::from_context(ctx);
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
            CURRENT_WORKSPACE_IO
                .scope(
                    workspace_io,
                    CURRENT_CANCEL.scope(
                        cancel,
                        CURRENT_RUN_ID.scope(rid, CURRENT_TRACE_SINK.scope(trace_sink, f())),
                    ),
                )
                .await
        }
        None => CURRENT_WORKSPACE_IO.scope(workspace_io, f()).await,
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
        let ctx = echo_agent::tools::ToolContext {
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

fn complex_run_prompt(
    user_goal: &str,
    reason: &str,
    domain: DomainProfile,
    plan_mode: &str,
    initial_plan: &[String],
    subagent_catalog: &SubagentCatalogSnapshot,
) -> String {
    let template = ProfileTemplate::for_profile(domain);
    let plan_contract = if plan_mode == "direct_execute" {
        "Complete the goal directly with ordinary tools when that remains the lightest reliable path. Do not create a placeholder plan merely for ceremony. If execution reveals real dependencies, parallel work, or separately verifiable outcomes, upgrade by submitting the complete DAG in one task_create call and execute its returned revision."
    } else {
        "This run requires a formal, reviewable DAG. The TaskRun already represents the overall goal, so do not create a wrapper, placeholder, or prose-only summary PlanTask for it. Submit every executable node together in one task_create call with stable ids and explicit dependencies. Assign an appropriate Subagent to every node and declare artifacts, files, executable checks, and semantic acceptance criteria. A Subagent completing is not the PlanTask completing — tasks blocked on acceptance pause the run and wait for explicit retry. Execute exactly the committed revision returned by task_create or task_update."
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
        "[complex_run]\nUser goal: {user_goal}\nComplexity rationale: {reason}\nDomain profile: {} ({})\nPlan mode: {plan_mode}\n\nRun contract:\n{plan_contract}\n\nDomain planning methodology:\n{}\n\nDomain execution standard:\n{}\n\nPreferred Subagents for this domain: {}\nAvailable Subagents:\n{}\n\nInitial decomposition brief:\n{initial}\n[/complex_run]",
        template.key,
        template.label,
        template.prompt_suffix,
        template.execution_guidance,
        template.default_subagent_roles.join(", "),
        subagent_catalog.prompt(),
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // complex-task tool impls below are production code; moving them is churn
mod task_create_tests {
    use super::super::store::TaskRuntimeStore;
    use super::super::types::RuntimeEventKind;
    use super::super::types::{AttendedMode, TaskRunStatus};
    use super::*;
    use echo_agent::tasks::{
        TaskCreateTool as FrameworkTaskCreateTool, TaskUpdateTool as FrameworkTaskUpdateTool,
    };

    fn test_subagent_catalog() -> Arc<SubagentCatalogSnapshot> {
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        Arc::new(SubagentCatalogSnapshot::from_definitions(&definitions))
    }

    fn test_capabilities() -> Arc<TaskCapabilityCatalog> {
        Arc::new(TaskCapabilityCatalog::new(
            test_subagent_catalog(),
            Vec::<String>::new(),
        ))
    }

    fn task_service(store: Arc<TaskRuntimeStore>) -> Arc<echo_agent::tasks::TaskRevisionService> {
        super::super::revisioned_runtime::build_task_revision_service(store, test_capabilities())
    }

    fn one_task_params(task: serde_json::Value) -> ToolParameters {
        let mut params = ToolParameters::new();
        params.insert("tasks".to_string(), serde_json::json!([task]));
        params
    }

    #[test]
    fn plan_task_schema_keeps_generated_briefs_in_user_language() -> std::result::Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let schema = FrameworkTaskCreateTool::new(task_service(store)).parameters();
        let task_prefix = if schema.pointer("/properties/tasks/items").is_some() {
            "/properties/tasks/items"
        } else {
            return Err("task_create schema is missing task input".to_string());
        };
        let title = schema
            .pointer(&format!("{task_prefix}/properties/title/description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let description = schema
            .pointer(&format!("{task_prefix}/properties/description/description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(title.contains("user's current language"));
        assert!(description.contains("user's current language"));
        Ok(())
    }

    #[tokio::test]
    async fn task_create_bootstraps_run_before_plan_events() -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|e| e.to_string())?,
        );
        let tool = FrameworkTaskCreateTool::new(task_service(store.clone()));
        let run_id = "run_task_create_bootstrap";
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
            return Err(format!("task_create failed: {:?}", result.error));
        }
        if result
            .output
            .contains(super::super::compact_context::RUNTIME_RECOVERY_MARKER)
        {
            return Err(
                "task_create result must not embed the runtime recovery capsule".to_string(),
            );
        }
        if !result
            .output
            .contains("Created task graph revision 1 with 1 task(s)")
        {
            return Err(format!(
                "task_create must report the materialized task count: {}",
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
        assert_eq!(
            plan.tasks.first().map(|task| task.agent_role.as_str()),
            Some("explorer")
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_create_appends_to_the_same_revisioned_graph() -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = FrameworkTaskCreateTool::new(task_service(store.clone()));
        let run_id = "run_incremental_task_create";
        let first = one_task_params(serde_json::json!({
            "id": "inspect",
            "title": "Inspect",
            "description": "Inspect the task runtime",
            "kind": "investigation",
            "subagent": "explorer"
        }));
        let first_result = with_run_id(run_id.to_string(), tool.execute(first))
            .await
            .map_err(|error| error.to_string())?;
        assert!(first_result.success);

        let mut second = one_task_params(serde_json::json!({
            "id": "verify",
            "title": "Verify",
            "description": "Verify the inspected runtime",
            "kind": "verification",
            "subagent": "explorer",
            "depends_on": ["inspect"]
        }));
        second.insert("base_revision".to_string(), serde_json::json!(1));
        let second_result = with_run_id(run_id.to_string(), tool.execute(second))
            .await
            .map_err(|error| error.to_string())?;
        assert!(second_result.success);
        assert!(
            second_result
                .output
                .contains("Created task graph revision 2 with 2 total task(s)")
        );

        let graph = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "task graph missing".to_string())?;
        assert_eq!(graph.revision, 2);
        assert_eq!(graph.tasks.len(), 2);
        assert_eq!(
            graph.tasks.get(1).map(|task| task.depends_on.as_slice()),
            Some(["inspect".to_string()].as_slice())
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_create_bootstrap_preserves_chat_identity_from_tool_context()
    -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let tool = FrameworkTaskCreateTool::new(task_service(store.clone()));
        let run_id = "taskrun:message-identity";
        let params = one_task_params(serde_json::json!({
            "id": "parallel-review",
            "title": "并行架构分析",
            "description": "由多个 Subagent 分析当前项目",
            "kind": "read_only_review"
        }));
        let ctx = echo_agent::tools::ToolContext {
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
            return Err(format!("task_create failed: {:?}", result.error));
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
    async fn task_create_inherits_domain_and_routes_data_subagents()
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
        let tool = FrameworkTaskCreateTool::new(task_service(store.clone()));
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
                "atomic data task_create failed: {:?}",
                result.error
            ));
        }
        if !result
            .output
            .contains("Created task graph revision 1 with 2 task(s)")
        {
            return Err(format!(
                "second task_create must report two materialized tasks: {}",
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
    async fn task_update_insert_accepts_task_create_task_shape() -> std::result::Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let create = FrameworkTaskCreateTool::new(task_service(store.clone()));
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
            return Err(format!("task_create failed: {:?}", created.error));
        }

        let patch = FrameworkTaskUpdateTool::new(task_service(store.clone()));
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
            return Err(format!("task_update failed: {:?}", result.error));
        }
        let plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "patched plan missing".to_string())?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(
            plan.tasks.get(1).map(|task| task.agent_role.as_str()),
            Some("explorer")
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_create_rejects_task_control_tools_in_subagent_allowlist()
    -> std::result::Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tool = FrameworkTaskCreateTool::new(task_service(store));
        let result = with_run_id(
            "run_forbidden_tool".to_string(),
            tool.execute(one_task_params(serde_json::json!({
                "id": "bad-tools",
                "title": "Bad tools",
                "description": "Attempt to delegate task control",
                "kind": "investigation",
                "subagent": "explorer",
                "allowed_tools": ["task_update"]
            }))),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("cannot delegate task-control tool")
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
            &test_subagent_catalog(),
        );
        assert!(prompt.contains("medical_research"));
        assert!(prompt.contains("PICO"));
        assert!(prompt.contains("formal, reviewable DAG"));
        assert!(prompt.contains("do not create a wrapper"));
        assert!(prompt.contains("Available Subagents"));
        assert!(prompt.contains("检索指南: 形成证据表"));
    }

    #[test]
    fn complex_run_prompt_includes_project_subagent() -> std::result::Result<(), String> {
        let project = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = project.path().join(".eko").join("subagents");
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(
            directory.join("domain-expert.md"),
            "---\nname: domain-expert\ndescription: \"project specialist\"\nreadonly: true\n---\n# Role\nInspect domain evidence.",
        )
        .map_err(|error| error.to_string())?;
        let definitions = crate::subagent_loader::discover_subagents(Some(project.path()), None);
        let catalog = SubagentCatalogSnapshot::from_definitions(&definitions);

        let prompt = complex_run_prompt(
            "inspect",
            "multi_source synthesis",
            DomainProfile::General,
            "plan_then_execute",
            &[],
            &catalog,
        );

        assert!(prompt.contains("domain-expert"));
        assert!(prompt.contains("project specialist"));
        Ok(())
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
pub struct CreateComplexTaskTool {
    pub subagent_catalog: Arc<SubagentCatalogSnapshot>,
}

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
                "initial_plan": { "type": "array", "items": { "type": "object", "properties": { "step_name": {"type":"string"}, "expected_outcome": {"type":"string"} }, "required": ["step_name"] }, "description": "Optional coarse decomposition (>=2 steps) as a brief. Not the PlanTask DAG — the Run's agent refines via task_create." },
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
        _ctx: &'a echo_agent::tools::ToolContext,
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
        let run_prompt = complex_run_prompt(
            &user_goal,
            &reason,
            domain,
            plan_mode,
            &initial_plan,
            &self.subagent_catalog,
        );

        let run_id = uuid::Uuid::new_v4().to_string();
        let conv = res
            .conv_id
            .clone()
            .unwrap_or_else(|| format!("message:{run_id}"));
        let attended = super::types::AttendedMode::Attended;
        // Independent cancel token: background runs must not reuse the chat
        // turn's token. Reserve the canonical driver before any TaskRuntime or
        // memory mutation so shutdown cannot overtake this accepted run.
        let run_cancel = echo_agent::agent::CancellationToken::new();
        let admission = match store.reserve_run_driver_admission(run_id.clone(), run_cancel.clone())
        {
            Ok(admission) => admission,
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "Task runtime driver admission failed: {error}"
                )));
            }
        };
        let generation_lease = match store.lease_active_workspace_generation() {
            Ok(lease) => lease,
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "Task runtime workspace admission failed: {error}"
                )));
            }
        };
        let mut registration =
            match store.register_run_driver::<RunOutcome>(admission, generation_lease) {
                Ok(registration) => registration,
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "Task runtime driver registration failed: {error}"
                    )));
                }
            };
        let memory_generation = match res
            .review_integration
            .as_ref()
            .map(|integration| integration.lease_generation())
            .transpose()
        {
            Ok(generation) => generation.or_else(|| res.memory_generation.clone()),
            Err(error) => {
                registration.reject(error.to_string());
                return Ok(ToolResult::error(format!(
                    "Memory unavailable during workspace transition: {error}"
                )));
            }
        };
        registration.mark_preparation_started();
        if let Err(e) = store.create_run_for_active_workspace(
            &run_id,
            &conv,
            &res.root_message_id,
            domain,
            &goal,
            "agent_autonomous",
            attended,
        ) {
            registration.fail_preparation(e.to_string());
            return Ok(ToolResult::error(format!("Failed to create run: {e}")));
        }
        #[allow(clippy::collapsible_if)]
        // outer guard + inner if-let-Err reads clearer than a let-chain
        if !res.attachments.is_empty() {
            if let Err(e) = store.set_run_attachments(&run_id, &res.attachments) {
                tracing::warn!(error = %e, "failed to bind attachments to run");
            }
        }
        if let Err(e) = store.configure_run_continuation(&run_id, true, false, None, None) {
            registration.fail_preparation(e.to_string());
            return Ok(ToolResult::error(format!(
                "Failed to configure run continuation: {e}"
            )));
        }
        if let Err(e) = store.transition_run(&run_id, super::types::TaskRunStatus::Running) {
            registration.fail_preparation(e.to_string());
            return Ok(ToolResult::error(format!(
                "Failed to transition run to Running: {e}"
            )));
        }
        let trace_sink = if priority == "foreground" {
            Some(crate::chat_driver::subagent_trace_sink_for(&res.sink))
        } else {
            None
        };
        let payload_run_id = run_id.clone();
        let payload_store = store.clone();
        let result_waiter = registration.start(move |receipt_owner| {
            crate::run_driver::drive_run_async(crate::run_driver::RunPayload {
                run_id: payload_run_id,
                pool,
                store: payload_store,
                cancel: run_cancel,
                memory_generation,
                trace_sink,
                prompt: run_prompt,
                plan_policy,
                human_loop_provider: res.human_loop_provider.clone(),
                workspace_io: res
                    .workspace_io_receipt
                    .as_ref()
                    .map(crate::state::ScopedWorkspaceIoReceipt::invocation),
                receipt_owner,
            })
        });

        if priority == "foreground" {
            // Block the turn: drive_run_async streams subagent events to the chat
            // sink (via trace_sink), returns the terminal RunOutcome so the
            // agent can use the result in-turn (Claude Code Task style).
            match result_waiter.await {
                Ok(Ok(outcome)) => {
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
                Ok(Err(error)) => Ok(ToolResult::error(format!("Run failed: {error}"))),
                Err(error) => Ok(ToolResult::error(format!(
                    "Run driver settlement was lost: {error}"
                ))),
            }
        } else {
            // Background: the canonical TaskRuntime owner retains the driver;
            // this surface drops only its result waiter.
            drop(result_waiter);
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
        _ctx: &'a echo_agent::tools::ToolContext,
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
        let lookup_run_id = run_id.clone();
        match super::executor::TaskRuntimeOperation::new(store)
            .run_store("check TaskRun status tool", move |store| {
                store.get_run(&lookup_run_id)
            })
            .await
        {
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
        _ctx: &'a echo_agent::tools::ToolContext,
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
        let cancel_run_id = run_id.clone();
        match super::executor::TaskRuntimeOperation::new(store)
            .run_store("cancel TaskRun tool", move |store| {
                store.request_cancel(&cancel_run_id)
            })
            .await
        {
            Ok(cancelled) => Ok(ToolResult::success(
                serde_json::json!({"run_id": run_id, "cancelled": cancelled}).to_string(),
            )),
            Err(error) => Ok(ToolResult::error(format!(
                "Failed to cancel run {run_id}: {error}"
            ))),
        }
    }
}
