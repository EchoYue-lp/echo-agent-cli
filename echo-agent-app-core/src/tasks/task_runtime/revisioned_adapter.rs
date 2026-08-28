//! Thin EKO adapters for the framework-owned task revision service.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use echo_agent::tasks::{
    DefaultTaskToolPolicy, PreparedTaskPolicy, RevisionedTaskGraph, RevisionedTaskStore,
    RevisionedTaskStoreError, TaskCreateInput, TaskDraft, TaskGraphCommit, TaskGraphContext,
    TaskPolicyError, TaskRevisionError, TaskRevisionService, TaskToolPolicy,
};
use echo_agent::tools::ToolContext;

use super::executor::TaskRuntimeBlockingAdapter;
use super::profiles::default_subagent_for;
use super::store::{InitialRunTriggerMetadata, StoreError, TaskRuntimeStore};
use super::task_tools::{TaskCapabilityCatalog, current_run_id, formal_run_id_for_turn};
use super::types::{
    AttendedMode, DomainProfile, EkoPlanMetadata, EkoTaskExtension, PlanTaskKind,
    TaskExecutionSummary, TaskExecutionTarget, TaskRun, TaskUpdateRequest,
};

#[derive(Clone)]
struct PendingInitialRun {
    run: TaskRun,
    trigger: InitialRunTriggerMetadata,
    continuation: Option<(bool, bool, Option<u64>, Option<u64>)>,
}

type PendingInitialRuns = Arc<Mutex<HashMap<String, PendingInitialRun>>>;
#[derive(Clone)]
struct PendingDirectCompletion {
    summary: TaskExecutionSummary,
    task_summary: String,
}

type PendingDirectCompletions = Arc<Mutex<HashMap<String, PendingDirectCompletion>>>;

/// File persistence adapter. It deliberately has no patch or validation
/// logic; those remain authoritative in the framework service.
pub struct EkoRevisionedTaskStore {
    blocking: TaskRuntimeBlockingAdapter,
    pending_initial_runs: PendingInitialRuns,
    pending_direct_completions: PendingDirectCompletions,
}

impl EkoRevisionedTaskStore {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        Self {
            blocking: TaskRuntimeBlockingAdapter::new(store),
            pending_initial_runs: Arc::new(Mutex::new(HashMap::new())),
            pending_direct_completions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn with_pending_initial_runs(
        store: Arc<TaskRuntimeStore>,
        pending_initial_runs: PendingInitialRuns,
    ) -> Self {
        Self {
            blocking: TaskRuntimeBlockingAdapter::new(store),
            pending_initial_runs,
            pending_direct_completions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn with_pending_direct_completion(
        store: Arc<TaskRuntimeStore>,
        pending_direct_completions: PendingDirectCompletions,
    ) -> Self {
        Self {
            blocking: TaskRuntimeBlockingAdapter::new(store),
            pending_initial_runs: Arc::new(Mutex::new(HashMap::new())),
            pending_direct_completions,
        }
    }
}

#[async_trait]
impl RevisionedTaskStore for EkoRevisionedTaskStore {
    async fn load(
        &self,
        scope_id: &str,
    ) -> Result<Option<RevisionedTaskGraph>, RevisionedTaskStoreError> {
        let scope_id = scope_id.to_string();
        self.blocking
            .run_store("load revisioned task graph", move |store| {
                store.load_revisioned_task_graph(&scope_id)
            })
            .await
            .map_err(store_error)
    }

    async fn compare_and_commit(
        &self,
        scope_id: &str,
        commit: TaskGraphCommit,
    ) -> Result<RevisionedTaskGraph, RevisionedTaskStoreError> {
        let pending = self
            .pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(scope_id)
            .cloned();
        let pending_direct = self
            .pending_direct_completions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(scope_id)
            .cloned();
        let owned_scope_id = scope_id.to_string();
        let committed = self
            .blocking
            .run_store("commit revisioned task graph", move |store| {
                match (pending, pending_direct) {
                    (Some(pending), None) => store
                        .compare_and_publish_initial_revisioned_task_graph(
                            &pending.run,
                            &pending.trigger,
                            pending.continuation,
                            commit,
                        ),
                    (None, Some(completion)) => store.compare_and_commit_direct_completion(
                        &owned_scope_id,
                        commit,
                        &completion.summary,
                        &completion.task_summary,
                    ),
                    (None, None) => store
                        .compare_and_commit_revisioned_task_graph(&owned_scope_id, commit),
                    (Some(_), Some(_)) => Err(StoreError::InvalidPlan(format!(
                        "TaskRun {owned_scope_id} cannot be both an initial publication and direct completion"
                    ))),
                }
            })
            .await
            .map_err(store_error)?;
        self.pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(scope_id);
        self.pending_direct_completions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(scope_id);
        Ok(committed)
    }
}

fn store_error(error: StoreError) -> RevisionedTaskStoreError {
    match error {
        StoreError::RunNotFound(scope_id) | StoreError::PlanNotFound(scope_id) => {
            RevisionedTaskStoreError::NotFound { scope_id }
        }
        StoreError::PlanConflict {
            expected, current, ..
        } => RevisionedTaskStoreError::Conflict {
            expected: Some(expected),
            current: Some(current),
        },
        StoreError::InvalidPlan(message) | StoreError::TaskNotFound(message) => {
            RevisionedTaskStoreError::Rejected { message }
        }
        error @ (StoreError::GoalConflict { .. }
        | StoreError::GoalUpdateRejected { .. }
        | StoreError::PlanGoalMismatch { .. }) => RevisionedTaskStoreError::Rejected {
            message: error.to_string(),
        },
        other => RevisionedTaskStoreError::Backend {
            message: other.to_string(),
        },
    }
}

/// EKO product policy: run bootstrap, domain defaults, capability validation,
/// and extension round trips. It cannot alter generic task fields.
pub struct EkoTaskToolPolicy {
    blocking: TaskRuntimeBlockingAdapter,
    capabilities: Arc<TaskCapabilityCatalog>,
    pending_initial_runs: PendingInitialRuns,
}

impl EkoTaskToolPolicy {
    fn new(
        store: Arc<TaskRuntimeStore>,
        capabilities: Arc<TaskCapabilityCatalog>,
        pending_initial_runs: PendingInitialRuns,
    ) -> Self {
        Self {
            blocking: TaskRuntimeBlockingAdapter::new(store),
            capabilities,
            pending_initial_runs,
        }
    }

    async fn run(&self, scope_id: &str) -> Result<super::types::TaskRun, TaskPolicyError> {
        if let Some(pending) = self
            .pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(scope_id)
            .cloned()
        {
            return Ok(pending.run);
        }
        let scope_id = scope_id.to_string();
        let lookup_scope_id = scope_id.clone();
        self.blocking
            .run_store("load task policy run", move |store| {
                store.get_run(&lookup_scope_id)
            })
            .await
            .map_err(|error| TaskPolicyError::Backend {
                message: format!("Failed to read task run domain: {error}"),
            })?
            .ok_or_else(|| TaskPolicyError::ScopeUnavailable {
                message: format!("Task run disappeared after task_create bootstrap: {scope_id}"),
            })
    }
}

#[async_trait]
impl TaskToolPolicy for EkoTaskToolPolicy {
    fn task_input_schema_extensions(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([
            (
                "kind".to_string(),
                serde_json::json!({
                    "type": "string",
                    "enum": ["implementation", "debugging", "verification", "review", "investigation", "test_plan", "summary", "read_only_review"]
                }),
            ),
            (
                "subagent".to_string(),
                serde_json::json!({ "type": "string" }),
            ),
            (
                "agent_role".to_string(),
                serde_json::json!({ "type": "string" }),
            ),
            (
                "files".to_string(),
                serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "allowed_tools".to_string(),
                serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "required_artifacts".to_string(),
                serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "execution_checks".to_string(),
                serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "acceptance_criteria".to_string(),
                serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "parallel_group".to_string(),
                serde_json::json!({ "type": "string" }),
            ),
            (
                "execution_target".to_string(),
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Exact group, role, and address from the Agent group service.",
                    "properties": {
                        "group_id": { "type": "string" },
                        "subagent_role": { "type": "string" },
                        "address": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "workspace_id": { "type": "string" },
                                "conversation_id": { "type": "string" }
                            },
                            "required": ["workspace_id", "conversation_id"]
                        }
                    },
                    "required": ["group_id", "subagent_role", "address"]
                }),
            ),
        ])
    }

    fn required_task_input_extensions(&self) -> Vec<String> {
        vec!["kind".to_string()]
    }

    async fn resolve_scope(&self, context: &ToolContext) -> Result<String, TaskPolicyError> {
        context
            .run_id
            .clone()
            .or_else(|| context.turn_id.as_deref().map(formal_run_id_for_turn))
            .or_else(current_run_id)
            .ok_or_else(|| TaskPolicyError::ScopeUnavailable {
                message: "no active run - run_id not in ToolContext or task_local".to_string(),
            })
    }

    async fn ensure_scope(
        &self,
        scope_id: &str,
        input: &TaskCreateInput,
        context: &ToolContext,
    ) -> Result<(), TaskPolicyError> {
        let first = input
            .tasks
            .first()
            .ok_or_else(|| TaskPolicyError::Rejected {
                message: "task_create requires at least one task".to_string(),
            })?;
        let chat_resources = crate::chat_resources::current_chat_resources();
        let conversation_id = context
            .conversation_id
            .clone()
            .or_else(|| {
                chat_resources
                    .as_ref()
                    .and_then(|resources| resources.conv_id.clone())
            })
            .unwrap_or_else(|| format!("message:{scope_id}"));
        let root_message_id = context
            .message_id
            .clone()
            .or_else(|| context.turn_id.clone())
            .or_else(|| {
                chat_resources
                    .as_ref()
                    .map(|resources| resources.root_message_id.clone())
            })
            .unwrap_or_else(|| scope_id.to_string());
        let attachments = chat_resources
            .as_ref()
            .map(|resources| resources.attachments.clone())
            .unwrap_or_default();
        let goal = task_goal(&first.title, &first.description);
        let owned_scope_id = scope_id.to_string();
        let owned_conversation_id = conversation_id.clone();
        let owned_root_message_id = root_message_id.clone();
        let owned_goal = goal.clone();
        let prepared = self
            .blocking
            .run_store("prepare task policy scope", move |store| {
                if store.get_run(&owned_scope_id)?.is_some() {
                    return Ok(None);
                }
                store
                    .prepare_run_for_active_workspace(
                        &owned_scope_id,
                        &owned_conversation_id,
                        &owned_root_message_id,
                        DomainProfile::General,
                        &owned_goal,
                        "agent_task_plan",
                        AttendedMode::Attended,
                    )
                    .map(Some)
            })
            .await
            .map_err(|error| TaskPolicyError::Backend {
                message: format!("Failed to prepare task run before creating task: {error}"),
            })?;
        let Some(mut run) = prepared else {
            return Ok(());
        };
        run.attachments = attachments;
        self.pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(scope_id.to_string())
            .or_insert(PendingInitialRun {
                run,
                trigger: InitialRunTriggerMetadata {
                    source: "task_create".to_string(),
                    kind: "agent_task_plan".to_string(),
                    prompt: goal,
                    priority: 5,
                },
                continuation: None,
            });
        Ok(())
    }

    async fn abort_scope_preparation(&self, scope_id: &str) -> Result<(), TaskPolicyError> {
        self.pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(scope_id);
        Ok(())
    }

    async fn prepare_task(
        &self,
        scope_id: &str,
        draft: &TaskDraft,
        position: usize,
    ) -> Result<PreparedTaskPolicy, TaskPolicyError> {
        let run = self.run(scope_id).await?;
        let kind = draft
            .extension
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .and_then(PlanTaskKind::from_str)
            .ok_or_else(|| TaskPolicyError::Rejected {
                message: format!("task '{}' has no valid kind", draft.id),
            })?;
        let parallel_group = draft
            .extension
            .get("parallel_group")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let agent_role = draft
            .extension
            .get("subagent")
            .or_else(|| draft.extension.get("agent_role"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_subagent_for(run.domain_profile, kind).to_string());
        let execution_target = draft
            .extension
            .get("execution_target")
            .cloned()
            .map(serde_json::from_value::<TaskExecutionTarget>)
            .transpose()
            .map_err(|error| TaskPolicyError::Rejected {
                message: format!("task '{}' has invalid execution_target: {error}", draft.id),
            })?;
        if let Some(target) = execution_target.as_ref() {
            target
                .validate()
                .map_err(|message| TaskPolicyError::Rejected {
                    message: format!("task '{}': {message}", draft.id),
                })?;
            if target.subagent_role != agent_role {
                return Err(TaskPolicyError::Rejected {
                    message: format!(
                        "task '{}' execution_target role '{}' does not match Subagent role '{}'",
                        draft.id, target.subagent_role, agent_role
                    ),
                });
            }
        }
        let extension = serde_json::to_value(EkoTaskExtension {
            kind,
            agent_role,
            domain_profile: run.domain_profile,
            parallel_group,
            execution_target,
            files: string_array_extension(&draft.extension, "files"),
            allowed_tools: string_array_extension(&draft.extension, "allowed_tools"),
            required_artifacts: string_array_extension(&draft.extension, "required_artifacts"),
            execution_checks: string_array_extension(&draft.extension, "execution_checks"),
            acceptance_criteria: string_array_extension(&draft.extension, "acceptance_criteria"),
            sort_order: i64::try_from(position).unwrap_or(i64::MAX),
        })
        .map_err(|error| TaskPolicyError::Backend {
            message: format!("Failed to encode EKO task extension: {error}"),
        })?;
        Ok(PreparedTaskPolicy { extension })
    }

    async fn prepare_initial_context(
        &self,
        scope_id: &str,
        input: &TaskCreateInput,
    ) -> Result<TaskGraphContext, TaskPolicyError> {
        let run = self.run(scope_id).await?;
        let metadata = serde_json::to_value(EkoPlanMetadata {
            plan_id: format!("plan_{}", uuid::Uuid::new_v4().as_simple()),
            domain_profile: run.domain_profile,
            goal_revision: run.goal_revision,
            goal_sha256: run.goal_sha256.clone(),
        })
        .map_err(|error| TaskPolicyError::Backend {
            message: format!("Failed to encode EKO plan metadata: {error}"),
        })?;
        Ok(TaskGraphContext {
            goal: run.goal,
            assumptions: input.assumptions.clone(),
            risks: input.risks.clone(),
            execution_mode: input.execution_mode,
            metadata,
        })
    }

    async fn finalize_task_extension(
        &self,
        _scope_id: &str,
        task_id: &str,
        position: usize,
        mut extension: serde_json::Value,
    ) -> Result<serde_json::Value, TaskPolicyError> {
        if let Some(fields) = extension.as_object_mut()
            && let Some(subagent) = fields.remove("subagent")
        {
            fields.insert("agent_role".to_string(), subagent);
        }
        let mut extension: EkoTaskExtension =
            serde_json::from_value(extension).map_err(|error| TaskPolicyError::Rejected {
                message: format!("task '{task_id}' has invalid EKO extension: {error}"),
            })?;
        extension.sort_order = i64::try_from(position).unwrap_or(i64::MAX);
        serde_json::to_value(extension).map_err(|error| TaskPolicyError::Backend {
            message: format!("Failed to encode EKO task extension: {error}"),
        })
    }

    async fn validate_candidate(
        &self,
        _scope_id: &str,
        tasks: &[echo_agent::tasks::Task],
    ) -> Result<(), TaskPolicyError> {
        for task in tasks {
            self.capabilities
                .validate_task_spec(&task.spec)
                .map_err(|message| TaskPolicyError::Rejected { message })?;
        }
        Ok(())
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

fn string_array_extension(extension: &serde_json::Value, key: &str) -> Vec<String> {
    extension
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build the one revision service used by EKO's framework tools.
pub fn build_eko_task_revision_service(
    store: Arc<TaskRuntimeStore>,
    capabilities: Arc<TaskCapabilityCatalog>,
) -> Arc<TaskRevisionService> {
    let pending_initial_runs = Arc::new(Mutex::new(HashMap::new()));
    Arc::new(TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::with_pending_initial_runs(
            store.clone(),
            pending_initial_runs.clone(),
        )),
        Arc::new(EkoTaskToolPolicy::new(
            store,
            capabilities,
            pending_initial_runs,
        )),
    ))
}

/// Adapt EKO's IPC/file DTO to the framework patch protocol, then return the
/// existing EKO projection expected by GUI/TUI callers.
pub async fn apply_eko_task_update(
    service: &TaskRevisionService,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    request: TaskUpdateRequest,
) -> Result<super::types::PlanRevision, TaskRevisionError> {
    let patch = request
        .to_task_plan_patch()
        .map_err(|message| TaskRevisionError::InvalidInput { message })?;
    service.apply_patch(run_id, patch).await?;
    let owned_run_id = run_id.to_string();
    TaskRuntimeBlockingAdapter::new(store)
        .run_store("load committed task update", move |store| {
            store.get_plan_revision(&owned_run_id)
        })
        .await
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or_else(|| TaskRevisionError::GraphNotFound {
            scope_id: run_id.to_string(),
        })
}

/// Commit a product-prepared initial plan through the same framework revision
/// service used by task tools. Planning policy has already selected the EKO
/// fields; the framework still owns canonical validation, revision 1, and CAS.
pub async fn commit_eko_task_plan(
    store: Arc<TaskRuntimeStore>,
    plan: super::types::TaskPlan,
) -> Result<super::types::TaskPlan, TaskRevisionError> {
    let plan_run_id = plan.run_id.clone();
    let run = TaskRuntimeBlockingAdapter::new(store.clone())
        .run_store("load task plan run", move |store| {
            store.get_run(&plan_run_id)
        })
        .await
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or_else(|| TaskRevisionError::GraphNotFound {
            scope_id: plan.run_id.clone(),
        })?;
    let (run_id, context, tasks) = plan_graph_input(plan, run)?;
    let service = TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::new(store.clone())),
        Arc::new(DefaultTaskToolPolicy::new(run_id.clone())),
    );
    service
        .create_prepared(&run_id, context, tasks, "initial complete plan".to_string())
        .await?;
    load_committed_plan(store, run_id).await
}

/// Framework-validate a fixed direct-answer graph, then atomically persist the
/// graph, structured evidence, task settlement, and run settlement.
pub(crate) async fn commit_eko_direct_completion(
    store: Arc<TaskRuntimeStore>,
    plan: super::types::TaskPlan,
    summary: TaskExecutionSummary,
    task_summary: String,
) -> Result<super::types::TaskPlan, TaskRevisionError> {
    let plan_run_id = plan.run_id.clone();
    let run = TaskRuntimeBlockingAdapter::new(store.clone())
        .run_store("load direct completion run", move |store| {
            store.get_run(&plan_run_id)
        })
        .await
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or_else(|| TaskRevisionError::GraphNotFound {
            scope_id: plan.run_id.clone(),
        })?;
    let (run_id, context, tasks) = plan_graph_input(plan, run)?;
    if summary.run_id != run_id {
        return Err(TaskRevisionError::InvalidInput {
            message: "direct completion summary belongs to another TaskRun".to_string(),
        });
    }
    let pending_direct_completions = Arc::new(Mutex::new(HashMap::from([(
        run_id.clone(),
        PendingDirectCompletion {
            summary,
            task_summary,
        },
    )])));
    let service = TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::with_pending_direct_completion(
            store.clone(),
            pending_direct_completions,
        )),
        Arc::new(DefaultTaskToolPolicy::new(run_id.clone())),
    );
    service
        .create_prepared(&run_id, context, tasks, "initial complete plan".to_string())
        .await?;
    load_committed_plan(store, run_id).await
}

/// Publish a prepared TaskRun and its first framework-validated graph as one
/// visible file generation. EKO owns this local-file transaction; graph
/// validation, revision calculation, and CAS remain framework-owned.
pub(crate) async fn publish_eko_task_plan(
    store: Arc<TaskRuntimeStore>,
    run: TaskRun,
    trigger: InitialRunTriggerMetadata,
    continuation: Option<(bool, bool, Option<u64>, Option<u64>)>,
    plan: super::types::TaskPlan,
) -> Result<super::types::TaskPlan, TaskRevisionError> {
    if plan.run_id != run.run_id {
        return Err(TaskRevisionError::InvalidInput {
            message: format!(
                "TaskPlan run '{}' does not match prepared run '{}'",
                plan.run_id, run.run_id
            ),
        });
    }
    let (run_id, context, tasks) = plan_graph_input(plan, run.clone())?;
    let pending_initial_runs = Arc::new(Mutex::new(HashMap::from([(
        run_id.clone(),
        PendingInitialRun {
            run,
            trigger,
            continuation,
        },
    )])));
    let service = TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::with_pending_initial_runs(
            store.clone(),
            pending_initial_runs.clone(),
        )),
        Arc::new(DefaultTaskToolPolicy::new(run_id.clone())),
    );
    let create_result = service
        .create_prepared(&run_id, context, tasks, "initial complete plan".to_string())
        .await;
    if create_result.is_err() {
        pending_initial_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run_id);
    }
    create_result?;
    load_committed_plan(store, run_id).await
}

fn plan_graph_input(
    plan: super::types::TaskPlan,
    run: TaskRun,
) -> Result<(String, TaskGraphContext, Vec<echo_agent::tasks::Task>), TaskRevisionError> {
    let context_metadata = serde_json::to_value(EkoPlanMetadata {
        plan_id: plan.plan_id,
        domain_profile: plan.domain_profile,
        goal_revision: run.goal_revision,
        goal_sha256: run.goal_sha256.clone(),
    })
    .map_err(|error| TaskRevisionError::InvalidInput {
        message: format!("Failed to encode EKO plan metadata: {error}"),
    })?;
    let context = TaskGraphContext {
        goal: run.goal,
        assumptions: plan.assumptions,
        risks: plan.risks,
        execution_mode: match plan.execution_mode {
            super::types::ExecutionMode::Sequential => {
                echo_agent::tasks::TaskGraphExecutionMode::Sequential
            }
            super::types::ExecutionMode::Parallel => {
                echo_agent::tasks::TaskGraphExecutionMode::Parallel
            }
        },
        metadata: context_metadata,
    };
    let run_id = plan.run_id;
    let tasks = plan
        .tasks
        .iter()
        .map(super::types::PlanTask::to_task)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| TaskRevisionError::InvalidInput { message })?;
    Ok((run_id, context, tasks))
}

async fn load_committed_plan(
    store: Arc<TaskRuntimeStore>,
    run_id: String,
) -> Result<super::types::TaskPlan, TaskRevisionError> {
    let missing_scope = run_id.clone();
    TaskRuntimeBlockingAdapter::new(store)
        .run_store("load committed task plan", move |store| {
            store.get_plan(&run_id)
        })
        .await
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or(TaskRevisionError::GraphNotFound {
            scope_id: missing_scope,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tasks::TaskGraphExecutionMode;

    fn test_capabilities() -> Arc<TaskCapabilityCatalog> {
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        Arc::new(TaskCapabilityCatalog::new(
            Arc::new(
                crate::subagent_loader::SubagentCatalogSnapshot::from_definitions(&definitions),
            ),
            Vec::<String>::new(),
        ))
    }

    fn test_context(run_id: &str) -> ToolContext {
        ToolContext {
            run_id: Some(run_id.to_string()),
            conversation_id: Some(format!("{run_id}-conversation")),
            turn_id: Some(format!("{run_id}-turn")),
            message_id: Some(format!("{run_id}-message")),
            ..ToolContext::default()
        }
    }

    fn task_input(subagent: &str) -> TaskCreateInput {
        TaskCreateInput {
            tasks: vec![TaskDraft {
                id: "task-1".to_string(),
                title: "Inspect runtime".to_string(),
                description: "Inspect the current runtime state".to_string(),
                depends_on: Vec::new(),
                max_retries: 1,
                extension: serde_json::json!({
                    "kind": "investigation",
                    "subagent": subagent,
                    "execution_checks": ["facts captured"],
                    "acceptance_criteria": ["summary is grounded"],
                }),
            }],
            base_revision: None,
            reason: None,
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: TaskGraphExecutionMode::Sequential,
        }
    }

    #[tokio::test]
    async fn failed_initial_graph_validation_rolls_back_staged_run() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        let service = build_eko_task_revision_service(store.clone(), test_capabilities());
        let context = test_context("rollback-run");
        let result = service
            .create_from_tool(task_input("missing-subagent"), &context)
            .await;

        assert!(result.is_err());
        assert!(
            store
                .get_run("rollback-run")
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(!root.join("rollback-run").exists());
        Ok(())
    }

    #[tokio::test]
    async fn direct_completion_rejects_mutating_evidence_without_publishing_plan()
    -> Result<(), String> {
        use super::super::types::{
            ExecutionMode, PlanTask, SubagentEvidenceResult, SubagentRunStatus, SubagentTaskResult,
            SubagentVerificationSource, TaskExecutionSummary, TaskPlan, TaskRunStatus,
        };

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
                .map_err(|error| error.to_string())?,
        );
        let run_id = "direct-mutating-evidence";
        store
            .create_run(
                run_id,
                "workspace",
                "conversation",
                "message",
                DomainProfile::General,
                "answer without side effects",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let task_id = "direct-answer";
        let plan = TaskPlan {
            plan_id: format!("plan:{run_id}"),
            run_id: run_id.to_string(),
            revision: 0,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: super::super::types::task_goal_sha256("answer without side effects"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: task_id.to_string(),
                title: "Direct answer".to_string(),
                description: "answer without side effects".to_string(),
                kind: PlanTaskKind::Summary,
                agent_role: "primary-agent".to_string(),
                domain_profile: DomainProfile::General,
                ..PlanTask::default()
            }],
        };
        let mut result =
            SubagentTaskResult::terminal(SubagentRunStatus::Completed, "done", Vec::new());
        result.evidence.push(SubagentEvidenceResult {
            kind: "file_write".to_string(),
            subject: "src/lib.rs".to_string(),
            outcome: Some("succeeded".to_string()),
            details: "unexpected mutation".to_string(),
            source: SubagentVerificationSource::Observed,
            attributes: serde_json::json!({}),
        });
        let summary = TaskExecutionSummary {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            subagent_name: "primary-agent".to_string(),
            result,
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: chrono::Utc::now(),
        };

        let error = commit_eko_direct_completion(
            store.clone(),
            plan,
            summary,
            "complete answer".to_string(),
        )
        .await
        .err()
        .ok_or_else(|| "mutating direct evidence was accepted".to_string())?;
        assert!(error.to_string().contains("direct completion"));
        assert!(
            store
                .get_plan(run_id)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert_eq!(
            store
                .get_run(run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "direct evidence TaskRun missing".to_string())?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_create_publishes_one_complete_pending_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
                .map_err(|error| error.to_string())?,
        );
        let service = build_eko_task_revision_service(store.clone(), test_capabilities());

        service
            .create_from_tool(task_input("explorer"), &test_context("atomic-run"))
            .await
            .map_err(|error| error.to_string())?;

        let run = store
            .get_run("atomic-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "published run is missing".to_string())?;
        assert_eq!(run.status, super::super::types::TaskRunStatus::Pending);
        let plan = store
            .get_plan("atomic-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "published plan is missing".to_string())?;
        assert_eq!(plan.revision, 1);
        assert_eq!(plan.tasks.len(), 1);
        let events = store
            .list_events("atomic-run", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(events.len(), 3);
        assert_eq!(events.first().map(|event| event.seq), Some(1));
        assert_eq!(events.last().map(|event| event.seq), Some(3));
        assert_eq!(
            events.last().and_then(|event| event.payload.get("kind")),
            Some(&serde_json::json!("trigger_metadata"))
        );
        assert!(!events.iter().any(|event| {
            event.event_type == super::super::types::RuntimeEventKind::RunStatusChanged
        }));
        Ok(())
    }

    #[tokio::test]
    async fn crash_before_initial_rename_is_invisible_and_retryable_after_restart()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store.fail_next_initial_publish_before_rename();
        let service = build_eko_task_revision_service(store.clone(), test_capabilities());
        let result = service
            .create_from_tool(task_input("explorer"), &test_context("crash-run"))
            .await;
        assert!(result.is_err());
        assert!(
            store
                .get_run("crash-run")
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(!root.join("crash-run").exists());
        let staged_before_restart = std::fs::read_dir(&root)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".preparing-"))
            });
        assert!(staged_before_restart);
        drop(service);
        drop(store);

        let restarted = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        let staged_after_restart = std::fs::read_dir(&root)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".preparing-"))
            });
        assert!(!staged_after_restart);
        build_eko_task_revision_service(restarted.clone(), test_capabilities())
            .create_from_tool(task_input("explorer"), &test_context("crash-run"))
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            restarted
                .get_plan("crash-run")
                .map_err(|error| error.to_string())?
                .is_some()
        );
        Ok(())
    }
}
