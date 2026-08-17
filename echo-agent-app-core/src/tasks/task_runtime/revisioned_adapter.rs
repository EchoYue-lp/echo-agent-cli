//! Thin EKO adapters for the framework-owned task revision service.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use echo_agent::tasks::{
    DefaultTaskToolPolicy, PreparedTaskPolicy, RevisionedTaskGraph, RevisionedTaskStore,
    RevisionedTaskStoreError, TaskCreateInput, TaskDraft, TaskGraphCommit, TaskGraphContext,
    TaskPolicyError, TaskRevisionError, TaskRevisionService, TaskToolPolicy,
};
use echo_core::tools::ToolContext;

use super::executor::ExecEvent;
use super::profiles::default_subagent_for;
use super::store::{StoreError, TaskRuntimeStore};
use super::task_tools::{
    TaskCapabilityCatalog, current_run_id, formal_run_id_for_turn, trace_sink_from_tool_context,
};
use super::types::{
    AttendedMode, DomainProfile, EkoPlanMetadata, EkoTaskMetadata, PlanTaskKind, TaskRunStatus,
    TaskUpdateRequest,
};

/// File persistence adapter. It deliberately has no patch or validation
/// logic; those remain authoritative in the framework service.
pub struct EkoRevisionedTaskStore {
    store: Arc<TaskRuntimeStore>,
}

impl EkoRevisionedTaskStore {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl RevisionedTaskStore for EkoRevisionedTaskStore {
    async fn load(
        &self,
        scope_id: &str,
    ) -> Result<Option<RevisionedTaskGraph>, RevisionedTaskStoreError> {
        self.store
            .load_revisioned_task_graph(scope_id)
            .map_err(store_error)
    }

    async fn compare_and_commit(
        &self,
        scope_id: &str,
        commit: TaskGraphCommit,
    ) -> Result<RevisionedTaskGraph, RevisionedTaskStoreError> {
        self.store
            .compare_and_commit_revisioned_task_graph(scope_id, commit)
            .map_err(store_error)
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
/// and metadata round trips. It cannot alter generic task fields.
pub struct EkoTaskToolPolicy {
    store: Arc<TaskRuntimeStore>,
    capabilities: Arc<TaskCapabilityCatalog>,
    staged_scopes: Mutex<HashSet<String>>,
}

impl EkoTaskToolPolicy {
    pub fn new(store: Arc<TaskRuntimeStore>, capabilities: Arc<TaskCapabilityCatalog>) -> Self {
        Self {
            store,
            capabilities,
            staged_scopes: Mutex::new(HashSet::new()),
        }
    }

    fn run(&self, scope_id: &str) -> Result<super::types::TaskRun, TaskPolicyError> {
        self.store
            .get_run(scope_id)
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
        serde_json::Map::from_iter([(
            "parallel_group".to_string(),
            serde_json::json!({ "type": "string" }),
        )])
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
        match self.store.get_run(scope_id) {
            Ok(Some(_)) => {
                self.staged_scopes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(scope_id);
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                return Err(TaskPolicyError::Backend {
                    message: format!("Failed to inspect task run before creating task: {error}"),
                });
            }
        }
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
        let trace_sink = trace_sink_from_tool_context(context).or_else(|| {
            chat_resources
                .as_ref()
                .map(|resources| crate::chat_driver::subagent_trace_sink_for(&resources.sink))
        });
        let goal = task_goal(&first.title, &first.description);
        self.store
            .create_run(
                scope_id,
                "default",
                &conversation_id,
                &root_message_id,
                DomainProfile::General,
                &goal,
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| TaskPolicyError::Backend {
                message: format!("Failed to create task run before creating task: {error}"),
            })?;
        if !attachments.is_empty()
            && let Err(error) = self.store.set_run_attachments(scope_id, &attachments)
        {
            tracing::warn!(run_id = scope_id, %error, "failed to bind attachments to task run");
        }
        if let Err(error) = self.store.transition_run(scope_id, TaskRunStatus::Running) {
            let cleanup = self.store.abort_unpublished_run_creation(scope_id);
            return Err(TaskPolicyError::Backend {
                message: match cleanup {
                    Ok(_) => format!("Failed to start task run before creating task: {error}"),
                    Err(cleanup_error) => format!(
                        "Failed to start task run before creating task: {error}; cleanup failed: {cleanup_error}"
                    ),
                },
            });
        }
        self.staged_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(scope_id.to_string());
        if let Some(sink) = trace_sink {
            sink(ExecEvent::run(
                scope_id.to_string(),
                super::types::RuntimeEventKind::RunStarted,
                serde_json::json!({
                    "goal": goal,
                    "route": "agent_task_plan",
                    "source": "task_create",
                }),
            ));
        }
        Ok(())
    }

    async fn abort_scope_preparation(&self, scope_id: &str) -> Result<(), TaskPolicyError> {
        let was_staged = self
            .staged_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(scope_id);
        if !was_staged {
            return Ok(());
        }
        if self
            .store
            .load_revisioned_task_graph(scope_id)
            .map_err(|error| TaskPolicyError::Backend {
                message: format!("Failed to inspect staged task scope: {error}"),
            })?
            .is_some()
        {
            return Ok(());
        }
        self.store
            .abort_unpublished_run_creation(scope_id)
            .map(|_| ())
            .map_err(|error| TaskPolicyError::Backend {
                message: format!("Failed to roll back staged task scope: {error}"),
            })
    }

    async fn prepare_task(
        &self,
        scope_id: &str,
        draft: &TaskDraft,
        position: usize,
    ) -> Result<PreparedTaskPolicy, TaskPolicyError> {
        let run = self.run(scope_id)?;
        let parallel_group = draft
            .extensions
            .get("parallel_group")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let metadata = serde_json::to_value(EkoTaskMetadata {
            domain_profile: run.domain_profile,
            parallel_group,
            sort_order: i64::try_from(position).unwrap_or(i64::MAX),
        })
        .map_err(|error| TaskPolicyError::Backend {
            message: format!("Failed to encode EKO task metadata: {error}"),
        })?;
        Ok(PreparedTaskPolicy {
            agent_role: draft.subagent.clone().unwrap_or_else(|| {
                default_subagent_for(run.domain_profile, PlanTaskKind::from_task_kind(draft.kind))
                    .to_string()
            }),
            metadata,
        })
    }

    async fn prepare_initial_context(
        &self,
        scope_id: &str,
        input: &TaskCreateInput,
    ) -> Result<TaskGraphContext, TaskPolicyError> {
        let run = self.run(scope_id)?;
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

    async fn finalize_task_metadata(
        &self,
        _scope_id: &str,
        task_id: &str,
        position: usize,
        metadata: serde_json::Value,
    ) -> Result<serde_json::Value, TaskPolicyError> {
        let mut metadata: EkoTaskMetadata =
            serde_json::from_value(metadata).map_err(|error| TaskPolicyError::Rejected {
                message: format!("task '{task_id}' has invalid EKO metadata: {error}"),
            })?;
        metadata.sort_order = i64::try_from(position).unwrap_or(i64::MAX);
        serde_json::to_value(metadata).map_err(|error| TaskPolicyError::Backend {
            message: format!("Failed to encode EKO task metadata: {error}"),
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

/// Build the one revision service used by EKO's framework tools.
pub fn build_eko_task_revision_service(
    store: Arc<TaskRuntimeStore>,
    capabilities: Arc<TaskCapabilityCatalog>,
) -> Arc<TaskRevisionService> {
    Arc::new(TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::new(store.clone())),
        Arc::new(EkoTaskToolPolicy::new(store, capabilities)),
    ))
}

/// Adapt EKO's IPC/file DTO to the framework patch protocol, then return the
/// existing EKO projection expected by GUI/TUI callers.
pub async fn apply_eko_task_update(
    service: &TaskRevisionService,
    store: &TaskRuntimeStore,
    run_id: &str,
    request: TaskUpdateRequest,
) -> Result<super::types::TaskPlan, TaskRevisionError> {
    let patch = request
        .to_task_plan_patch()
        .map_err(|message| TaskRevisionError::InvalidInput { message })?;
    service.apply_patch(run_id, patch).await?;
    store
        .get_plan(run_id)
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
    let run = store
        .get_run(&plan.run_id)
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or_else(|| TaskRevisionError::GraphNotFound {
            scope_id: plan.run_id.clone(),
        })?;
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
        .collect();
    let service = TaskRevisionService::new(
        Arc::new(EkoRevisionedTaskStore::new(store.clone())),
        Arc::new(DefaultTaskToolPolicy::new(run_id.clone())),
    );
    service
        .create_prepared(&run_id, context, tasks, "initial complete plan".to_string())
        .await?;
    store
        .get_plan(&run_id)
        .map_err(|error| TaskRevisionError::Backend {
            message: error.to_string(),
        })?
        .ok_or(TaskRevisionError::GraphNotFound { scope_id: run_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tasks::{TaskGraphExecutionMode, TaskKind};

    #[tokio::test]
    async fn failed_initial_graph_validation_rolls_back_staged_run() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        let service = build_eko_task_revision_service(
            store.clone(),
            Arc::new(TaskCapabilityCatalog::new(
                Arc::new(crate::subagent_loader::SubagentCatalogSnapshot::default()),
                Vec::<String>::new(),
            )),
        );
        let context = ToolContext {
            run_id: Some("rollback-run".to_string()),
            conversation_id: Some("rollback-conversation".to_string()),
            turn_id: Some("rollback-turn".to_string()),
            message_id: Some("rollback-message".to_string()),
            ..ToolContext::default()
        };
        let result = service
            .create_from_tool(
                TaskCreateInput {
                    tasks: vec![TaskDraft {
                        id: "task-1".to_string(),
                        title: "Rejected task".to_string(),
                        description: "Unknown Subagent must fail validation".to_string(),
                        kind: TaskKind::Implementation,
                        subagent: Some("missing-subagent".to_string()),
                        depends_on: Vec::new(),
                        files: Vec::new(),
                        allowed_tools: Vec::new(),
                        required_artifacts: Vec::new(),
                        execution_checks: vec!["cargo test".to_string()],
                        acceptance_criteria: vec!["validation passes".to_string()],
                        max_retries: 1,
                        extensions: serde_json::Value::Null,
                    }],
                    base_revision: None,
                    reason: None,
                    assumptions: Vec::new(),
                    risks: Vec::new(),
                    execution_mode: TaskGraphExecutionMode::Sequential,
                },
                &context,
            )
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
}
