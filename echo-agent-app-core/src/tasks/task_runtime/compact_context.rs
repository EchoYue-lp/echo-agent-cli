//! Compression-safe TaskRuntime context capsules.
//!
//! EKO's task runtime state is an application-layer concern: run status, plan
//! tasks, subagent summaries, and GUI trace projections do not belong in the
//! generic `echo-agent` framework. When the main-agent context is prepared or
//! compacted mid-task, however, the LLM still needs a concise recovery view of
//! the active run. This module derives that view from the file-backed
//! `TaskRuntimeStore`; the framework projection envelope protects it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use echo_agent::agent::ReactAgent;
use echo_agent::compression::{ContextProjection, PreModelContextProjector, ProjectionContext};
use echo_agent::error::Result as AgentResult;
use echo_agent::llm::types::Message;
use futures::future::BoxFuture;

use super::store::TaskRuntimeStore;
use super::types::{PlanTask, TaskExecutionSummary, TodoItem, TodoStatus};

/// Stable application marker identifying the run-level recovery projection.
pub const RUNTIME_RECOVERY_MARKER: &str = "[eko_runtime_recovery_capsule]";

/// Marker emitted by the EKO Subagent prompt compiler for planned invocations.
/// Protecting it keeps the per-task brief alive if a Subagent compact runs
/// before the Subagent finishes.
pub const TASK_CONTEXT_MARKER: &str = "[task_context]";

const MAX_GOAL_CHARS: usize = 420;
const MAX_TASK_TITLE_CHARS: usize = 96;
const MAX_TASK_DESC_CHARS: usize = 220;
const MAX_SUMMARY_CHARS: usize = 260;
const MAX_ITEMS_PER_FIELD: usize = 3;

/// Projects the authoritative file-backed TaskRuntime state at every model boundary.
pub struct TaskRuntimeContextProjector {
    registry: Arc<TaskRuntimeProjectionRegistry>,
}

impl TaskRuntimeContextProjector {
    pub fn new(registry: Arc<TaskRuntimeProjectionRegistry>) -> Self {
        Self { registry }
    }
}

struct ProjectionRegistration {
    id: uuid::Uuid,
    store: Arc<TaskRuntimeStore>,
}

/// Process-stable registry bridging application run ownership across framework spawns.
pub struct TaskRuntimeProjectionRegistry {
    registrations: RwLock<HashMap<String, ProjectionRegistration>>,
}

impl TaskRuntimeProjectionRegistry {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(
        self: &Arc<Self>,
        run_id: impl Into<String>,
        store: Arc<TaskRuntimeStore>,
    ) -> TaskRuntimeProjectionRegistration {
        let run_id = run_id.into();
        let id = uuid::Uuid::new_v4();
        self.registrations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(run_id.clone(), ProjectionRegistration { id, store });
        TaskRuntimeProjectionRegistration {
            registry: Arc::clone(self),
            run_id,
            id,
        }
    }

    fn store(&self, run_id: &str) -> Option<Arc<TaskRuntimeStore>> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(run_id)
            .map(|registration| Arc::clone(&registration.store))
    }

    pub fn contains(&self, run_id: &str) -> bool {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(run_id)
    }
}

impl Default for TaskRuntimeProjectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TaskRuntimeProjectionRegistration {
    registry: Arc<TaskRuntimeProjectionRegistry>,
    run_id: String,
    id: uuid::Uuid,
}

impl Drop for TaskRuntimeProjectionRegistration {
    fn drop(&mut self) {
        let mut registrations = self
            .registry
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let owns_registration = registrations
            .get(&self.run_id)
            .is_some_and(|registration| registration.id == self.id);
        if owns_registration {
            registrations.remove(&self.run_id);
        }
    }
}

static TASK_RUNTIME_PROJECTION_REGISTRY: LazyLock<Arc<TaskRuntimeProjectionRegistry>> =
    LazyLock::new(|| Arc::new(TaskRuntimeProjectionRegistry::new()));

pub fn task_runtime_projection_registry() -> Arc<TaskRuntimeProjectionRegistry> {
    Arc::clone(&TASK_RUNTIME_PROJECTION_REGISTRY)
}

impl PreModelContextProjector for TaskRuntimeContextProjector {
    fn project<'a>(
        &'a self,
        context: &'a ProjectionContext,
    ) -> BoxFuture<'a, AgentResult<Vec<ContextProjection>>> {
        Box::pin(async move {
            let derived_run_id = context
                .turn_id
                .as_deref()
                .map(super::task_tools::formal_run_id_for_turn);
            let run_id = context.run_id.as_deref().or(derived_run_id.as_deref());
            let store = run_id.and_then(|run_id| self.registry.store(run_id));
            Ok(vec![ContextProjection {
                marker: RUNTIME_RECOVERY_MARKER.to_string(),
                message: run_id
                    .zip(store.as_deref())
                    .and_then(|(run_id, store)| build_runtime_recovery_capsule(store, run_id))
                    .map(Message::user),
            }])
        })
    }
}

/// Protect the dynamic task brief for the current Subagent invocation.
pub async fn install_task_context_protection(agent: &ReactAgent) {
    let mut ctx = agent.context().lock().await;
    ctx.add_replaceable_protected_marker(TASK_CONTEXT_MARKER.to_string());
}

/// Derive a compact, compression-safe view of the active runtime state.
pub fn build_runtime_recovery_capsule(store: &TaskRuntimeStore, run_id: &str) -> Option<String> {
    let run = match store.get_run(run_id) {
        Ok(Some(run)) => run,
        Ok(None) | Err(_) => return None,
    };
    let plan = store.get_plan(run_id).unwrap_or_default();
    let todos = store.list_todos(run_id).unwrap_or_default();

    let has_plan_tasks = plan
        .as_ref()
        .map(|plan| !plan.tasks.is_empty())
        .unwrap_or(false);
    let has_runtime_todos = todos.iter().any(|todo| !todo.task_id.trim().is_empty());
    if !has_plan_tasks && !has_runtime_todos {
        return None;
    }

    let mut out = String::new();
    out.push_str(RUNTIME_RECOVERY_MARKER);
    out.push('\n');
    out.push_str(
        "Purpose: recover active TaskRuntime state after context compression. \
         Continue unfinished work from this capsule; do not treat it as a new user request. \
         The file-backed TaskRuntimeStore remains authoritative.\n",
    );
    out.push_str(&format!(
        "Run: id={}, status={}, route={}, domain={}, goal={}\n",
        run.run_id,
        run.status.as_str(),
        run.route,
        run.domain_profile.as_str(),
        truncate_chars(&run.goal, MAX_GOAL_CHARS),
    ));

    if let Some(plan) = &plan {
        out.push_str(&format!(
            "Plan: id={}, execution_mode={}, tasks={}\n",
            plan.plan_id,
            plan.execution_mode.as_str(),
            plan.tasks.len()
        ));
        push_short_list(&mut out, "Assumptions", &plan.assumptions, 3, 120);
        push_short_list(&mut out, "Risks", &plan.risks, 3, 120);
    }

    push_task_group(
        &mut out,
        "Running tasks",
        &todos,
        plan.as_ref().map(|p| p.tasks.as_slice()).unwrap_or(&[]),
        store,
        run_id,
        &[TodoStatus::Running],
        4,
    );
    push_task_group(
        &mut out,
        "Blocked/failed tasks",
        &todos,
        plan.as_ref().map(|p| p.tasks.as_slice()).unwrap_or(&[]),
        store,
        run_id,
        &[
            TodoStatus::Blocked,
            TodoStatus::Failed,
            TodoStatus::Cancelled,
            TodoStatus::TimedOut,
        ],
        4,
    );
    push_task_group(
        &mut out,
        "Pending next tasks",
        &todos,
        plan.as_ref().map(|p| p.tasks.as_slice()).unwrap_or(&[]),
        store,
        run_id,
        &[TodoStatus::Pending],
        6,
    );
    push_task_group(
        &mut out,
        "Recently completed tasks",
        &todos,
        plan.as_ref().map(|p| p.tasks.as_slice()).unwrap_or(&[]),
        store,
        run_id,
        &[TodoStatus::Completed],
        5,
    );

    Some(out)
}

#[allow(clippy::too_many_arguments)] // Compact rendering keeps the shared truncation/state inputs explicit.
fn push_task_group(
    out: &mut String,
    label: &str,
    todos: &[TodoItem],
    tasks: &[PlanTask],
    store: &TaskRuntimeStore,
    run_id: &str,
    statuses: &[TodoStatus],
    limit: usize,
) {
    let mut selected: Vec<&TodoItem> = todos
        .iter()
        .filter(|todo| statuses.contains(&todo.status))
        .collect();
    selected.sort_by_key(|todo| task_sort_key(tasks, &todo.task_id));

    if selected.is_empty() {
        return;
    }

    out.push_str(label);
    out.push_str(":\n");
    for todo in selected.into_iter().take(limit) {
        let task = tasks.iter().find(|task| task.id == todo.task_id);
        let agent = todo.owner_agent.as_deref().unwrap_or_else(|| {
            task.map(|task| task.agent_role.as_str())
                .unwrap_or("unknown")
        });
        let title = task
            .map(|task| task.title.as_str())
            .unwrap_or(todo.title.as_str());
        out.push_str(&format!(
            "- [{}] {} ({}, agent={})",
            todo.status.as_str(),
            truncate_chars(title, MAX_TASK_TITLE_CHARS),
            todo.task_id,
            agent,
        ));
        if let Some(task) = task {
            out.push_str(&format!(
                "; kind={}; deps={}",
                task.kind.as_str(),
                if task.depends_on.is_empty() {
                    "none".to_string()
                } else {
                    task.depends_on.join(",")
                }
            ));
            if !task.description.trim().is_empty() {
                out.push_str(&format!(
                    "; brief={}",
                    truncate_chars(&task.description, MAX_TASK_DESC_CHARS)
                ));
            }
        }
        if let Ok(Some(summary)) = store.get_summary(run_id, &todo.task_id) {
            let compact = format_summary(&summary);
            if !compact.is_empty() {
                out.push_str(&format!("; summary={compact}"));
            }
        } else if let Some(summary) = &todo.summary {
            out.push_str(&format!(
                "; summary={}",
                truncate_chars(summary, MAX_SUMMARY_CHARS)
            ));
        }
        out.push('\n');
    }
}

fn task_sort_key(tasks: &[PlanTask], task_id: &str) -> i64 {
    tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.sort_order)
        .unwrap_or(i64::MAX)
}

fn format_summary(summary: &TaskExecutionSummary) -> String {
    let mut parts = Vec::new();
    if !summary.result.summary.trim().is_empty() {
        push_summary_field(
            &mut parts,
            "done",
            std::slice::from_ref(&summary.result.summary),
        );
    }
    push_summary_field(&mut parts, "changed", &summary.result.touched_files.written);
    push_summary_field(&mut parts, "decisions", &summary.decisions);
    push_summary_field(&mut parts, "remaining", &summary.result.remaining_work);
    push_summary_field(&mut parts, "next", &summary.next_implications);
    truncate_chars(&parts.join(" | "), MAX_SUMMARY_CHARS)
}

fn push_summary_field(parts: &mut Vec<String>, label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let joined = items
        .iter()
        .take(MAX_ITEMS_PER_FIELD)
        .map(|item| truncate_chars(item, 80))
        .collect::<Vec<_>>()
        .join("; ");
    parts.push(format!("{label}: {joined}"));
}

fn push_short_list(
    out: &mut String,
    label: &str,
    items: &[String],
    limit: usize,
    max_chars: usize,
) {
    if items.is_empty() {
        return;
    }
    out.push_str(label);
    out.push_str(": ");
    out.push_str(
        &items
            .iter()
            .take(limit)
            .map(|item| truncate_chars(item, max_chars))
            .collect::<Vec<_>>()
            .join("; "),
    );
    out.push('\n');
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
    }
    if truncated {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTaskKind, SubagentRunStatus,
        SubagentTaskResult, SubagentTouchedFiles, TaskPlan, TaskRunStatus,
    };
    use chrono::Utc;
    use echo_agent::compression::{ContextManager, PreModelContextProjector, ProjectionContext};
    use std::sync::Arc;

    #[tokio::test]
    async fn subagent_task_context_protection_replaces_previous_brief() -> Result<(), String> {
        let agent = ReactAgent::new(echo_agent::agent::AgentConfig::minimal(
            "test-model",
            "subagent",
        ));
        install_task_context_protection(&agent).await;
        let context_handle = agent.context();
        let mut context = context_handle.lock().await;
        context.push(Message::user(format!(
            "{TASK_CONTEXT_MARKER} previous assignment"
        )));
        context.push(Message::user(format!(
            "{TASK_CONTEXT_MARKER} current assignment"
        )));

        let briefs: Vec<_> = context
            .messages()
            .iter()
            .filter(|message| {
                message
                    .content
                    .as_text_ref()
                    .is_some_and(|text| text.contains(TASK_CONTEXT_MARKER))
            })
            .collect();
        if briefs.len() != 1 {
            return Err(format!(
                "expected one current task brief, got {}",
                briefs.len()
            ));
        }
        if !briefs.first().is_some_and(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|text| text.ends_with("current assignment"))
        }) {
            return Err("latest task brief was not retained".to_string());
        }
        Ok(())
    }

    fn seed_store() -> Result<TaskRuntimeStore, String> {
        let store =
            TaskRuntimeStore::new_in_memory().map_err(|err| format!("seed store failed: {err}"))?;
        let _run = store
            .create_run(
                "r1",
                "default",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "修复一个很长的中文任务，要求压缩之后不能丢失未完成计划",
                "complex_runtime",
                AttendedMode::Attended,
            )
            .map_err(|err| format!("seed create_run failed: {err}"))?;
        let _run = store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|err| format!("seed transition failed: {err}"))?;
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal: "修复上下文压缩继承".to_string(),
            assumptions: vec!["运行态来自文件 store".to_string()],
            risks: vec!["自动压缩发生在 task_execute 之前".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                PlanTask {
                    id: "t1".to_string(),
                    title: "调查压缩路径".to_string(),
                    description: "读取 ContextManager 和 TaskRuntimeStore".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "explorer".to_string(),
                    status: TodoStatus::Pending,
                    sort_order: 0,
                    ..PlanTask::default()
                },
                PlanTask {
                    id: "t2".to_string(),
                    title: "实现恢复胶囊".to_string(),
                    description: "把未完成任务写入受保护 runtime context".to_string(),
                    kind: PlanTaskKind::Implementation,
                    agent_role: "implementer".to_string(),
                    depends_on: vec!["t1".to_string()],
                    status: TodoStatus::Pending,
                    sort_order: 1,
                    ..PlanTask::default()
                },
            ],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|err| format!("seed plan commit failed: {err}"))?;
        store
            .set_task_status("r1", "t1", TodoStatus::Completed, Some("explorer"), None)
            .map_err(|err| format!("seed t1 status failed: {err}"))?;
        store
            .set_task_status("r1", "t2", TodoStatus::Running, Some("implementer"), None)
            .map_err(|err| format!("seed t2 status failed: {err}"))?;
        store
            .put_summary(&TaskExecutionSummary {
                run_id: "r1".to_string(),
                task_id: "t1".to_string(),
                subagent_name: "explorer".to_string(),
                result: SubagentTaskResult {
                    contract_version: 1,
                    status: SubagentRunStatus::Completed,
                    summary: "确认 force_compress 不携带 TaskRuntime 状态".to_string(),
                    artifacts: Vec::new(),
                    verification: Vec::new(),
                    remaining_work: Vec::new(),
                    touched_files: SubagentTouchedFiles {
                        read: vec!["echo-state/src/compression/mod.rs".to_string()],
                        written: Vec::new(),
                    },
                },
                decisions: vec!["恢复信息放应用层".to_string()],
                next_implications: vec!["t2 需要保护 runtime capsule".to_string()],
                suggested_tasks: Vec::new(),
                created_at: Utc::now(),
            })
            .map_err(|err| format!("seed summary failed: {err}"))?;
        Ok(store)
    }

    #[test]
    fn capsule_includes_unfinished_tasks_and_completed_summary() -> Result<(), String> {
        let store = seed_store()?;
        let capsule = build_runtime_recovery_capsule(&store, "r1")
            .ok_or_else(|| "capsule should be built".to_string())?;
        for expected in [
            RUNTIME_RECOVERY_MARKER,
            "Running tasks",
            "实现恢复胶囊",
            "Recently completed tasks",
            "恢复信息放应用层",
        ] {
            if !capsule.contains(expected) {
                return Err(format!("capsule missing expected text: {expected}"));
            }
        }
        Ok(())
    }

    #[test]
    fn ordinary_chat_run_without_plan_gets_no_capsule() -> Result<(), String> {
        let store =
            TaskRuntimeStore::new_in_memory().map_err(|err| format!("seed store failed: {err}"))?;
        let _run = store
            .create_run(
                "r2",
                "default",
                "c1",
                "m2",
                DomainProfile::General,
                "普通聊天",
                "chat_turn",
                AttendedMode::Attended,
            )
            .map_err(|err| format!("seed create_run failed: {err}"))?;
        if build_runtime_recovery_capsule(&store, "r2").is_some() {
            return Err("ordinary chat run should not get a capsule".to_string());
        }
        Ok(())
    }

    fn projection_context(run_id: Option<&str>) -> ProjectionContext {
        ProjectionContext {
            iteration: 0,
            agent_name: "test-agent".to_string(),
            session_id: None,
            conversation_id: Some("c1".to_string()),
            run_id: run_id.map(str::to_string),
            turn_id: None,
        }
    }

    fn seed_projection_store(
        run_id: &str,
        task_title: &str,
    ) -> Result<Arc<TaskRuntimeStore>, String> {
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|err| format!("seed projection store failed: {err}"))?,
        );
        store
            .create_run(
                run_id,
                "default",
                "c1",
                run_id,
                DomainProfile::General,
                task_title,
                "complex_runtime",
                AttendedMode::Attended,
            )
            .map_err(|err| format!("seed projection run failed: {err}"))?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal: task_title.to_string(),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: format!("task-{run_id}"),
                    title: task_title.to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "explorer".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|err| format!("seed projection plan failed: {err}"))?;
        Ok(store)
    }

    async fn project_after_barrier(
        projector: Arc<TaskRuntimeContextProjector>,
        run_id: &str,
        barrier: Arc<tokio::sync::Barrier>,
    ) -> Result<Vec<ContextProjection>, String> {
        barrier.wait().await;
        projector
            .project(&projection_context(Some(run_id)))
            .await
            .map_err(|err| err.to_string())
    }

    #[tokio::test]
    async fn overlapping_projector_calls_read_their_registered_run_and_store() -> Result<(), String>
    {
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let projector = Arc::new(TaskRuntimeContextProjector::new(Arc::clone(&registry)));
        let first_store = seed_projection_store("run-a", "alpha task")?;
        let second_store = seed_projection_store("run-b", "beta task")?;
        let _first_registration = registry.register("run-a", first_store);
        let _second_registration = registry.register("run-b", second_store);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let first = tokio::spawn(project_after_barrier(
            Arc::clone(&projector),
            "run-a",
            Arc::clone(&barrier),
        ));
        let second = tokio::spawn(project_after_barrier(
            Arc::clone(&projector),
            "run-b",
            Arc::clone(&barrier),
        ));
        let (first_result, second_result) = tokio::join!(first, second);
        let first_projection =
            first_result.map_err(|err| format!("first projection task failed: {err}"))??;
        let second_projection =
            second_result.map_err(|err| format!("second projection task failed: {err}"))??;

        let first_text = first_projection
            .first()
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "first scoped projection missing".to_string())?;
        let second_text = second_projection
            .first()
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "second scoped projection missing".to_string())?;
        if !first_text.contains("run-a") || !first_text.contains("alpha task") {
            return Err(format!(
                "first projection leaked another scope: {first_text}"
            ));
        }
        if !second_text.contains("run-b") || !second_text.contains("beta task") {
            return Err(format!(
                "second projection leaked another scope: {second_text}"
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn projector_after_registration_drop_removes_existing_capsule() -> Result<(), String> {
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let projector = Arc::new(TaskRuntimeContextProjector::new(Arc::clone(&registry)));
        let store = seed_projection_store("run-a", "alpha task")?;
        let mut context = ContextManager::builder(4096).build();
        let registration = registry.register("run-a", store);
        let scoped = projector
            .project(&projection_context(Some("run-a")))
            .await
            .map_err(|err| err.to_string())?;
        context.apply_projections(&scoped);
        if !context.has_projection(RUNTIME_RECOVERY_MARKER) {
            return Err("scoped call should install the runtime capsule".to_string());
        }
        drop(registration);

        let outside = projector
            .project(&projection_context(Some("run-a")))
            .await
            .map_err(|err| err.to_string())?;
        context.apply_projections(&outside);
        if context.has_projection(RUNTIME_RECOVERY_MARKER) {
            return Err("outside scope must remove the runtime capsule".to_string());
        }
        Ok(())
    }
}
