//! Compression-safe TaskRuntime context capsules.
//!
//! EKO's task runtime state is an application-layer concern: run status, plan
//! tasks, subagent summaries, and GUI trace projections do not belong in the
//! generic `echo-agent` framework. When the main-agent context is prepared or
//! compacted mid-task, however, the LLM still needs a concise recovery view of
//! the active run. This module derives that view from the file-backed
//! `TaskRuntimeStore`; the framework projection envelope protects it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock, Weak};

use echo_agent::agent::ReactAgent;
use echo_agent::compression::{ContextProjection, PreModelContextProjector, ProjectionContext};
use echo_agent::error::Result as AgentResult;
use echo_agent::llm::types::Message;
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};

use super::store::TaskRuntimeStore;
use super::types::{BackgroundCellState, PlanTask, TaskExecutionSummary, TodoItem, TodoStatus};

/// Stable application marker identifying the run-level recovery projection.
pub const RUNTIME_RECOVERY_MARKER: &str = "[eko_runtime_recovery_capsule]";
/// Stable objective contract projected separately from volatile recovery state.
pub const RUNTIME_GOAL_MARKER: &str = "[eko_run_goal_contract]";
pub(crate) const DOMAIN_METHOD_MARKER: &str = "[eko_bound_domain_method]";

/// 共用原 typed failure 的 machine code 分类，不能解析展示文字或改自动重试语义。
pub(crate) fn is_required_context_unavailable(failure: &echo_agent::error::AgentFailure) -> bool {
    failure.category == echo_agent::error::AgentFailureCategory::Other
        && failure.code == "required_context_unavailable"
}

/// Marker emitted by the EKO Subagent prompt compiler for planned invocations.
/// Protecting it keeps the per-task brief alive if a Subagent compact runs
/// before the Subagent finishes.
pub const TASK_CONTEXT_MARKER: &str = "[task_context]";

const MAX_GOAL_CHARS: usize = 420;
const MAX_GOAL_CONTRACT_CHARS: usize = 8_000;
const MAX_TASK_TITLE_CHARS: usize = 96;
const MAX_TASK_DESC_CHARS: usize = 220;
const MAX_SUMMARY_CHARS: usize = 260;
const MAX_ITEMS_PER_FIELD: usize = 3;

/// Projects the authoritative file-backed TaskRuntime state at every model boundary.
pub struct TaskRuntimeContextProjector {
    registry: Arc<TaskRuntimeProjectionRegistry>,
    domain_only: bool,
}

impl TaskRuntimeContextProjector {
    pub fn new(registry: Arc<TaskRuntimeProjectionRegistry>) -> Self {
        Self {
            registry,
            domain_only: false,
        }
    }

    pub(crate) fn domain_only(registry: Arc<TaskRuntimeProjectionRegistry>) -> Self {
        Self {
            registry,
            domain_only: true,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DomainMethodProjection {
    binding: crate::domain_plugin::DomainBindingRef,
    instructions: String,
}

struct ProjectionRegistration {
    id: uuid::Uuid,
    store: Arc<TaskRuntimeStore>,
    retention: Weak<TaskRuntimeProjectionRegistrationInner>,
    // A projection of already admitted immutable RunCreated data. This dies
    // with the original registration token; it is not another mutable store.
    domain_method: Option<DomainMethodProjection>,
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
        let mut registrations = self
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.register_locked(run_id.into(), store, &mut registrations)
    }

    fn register_locked(
        self: &Arc<Self>,
        run_id: String,
        store: Arc<TaskRuntimeStore>,
        registrations: &mut HashMap<String, ProjectionRegistration>,
    ) -> TaskRuntimeProjectionRegistration {
        let id = uuid::Uuid::new_v4();
        let inner = Arc::new(TaskRuntimeProjectionRegistrationInner {
            registry: self.clone(),
            run_id: run_id.clone(),
            id,
        });
        registrations.insert(
            run_id,
            ProjectionRegistration {
                id,
                store,
                retention: Arc::downgrade(&inner),
                domain_method: None,
            },
        );
        TaskRuntimeProjectionRegistration { _inner: inner }
    }

    /// Reuse the actual run owner's registration rather than overwriting its
    /// token when a task tool or cross-workspace physical invocation retains it.
    pub(crate) fn retain_for_invocation(
        self: &Arc<Self>,
        run_id: &str,
        store: Arc<TaskRuntimeStore>,
    ) -> Result<TaskRuntimeProjectionRegistration, String> {
        // Capture before publishing an invocation guard. The framework's
        // pre-model projector is observational (errors do not stop execution),
        // so a required method cannot depend on another fallible disk/operation
        // read after this original, fallible application admission boundary.
        let domain_method = store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .and_then(|snapshot| {
                snapshot
                    .domain_binding
                    .map(|binding| (snapshot.run.domain_binding, binding))
            })
            .map(|(reference, binding)| {
                binding.validate()?;
                if reference.as_ref() != Some(&binding.reference()) {
                    return Err(
                        "TaskRun method projection does not match its original binding".to_string(),
                    );
                }
                Ok(DomainMethodProjection {
                    binding: binding.reference(),
                    instructions: binding.prompt_instructions(),
                })
            })
            .transpose()?;
        let mut registrations = self
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = registrations.get_mut(run_id) {
            if !Arc::ptr_eq(&existing.store, &store) {
                return Err("TaskRun projection is owned by another original store".to_string());
            }
            if existing.domain_method.is_some() && existing.domain_method != domain_method {
                return Err(
                    "TaskRun method projection cannot change its original binding".to_string(),
                );
            }
            if let Some(inner) = existing.retention.upgrade() {
                existing.domain_method = domain_method;
                return Ok(TaskRuntimeProjectionRegistration { _inner: inner });
            }
        }
        let token = self.register_locked(run_id.to_string(), store, &mut registrations);
        if let Some(registration) = registrations.get_mut(run_id) {
            registration.domain_method = domain_method;
        }
        Ok(token)
    }

    pub(crate) fn store(&self, run_id: &str) -> Option<Arc<TaskRuntimeStore>> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(run_id)
            .map(|registration| Arc::clone(&registration.store))
    }

    fn domain_method(&self, run_id: &str) -> Option<String> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(run_id)
            .and_then(|registration| registration.domain_method.as_ref())
            .map(|projection| format!("{DOMAIN_METHOD_MARKER}\n{}", projection.instructions))
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

#[derive(Clone)]
pub struct TaskRuntimeProjectionRegistration {
    _inner: Arc<TaskRuntimeProjectionRegistrationInner>,
}

struct TaskRuntimeProjectionRegistrationInner {
    registry: Arc<TaskRuntimeProjectionRegistry>,
    run_id: String,
    id: uuid::Uuid,
}

impl Drop for TaskRuntimeProjectionRegistrationInner {
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
    fn validate_model_request<'a>(
        &'a self,
        context: &'a ProjectionContext,
        request: &'a echo_agent::llm::ChatRequest,
        artifacts: Option<&'a echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    ) -> BoxFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            if self.domain_only {
                return Ok(());
            }
            let derived = context
                .turn_id
                .as_deref()
                .map(super::task_tools::formal_run_id_for_turn);
            let Some(run_id) = context.run_id.as_deref().or(derived.as_deref()) else {
                return Ok(());
            };
            let Some(store) = self.registry.store(run_id) else {
                return Ok(());
            };
            let run_id = run_id.to_owned();
            let model_tools = request
                .tools
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.function.name.clone())
                .collect::<std::collections::HashSet<_>>();
            let artifacts = artifacts.cloned();
            super::executor::TaskRuntimeOperation::new(store)
                .run_store("qualify TaskRuntime source references against final model request", move |store| {
                    let state = store.get_run_state(&run_id)?
                        .ok_or_else(|| super::store::StoreError::RunNotFound(run_id.clone()))?;
                    let required = state.recent_constraints.iter().rev().take(4)
                        .filter(|steer| steer.text.chars().count() > crate::prepared_turn::PREVIEW_CHARS)
                        .collect::<Vec<_>>();
                    if required.is_empty() { return Ok(()); }
                    // 本引用合同使用原校验分页工具，不能因注册存在或 SSH read_file 可用就认定可达。
                    if !model_tools.contains("read_artifact") {
                        return Err(super::store::StoreError::InvalidPlan(
                            "本次最终模型工具范围无法读取完整用户约束；保持原工具上界并等待恢复".to_owned(),
                        ));
                    }
                    let config = artifacts.ok_or_else(|| super::store::StoreError::InvalidPlan(
                        "本次执行缺少原 host artifact 配置，不能读取完整用户约束".to_owned(),
                    ))?;
                    for steer in required {
                        let artifact = store.recorded_user_steer_artifact(&run_id, steer)?;
                        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
                            path: artifact.path, artifact_bytes: artifact.bytes, payload_bytes: artifact.bytes,
                            sha256: artifact.sha256.ok_or_else(|| super::store::StoreError::InvalidPlan("原文 hash 缺失".to_owned()))?,
                            retention: config.retention.clone(),
                        };
                        // 使用真实 snapshot 配置再核一次可达性；不手造一个宽松根冒 invocation 可读。
                        echo_agent::tools::files::artifact::read_artifact_page(
                            &config, &reference, None,
                            echo_agent::tools::files::artifact::ArtifactPageLimit::Bytes(1),
                        ).map_err(|error| super::store::StoreError::InvalidPlan(format!("本次调用无法读取完整用户约束：{error}")))?;
                    }
                    Ok(())
                }).await.map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
        })
    }

    fn is_required(&self) -> bool {
        !self.domain_only
    }

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
            let domain = run_id.and_then(|run_id| self.registry.domain_method(run_id));
            if self.domain_only {
                // Actual invocation resources retain this same registration.
                // General None still clears an earlier domain marker.
                return Ok(vec![ContextProjection {
                    marker: DOMAIN_METHOD_MARKER.to_string(),
                    message: domain.map(Message::user),
                }]);
            }
            let store = run_id.and_then(|run_id| self.registry.store(run_id));
            let (goal, recovery) = match (run_id, store) {
                (Some(run_id), Some(store)) => {
                    let run_id = run_id.to_string();
                    super::executor::TaskRuntimeOperation::new(store)
                        .run_store("project TaskRuntime model context", move |store| {
                            Ok((
                                build_runtime_goal_contract(&store, &run_id),
                                build_runtime_recovery_capsule_checked(&store, &run_id)?,
                            ))
                        })
                        .await
                        .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                }
                _ => (None, None),
            };
            Ok(vec![
                ContextProjection {
                    marker: RUNTIME_GOAL_MARKER.to_string(),
                    message: goal.map(Message::user),
                },
                ContextProjection {
                    marker: RUNTIME_RECOVERY_MARKER.to_string(),
                    message: recovery.map(Message::user),
                },
                ContextProjection {
                    marker: DOMAIN_METHOD_MARKER.to_string(),
                    message: domain.map(Message::user),
                },
            ])
        })
    }
}

/// Protect the dynamic task brief for the current Subagent invocation.
pub async fn install_task_context_protection(agent: &ReactAgent) {
    let mut ctx = agent.context().lock().await;
    ctx.add_replaceable_protected_marker(TASK_CONTEXT_MARKER.to_string());
    ctx.add_replaceable_protected_marker(DOMAIN_METHOD_MARKER.to_string());
}

/// Adhoc factory Agents consume the same persisted rules without inheriting
/// another turn's transient selection or adding General planning instructions.
pub(crate) fn install_domain_context_projection(agent: &mut ReactAgent) {
    agent.set_pre_model_context_projector(Some(Arc::new(
        TaskRuntimeContextProjector::domain_only(task_runtime_projection_registry()),
    )));
}

/// Derive a compact, compression-safe view of the active runtime state.
pub fn build_runtime_goal_contract(store: &TaskRuntimeStore, run_id: &str) -> Option<String> {
    let state = store.get_run_state(run_id).ok().flatten()?;
    let continuation = state.continuation.as_ref()?;
    if !continuation.enabled {
        return None;
    }
    let run = state.run;
    let mut hasher = Sha256::new();
    hasher.update(run.goal.as_bytes());
    let goal_hash = format!("{:x}", hasher.finalize());
    let objective = run
        .goal
        .chars()
        .take(MAX_GOAL_CONTRACT_CHARS)
        .collect::<String>();
    let objective_truncated = run.goal.chars().count() > MAX_GOAL_CONTRACT_CHARS;
    let plan_revision = store
        .get_plan(run_id)
        .ok()
        .flatten()
        .map(|plan| plan.revision);
    let remaining_tokens = continuation
        .token_budget
        .map(|budget| budget.saturating_sub(continuation.tokens_used));
    let remaining_time_seconds = continuation
        .time_budget_seconds
        .map(|budget| budget.saturating_sub(continuation.time_used_seconds));

    Some(format!(
        "{RUNTIME_GOAL_MARKER}\n\
         Run: id={}, status={}, objective_sha256={}, plan_revision={}\n\
         Objective{}:\n{}\n\
         Continuation contract:\n\
         - This finite Turn ending is not evidence that the TaskRun Goal is complete.\n\
         - Do not narrow or replace the objective with what fits in one Turn.\n\
         - Before completion, audit the original objective, current plan, artifacts, and verification evidence.\n\
         - TaskRuntime completion blockers are authoritative; final wording never overrides them.\n\
         - On a recoverable blocker, record the concrete blocker and preserve resumable state.\n\
         Budget: tokens_used={}, token_budget={}, tokens_remaining={}; time_used_seconds={}, time_budget_seconds={}, time_remaining_seconds={}\n",
        run.run_id,
        run.status.as_str(),
        goal_hash,
        plan_revision
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "none".to_string()),
        if objective_truncated {
            let artifact = store
                .objective_artifact_path(run_id)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unavailable".to_string());
            format!(
                " (bounded here; recover the full objective artifact before acting: {artifact})"
            )
        } else {
            String::new()
        },
        objective,
        continuation.tokens_used,
        continuation
            .token_budget
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "none".to_string()),
        remaining_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        continuation.time_used_seconds,
        continuation
            .time_budget_seconds
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "none".to_string()),
        remaining_time_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
    ))
}

/// Derive a compact, compression-safe view of the active runtime state.
pub fn build_runtime_recovery_capsule(store: &TaskRuntimeStore, run_id: &str) -> Option<String> {
    build_runtime_recovery_capsule_checked(store, run_id)
        .ok()
        .flatten()
}

/// 执行投影必须观察真实原文/物化错误，不能降为空投影后继续模型调用。
fn build_runtime_recovery_capsule_checked(
    store: &TaskRuntimeStore,
    run_id: &str,
) -> Result<Option<String>, super::store::StoreError> {
    let state = store
        .get_run_state(run_id)?
        .ok_or_else(|| super::store::StoreError::RunNotFound(run_id.to_string()))?;
    let run = state.run;
    let continuation = state.continuation;
    let recent_constraints = state.recent_constraints;
    let plan = store.get_plan(run_id).unwrap_or_default();
    let todos = store.list_todos(run_id).unwrap_or_default();
    let cells = store.list_background_cells(run_id).unwrap_or_default();

    let has_plan_tasks = plan
        .as_ref()
        .map(|plan| !plan.tasks.is_empty())
        .unwrap_or(false);
    let has_runtime_todos = todos.iter().any(|todo| !todo.task_id.trim().is_empty());
    if !has_plan_tasks
        && !has_runtime_todos
        && recent_constraints.is_empty()
        && cells.is_empty()
        && continuation.as_ref().is_none_or(|state| !state.enabled)
    {
        return Ok(None);
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
    if run.goal.chars().count() > MAX_GOAL_CHARS
        && let Some(artifact) = store.objective_artifact_path(run_id)
    {
        out.push_str(&format!(
            "Full objective artifact: {}\n",
            artifact.display()
        ));
    }
    if let Some(continuation) = continuation {
        out.push_str(&format!(
            "Continuation: active_turn={}, last_turn={}, turns_next={}, tokens_used={}, token_budget={}, tokens_remaining={}, time_used_seconds={}, time_budget_seconds={}, time_remaining_seconds={}, compactions={}, deferred={}, pause={}\n",
            continuation
                .active_turn
                .as_ref()
                .map(|turn| format!("{}#{}", turn.turn_id, turn.ordinal))
                .unwrap_or_else(|| "none".to_string()),
            continuation
                .last_turn
                .as_ref()
                .map(|turn| format!("{}:{}", turn.turn_id, turn.status.as_str()))
                .unwrap_or_else(|| "none".to_string()),
            continuation.next_turn_ordinal,
            continuation.tokens_used,
            continuation
                .token_budget
                .map(|budget| budget.to_string())
                .unwrap_or_else(|| "none".to_string()),
            continuation
                .token_budget
                .map(|budget| budget.saturating_sub(continuation.tokens_used).to_string())
                .unwrap_or_else(|| "unbounded".to_string()),
            continuation.time_used_seconds,
            continuation
                .time_budget_seconds
                .map(|budget| budget.to_string())
                .unwrap_or_else(|| "none".to_string()),
            continuation
                .time_budget_seconds
                .map(|budget| {
                    budget
                        .saturating_sub(continuation.time_used_seconds)
                        .to_string()
                })
                .unwrap_or_else(|| "unbounded".to_string()),
            continuation.compaction_count,
            continuation.deferred,
            continuation
                .pause
                .as_ref()
                .map(|pause| pause.reason.as_str())
                .unwrap_or("none"),
        ));
    }

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

    // ADR 0037: incremental user constraints recorded via run steers are as
    // binding as the Goal — surface them in the compression-safe capsule so
    // narrowed context cannot silently drop them.
    if !recent_constraints.is_empty() {
        out.push_str("Recent user constraints:\n");
        // journal 保留全文；受保护 capsule 只放有界预览和可恢复原文引用。
        // 最近四条仍仅是本切片展示，不代表全部有效要求已得到覆盖。
        for steer in recent_constraints
            .iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let turn_preview = truncate_chars(&steer.turn_id, MAX_TASK_TITLE_CHARS);
            if steer.text.chars().count() <= crate::prepared_turn::PREVIEW_CHARS {
                out.push_str(&format!("- (turn {turn_preview}) {}\n", steer.text));
                continue;
            }
            let artifact = store.recorded_user_steer_artifact(run_id, steer)?;
            out.push_str(&format!(
                "- (turn {turn_preview}) {}\n",
                crate::prepared_turn::build_original_message_reference(&steer.text, &artifact),
            ));
        }
        out.push('\n');
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
    push_background_cells(&mut out, &cells);

    Ok(Some(out))
}

fn push_background_cells(out: &mut String, cells: &[BackgroundCellState]) {
    let mut active = cells
        .iter()
        .filter(|cell| cell.is_active())
        .collect::<Vec<_>>();
    active.sort_by_key(|cell| cell.started_at);
    if !active.is_empty() {
        out.push_str("Active background cells:\n");
        for cell in active.into_iter().take(6) {
            out.push_str(&format!(
                "- [{}] {} ({}, {} bytes); use wait(cell_id={}, cursor=0) to observe or stop_cell to cancel\n",
                cell.phase,
                truncate_chars(&cell.name, MAX_TASK_TITLE_CHARS),
                cell.cell_id,
                cell.total_output_bytes,
                cell.cell_id,
            ));
        }
    }

    let mut terminal = cells
        .iter()
        .filter(|cell| !cell.is_active())
        .collect::<Vec<_>>();
    terminal.sort_by_key(|cell| std::cmp::Reverse(cell.finished_at));
    if terminal.is_empty() {
        return;
    }
    out.push_str("Recent background cell results:\n");
    for cell in terminal.into_iter().take(4) {
        out.push_str(&format!(
            "- [{}] {} ({}, exit={}, {} bytes)",
            cell.phase,
            truncate_chars(&cell.name, MAX_TASK_TITLE_CHARS),
            cell.cell_id,
            cell.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string()),
            cell.total_output_bytes,
        ));
        if let Some(excerpt) = &cell.output_excerpt {
            out.push_str(&format!(
                "; output={}",
                truncate_chars(excerpt, MAX_SUMMARY_CHARS)
            ));
        }
        if let Some(path) = &cell.artifact_path {
            out.push_str(&format!("; artifact={}", truncate_chars(path, 220)));
        }
        out.push('\n');
    }
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
    if !summary.outcome.summary.trim().is_empty() {
        push_summary_field(
            &mut parts,
            "done",
            std::slice::from_ref(&summary.outcome.summary),
        );
    }
    push_summary_field(
        &mut parts,
        "changed",
        &summary.outcome.touched_files.written,
    );
    push_summary_field(&mut parts, "decisions", &summary.decisions);
    push_summary_field(&mut parts, "remaining", &summary.outcome.remaining_work);
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
        AttendedMode, DomainProfile, ExecutionMode, PlanTaskKind, SubagentOutcome, SubagentStatus,
        SubagentTouchedFiles, TaskPlan, TaskRunStatus,
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
            domain_binding: None,

            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("修复上下文压缩继承"),
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
                    status: echo_agent::tasks::TaskStatus::Pending,
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
                    status: echo_agent::tasks::TaskStatus::Pending,
                    sort_order: 1,
                    ..PlanTask::default()
                },
            ],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|err| format!("seed plan commit failed: {err}"))?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Completed,
                Some("explorer"),
                None,
            )
            .map_err(|err| format!("seed t1 status failed: {err}"))?;
        store
            .set_task_status(
                "r1",
                "t2",
                echo_agent::tasks::TaskStatus::Running,
                Some("implementer"),
                None,
            )
            .map_err(|err| format!("seed t2 status failed: {err}"))?;
        store
            .put_summary(&TaskExecutionSummary {
                run_id: "r1".to_string(),
                task_id: "t1".to_string(),
                subagent_name: "explorer".to_string(),
                outcome: SubagentOutcome {
                    contract_version: 1,
                    status: SubagentStatus::Completed,
                    summary: "确认 force_compress 不携带 TaskRuntime 状态".to_string(),
                    artifacts: Vec::new(),
                    evidence: Vec::new(),
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

    #[test]
    fn recorded_user_steer_survives_into_recovery_capsule() -> Result<(), String> {
        let store =
            TaskRuntimeStore::new_in_memory().map_err(|err| format!("seed store failed: {err}"))?;
        let _run = store
            .create_run(
                "r3",
                "default",
                "c1",
                "m3",
                DomainProfile::General,
                "升级依赖并修复测试",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|err| format!("seed create_run failed: {err}"))?;
        store
            .configure_run_continuation("r3", true, false, None, None)
            .map_err(|err| format!("configure continuation failed: {err}"))?;
        store
            .record_run_steer("r3", "turn-1", "不要引入新依赖,优先修改现有实现")
            .map_err(|err| format!("record steer failed: {err}"))?;
        let capsule = build_runtime_recovery_capsule(&store, "r3")
            .ok_or_else(|| "capsule should be built after a recorded steer".to_string())?;
        if !capsule.contains("Recent user constraints") || !capsule.contains("不要引入新依赖")
        {
            return Err("capsule missing the recorded user constraint".to_string());
        }
        // The journal fold keeps the constraint across a rebuild.
        let folded = store
            .get_run_state("r3")
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "run state missing".to_string())?;
        assert_eq!(folded.recent_constraints.len(), 1);
        assert_eq!(folded.recent_constraints[0].turn_id, "turn-1");
        Ok(())
    }

    #[test]
    fn oversized_goal_spills_objective_artifact_and_points_at_it() -> Result<(), String> {
        let store =
            TaskRuntimeStore::new_in_memory().map_err(|err| format!("seed store failed: {err}"))?;
        let long_goal: String =
            "跨窗口目标: ".to_string() + &std::iter::repeat_n('目', 9_000).collect::<String>();
        let _run = store
            .create_run(
                "r4",
                "default",
                "c1",
                "m4",
                DomainProfile::General,
                &long_goal,
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|err| format!("seed create_run failed: {err}"))?;
        store
            .configure_run_continuation("r4", true, false, None, None)
            .map_err(|err| format!("configure continuation failed: {err}"))?;
        let artifact = store
            .objective_artifact_path("r4")
            .ok_or("oversized goal should spill an objective artifact")?;
        assert_eq!(
            std::fs::read_to_string(&artifact).map_err(|err| err.to_string())?,
            long_goal,
            "spilled artifact must hold the full objective verbatim"
        );
        let contract = build_runtime_goal_contract(&store, "r4")
            .ok_or_else(|| "goal contract should be built".to_string())?;
        if !contract.contains("objective artifact before acting")
            || !contract.contains("objective-r1-")
        {
            return Err("goal contract should point at the spilled objective artifact".to_string());
        }
        let capsule = build_runtime_recovery_capsule(&store, "r4")
            .ok_or_else(|| "capsule should be built".to_string())?;
        assert!(
            capsule.contains("Full objective artifact:"),
            "capsule should point at the spilled objective artifact"
        );
        echo_agent::utils::fs::atomic_write(&artifact, b"stale objective")
            .map_err(|error| error.to_string())?;
        assert!(
            store.objective_artifact_path("r4").is_none(),
            "a digest-mismatched objective artifact must not be exposed"
        );
        let degraded_contract = build_runtime_goal_contract(&store, "r4")
            .ok_or_else(|| "goal contract should survive artifact degradation".to_string())?;
        assert!(degraded_contract.contains("unavailable"));
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
                domain_binding: None,

                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(task_title),
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
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|err| format!("seed continuation failed: {err}"))?;
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

    #[test]
    fn invocation_retains_original_projection_token_and_rejects_foreign_store() -> Result<(), String>
    {
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let store = seed_projection_store("retained-run", "original run")?;
        let original = registry.register("retained-run", store.clone());
        let retained = registry.retain_for_invocation("retained-run", store.clone())?;
        assert!(Arc::ptr_eq(&original._inner, &retained._inner));
        let other = seed_projection_store("retained-run", "other owner")?;
        assert!(
            registry
                .retain_for_invocation("retained-run", other)
                .is_err()
        );
        assert!(
            registry
                .store("retained-run")
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, &store))
        );
        drop(original);
        assert!(registry.contains("retained-run"));
        drop(retained);
        assert!(!registry.contains("retained-run"));
        Ok(())
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

    struct UnreachableSshFiles(std::sync::atomic::AtomicUsize);

    impl UnreachableSshFiles {
        fn attempted<T>(&self) -> echo_agent::error::Result<T> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(echo_agent::error::ReactError::Other(
                "本机原文引用不应触达 SSH FileSystem".to_owned(),
            ))
        }
    }

    impl echo_agent::tools::filesystem::FileSystem for UnreachableSshFiles {
        fn current_dir(&self) -> echo_agent::error::Result<std::path::PathBuf> {
            self.attempted()
        }
        fn canonicalize(
            &self,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<std::path::PathBuf> {
            self.attempted()
        }
        fn metadata(
            &self,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<echo_agent::tools::filesystem::FileMetadata> {
            self.attempted()
        }
        fn symlink_metadata(
            &self,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<echo_agent::tools::filesystem::FileMetadata> {
            self.attempted()
        }
        fn read(&self, _: &std::path::Path) -> echo_agent::error::Result<Vec<u8>> {
            self.attempted()
        }
        fn open_read(
            &self,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<Box<dyn std::io::Read + Send>> {
            self.attempted()
        }
        fn read_dir(
            &self,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<Vec<echo_agent::tools::filesystem::FileEntry>> {
            self.attempted()
        }
        fn create_dir_all(&self, _: &std::path::Path) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn create_file(&self, _: &std::path::Path) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn write(&self, _: &std::path::Path, _: &[u8]) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn append(&self, _: &std::path::Path, _: &[u8]) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn rename(
            &self,
            _: &std::path::Path,
            _: &std::path::Path,
        ) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn remove_file(&self, _: &std::path::Path) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn remove_dir(&self, _: &std::path::Path) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn remove_dir_all(&self, _: &std::path::Path) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn set_mode(&self, _: &std::path::Path, _: u32) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn atomic_write(&self, _: &std::path::Path, _: &[u8]) -> echo_agent::error::Result<()> {
            self.attempted()
        }
        fn checkpoint(&self, _: &std::path::Path) -> echo_agent::error::Result<Option<String>> {
            self.attempted()
        }
    }

    fn seed_bounded_steer_run(
        root: &std::path::Path,
        run_id: &str,
        original: &str,
    ) -> Result<TaskRuntimeStore, String> {
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(root)
            .map_err(|error| error.to_string())?;
        store
            .create_run(
                run_id,
                &store.active_workspace_id(),
                "bounded-conversation",
                "root-message",
                DomainProfile::General,
                "保留原要求并继续当前任务",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .record_run_steer(run_id, "oversized-steer", original)
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    // 此用例只依赖 raw 两源已有 API，可先作为 test-only 反例采用再运行。
    #[tokio::test]
    async fn oversized_steer_capsule_fits_real_context_after_cold_reopen() -> Result<(), String> {
        use echo_agent::compression::compressor::SlidingWindowCompressor;
        use echo_agent::tokenizer::{HeuristicTokenizer, Tokenizer};
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!("{}末尾必须保留：禁止推送🚀", "大段背景说明 ".repeat(12_000));
        assert!(HeuristicTokenizer.count_tokens(&original) > 8_000);
        let root = temporary.path().join(".eko/tasks");
        let store = seed_bounded_steer_run(&root, "oversized-context", &original)?;
        drop(store);
        let reopened = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
                .map_err(|error| error.to_string())?,
        );
        let state = reopened
            .get_run_state("oversized-context")
            .map_err(|error| error.to_string())?
            .ok_or("冷重开原 Run 缺失")?;
        assert_eq!(
            state
                .recent_constraints
                .first()
                .map(|steer| steer.text.as_str()),
            Some(original.as_str())
        );
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register("oversized-context", reopened);
        let projector = TaskRuntimeContextProjector::new(registry);
        let mut context = ContextManager::builder(8_000)
            .compressor(SlidingWindowCompressor::new(20).with_recent_token_budget(400))
            .build();
        for _ in 0..3 {
            context.apply_projections(
                &projector
                    .project(&projection_context(Some("oversized-context")))
                    .await
                    .map_err(|error| error.to_string())?,
            );
            context.push(Message::assistant("历史执行输出 ".repeat(12_000)));
            context.push(Message::user("继续当前任务".to_owned()));
            assert!(context.token_estimate() > 8_000);
            let prepared = context
                .prepare(Some("继续当前任务"))
                .await
                .map_err(|error| error.to_string())?;
            assert!(prepared.compressed.is_some(), "必须真正进入原压缩路径");
            assert!(context.token_estimate() <= 8_000);
            let capsule = prepared
                .messages
                .iter()
                .filter_map(|message| message.content.as_text())
                .find(|text| text.contains(RUNTIME_RECOVERY_MARKER))
                .ok_or("模型上下文缺恢复引用")?;
            assert!(!capsule.contains(&original));
            assert!(capsule.contains(&hex::encode(Sha256::digest(original.as_bytes()))));
        }
        Ok(())
    }

    #[tokio::test]
    async fn oversized_steer_reference_reaches_actual_model_and_verified_reader()
    -> Result<(), String> {
        use echo_agent::agent::{
            Agent, AgentEvent, AgentInvocationContext, CancellationToken, ReactAgentBuilder,
        };
        use echo_agent::compression::compressor::SlidingWindowCompressor;
        use echo_agent::tools::ToolContext;
        use echo_agent::tools::files::artifact::{ArtifactPageLimit, read_artifact_page};
        use futures::StreamExt;
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!(
            "{}独立尾部约束：禁止发布🚀",
            "实际模型边界背景 ".repeat(12_000)
        );
        let root = temporary.path().join(".eko/tasks");
        let store = seed_bounded_steer_run(&root, "actual-model-context", &original)?;
        let original_state = store
            .get_run_state("actual-model-context")
            .map_err(|error| error.to_string())?
            .ok_or("原 Run 缺失")?;
        let original_steer = original_state
            .recent_constraints
            .first()
            .ok_or("原约束缺失")?;
        let before_reopen = store
            .recorded_user_steer_artifact("actual-model-context", original_steer)
            .map_err(|error| error.to_string())?;
        drop(store);
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
                .map_err(|error| error.to_string())?,
        );
        let state = store
            .get_run_state("actual-model-context")
            .map_err(|error| error.to_string())?
            .ok_or("冷恢复原文缺失")?;
        let steer = state.recent_constraints.first().ok_or("原约束缺失")?;
        let artifact = store
            .recorded_user_steer_artifact("actual-model-context", steer)
            .map_err(|error| error.to_string())?;
        assert_eq!(before_reopen.path, artifact.path);
        assert_eq!(before_reopen.sha256, artifact.sha256);
        let sha256 = artifact.sha256.clone().ok_or("原文摘要缺失")?;
        let config = crate::infra::tool_output_artifact_config(Some(temporary.path()));
        let input =
            crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
                text: &original,
                attachments: &[],
                spill_dir: &crate::prepared_turn::resolve_user_input_spill_dir(Some(
                    temporary.path(),
                )),
                conversation_id: Some("bounded-conversation"),
                turn_id: Some("original-message"),
            })
            .map_err(|error| error.to_string())?;
        assert!(
            !input.instruction.contains(&original),
            "原长输入仍须经过 PreparedUserTurn spill"
        );
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register("actual-model-context", store);
        let llm =
            Arc::new(echo_agent::testing::MockLlmClient::new().with_response("继续处理原要求。"));
        let remote = Arc::new(UnreachableSshFiles(std::sync::atomic::AtomicUsize::new(0)));
        let agent = ReactAgentBuilder::new()
            .enable_tools()
            .file_system(remote.clone())
            .auto_project_rules(false)
            .tool_output_artifacts(config.clone())
            .model("mock-model")
            .system_prompt("保留用户约束")
            .llm_client(llm.clone())
            .token_limit(8_000)
            .max_iterations(2)
            .build()
            .map_err(|error| error.to_string())?;
        agent.set_pre_model_context_projector(Some(Arc::new(TaskRuntimeContextProjector::new(
            registry,
        ))));
        {
            let mut context = agent.context().lock().await;
            *context = ContextManager::builder(8_000)
                .compressor(SlidingWindowCompressor::new(20).with_recent_token_budget(400))
                .build();
        }
        let invocation = AgentInvocationContext {
            // Execute 会先重置活动消息；同一历史必须沿原 invocation 在恢复后重放。
            history: Some(vec![
                Message::user("之前的任务".to_owned()),
                Message::assistant("过期执行信息 ".repeat(12_000)),
            ]),
            allowed_tools: Some(["read_artifact".to_owned()].into_iter().collect()),
            runtime: Some(echo_agent::tools::ExternalRunContext {
                conversation_id: Some("bounded-conversation".to_owned()),
                run_id: Some("actual-model-context".to_owned()),
                turn_id: Some("model-turn".to_owned()),
                execution_id: Some("model-turn".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut events = agent
            .execute_stream_message_with_invocation_context(
                input.to_message().map_err(|error| error.to_string())?,
                CancellationToken::new(),
                invocation,
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut compressed = false;
        while let Some(event) = events.next().await {
            compressed |= matches!(
                event.map_err(|error| error.to_string())?,
                AgentEvent::ContextCompressed { .. }
            );
        }
        assert!(compressed, "实际 Agent 必须观测到原压缩完成事件");
        let requests = llm.all_calls();
        assert_eq!(requests.len(), 1, "不得以未命中模型的零请求通过");
        let actual = requests.first().ok_or("实际模型请求缺失")?;
        let texts = actual
            .iter()
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>();
        assert!(
            texts
                .iter()
                .any(|text| text.contains(RUNTIME_RECOVERY_MARKER) && text.contains(&sha256))
        );
        assert!(!texts.iter().any(|text| text.contains(&original)));
        assert!(agent.context().lock().await.token_estimate() <= 8_000);
        // 独立走真实注册工具的读取实现与路径/SHA gate，不以内存字符串拼接互证。
        let parameters = serde_json::from_value(serde_json::json!({
            "path": artifact.path.clone(), "expected_sha256": sha256.clone(), "max_tokens": 256,
        }))
        .map_err(|error| error.to_string())?;
        let tool = agent
            .tool_manager()
            .get_tool("read_artifact")
            .ok_or("原注册目录缺少 read_artifact")?;
        let tool_result = tool
            .execute_with_context(
                parameters,
                &ToolContext {
                    output_artifacts: Some(config.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(tool_result.success);
        assert_eq!(tool_result.metadata.get("sha256"), Some(&sha256));
        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
            path: artifact.path.clone(),
            artifact_bytes: artifact.bytes,
            payload_bytes: artifact.bytes,
            sha256,
            retention: config.retention.clone(),
        };
        let mut restored = String::new();
        let mut cursor = None;
        loop {
            let page = read_artifact_page(
                &config,
                &reference,
                cursor.as_deref(),
                ArtifactPageLimit::Tokens(1_000),
            )
            .map_err(|error| error.to_string())?;
            restored.push_str(&page.content);
            if page.complete {
                break;
            }
            cursor = Some(page.next_cursor.ok_or("原分页未给 cursor")?);
        }
        assert_eq!(restored, original);
        assert!(restored.ends_with("独立尾部约束：禁止发布🚀"));
        assert_eq!(
            remote.0.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "真实原文分页读取必须留在 host 本机，不能转给 SSH 文件系统"
        );
        Ok(())
    }

    #[test]
    fn oversized_steer_missing_or_changed_cache_recovers_only_from_journal() -> Result<(), String> {
        use echo_agent::tools::files::artifact::{ArtifactPageLimit, read_artifact_page};
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!("{}尾部义务仍在", "缓存缺失恢复 ".repeat(12_000));
        let store = seed_bounded_steer_run(
            &temporary.path().join(".eko/tasks"),
            "cache-recovery",
            &original,
        )?;
        let state = store
            .get_run_state("cache-recovery")
            .map_err(|error| error.to_string())?
            .ok_or("Run 缺失")?;
        let steer = state.recent_constraints.first().ok_or("原文缺失")?;
        let first = store
            .recorded_user_steer_artifact("cache-recovery", steer)
            .map_err(|error| error.to_string())?;
        let second = store
            .recorded_user_steer_artifact("cache-recovery", steer)
            .map_err(|error| error.to_string())?;
        assert_eq!(first.path, second.path);
        assert_eq!(first.sha256, second.sha256);
        let config = crate::infra::tool_output_artifact_config(Some(temporary.path()));
        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
            path: first.path.clone(),
            artifact_bytes: first.bytes,
            payload_bytes: first.bytes,
            sha256: first.sha256.clone().ok_or("hash 缺失")?,
            retention: config.retention.clone(),
        };
        std::fs::write(&first.path, "已变更的缓存").map_err(|error| error.to_string())?;
        assert!(
            read_artifact_page(&config, &reference, None, ArtifactPageLimit::Tokens(256)).is_err()
        );
        let capsule =
            build_runtime_recovery_capsule(&store, "cache-recovery").ok_or("capsule 缺失")?;
        assert_eq!(
            std::fs::read_to_string(&first.path).map_err(|error| error.to_string())?,
            original
        );
        assert!(capsule.contains(&reference.sha256));
        std::fs::remove_file(&first.path).map_err(|error| error.to_string())?;
        let missing =
            build_runtime_recovery_capsule(&store, "cache-recovery").ok_or("capsule 缺失")?;
        assert!(missing.contains(&reference.sha256));
        assert_eq!(
            std::fs::read_to_string(&first.path).map_err(|error| error.to_string())?,
            original
        );
        std::fs::remove_file(&first.path).map_err(|error| error.to_string())?;
        let directory = first.path.parent().ok_or("原目录缺失")?;
        std::fs::remove_dir(directory).map_err(|error| error.to_string())?;
        std::fs::write(directory, "阻断原 artifact 目录").map_err(|error| error.to_string())?;
        assert!(build_runtime_recovery_capsule_checked(&store, "cache-recovery").is_err());
        let still_original = store
            .get_run_state("cache-recovery")
            .map_err(|error| error.to_string())?
            .ok_or("journal 丢失")?;
        assert_eq!(
            still_original
                .recent_constraints
                .first()
                .map(|steer| steer.text.as_str()),
            Some(original.as_str())
        );
        Ok(())
    }

    struct ConstraintFailureProbe(Arc<std::sync::atomic::AtomicUsize>);

    impl echo_agent::tools::Tool for ConstraintFailureProbe {
        fn name(&self) -> &str {
            "after_missing_constraint_probe"
        }
        fn description(&self) -> &str {
            "确认必要上下文错误后未进入后续工具"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{}})
        }
        fn execute<'a>(
            &'a self,
            _: echo_agent::tools::ToolParameters,
        ) -> BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>> {
            Box::pin(async move {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(echo_agent::tools::ToolResult::success("此工具不应执行"))
            })
        }
    }

    #[tokio::test]
    async fn oversized_steer_materialization_failure_blocks_actual_model() -> Result<(), String> {
        use echo_agent::agent::{
            Agent, AgentEvent, AgentInvocationContext, CancellationToken, ReactAgentBuilder,
        };
        use futures::StreamExt;
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!("{}尾部仍是必须遵守的限制", "物化失败背景 ".repeat(12_000));
        let store = Arc::new(seed_bounded_steer_run(
            &temporary.path().join(".eko/tasks"),
            "blocked-model",
            &original,
        )?);
        std::fs::write(temporary.path().join(".eko/artifacts"), "阻断原文缓存目录")
            .map_err(|error| error.to_string())?;
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register("blocked-model", store.clone());
        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .then_tool_call("after-failure", "after_missing_constraint_probe", "{}")
                .with_response("此回答不得到达模型。"),
        );
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent = ReactAgentBuilder::new()
            .model("mock-model")
            .llm_client(llm.clone())
            .tool(Box::new(ConstraintFailureProbe(tool_calls.clone())))
            .token_limit(8_000)
            .build()
            .map_err(|error| error.to_string())?;
        // 使用生产组合 projector，避免只给直接 TaskRuntime helper 上 strict 而漏实际包装。
        agent.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                registry,
                Arc::new(crate::turn_context::TurnPromptContextRegistry::new()),
            ),
        )));
        let invocation = AgentInvocationContext {
            runtime: Some(echo_agent::tools::ExternalRunContext {
                run_id: Some("blocked-model".to_owned()),
                turn_id: Some("blocked-turn".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = agent
            .execute_stream_message_with_invocation_context(
                Message::user("继续原任务".to_owned()),
                CancellationToken::new(),
                invocation,
            )
            .await;
        let mut failed = false;
        match result {
            Err(error) => {
                failed = error.to_string().contains("用户约束原文");
            }
            Ok(mut stream) => {
                while let Some(event) = stream.next().await {
                    match event {
                        Err(error) => failed |= error.to_string().contains("用户约束原文"),
                        Ok(AgentEvent::Error { message, .. }) => {
                            failed |= message.contains("用户约束原文")
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(failed, "必须沿原失败通道报告原文引用错误");
        assert_eq!(llm.call_count(), 0, "不能在缺约束时仅告警后继续模型");
        assert_eq!(
            tool_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "模型脚本明确安排的真实注册探针工具不得执行"
        );
        let state = store
            .get_run_state("blocked-model")
            .map_err(|error| error.to_string())?
            .ok_or("journal 缺失")?;
        assert_eq!(
            state
                .recent_constraints
                .first()
                .map(|steer| steer.text.as_str()),
            Some(original.as_str())
        );
        Ok(())
    }

    type FinalToolObservations = Arc<std::sync::Mutex<Vec<Vec<String>>>>;

    struct ActualRequestAudit {
        inner: crate::turn_context::EkoContextProjector,
        tools: FinalToolObservations,
    }

    impl PreModelContextProjector for ActualRequestAudit {
        fn is_required(&self) -> bool {
            self.inner.is_required()
        }
        fn project<'a>(
            &'a self,
            context: &'a ProjectionContext,
        ) -> BoxFuture<'a, AgentResult<Vec<ContextProjection>>> {
            self.inner.project(context)
        }
        fn validate_model_request<'a>(
            &'a self,
            context: &'a ProjectionContext,
            request: &'a echo_agent::llm::ChatRequest,
            artifacts: Option<&'a echo_agent::tools::artifact::ToolOutputArtifactConfig>,
        ) -> BoxFuture<'a, AgentResult<()>> {
            Box::pin(async move {
                let mut names = request
                    .tools
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|tool| tool.function.name.clone())
                    .collect::<Vec<_>>();
                names.sort_unstable();
                self.tools
                    .lock()
                    .map_err(|_| {
                        echo_agent::error::ReactError::Other("最终工具观察锁不可用".to_owned())
                    })?
                    .push(names);
                self.inner
                    .validate_model_request(context, request, artifacts)
                    .await
            })
        }
    }

    fn seed_primary_context_run(
        root: &std::path::Path,
        run_id: &str,
        text: &str,
        allowed: &[&str],
    ) -> Result<Arc<TaskRuntimeStore>, String> {
        let store = Arc::new(seed_bounded_steer_run(root, run_id, text)?);
        store
            .attach_plan_for_test(&TaskPlan {
                domain_binding: None,
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_owned(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: super::super::task_goal_sha256("保留原要求并继续当前任务"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "primary-verification".to_owned(),
                    title: "核对真实 primary verification 恢复".to_owned(),
                    description: "遵守完整用户约束并报告验证结果".to_owned(),
                    kind: PlanTaskKind::Verification,
                    agent_role: "primary".to_owned(),
                    allowed_tools: allowed.iter().map(|name| (*name).to_owned()).collect(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    async fn primary_context_agent(
        registry: Arc<TaskRuntimeProjectionRegistry>,
        config: echo_agent::tools::artifact::ToolOutputArtifactConfig,
        llm: Arc<dyn echo_agent::llm::LlmClient>,
        remote: Arc<UnreachableSshFiles>,
        observed: FinalToolObservations,
        probe_calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<crate::agent_handle::AgentHandle, String> {
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("mock-model")
            .enable_tools()
            .file_system(remote)
            .auto_project_rules(false)
            .tool_output_artifacts(config)
            .llm_client(llm)
            .token_limit(8_000)
            .max_iterations(32)
            .tool(Box::new(ConstraintFailureProbe(probe_calls)))
            .build()
            .map_err(|error| error.to_string())?;
        assert!(
            agent
                .tool_names()
                .iter()
                .any(|name| name == "read_artifact")
        );
        assert!(agent.tool_names().iter().any(|name| name == "read_file"));
        *agent.context().lock().await = ContextManager::builder(8_000)
            .compressor(
                echo_agent::compression::compressor::SlidingWindowCompressor::new(20)
                    .with_recent_token_budget(400),
            )
            .build();
        agent.set_pre_model_context_projector(Some(Arc::new(ActualRequestAudit {
            inner: crate::turn_context::EkoContextProjector::new(
                registry,
                Arc::new(crate::turn_context::TurnPromptContextRegistry::new()),
            ),
            tools: observed,
        })));
        Ok(crate::agent_handle::AgentHandle::new(agent))
    }

    async fn execute_primary_context_run(
        store: Arc<TaskRuntimeStore>,
        run_id: &str,
        agent: crate::agent_handle::AgentHandle,
    ) -> Result<super::super::executor::RunOutcome, String> {
        let cancel = echo_agent::agent::CancellationToken::new();
        let owner = store
            .register_run_cancellation(run_id, cancel.clone())
            .map_err(|error| error.to_string())?;
        let outcome = super::super::executor::execute_run(
            store.clone(),
            Some(agent),
            None,
            None,
            None,
            None,
            run_id,
            cancel,
            super::super::memory_bridge::MemoryPolicy::None,
            None,
        )
        .await
        .map_err(|error| error.to_string());
        drop(owner);
        store.wait_for_run_driver_idle(run_id).await;
        outcome
    }

    #[tokio::test]
    async fn primary_required_context_failure_pauses_then_real_resume_reads_full_source()
    -> Result<(), String> {
        use echo_agent::tools::files::artifact::{ArtifactPageLimit, read_artifact_page};
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!(
            "{}FULL_REQUIREMENT_TAIL_6509🚀",
            "持续约束背景 ".repeat(2_000)
        );
        let run_id = "primary-context-recovery";
        let store = seed_primary_context_run(
            &temporary.path().join(".eko/tasks"),
            run_id,
            &original,
            &["read_artifact", "after_missing_constraint_probe"],
        )?;
        let _resources = store.test_resource_scope();
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register(run_id, store.clone());
        let config = crate::infra::tool_output_artifact_config(Some(temporary.path()));
        let artifact_root = temporary.path().join(".eko/artifacts");
        std::fs::write(&artifact_root, "阻止本机原文缓存物化")
            .map_err(|error| error.to_string())?;
        let first_model = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .then_tool_call("after-failure", "after_missing_constraint_probe", "{}")
                .with_response("不应到达模型"),
        );
        let remote = Arc::new(UnreachableSshFiles(std::sync::atomic::AtomicUsize::new(0)));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let probe_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_agent = primary_context_agent(
            registry.clone(),
            config.clone(),
            first_model.clone(),
            remote.clone(),
            observed.clone(),
            probe_calls.clone(),
        )
        .await?;
        let result = execute_primary_context_run(store.clone(), run_id, first_agent).await?;
        assert!(matches!(
            result,
            super::super::executor::RunOutcome::Paused { .. }
        ));
        assert_eq!(first_model.call_count(), 0);
        assert_eq!(probe_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let paused = store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("暂停 Run 缺失")?;
        assert_eq!(paused.run.status, TaskRunStatus::Paused);
        let paused_plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("暂停计划缺失")?;
        let task = paused_plan.tasks.first().ok_or("暂停任务缺失")?;
        assert!(matches!(
            task.status,
            echo_agent::tasks::TaskStatus::Paused(_)
        ));
        assert_eq!(task.retry_count, 0);
        let stale = super::super::types::TaskRunResumeIdentity::capture(&paused);
        store
            .record_run_steer(run_id, "after-pause", "继续保持原工具范围")
            .map_err(|error| error.to_string())?;
        assert!(
            store.resume_task_run_expected(&stale).is_err(),
            "原版本重读必须拒绝旧恢复身份"
        );
        std::fs::remove_file(&artifact_root).map_err(|error| error.to_string())?;
        let current = store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("当前 Run 缺失")?;
        let steer = current
            .recent_constraints
            .iter()
            .find(|steer| steer.turn_id == "oversized-steer")
            .ok_or("原约束缺失")?;
        let artifact = store
            .recorded_user_steer_artifact(run_id, steer)
            .map_err(|error| error.to_string())?;
        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
            path: artifact.path.clone(),
            artifact_bytes: artifact.bytes,
            payload_bytes: artifact.bytes,
            sha256: artifact.sha256.clone().ok_or("原文 hash 缺失")?,
            retention: config.retention.clone(),
        };
        // 用真实 reader 生成同文件快照的 cursor fixture；实际消费仍由原模型/工具 pipeline 完成。
        let mut script = echo_agent::testing::MockLlmClient::new();
        let mut cursor = None;
        for ordinal in 0..32 {
            let parameters = serde_json::json!({"path": artifact.path.clone(), "expected_sha256": reference.sha256.clone(),
                "max_tokens": 2_000, "cursor": cursor.clone()});
            script = script.then_tool_call(
                &format!("source-page-{ordinal}"),
                "read_artifact",
                &parameters.to_string(),
            );
            let page = read_artifact_page(
                &config,
                &reference,
                cursor.as_deref(),
                ArtifactPageLimit::Tokens(2_000),
            )
            .map_err(|error| error.to_string())?;
            if page.complete {
                break;
            }
            cursor = Some(page.next_cursor.ok_or("原 reader 未给下一页")?);
            if ordinal == 31 {
                return Err("原文分页 fixture 超出本测试预期范围".to_owned());
            }
        }
        let resumed_model = Arc::new(script.with_response("已读取完整约束并完成验证。"));
        let resumed_agent = primary_context_agent(
            registry,
            config,
            resumed_model.clone(),
            remote.clone(),
            observed.clone(),
            probe_calls,
        )
        .await?;
        let expected = super::super::types::TaskRunResumeIdentity::capture(&current);
        let launch = super::super::executor::launch_task_run_resume(
            store.clone(),
            expected,
            resumed_agent,
            None,
            None,
            None,
            echo_agent::agent::CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        let recovered = launch.wait().await.map_err(|error| error.to_string())?;
        store.wait_for_run_driver_idle(run_id).await;
        assert!(matches!(
            recovered,
            super::super::executor::RunOutcome::Completed
        ));
        assert_eq!(
            store
                .get_run(run_id)
                .map_err(|error| error.to_string())?
                .ok_or("Run 缺失")?
                .status,
            TaskRunStatus::Completed
        );
        let final_plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("计划缺失")?;
        assert_eq!(final_plan.tasks.first().ok_or("任务缺失")?.retry_count, 0);
        assert!(
            resumed_model
                .all_calls()
                .iter()
                .any(|messages| messages.iter().any(|message| {
                    message
                        .content
                        .as_text_ref()
                        .is_some_and(|text| text.contains("FULL_REQUIREMENT_TAIL_6509🚀"))
                })),
            "完整尾部必须真正进入原模型 API，不能仅用 helper 回读互证"
        );
        assert!(
            observed
                .lock()
                .map_err(|_| "观察锁不可用")?
                .iter()
                .all(|names| names == &["read_artifact".to_owned()])
        );
        assert_eq!(remote.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn primary_required_source_with_ssh_read_file_only_pauses_without_widening_scope()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = format!(
            "{}SSH_PRIVATE_TAIL_8462🚀",
            "原文不能交给远端读取 ".repeat(2_000)
        );
        let run_id = "primary-hard-w";
        let store = seed_primary_context_run(
            &temporary.path().join(".eko/tasks"),
            run_id,
            &original,
            &["read_file"],
        )?;
        let _resources = store.test_resource_scope();
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register(run_id, store.clone());
        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new().with_response("此调用必须被准入阻止"),
        );
        let remote = Arc::new(UnreachableSshFiles(std::sync::atomic::AtomicUsize::new(0)));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent = primary_context_agent(
            registry,
            crate::infra::tool_output_artifact_config(Some(temporary.path())),
            llm.clone(),
            remote.clone(),
            observed.clone(),
            probes.clone(),
        )
        .await?;
        let outcome = execute_primary_context_run(store.clone(), run_id, agent).await?;
        assert!(matches!(
            outcome,
            super::super::executor::RunOutcome::Paused { .. }
        ));
        let actual = observed.lock().map_err(|_| "观察锁不可用")?;
        assert_eq!(
            actual.as_slice(),
            &[vec!["read_file".to_owned()]],
            "必须检查原 snapshot 最终请求，不能用注册目录冒可读"
        );
        assert_eq!(llm.call_count(), 0);
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(remote.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Run 缺失")?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let plan = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("计划缺失")?;
        let task = plan.tasks.first().ok_or("任务缺失")?;
        assert_eq!(task.allowed_tools, vec!["read_file".to_owned()]);
        assert!(matches!(
            task.status,
            echo_agent::tasks::TaskStatus::Paused(_)
        ));
        assert_eq!(task.retry_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn goal_and_recorded_constraints_survive_real_compression_and_refresh()
    -> Result<(), String> {
        use echo_agent::compression::compressor::SlidingWindowCompressor;
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let store = seed_projection_store("compression-run", "keep the original goal")?;
        let constraint = format!("{}必须保留结尾限制🚀", "背景介绍".repeat(600));
        store
            .record_run_steer("compression-run", "first-steer", &constraint)
            .map_err(|error| error.to_string())?;
        let steer = store
            .get_run_state("compression-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.recent_constraints.into_iter().next())
            .ok_or_else(|| "原 journal 约束缺失".to_string())?;
        let artifact = store
            .recorded_user_steer_artifact("compression-run", &steer)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            std::fs::read_to_string(&artifact.path).map_err(|error| error.to_string())?,
            constraint
        );
        let sha256 = artifact
            .sha256
            .as_deref()
            .ok_or("原文 hash 缺失")?
            .to_string();
        let _registration = registry.register("compression-run", store);
        let projector = TaskRuntimeContextProjector::new(registry);
        let mut context = ContextManager::builder(8_000)
            .compressor(SlidingWindowCompressor::new(20).with_recent_token_budget(40))
            .build();
        for _ in 0..3 {
            context.apply_projections(
                &projector
                    .project(&projection_context(Some("compression-run")))
                    .await
                    .map_err(|error| error.to_string())?,
            );
            context.push(Message::assistant("old execution output ".repeat(500)));
            context.push(Message::user("continue current work".to_string()));
            context
                .force_compress(20)
                .await
                .map_err(|error| error.to_string())?;
            let texts = context
                .messages()
                .iter()
                .filter_map(|message| message.content.as_text())
                .collect::<Vec<_>>();
            assert!(
                texts
                    .iter()
                    .any(|text| text.contains("keep the original goal"))
            );
            assert!(texts.iter().any(|text| text.contains(&sha256)));
            assert!(!texts.iter().any(|text| text.contains(&constraint)));
            assert_eq!(
                texts
                    .iter()
                    .filter(|text| text.contains(RUNTIME_GOAL_MARKER))
                    .count(),
                1
            );
            assert_eq!(
                texts
                    .iter()
                    .filter(|text| text.contains(RUNTIME_RECOVERY_MARKER))
                    .count(),
                1
            );
        }
        Ok(())
    }

    #[test]
    fn recorded_user_steer_retains_full_original_after_cold_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let original = format!("{}禁止推送且保留末尾限制🚀", "完整背景".repeat(600));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
        store.create_run(
            "full-steer",
            "default",
            "original-conversation",
            "original-message",
            DomainProfile::General,
            "original goal",
            "agent_task_plan",
            AttendedMode::Attended,
        )?;
        store.record_run_steer("full-steer", "original-turn", &original)?;
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
        let event = reopened
            .list_events("full-steer", 0)?
            .into_iter()
            .find(|event| {
                event.event_type == super::super::types::RuntimeEventKind::RunSteerRecorded
            })
            .ok_or("original steer journal event missing")?;
        assert_eq!(
            event
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str),
            Some(original.as_str())
        );
        let state = reopened
            .get_run_state("full-steer")?
            .ok_or("original run missing")?;
        assert_eq!(state.run.conversation_id, "original-conversation");
        let steer = state
            .recent_constraints
            .first()
            .ok_or("original steer projection missing")?;
        assert_eq!(steer.turn_id, "original-turn");
        assert_eq!(steer.text, original);
        let capsule = build_runtime_recovery_capsule(&reopened, "full-steer")
            .ok_or("recovery capsule missing")?;
        assert!(!capsule.contains(&original));
        let artifact = reopened.recorded_user_steer_artifact("full-steer", steer)?;
        assert_eq!(std::fs::read_to_string(&artifact.path)?, original);
        assert!(capsule.contains(artifact.sha256.as_deref().ok_or("原文 hash 缺失")?));
        Ok(())
    }

    #[tokio::test]
    async fn projector_keeps_one_stable_goal_contract_across_one_hundred_refreshes()
    -> Result<(), String> {
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let projector = TaskRuntimeContextProjector::new(Arc::clone(&registry));
        let objective = format!(
            "{}终点🚀",
            "跨窗口目标".repeat(MAX_GOAL_CONTRACT_CHARS.saturating_add(32))
        );
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| format!("seed store failed: {error}"))?,
        );
        store
            .create_run(
                "long-goal",
                "default",
                "c1",
                "root-message",
                DomainProfile::General,
                &objective,
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("long-goal", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("long-goal", true, false, Some(200_000), None)
            .map_err(|error| error.to_string())?;
        let _registration = registry.register("long-goal", store);
        let mut context = ContextManager::builder(64_000).build();
        let mut expected_hash = None;

        for _ in 0..100 {
            let projections = projector
                .project(&projection_context(Some("long-goal")))
                .await
                .map_err(|error| error.to_string())?;
            if projections.len() != 2 {
                return Err(format!(
                    "expected two replaceable projections, got {}",
                    projections.len()
                ));
            }
            let contract = projections
                .first()
                .and_then(|projection| projection.message.as_ref())
                .and_then(|message| message.content.as_text())
                .ok_or_else(|| "Goal contract missing before Plan creation".to_string())?;
            let hash = contract
                .split("objective_sha256=")
                .nth(1)
                .and_then(|suffix| suffix.split(',').next())
                .ok_or_else(|| "objective hash missing".to_string())?
                .to_string();
            match expected_hash.as_ref() {
                Some(expected) if expected != &hash => {
                    return Err("objective hash changed across projection refreshes".to_string());
                }
                None => expected_hash = Some(hash),
                _ => {}
            }
            if !contract.contains("跨窗口目标") || !contract.contains("bounded here") {
                return Err("UTF-8 Goal contract was not safely bounded".to_string());
            }
            let capsule = projections
                .get(1)
                .and_then(|projection| projection.message.as_ref())
                .and_then(|message| message.content.as_text())
                .ok_or_else(|| "recovery capsule missing before Plan creation".to_string())?;
            if !capsule.contains("Continuation:") || capsule.contains("Plan: id=") {
                return Err("unexpected Plan projection before Plan creation".to_string());
            }
            context.apply_projections(&projections);
        }

        let goal_contracts = context
            .messages()
            .iter()
            .filter(|message| {
                message
                    .content
                    .as_text_ref()
                    .is_some_and(|text| text.contains(RUNTIME_GOAL_MARKER))
            })
            .count();
        let recovery_capsules = context
            .messages()
            .iter()
            .filter(|message| {
                message
                    .content
                    .as_text_ref()
                    .is_some_and(|text| text.contains(RUNTIME_RECOVERY_MARKER))
            })
            .count();
        assert_eq!(goal_contracts, 1);
        assert_eq!(recovery_capsules, 1);
        Ok(())
    }
}
