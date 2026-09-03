//! Model-callable Agent collaboration control plane.
//!
//! This is deliberately an application routing service. `AgentRouter` remains the
//! durable Conversation inbox owner, `SubagentControlService` remains the
//! exact TaskRun attempt owner, and `TaskRuntimeStore` remains the event
//! authority. This module only validates discriminated targets, selects the
//! existing owner, and projects bounded tool results.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use echo_agent::agent::AgentHandle;
use echo_agent::memory::{ConversationFilter, ConversationStore};
use echo_agent::tools::{Tool, ToolContext, ToolParameters, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::agent_router::{AgentAddress, AgentDeliveryReceipt, AgentRouter};
use crate::tasks::task_runtime::{
    RuntimeEventKind, RuntimeEventQuery, StoreError, SubagentControlActorSource,
    SubagentControlIdentity, SubagentControlPhase, SubagentControlReceipt, SubagentControlService,
    SubagentGuidanceKind, SubagentRun, SubagentRunSnapshot, TaskPlan, TaskRun, TaskRunStatus,
    TaskRuntimeOperation, TaskRuntimeStore,
};
use crate::workspace::WorkspaceId;
use crate::workspace::registry::WorkspaceRegistry;

const MAX_LIST_LIMIT: usize = 32;
const MAX_WAIT_TARGETS: usize = 8;
const MAX_WAIT_MS: u64 = 30_000;
const WAIT_INITIAL_POLL_MS: u64 = 100;
const WAIT_MAX_POLL_MS: u64 = 500;
const MAX_EVENTS: usize = 32;
const MAX_TEXT_CHARS: usize = 16_000;
const MAX_SUMMARY_CHARS: usize = 800;

pub type DeliveryWake = Arc<dyn Fn(AgentAddress) -> Result<(), String> + Send + Sync>;

/// A persisted Conversation Agent address. The optional generation is an
/// opaque workspace incarnation token; when supplied it is checked against
/// the existing workspace registry before any routing side effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "ConversationTarget")]
pub struct ConversationTarget {
    pub workspace_id: String,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_generation: Option<String>,
}

impl ConversationTarget {
    fn into_address(self) -> crate::agent_router::AgentAddress {
        let workspace = crate::workspace::WorkspaceId::from_raw(self.workspace_id);
        crate::agent_router::AgentAddress::new(workspace, self.conversation_id)
    }
}

/// One exact PlanTask execution attempt. This is intentionally not a
/// conversation address and cannot be used with Conversation operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskSubagentTarget")]
pub struct TaskSubagentTarget {
    pub workspace_id: String,
    pub run_id: String,
    pub task_id: String,
    #[ts(type = "number")]
    pub plan_revision: u64,
    pub execution_id: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_generation: Option<String>,
}

/// Discriminator shared by every model-facing collaboration tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, rename = "AgentTarget")]
pub enum AgentTarget {
    Conversation {
        #[serde(flatten)]
        target: ConversationTarget,
    },
    TaskSubagent {
        #[serde(flatten)]
        target: TaskSubagentTarget,
    },
}

impl AgentTarget {
    fn kind(&self) -> &'static str {
        match self {
            Self::Conversation { .. } => "conversation",
            Self::TaskSubagent { .. } => "task_subagent",
        }
    }

    fn identity_key(&self) -> String {
        match self {
            Self::Conversation { target } => format!(
                "conversation\0{}\0{}\0{}",
                target.workspace_id,
                target.conversation_id,
                target.workspace_generation.as_deref().unwrap_or_default()
            ),
            Self::TaskSubagent { target } => format!(
                "task_subagent\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                target.workspace_id,
                target.run_id,
                target.task_id,
                target.plan_revision,
                target.execution_id,
                target.attempt,
                target.workspace_generation.as_deref().unwrap_or_default()
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AgentListScope")]
pub enum AgentListScope {
    #[default]
    All,
    Conversation,
    TaskSubagent,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentListRequest")]
pub struct AgentListRequest {
    #[serde(default)]
    pub scope: AgentListScope,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Optional collaboration-group filter: only conversations that are the
    /// leader or a member of this group are returned.
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_list_limit")]
    pub limit: usize,
}

fn default_list_limit() -> usize {
    16
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentListEntry")]
pub struct AgentListEntry {
    pub target: AgentTarget,
    pub status: String,
    pub summary: Option<String>,
    pub attempt: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentListResponse")]
pub struct AgentListResponse {
    pub entries: Vec<AgentListEntry>,
    pub count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentMessageRequest")]
pub struct AgentMessageRequest {
    pub target: AgentTarget,
    pub text: String,
    /// Required for TaskSubagent commands; optional Conversation idempotency.
    #[serde(default)]
    pub command_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// `live` sends to the active exact attempt; `next_attempt` queues future
    /// guidance through the existing SubagentControlService boundary.
    #[serde(default)]
    pub delivery: AgentMessageDelivery,
    #[serde(default)]
    pub from: Option<ConversationTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AgentMessageDelivery")]
pub enum AgentMessageDelivery {
    #[default]
    Live,
    NextAttempt,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentInspectResponse")]
pub struct AgentInspectResponse {
    pub target: AgentTarget,
    pub status: String,
    pub phase: Option<String>,
    pub outcome: Option<String>,
    pub summary: Option<String>,
    pub attempt: Option<u32>,
    pub cursor: String,
    pub needs_attention: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentWaitEvent")]
pub struct AgentWaitEvent {
    pub target: AgentTarget,
    pub kind: String,
    pub summary: Option<String>,
    pub cursor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AgentWaitStatus")]
pub enum AgentWaitStatus {
    Changed,
    Timeout,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentWaitRequest")]
pub struct AgentWaitRequest {
    pub targets: Vec<AgentTarget>,
    #[serde(default)]
    pub after_cursor: Option<String>,
    #[serde(default = "default_wait_ms")]
    pub timeout_ms: u64,
}

fn default_wait_ms() -> u64 {
    1_000
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentWaitResponse")]
pub struct AgentWaitResponse {
    pub status: AgentWaitStatus,
    pub events: Vec<AgentWaitEvent>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentInterruptRequest")]
pub struct AgentInterruptRequest {
    pub target: AgentTarget,
    pub reason: String,
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentControlReceipt")]
pub struct AgentControlReceipt {
    pub operation: String,
    pub target: AgentTarget,
    pub status: String,
    pub phase: String,
    pub outcome: Option<String>,
    pub duplicate: bool,
    pub message_id: Option<String>,
    pub command_id: Option<String>,
    pub cursor: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentControlError {
    #[error("invalid Agent control request: {0}")]
    Invalid(String),
    #[error("target kind mismatch: operation '{operation}' requires {expected}, got {actual}")]
    WrongTargetKind {
        operation: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("workspace '{workspace_id}' generation is stale or invalid")]
    WrongWorkspaceGeneration { workspace_id: String },
    #[error("TaskRun '{run_id}' was not found")]
    RunNotFound { run_id: String },
    #[error("TaskRun '{run_id}' plan revision mismatch: expected {expected}, current {current}")]
    WrongRevision {
        run_id: String,
        expected: u64,
        current: u64,
    },
    #[error("Subagent attempt '{execution_id}' is stale or does not match attempt {attempt}")]
    StaleAttempt { execution_id: String, attempt: u32 },
    #[error("cursor is invalid for this target")]
    InvalidCursor,
    #[error("target '{0}' is not available")]
    TargetUnavailable(String),
    #[error("existing idempotency key is bound to different content")]
    DuplicateConflict,
    #[error("TaskRuntime control failed: {0}")]
    Runtime(String),
    #[error("AgentRouter control failed: {0}")]
    Router(String),
}

impl AgentControlError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::WrongTargetKind { .. } => "wrong_target_kind",
            Self::WrongWorkspaceGeneration { .. } => "wrong_workspace_generation",
            Self::RunNotFound { .. } => "run_not_found",
            Self::WrongRevision { .. } => "wrong_revision",
            Self::StaleAttempt { .. } => "stale_attempt",
            Self::InvalidCursor => "invalid_cursor",
            Self::TargetUnavailable(_) => "target_unavailable",
            Self::DuplicateConflict => "duplicate_conflict",
            Self::Runtime(_) => "runtime_error",
            Self::Router(_) => "router_error",
        }
    }
}

fn runtime_control_error(error: StoreError, target: &TaskSubagentTarget) -> AgentControlError {
    match error {
        StoreError::RunNotFound(_) => AgentControlError::RunNotFound {
            run_id: target.run_id.clone(),
        },
        StoreError::PlanNotFound(_) | StoreError::TaskNotFound(_) => {
            AgentControlError::TargetUnavailable(format!(
                "task {} in run {} is unavailable",
                target.task_id, target.run_id
            ))
        }
        StoreError::PlanConflict { current, .. } => AgentControlError::WrongRevision {
            run_id: target.run_id.clone(),
            expected: target.plan_revision,
            current,
        },
        StoreError::InvalidPlan(detail)
            if detail.contains("attempt") || detail.contains("execution") =>
        {
            AgentControlError::StaleAttempt {
                execution_id: target.execution_id.clone(),
                attempt: target.attempt,
            }
        }
        StoreError::InvalidPlan(detail)
            if detail.contains("different command payload")
                || detail.contains("different command")
                || detail.contains("already bound") =>
        {
            AgentControlError::DuplicateConflict
        }
        other => AgentControlError::Runtime(other.to_string()),
    }
}

fn group_brief(group: &crate::agent_router::AgentGroup) -> Value {
    json!({
        "group_id": group.group_id,
        "name": group.name,
        "leader": {
            "workspace_id": group.leader.workspace_id.as_str(),
            "conversation_id": group.leader.conversation_id,
        },
        "members": group.members.iter().map(|member| json!({
            "workspace_id": member.address.workspace_id.as_str(),
            "conversation_id": member.address.conversation_id,
            "subagent_role": member.subagent_role,
            "label": member.label,
        })).collect::<Vec<_>>(),
        "created_at": group.created_at.to_rfc3339(),
        "updated_at": group.updated_at.to_rfc3339(),
    })
}

/// Application operations backing `agent_spawn` / `agent_resume` /
/// `agent_handoff`. Implemented by AppState-backed wiring because these
/// operations open workspace hosts and drive chat runtimes — capabilities
/// `AgentControlService` deliberately does not own (thin-service rule from
/// ADR 0016). Every call returns a bounded JSON value for the model.
pub trait AgentControlAppOps: Send + Sync {
    fn spawn_conversation(
        &self,
        request: AgentSpawnRequest,
    ) -> futures::future::BoxFuture<'static, Result<Value, AgentControlError>>;

    fn resume_target(
        &self,
        request: AgentResumeRequest,
    ) -> futures::future::BoxFuture<'static, Result<Value, AgentControlError>>;

    fn handoff_conversation(
        &self,
        request: AgentHandoffRequest,
    ) -> futures::future::BoxFuture<'static, Result<Value, AgentControlError>>;
}

/// Routing service shared by all model invocations and all surfaces.
#[derive(Clone)]
pub struct AgentControlService {
    router: Arc<AgentRouter>,
    task_runtime: Arc<TaskRuntimeStore>,
    task_runtime_blocking: TaskRuntimeOperation,
    workspace_registry: Arc<WorkspaceRegistry>,
    known_conversations: Arc<std::sync::Mutex<HashSet<AgentAddress>>>,
    delivery_wake: Option<DeliveryWake>,
    conversation_store: Option<Arc<dyn ConversationStore>>,
    conversation_workspace_id: Option<String>,
    /// Optional application operations (spawn/resume/handoff). When absent
    /// those tools fail closed with `app_ops_unavailable`.
    app_ops: Option<Arc<dyn AgentControlAppOps>>,
}

impl AgentControlService {
    pub fn new(
        router: Arc<AgentRouter>,
        task_runtime: Arc<TaskRuntimeStore>,
        workspace_registry: Arc<WorkspaceRegistry>,
    ) -> Self {
        Self {
            router,
            task_runtime: Arc::clone(&task_runtime),
            task_runtime_blocking: TaskRuntimeOperation::new(Arc::clone(&task_runtime)),
            workspace_registry,
            known_conversations: Arc::new(std::sync::Mutex::new(HashSet::new())),
            delivery_wake: None,
            conversation_store: None,
            conversation_workspace_id: None,
            app_ops: None,
        }
    }

    /// Bind the existing application delivery supervisor. The callback is
    /// intentionally a composition boundary: AgentControlService persists the
    /// message through AgentRouter, then asks AppState to wake its sole owner.
    pub fn with_delivery_wake(mut self, wake: DeliveryWake) -> Self {
        self.delivery_wake = Some(wake);
        self
    }

    /// Bind the ConversationStore for this service's workspace. Router target
    /// manifests are an inbox concern; persisted conversations remain owned by
    /// ConversationStore and may legitimately predate their first delivery.
    pub fn with_conversation_store(
        mut self,
        store: Arc<dyn ConversationStore>,
        workspace_id: String,
    ) -> Self {
        self.conversation_store = Some(store);
        self.conversation_workspace_id = Some(workspace_id);
        self
    }

    /// Attach application operations for spawn/resume/handoff.
    pub fn with_app_ops(mut self, ops: Arc<dyn AgentControlAppOps>) -> Self {
        self.app_ops = Some(ops);
        self
    }

    pub fn router(&self) -> Arc<AgentRouter> {
        Arc::clone(&self.router)
    }

    pub fn task_runtime(&self) -> Arc<TaskRuntimeStore> {
        Arc::clone(&self.task_runtime)
    }

    async fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, AgentControlError> {
        let run_id = run_id.to_string();
        self.task_runtime_blocking
            .run_store("read Agent control TaskRun", move |store| {
                store.get_run(&run_id)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    async fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, AgentControlError> {
        let run_id = run_id.to_string();
        self.task_runtime_blocking
            .run_store("read Agent control TaskPlan", move |store| {
                store.get_plan(&run_id)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    async fn query_events(
        &self,
        run_id: &str,
        query: RuntimeEventQuery,
    ) -> Result<Vec<crate::tasks::task_runtime::RuntimeTaskEvent>, AgentControlError> {
        let run_id = run_id.to_string();
        self.task_runtime_blocking
            .run_store("read Agent control events", move |store| {
                store.query_events_bounded(&run_id, query)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    async fn list_subagent_run_snapshots(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<SubagentRunSnapshot>, AgentControlError> {
        let run_id = run_id.to_string();
        self.task_runtime_blocking
            .run_store("read Agent control Subagent runs", move |store| {
                store.list_subagent_run_snapshots(&run_id, limit)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    async fn get_subagent_run_snapshot(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<Option<SubagentRunSnapshot>, AgentControlError> {
        let run_id = run_id.to_string();
        let execution_id = execution_id.to_string();
        self.task_runtime_blocking
            .run_store("read exact Agent control Subagent run", move |store| {
                store.get_subagent_run_snapshot(&run_id, &execution_id)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    async fn list_runs_in(
        &self,
        statuses: Vec<TaskRunStatus>,
    ) -> Result<Vec<TaskRun>, AgentControlError> {
        self.task_runtime_blocking
            .run_store("list Agent control TaskRuns", move |store| {
                store.list_runs_in(&statuses)
            })
            .await
            .map_err(|error| AgentControlError::Runtime(error.to_string()))
    }

    /// Create a new conversation Agent (application-backed).
    pub async fn spawn(&self, request: AgentSpawnRequest) -> Result<Value, AgentControlError> {
        let ops = self
            .app_ops
            .clone()
            .ok_or_else(|| AgentControlError::Runtime("app_ops_unavailable".to_string()))?;
        ops.spawn_conversation(request).await
    }

    /// Wake an idle conversation or resume its paused TaskRun.
    pub async fn resume(&self, request: AgentResumeRequest) -> Result<Value, AgentControlError> {
        let ops = self
            .app_ops
            .clone()
            .ok_or_else(|| AgentControlError::Runtime("app_ops_unavailable".to_string()))?;
        ops.resume_target(request).await
    }

    /// Migrate one conversation to another workspace host.
    pub async fn handoff(&self, request: AgentHandoffRequest) -> Result<Value, AgentControlError> {
        let ops = self
            .app_ops
            .clone()
            .ok_or_else(|| AgentControlError::Runtime("app_ops_unavailable".to_string()))?;
        ops.handoff_conversation(request).await
    }

    /// Collaboration-group CRUD over the persistent router authority.
    pub async fn group(&self, request: AgentGroupToolRequest) -> Result<Value, AgentControlError> {
        match request.action {
            AgentGroupAction::List => {
                let limit = if request.limit == 0 {
                    32
                } else {
                    request.limit.min(64)
                };
                let groups = self
                    .router
                    .list_groups()
                    .await
                    .map_err(|error| AgentControlError::Runtime(error.to_string()))?;
                let truncated = groups.len() > limit;
                let groups: Vec<Value> = groups
                    .into_iter()
                    .take(limit)
                    .map(|group| group_brief(&group))
                    .collect();
                Ok(Value::Object(
                    [
                        ("count".to_string(), Value::from(groups.len())),
                        ("groups".to_string(), Value::Array(groups)),
                        ("truncated".to_string(), Value::Bool(truncated)),
                    ]
                    .into_iter()
                    .collect(),
                ))
            }
            AgentGroupAction::Create => {
                let name = request
                    .name
                    .ok_or_else(|| AgentControlError::Invalid("create requires name".into()))?;
                let leader = request
                    .leader
                    .ok_or_else(|| AgentControlError::Invalid("create requires leader".into()))?;
                let members = request
                    .members
                    .unwrap_or_default()
                    .into_iter()
                    .map(AgentGroupMemberInput::into_member)
                    .collect::<Vec<_>>();
                let group = self
                    .router
                    .create_group(name, leader.into_address(), members)
                    .await
                    .map_err(|error| AgentControlError::Invalid(error.to_string()))?;
                Ok(group_brief(&group))
            }
            AgentGroupAction::Update => {
                let group_id = request
                    .group_id
                    .clone()
                    .ok_or_else(|| AgentControlError::Invalid("update requires group_id".into()))?;
                let name = request
                    .name
                    .ok_or_else(|| AgentControlError::Invalid("update requires name".into()))?;
                let leader = request
                    .leader
                    .ok_or_else(|| AgentControlError::Invalid("update requires leader".into()))?;
                let members = request
                    .members
                    .unwrap_or_default()
                    .into_iter()
                    .map(AgentGroupMemberInput::into_member)
                    .collect::<Vec<_>>();
                let group = self
                    .router
                    .update_group(group_id, name, leader.into_address(), members)
                    .await
                    .map_err(|error| AgentControlError::Invalid(error.to_string()))?;
                Ok(group_brief(&group))
            }
            AgentGroupAction::Delete => {
                let group_id = request
                    .group_id
                    .clone()
                    .ok_or_else(|| AgentControlError::Invalid("delete requires group_id".into()))?;
                let deleted = self
                    .router
                    .delete_group(&group_id)
                    .await
                    .map_err(|error| AgentControlError::Invalid(error.to_string()))?;
                Ok(Value::Object(
                    [
                        ("group_id".to_string(), Value::String(group_id)),
                        ("deleted".to_string(), Value::Bool(deleted)),
                    ]
                    .into_iter()
                    .collect(),
                ))
            }
        }
    }

    pub async fn list(
        &self,
        request: AgentListRequest,
    ) -> Result<AgentListResponse, AgentControlError> {
        let limit = request.limit.clamp(1, MAX_LIST_LIMIT);
        let group_members: Option<std::collections::HashSet<AgentAddress>> =
            match request.group_id.as_deref() {
                Some(group_id) if !group_id.trim().is_empty() => {
                    let group = self
                        .router
                        .list_groups()
                        .await
                        .map_err(|error| AgentControlError::Runtime(error.to_string()))?
                        .into_iter()
                        .find(|group| group.group_id == group_id)
                        .ok_or_else(|| {
                            AgentControlError::Invalid(format!("unknown agent group {group_id}"))
                        })?;
                    let mut members: std::collections::HashSet<AgentAddress> =
                        group.members.iter().map(|m| m.address.clone()).collect();
                    members.insert(group.leader);
                    Some(members)
                }
                _ => None,
            };
        let mut entries = Vec::new();
        let include_conversations = matches!(
            request.scope,
            AgentListScope::All | AgentListScope::Conversation
        );
        if include_conversations {
            let mut known = self
                .known_conversations
                .lock()
                .map_err(|_| {
                    AgentControlError::Runtime("conversation directory unavailable".to_string())
                })?
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let persisted = self
                .router
                .list_targets()
                .await
                .map_err(|error| AgentControlError::Router(error.to_string()))?;
            known.extend(persisted);
            if let (Some(store), Some(workspace_id)) = (
                self.conversation_store.as_ref(),
                self.conversation_workspace_id.as_deref(),
            ) {
                let conversations = store
                    .list_conversations(ConversationFilter {
                        limit: Some(MAX_LIST_LIMIT),
                        ..ConversationFilter::default()
                    })
                    .await
                    .map_err(|error| AgentControlError::Runtime(error.to_string()))?;
                let workspace = WorkspaceId::from_raw(workspace_id.to_string());
                known.extend(conversations.into_iter().map(|conversation| {
                    AgentAddress::new(workspace.clone(), conversation.conversation_id)
                }));
            }
            let mut known = known.into_iter().collect::<Vec<_>>();
            known.sort_by(|left, right| {
                left.workspace_id
                    .as_str()
                    .cmp(right.workspace_id.as_str())
                    .then_with(|| left.conversation_id.cmp(&right.conversation_id))
            });
            for address in known {
                if request
                    .workspace_id
                    .as_deref()
                    .is_some_and(|workspace| workspace != address.workspace_id.as_str())
                {
                    continue;
                }
                if group_members
                    .as_ref()
                    .is_some_and(|members| !members.contains(&address))
                {
                    continue;
                }
                let target = AgentTarget::Conversation {
                    target: ConversationTarget {
                        workspace_id: address.workspace_id.to_string(),
                        conversation_id: address.conversation_id.clone(),
                        workspace_generation: self
                            .workspace_generation(address.workspace_id.as_str())
                            .await,
                    },
                };
                if let AgentTarget::Conversation { target } = &target
                    && target.workspace_generation.is_none()
                {
                    continue;
                }
                match self.require_readable_target(&target).await {
                    Ok(()) => {}
                    Err(AgentControlError::TargetUnavailable(_)) => continue,
                    Err(error) => return Err(error),
                }
                let (status, summary, cursor) = self.inspect_conversation(&target).await?;
                if request
                    .status
                    .as_deref()
                    .is_some_and(|wanted| wanted != status)
                {
                    continue;
                }
                entries.push(AgentListEntry {
                    target: target.clone(),
                    status,
                    summary,
                    attempt: None,
                    cursor: Some(cursor),
                });
                if entries.len() >= limit {
                    return Ok(AgentListResponse {
                        count: entries.len(),
                        entries,
                        truncated: true,
                    });
                }
            }
        }

        if matches!(
            request.scope,
            AgentListScope::All | AgentListScope::TaskSubagent
        ) {
            let statuses = [
                TaskRunStatus::Pending,
                TaskRunStatus::Running,
                TaskRunStatus::Paused,
                TaskRunStatus::Cancelled,
                TaskRunStatus::Failed,
                TaskRunStatus::Completed,
            ];
            let runs = self.list_runs_in(statuses.to_vec()).await?;
            for run in runs {
                if request
                    .workspace_id
                    .as_deref()
                    .is_some_and(|workspace| workspace != run.workspace_id)
                {
                    continue;
                }
                let remaining = limit.saturating_sub(entries.len());
                for snapshot in self
                    .list_subagent_run_snapshots(&run.run_id, remaining)
                    .await?
                {
                    let subagent = snapshot.run;
                    // A retry can outlive a later plan revision. The bounded
                    // snapshot keeps the revision recorded on this exact
                    // Assigned boundary.
                    let Some(plan_revision) = snapshot.plan_revision else {
                        continue;
                    };
                    let target = AgentTarget::TaskSubagent {
                        target: TaskSubagentTarget {
                            workspace_id: run.workspace_id.clone(),
                            run_id: subagent.run_id.clone(),
                            task_id: subagent.task_id.clone(),
                            plan_revision,
                            execution_id: subagent.subagent_run_id.clone(),
                            attempt: subagent.attempt,
                            workspace_generation: self
                                .workspace_generation(&run.workspace_id)
                                .await,
                        },
                    };
                    if let AgentTarget::TaskSubagent { target } = &target
                        && target.workspace_generation.is_none()
                    {
                        continue;
                    }
                    let status = subagent.status.as_str().to_string();
                    if request
                        .status
                        .as_deref()
                        .is_some_and(|wanted| wanted != status)
                    {
                        continue;
                    }
                    let sequence = snapshot.latest_event.seq;
                    let cursor = self.cursor_token(&target, sequence);
                    entries.push(AgentListEntry {
                        target,
                        status,
                        summary: subagent
                            .outcome
                            .as_ref()
                            .map(|result| bounded_text(&result.summary, MAX_SUMMARY_CHARS)),
                        attempt: Some(subagent.attempt),
                        cursor: Some(cursor),
                    });
                    if entries.len() >= limit {
                        return Ok(AgentListResponse {
                            count: entries.len(),
                            entries,
                            truncated: true,
                        });
                    }
                }
            }
        }

        let truncated = entries.len() >= limit;
        Ok(AgentListResponse {
            count: entries.len(),
            entries,
            truncated,
        })
    }

    pub async fn inspect(
        &self,
        target: AgentTarget,
    ) -> Result<AgentInspectResponse, AgentControlError> {
        self.validate_target(&target).await?;
        self.require_readable_target(&target).await?;
        match &target {
            AgentTarget::Conversation { .. } => {
                let (status, summary, cursor) = self.inspect_conversation(&target).await?;
                Ok(AgentInspectResponse {
                    target: target.clone(),
                    status: status.clone(),
                    phase: Some(status),
                    outcome: None,
                    summary,
                    attempt: None,
                    cursor,
                    needs_attention: false,
                })
            }
            AgentTarget::TaskSubagent { target: task } => {
                // Inspection is valid for a settled attempt as well as a live
                // one; exact_subagent below enforces identity without
                // requiring Running status.
                self.validate_observable_task_target(task).await?;
                let snapshot = self.exact_subagent_snapshot(task).await?;
                let subagent = snapshot.run;
                let latest = snapshot.latest_event;
                Ok(AgentInspectResponse {
                    target: target.clone(),
                    status: subagent.status.as_str().to_string(),
                    phase: Some(latest.event_type.as_str().to_string()),
                    outcome: subagent
                        .outcome
                        .as_ref()
                        .map(|result| result.status.as_str().to_string()),
                    summary: subagent
                        .outcome
                        .as_ref()
                        .map(|result| bounded_text(&result.summary, MAX_SUMMARY_CHARS)),
                    attempt: Some(subagent.attempt),
                    cursor: self.cursor_token(
                        &AgentTarget::TaskSubagent {
                            target: task.clone(),
                        },
                        latest.seq,
                    ),
                    needs_attention: latest.event_type.is_attention_event(),
                })
            }
        }
    }

    pub async fn message(
        &self,
        request: AgentMessageRequest,
    ) -> Result<AgentControlReceipt, AgentControlError> {
        self.message_inner(request, false).await
    }

    async fn message_inner(
        &self,
        request: AgentMessageRequest,
        wake_delivery: bool,
    ) -> Result<AgentControlReceipt, AgentControlError> {
        self.validate_text(&request.text)?;
        self.validate_target(&request.target).await?;
        if let Some(source) = request.from.as_ref() {
            self.validate_target(&AgentTarget::Conversation {
                target: source.clone(),
            })
            .await?;
        }
        match request.target.clone() {
            AgentTarget::Conversation { target } => {
                if request.delivery != AgentMessageDelivery::Live {
                    return Err(AgentControlError::Invalid(
                        "next_attempt delivery is only valid for TaskSubagentTarget".to_string(),
                    ));
                }
                // AgentRouter enqueue creates an inbox on demand. Guard the
                // model-facing path with the persisted ConversationStore so a
                // typo cannot create a phantom conversation; explicit
                // conversation creation remains owned by AppState.
                self.require_readable_target(&AgentTarget::Conversation {
                    target: target.clone(),
                })
                .await?;
                let address = self.conversation_address(&target)?;
                let from = request
                    .from
                    .as_ref()
                    .map(|source| self.conversation_address(source))
                    .transpose()?;
                if let Some(source) = request.from.as_ref() {
                    self.require_readable_target(&AgentTarget::Conversation {
                        target: source.clone(),
                    })
                    .await?;
                }
                let mut message = crate::agent_router::AgentMessage::agent_text(
                    from,
                    address.clone(),
                    request.text,
                );
                if let Some(message_id) = request.message_id {
                    message.message_id = self.validate_id(&message_id, "message_id")?;
                }
                message.correlation_id = request.correlation_id;
                let receipt = self
                    .router
                    .enqueue(message)
                    .await
                    .map_err(|error| match error {
                        crate::agent_router::AgentRouterError::IdCollision { .. } => {
                            AgentControlError::DuplicateConflict
                        }
                        other => AgentControlError::Router(other.to_string()),
                    })?;
                if wake_delivery && let Some(wake) = self.delivery_wake.as_ref() {
                    wake(address.clone()).map_err(AgentControlError::Router)?;
                }
                self.remember_conversation(address);
                Ok(conversation_receipt("message", target, receipt, self).await?)
            }
            AgentTarget::TaskSubagent { target } => {
                let command_id = request.command_id.as_deref().ok_or_else(|| {
                    AgentControlError::Invalid(
                        "command_id is required for TaskSubagentTarget".to_string(),
                    )
                })?;
                let command_id = self.validate_id(command_id, "command_id")?;
                let identity = SubagentControlIdentity {
                    run_id: target.run_id.clone(),
                    task_id: target.task_id.clone(),
                    execution_id: target.execution_id.clone(),
                    plan_revision: target.plan_revision,
                    attempt: target.attempt,
                    command_id,
                };
                let task_runtime = self.runtime_for_target(&target)?;
                let control = SubagentControlService::new(task_runtime);
                let guidance_kind = match request.delivery {
                    AgentMessageDelivery::Live => SubagentGuidanceKind::LiveMessage,
                    AgentMessageDelivery::NextAttempt => SubagentGuidanceKind::NextAttempt,
                };
                if let Some(existing) = control
                    .existing_guidance_receipt_async(
                        identity.clone(),
                        guidance_kind,
                        request.text.clone(),
                    )
                    .await
                    .map_err(|error| runtime_control_error(error, &target))?
                {
                    return Ok(subagent_receipt("message", target, existing));
                }
                if request.delivery == AgentMessageDelivery::Live {
                    self.validate_task_target(&target, true, true).await?;
                } else if target.execution_id
                    != format!(
                        "pending:{}:{}:{}:{}",
                        target.run_id, target.task_id, target.plan_revision, target.attempt
                    )
                {
                    return Err(AgentControlError::StaleAttempt {
                        execution_id: target.execution_id.clone(),
                        attempt: target.attempt,
                    });
                }
                let receipt = match request.delivery {
                    AgentMessageDelivery::Live => control
                        .send_message(identity, &request.text, SubagentControlActorSource::Cli)
                        .await
                        .map_err(|error| runtime_control_error(error, &target))?,
                    AgentMessageDelivery::NextAttempt => control
                        .queue_guidance_async(
                            identity,
                            request.text,
                            SubagentControlActorSource::Cli,
                        )
                        .await
                        .map_err(|error| runtime_control_error(error, &target))?,
                };
                Ok(subagent_receipt("message", target, receipt))
            }
        }
    }

    pub async fn followup(
        &self,
        request: AgentMessageRequest,
    ) -> Result<AgentControlReceipt, AgentControlError> {
        if !matches!(request.target, AgentTarget::Conversation { .. }) {
            return Err(AgentControlError::WrongTargetKind {
                operation: "agent_followup",
                expected: "conversation",
                actual: request.target.kind(),
            });
        }
        let mut receipt = self.message_inner(request, true).await?;
        receipt.operation = "followup".to_string();
        Ok(receipt)
    }

    pub async fn interrupt(
        &self,
        request: AgentInterruptRequest,
    ) -> Result<AgentControlReceipt, AgentControlError> {
        self.validate_text(&request.reason)?;
        let AgentTarget::TaskSubagent { target } = request.target.clone() else {
            return Err(AgentControlError::WrongTargetKind {
                operation: "agent_interrupt",
                expected: "task_subagent",
                actual: request.target.kind(),
            });
        };
        let identity = SubagentControlIdentity {
            run_id: target.run_id.clone(),
            task_id: target.task_id.clone(),
            execution_id: target.execution_id.clone(),
            plan_revision: target.plan_revision,
            attempt: target.attempt,
            command_id: self.validate_id(&request.command_id, "command_id")?,
        };
        let task_runtime = self.runtime_for_target(&target)?;
        let control = SubagentControlService::new(task_runtime);
        if let Some(existing) = control
            .existing_command_receipt_async(identity.clone())
            .await
            .map_err(|error| runtime_control_error(error, &target))?
        {
            return Ok(subagent_receipt("interrupt", target, existing));
        }
        self.validate_task_target(&target, true, true).await?;
        let receipt = control
            .interrupt_subagent(identity, SubagentControlActorSource::Cli)
            .await
            .map_err(|error| runtime_control_error(error, &target))?;
        Ok(subagent_receipt("interrupt", target, receipt))
    }

    pub async fn wait(
        &self,
        request: AgentWaitRequest,
        cancel: Option<Arc<tokio_util::sync::CancellationToken>>,
    ) -> Result<AgentWaitResponse, AgentControlError> {
        if request.targets.is_empty() || request.targets.len() > MAX_WAIT_TARGETS {
            return Err(AgentControlError::Invalid(format!(
                "agent_wait requires 1-{MAX_WAIT_TARGETS} targets"
            )));
        }
        for target in &request.targets {
            self.validate_target(target).await?;
            if let AgentTarget::TaskSubagent { target } = target {
                self.validate_observable_task_target(target).await?;
                self.exact_subagent(target).await?;
            }
            self.require_readable_target(target).await?;
            self.remember_target(target);
        }
        let timeout = Duration::from_millis(request.timeout_ms.min(MAX_WAIT_MS));
        let mut cursor_by_target =
            self.parse_wait_cursors(&request.targets, request.after_cursor.as_deref())?;
        let mut poll_ms = WAIT_INITIAL_POLL_MS;
        let started = tokio::time::Instant::now();
        loop {
            if cancel.as_ref().is_some_and(|token| token.is_cancelled()) {
                return Ok(AgentWaitResponse {
                    status: AgentWaitStatus::Cancelled,
                    events: Vec::new(),
                    next_cursor: Some(self.next_wait_cursor(&request.targets, &cursor_by_target)),
                });
            }
            let mut events = Vec::new();
            for target in &request.targets {
                let after = cursor_by_target
                    .get(&self.cursor_prefix(target))
                    .copied()
                    .unwrap_or(0);
                if let Some(event) = self.observe_change(target, after).await? {
                    if let Some(sequence) = Self::cursor_sequence(&event.cursor) {
                        cursor_by_target.insert(self.cursor_prefix(target), sequence);
                    }
                    events.push(event);
                    if events.len() >= MAX_EVENTS {
                        break;
                    }
                }
            }
            if !events.is_empty() {
                let next_cursor = Some(self.next_wait_cursor(&request.targets, &cursor_by_target));
                return Ok(AgentWaitResponse {
                    status: AgentWaitStatus::Changed,
                    events,
                    next_cursor,
                });
            }
            if started.elapsed() >= timeout {
                return Ok(AgentWaitResponse {
                    status: AgentWaitStatus::Timeout,
                    events: Vec::new(),
                    next_cursor: Some(self.next_wait_cursor(&request.targets, &cursor_by_target)),
                });
            }
            if let Some(token) = cancel.as_ref() {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(poll_ms)) => {}
                    _ = token.cancelled() => {
                        return Ok(AgentWaitResponse {
                            status: AgentWaitStatus::Cancelled,
                            events: Vec::new(),
                            next_cursor: Some(self.next_wait_cursor(
                                &request.targets,
                                &cursor_by_target,
                            )),
                        });
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_millis(poll_ms)).await;
            }
            poll_ms = poll_ms.saturating_mul(2).min(WAIT_MAX_POLL_MS);
        }
    }

    async fn observe_change(
        &self,
        target: &AgentTarget,
        after: i64,
    ) -> Result<Option<AgentWaitEvent>, AgentControlError> {
        match target {
            AgentTarget::Conversation { .. } => {
                let cursor = self.conversation_cursor(target).await?;
                if cursor <= after {
                    return Ok(None);
                }
                let (status, summary, _) = self.inspect_conversation(target).await?;
                Ok(Some(AgentWaitEvent {
                    target: target.clone(),
                    kind: if status == "turn_settled" {
                        "terminal".to_string()
                    } else {
                        "mailbox_changed".to_string()
                    },
                    summary,
                    cursor: self.cursor_token(target, cursor),
                }))
            }
            AgentTarget::TaskSubagent { target: task } => {
                let event = self
                    .query_events(
                        &task.run_id,
                        RuntimeEventQuery::new(after, 1)
                            .for_execution(task.execution_id.clone())
                            .with_event_types(subagent_wait_event_types()),
                    )
                    .await?
                    .into_iter()
                    .next();
                Ok(event.map(|event| AgentWaitEvent {
                    target: target.clone(),
                    kind: event.event_type.as_str().to_string(),
                    summary: event
                        .payload
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(|text| bounded_text(text, MAX_SUMMARY_CHARS)),
                    cursor: self.cursor_token(target, event.seq),
                }))
            }
        }
    }

    async fn validate_target(&self, target: &AgentTarget) -> Result<(), AgentControlError> {
        match target {
            AgentTarget::Conversation { target } => {
                self.conversation_address(target)?;
                self.validate_workspace_generation(
                    &target.workspace_id,
                    target.workspace_generation.as_deref(),
                )
                .await?;
                Ok(())
            }
            AgentTarget::TaskSubagent { target } => {
                self.validate_observable_task_target(target).await
            }
        }
    }

    async fn validate_observable_task_target(
        &self,
        target: &TaskSubagentTarget,
    ) -> Result<(), AgentControlError> {
        self.validate_task_target_impl(target, false, false, true)
            .await
    }

    async fn require_readable_target(&self, target: &AgentTarget) -> Result<(), AgentControlError> {
        let AgentTarget::Conversation { target } = target else {
            return Ok(());
        };
        let address = self.conversation_address(target)?;
        if let (Some(store), Some(workspace_id)) = (
            self.conversation_store.as_ref(),
            self.conversation_workspace_id.as_deref(),
        ) {
            if workspace_id != address.workspace_id.as_str() {
                return Err(AgentControlError::TargetUnavailable(format!(
                    "conversation {}/{}",
                    target.workspace_id, target.conversation_id
                )));
            }
            let conversation = store
                .get_conversation(&address.conversation_id)
                .await
                .map_err(|error| AgentControlError::Runtime(error.to_string()))?;
            if conversation.is_some() {
                return Ok(());
            }
            Err(AgentControlError::TargetUnavailable(format!(
                "conversation {}/{}",
                target.workspace_id, target.conversation_id
            )))
        } else {
            Err(AgentControlError::TargetUnavailable(format!(
                "ConversationStore is unavailable for workspace {}",
                target.workspace_id
            )))
        }
    }

    async fn validate_task_target(
        &self,
        target: &TaskSubagentTarget,
        require_active: bool,
        reject_terminal: bool,
    ) -> Result<(), AgentControlError> {
        self.validate_task_target_impl(target, require_active, reject_terminal, false)
            .await
    }

    async fn validate_task_target_impl(
        &self,
        target: &TaskSubagentTarget,
        require_active: bool,
        reject_terminal: bool,
        allow_historical_revision: bool,
    ) -> Result<(), AgentControlError> {
        for (field, value) in [
            ("workspace_id", target.workspace_id.as_str()),
            ("run_id", target.run_id.as_str()),
            ("task_id", target.task_id.as_str()),
            ("execution_id", target.execution_id.as_str()),
        ] {
            if value.trim().is_empty() || value.contains('\0') {
                return Err(AgentControlError::Invalid(format!(
                    "{field} must contain non-empty text without NUL"
                )));
            }
        }
        if target.plan_revision == 0 || target.attempt == 0 {
            return Err(AgentControlError::Invalid(
                "plan_revision and attempt must be positive".to_string(),
            ));
        }
        // A service is bound to one immutable workspace TaskRuntime. Reject
        // foreign targets before touching that store, so a same-named run in a
        // different workspace can never be mistaken for this target.
        self.runtime_for_target(target)?;
        let run =
            self.get_run(&target.run_id)
                .await?
                .ok_or_else(|| AgentControlError::RunNotFound {
                    run_id: target.run_id.clone(),
                })?;
        self.validate_workspace_generation(
            &run.workspace_id,
            target.workspace_generation.as_deref(),
        )
        .await?;
        if run.workspace_id != target.workspace_id {
            return Err(AgentControlError::TargetUnavailable(format!(
                "TaskRun {} belongs to workspace {}, not {}",
                target.run_id, run.workspace_id, target.workspace_id
            )));
        }
        if reject_terminal
            && matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            )
        {
            return Err(AgentControlError::TargetUnavailable(format!(
                "TaskRun {} is terminal",
                target.run_id
            )));
        }
        let plan = self.get_plan(&target.run_id).await?.ok_or_else(|| {
            AgentControlError::TargetUnavailable(format!("plan for {}", target.run_id))
        })?;
        if !allow_historical_revision && plan.revision != target.plan_revision {
            return Err(AgentControlError::WrongRevision {
                run_id: target.run_id.clone(),
                expected: target.plan_revision,
                current: plan.revision,
            });
        }
        if !allow_historical_revision && !plan.tasks.iter().any(|task| task.id == target.task_id) {
            return Err(AgentControlError::TargetUnavailable(format!(
                "task {} in run {}",
                target.task_id, target.run_id
            )));
        }
        if require_active {
            let subagent = self.exact_subagent(target).await?;
            if subagent.status != crate::tasks::task_runtime::SubagentStatus::Running {
                return Err(AgentControlError::StaleAttempt {
                    execution_id: target.execution_id.clone(),
                    attempt: target.attempt,
                });
            }
        }
        Ok(())
    }

    fn runtime_for_target(
        &self,
        target: &TaskSubagentTarget,
    ) -> Result<Arc<TaskRuntimeStore>, AgentControlError> {
        let runtime = Arc::clone(&self.task_runtime);
        if runtime.active_workspace_id() == target.workspace_id {
            Ok(runtime)
        } else {
            Err(AgentControlError::TargetUnavailable(format!(
                "TaskRuntime for workspace {} is not bound to this Agent",
                target.workspace_id
            )))
        }
    }

    async fn exact_subagent(
        &self,
        target: &TaskSubagentTarget,
    ) -> Result<SubagentRun, AgentControlError> {
        self.exact_subagent_snapshot(target)
            .await
            .map(|snapshot| snapshot.run)
    }

    async fn exact_subagent_snapshot(
        &self,
        target: &TaskSubagentTarget,
    ) -> Result<SubagentRunSnapshot, AgentControlError> {
        let Some(snapshot) = self
            .get_subagent_run_snapshot(&target.run_id, &target.execution_id)
            .await?
        else {
            return Err(AgentControlError::StaleAttempt {
                execution_id: target.execution_id.clone(),
                attempt: target.attempt,
            });
        };
        let run = &snapshot.run;
        if run.task_id != target.task_id || run.attempt != target.attempt {
            return Err(AgentControlError::StaleAttempt {
                execution_id: target.execution_id.clone(),
                attempt: target.attempt,
            });
        }
        Ok(snapshot)
    }

    fn conversation_address(
        &self,
        target: &ConversationTarget,
    ) -> Result<AgentAddress, AgentControlError> {
        if target.workspace_id.trim().is_empty() || target.conversation_id.trim().is_empty() {
            return Err(AgentControlError::Invalid(
                "workspace_id and conversation_id must not be empty".to_string(),
            ));
        }
        if target.workspace_id.contains('\0') || target.conversation_id.contains('\0') {
            return Err(AgentControlError::Invalid(
                "workspace_id and conversation_id must not contain NUL".to_string(),
            ));
        }
        let workspace = WorkspaceId::from_raw(target.workspace_id.clone());
        if workspace.as_str() != target.workspace_id {
            return Err(AgentControlError::Invalid(
                "workspace_id contains unsafe path characters".to_string(),
            ));
        }
        Ok(AgentAddress::new(workspace, target.conversation_id.clone()))
    }

    async fn validate_workspace_generation(
        &self,
        workspace_id: &str,
        generation: Option<&str>,
    ) -> Result<(), AgentControlError> {
        let Some(generation) = generation else {
            return Ok(());
        };
        if workspace_id == "global" {
            if generation == "global" {
                return Ok(());
            }
            return Err(AgentControlError::WrongWorkspaceGeneration {
                workspace_id: workspace_id.to_string(),
            });
        }
        let registry = Arc::clone(&self.workspace_registry);
        let workspace_id_owned = workspace_id.to_string();
        let current = tokio::task::spawn_blocking(move || {
            registry
                .inspect(&WorkspaceId::from_raw(workspace_id_owned))
                .ok()
                .map(|workspace| workspace.opaque_product_data_generation())
        })
        .await
        .ok()
        .flatten();
        if current.as_deref() != Some(generation) {
            return Err(AgentControlError::WrongWorkspaceGeneration {
                workspace_id: workspace_id.to_string(),
            });
        }
        Ok(())
    }

    async fn workspace_generation(&self, workspace_id: &str) -> Option<String> {
        if workspace_id == "global" {
            return Some("global".to_string());
        }
        let registry = Arc::clone(&self.workspace_registry);
        let workspace_id = workspace_id.to_string();
        tokio::task::spawn_blocking(move || {
            registry
                .inspect(&WorkspaceId::from_raw(workspace_id))
                .ok()
                .map(|workspace| workspace.opaque_product_data_generation())
        })
        .await
        .ok()
        .flatten()
    }

    fn validate_text(&self, text: &str) -> Result<(), AgentControlError> {
        if text.trim().is_empty() {
            return Err(AgentControlError::Invalid(
                "text/reason must not be empty".to_string(),
            ));
        }
        if text.contains('\0') {
            return Err(AgentControlError::Invalid(
                "text must not contain NUL characters".to_string(),
            ));
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(AgentControlError::Invalid(format!(
                "text exceeds {MAX_TEXT_CHARS} characters"
            )));
        }
        Ok(())
    }

    fn validate_id(&self, value: &str, field: &str) -> Result<String, AgentControlError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 128 || value.contains('\0') {
            return Err(AgentControlError::Invalid(format!(
                "{field} must contain 1-128 characters"
            )));
        }
        Ok(value.to_string())
    }

    fn remember_conversation(&self, address: AgentAddress) {
        if let Ok(mut known) = self.known_conversations.lock() {
            known.insert(address);
        }
    }

    fn remember_target(&self, target: &AgentTarget) {
        if let AgentTarget::Conversation {
            target: conversation,
        } = target
            && let Ok(address) = self.conversation_address(conversation)
        {
            self.remember_conversation(address);
        }
    }

    async fn inspect_conversation(
        &self,
        target: &AgentTarget,
    ) -> Result<(String, Option<String>, String), AgentControlError> {
        let AgentTarget::Conversation {
            target: conversation,
        } = target
        else {
            return Err(AgentControlError::WrongTargetKind {
                operation: "agent_inspect",
                expected: "conversation",
                actual: target.kind(),
            });
        };
        let address = self.conversation_address(conversation)?;
        let records = self
            .router
            .records(&address)
            .await
            .map_err(|error| AgentControlError::Router(error.to_string()))?;
        let cursor = self.conversation_cursor(target).await?;
        let latest = records.last();
        let status = latest
            .map(|record| record.phase.as_str().to_string())
            .unwrap_or_else(|| "idle".to_string());
        let summary = latest
            .and_then(|record| record.reason.clone())
            .map(|text| bounded_text(&text, MAX_SUMMARY_CHARS));
        Ok((status, summary, self.cursor_token(target, cursor)))
    }

    async fn conversation_cursor(&self, target: &AgentTarget) -> Result<i64, AgentControlError> {
        let AgentTarget::Conversation {
            target: conversation,
        } = target
        else {
            return Err(AgentControlError::WrongTargetKind {
                operation: "agent_wait",
                expected: "conversation",
                actual: target.kind(),
            });
        };
        let address = self.conversation_address(conversation)?;
        self.router
            .event_cursor(&address)
            .await
            .map(|cursor| i64::try_from(cursor).unwrap_or(i64::MAX))
            .map_err(|error| AgentControlError::Router(error.to_string()))
    }

    fn cursor_token(&self, target: &AgentTarget, sequence: i64) -> String {
        format!("{}:{}", self.cursor_prefix(target), sequence.max(0))
    }

    fn cursor_prefix(&self, target: &AgentTarget) -> String {
        let hash = hex::encode(Sha256::digest(target.identity_key().as_bytes()));
        hash.chars().take(24).collect()
    }

    fn cursor_sequence(cursor: &str) -> Option<i64> {
        cursor
            .rsplit_once(':')?
            .1
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
    }

    fn parse_wait_cursors(
        &self,
        targets: &[AgentTarget],
        cursor: Option<&str>,
    ) -> Result<HashMap<String, i64>, AgentControlError> {
        let mut parsed = HashMap::new();
        let expected = targets
            .iter()
            .map(|target| self.cursor_prefix(target))
            .collect::<HashSet<_>>();
        let Some(cursor) = cursor else {
            return Ok(parsed);
        };
        if targets.len() == 1 {
            let sequence = self.parse_cursor(
                targets.first().ok_or(AgentControlError::InvalidCursor)?,
                Some(cursor),
            )?;
            if let Some(prefix) = expected.iter().next() {
                parsed.insert(prefix.clone(), sequence);
            }
            return Ok(parsed);
        }
        if let Ok(map) = serde_json::from_str::<HashMap<String, i64>>(cursor) {
            if map.len() > targets.len()
                || map
                    .iter()
                    .any(|(prefix, sequence)| !expected.contains(prefix) || *sequence < 0)
            {
                return Err(AgentControlError::InvalidCursor);
            }
            return Ok(map);
        }
        let (prefix, sequence) = cursor
            .rsplit_once(':')
            .ok_or(AgentControlError::InvalidCursor)?;
        let sequence = sequence
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
            .ok_or(AgentControlError::InvalidCursor)?;
        if !expected.contains(prefix) {
            return Err(AgentControlError::InvalidCursor);
        }
        parsed.insert(prefix.to_string(), sequence);
        Ok(parsed)
    }

    fn next_wait_cursor(&self, targets: &[AgentTarget], cursors: &HashMap<String, i64>) -> String {
        if targets.len() == 1 {
            let sequence = targets
                .first()
                .and_then(|target| cursors.get(&self.cursor_prefix(target)))
                .copied()
                .unwrap_or(0);
            return targets
                .first()
                .map(|target| self.cursor_token(target, sequence))
                .unwrap_or_else(|| "0".to_string());
        }
        let mut ordered = std::collections::BTreeMap::new();
        for target in targets {
            ordered.insert(
                self.cursor_prefix(target),
                cursors
                    .get(&self.cursor_prefix(target))
                    .copied()
                    .unwrap_or(0),
            );
        }
        serde_json::to_string(&ordered).unwrap_or_else(|_| "{}".to_string())
    }

    fn parse_cursor(
        &self,
        target: &AgentTarget,
        cursor: Option<&str>,
    ) -> Result<i64, AgentControlError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        let (prefix, sequence) = cursor
            .rsplit_once(':')
            .ok_or(AgentControlError::InvalidCursor)?;
        let expected_prefix = self
            .cursor_token(target, 0)
            .rsplit_once(':')
            .map(|(prefix, _)| prefix.to_string())
            .ok_or(AgentControlError::InvalidCursor)?;
        if prefix != expected_prefix {
            return Err(AgentControlError::InvalidCursor);
        }
        sequence
            .parse::<i64>()
            .ok()
            .filter(|sequence| *sequence >= 0)
            .ok_or(AgentControlError::InvalidCursor)
    }
}

async fn conversation_receipt(
    operation: &str,
    target: ConversationTarget,
    receipt: AgentDeliveryReceipt,
    service: &AgentControlService,
) -> Result<AgentControlReceipt, AgentControlError> {
    let cursor = service
        .router
        .event_cursor(&receipt.target)
        .await
        .map_err(|error| AgentControlError::Router(error.to_string()))?;
    let cursor = service.cursor_token(
        &AgentTarget::Conversation {
            target: target.clone(),
        },
        i64::try_from(cursor).unwrap_or(i64::MAX),
    );
    Ok(AgentControlReceipt {
        operation: operation.to_string(),
        target: AgentTarget::Conversation { target },
        status: receipt.phase.as_str().to_string(),
        phase: receipt.phase.as_str().to_string(),
        outcome: receipt.outcome.map(|outcome| outcome.as_str().to_string()),
        duplicate: receipt.duplicate,
        message_id: Some(receipt.message_id),
        command_id: None,
        cursor: Some(cursor),
        detail: receipt.reason,
    })
}

fn subagent_receipt(
    operation: &str,
    target: TaskSubagentTarget,
    receipt: SubagentControlReceipt,
) -> AgentControlReceipt {
    AgentControlReceipt {
        operation: operation.to_string(),
        target: AgentTarget::TaskSubagent { target },
        status: receipt.status.as_str().to_string(),
        phase: control_phase_name(receipt.phase).to_string(),
        outcome: receipt.outcome.map(|outcome| outcome.as_str().to_string()),
        duplicate: receipt.duplicate,
        message_id: None,
        command_id: Some(receipt.identity.command_id),
        cursor: None,
        detail: receipt.detail,
    }
}

fn control_phase_name(phase: SubagentControlPhase) -> &'static str {
    match phase {
        SubagentControlPhase::Persisted => "persisted",
        SubagentControlPhase::MailboxAccepted => "mailbox_accepted",
        SubagentControlPhase::Drained => "drained",
        SubagentControlPhase::TurnSettled => "turn_settled",
    }
}

fn subagent_wait_event_types() -> Vec<RuntimeEventKind> {
    vec![
        RuntimeEventKind::SubagentReleased,
        RuntimeEventKind::SubagentGuidanceMailboxAccepted,
        RuntimeEventKind::SubagentGuidanceDrained,
        RuntimeEventKind::SubagentGuidanceSettled,
        RuntimeEventKind::RunGoalUpdated,
        RuntimeEventKind::RequirementEvidenceInvalidated,
        RuntimeEventKind::RequirementSkipped,
        RuntimeEventKind::RunFailed,
        RuntimeEventKind::RunCancelled,
        RuntimeEventKind::TaskFailed,
        RuntimeEventKind::TaskCancelled,
        RuntimeEventKind::TaskTimedOut,
        RuntimeEventKind::SubagentGuidanceRejected,
        RuntimeEventKind::SubagentInterruptSettled,
        RuntimeEventKind::Failed,
        RuntimeEventKind::Cancelled,
        RuntimeEventKind::TimedOut,
        RuntimeEventKind::ArtifactProduced,
        RuntimeEventKind::BackgroundCellFinished,
        RuntimeEventKind::MergeStarted,
        RuntimeEventKind::MergeCompleted,
        RuntimeEventKind::MergeFailed,
    ]
}

fn bounded_text(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn tool_value<T: Serialize>(value: &T) -> Result<ToolResult, AgentControlError> {
    let data = serde_json::to_value(value)
        .map_err(|error| AgentControlError::Runtime(error.to_string()))?;
    Ok(ToolResult::success_json(data))
}

fn operation_result<T: Serialize>(
    result: Result<T, AgentControlError>,
) -> Result<ToolResult, AgentControlError> {
    result.and_then(|value| tool_value(&value))
}

fn tool_error(error: AgentControlError) -> ToolResult {
    let message = error.to_string();
    let mut result = ToolResult::error(message.clone());
    result.data = Some(json!({
        "error": message,
        "code": error.code(),
        "fail_closed": true,
    }));
    result
}

#[derive(Debug, Clone, Copy)]
enum AgentControlOperation {
    List,
    Inspect,
    Message,
    Followup,
    Wait,
    Interrupt,
    Spawn,
    Resume,
    Handoff,
    Group,
}

struct AgentControlTool {
    service: Arc<AgentControlService>,
    operation: AgentControlOperation,
}

impl AgentControlTool {
    fn new(service: Arc<AgentControlService>, operation: AgentControlOperation) -> Self {
        Self { service, operation }
    }

    fn operation_name(&self) -> &'static str {
        match self.operation {
            AgentControlOperation::List => "agent_list",
            AgentControlOperation::Inspect => "agent_inspect",
            AgentControlOperation::Message => "agent_message",
            AgentControlOperation::Followup => "agent_followup",
            AgentControlOperation::Wait => "agent_wait",
            AgentControlOperation::Interrupt => "agent_interrupt",
            AgentControlOperation::Spawn => "agent_spawn",
            AgentControlOperation::Resume => "agent_resume",
            AgentControlOperation::Handoff => "agent_handoff",
            AgentControlOperation::Group => "agent_group",
        }
    }
}

impl Tool for AgentControlTool {
    fn name(&self) -> &str {
        self.operation_name()
    }

    fn description(&self) -> &str {
        match self.operation {
            AgentControlOperation::List => "List bounded Conversation and TaskSubagent targets.",
            AgentControlOperation::Inspect => {
                "Inspect one exact discriminated Agent target without returning full history."
            }
            AgentControlOperation::Message => {
                "Send one idempotent message to a Conversation or exact active/future TaskSubagent attempt."
            }
            AgentControlOperation::Followup => {
                "Queue one Conversation follow-up; TaskSubagent targets are rejected."
            }
            AgentControlOperation::Wait => {
                "Wait on existing event cursors for mailbox, Subagent terminal, or needs-attention events."
            }
            AgentControlOperation::Interrupt => "Interrupt one exact active TaskSubagent attempt.",
            AgentControlOperation::Spawn => {
                "Create a new conversation Agent in the current or any registered workspace, with an optional first message and cold-start first turn."
            }
            AgentControlOperation::Resume => {
                "Wake an idle conversation with a follow-up, or resume the paused TaskRun bound to a conversation."
            }
            AgentControlOperation::Handoff => {
                "Migrate one conversation Agent to another workspace host in this process: settle its active turn, copy the transcript, retire the source, optionally deliver a follow-up."
            }
            AgentControlOperation::Group => {
                "Manage long-lived collaboration groups (create / list / update / delete) over existing conversation Agents."
            }
        }
    }

    fn exempt_from_batch_timeout(&self) -> bool {
        // Waiting has its own bounded timeout/cancel contract, just like the
        // existing agent_tool dispatch. A shorter generic tool-batch timeout
        // would strand a valid cursor wait before its owner can respond.
        matches!(self.operation, AgentControlOperation::Wait)
    }

    fn parameters(&self) -> Value {
        let target_schema = json!({
            "type": "object",
            "description": "Discriminated target: {type:'conversation',workspace_id,conversation_id} or {type:'task_subagent',workspace_id,run_id,task_id,plan_revision,execution_id,attempt}.",
            "required": ["type"],
            "properties": {
                "type": {"type":"string", "enum":["conversation","task_subagent"]},
                "workspace_id": {"type":"string"},
                "conversation_id": {"type":"string"},
                "workspace_generation": {"type":"string"},
                "run_id": {"type":"string"},
                "task_id": {"type":"string"},
                "plan_revision": {"type":"integer", "minimum":1},
                "execution_id": {"type":"string"},
                "attempt": {"type":"integer", "minimum":1}
            },
            "additionalProperties": false
        });
        let conversation_target_schema = json!({
            "type": "object",
            "description": "Conversation source target.",
            "required": ["workspace_id", "conversation_id"],
            "properties": {
                "workspace_id": {"type":"string"},
                "conversation_id": {"type":"string"},
                "workspace_generation": {"type":"string"}
            },
            "additionalProperties": false
        });
        match self.operation {
            AgentControlOperation::List => json!({
                "type":"object",
                "properties": {
                    "scope":{"type":"string","enum":["all","conversation","task_subagent"]},
                    "workspace_id":{"type":"string"},
                    "group_id":{"type":"string","description":"Only conversations in this collaboration group."},
                    "status":{"type":"string"},
                    "limit":{"type":"integer","minimum":1,"maximum":32}
                },
                "additionalProperties": false
            }),
            AgentControlOperation::Inspect => json!({
                "type":"object",
                "properties":{"target":target_schema},
                "required":["target"],
                "additionalProperties":false
            }),
            AgentControlOperation::Message | AgentControlOperation::Followup => json!({
                "type":"object",
                "properties": {
                    "target":target_schema,
                    "text":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS},
                    "command_id":{"type":"string","maxLength":128},
                    "message_id":{"type":"string","maxLength":128},
                    "correlation_id":{"type":"string","maxLength":128},
                    "delivery":{"type":"string","enum":["live","next_attempt"]},
                    "from":conversation_target_schema
                },
                "required":["target","text"],
                "additionalProperties":false
            }),
            AgentControlOperation::Wait => json!({
                "type":"object",
                "properties": {
                    "targets":{"type":"array","minItems":1,"maxItems":MAX_WAIT_TARGETS,"items":target_schema},
                    "after_cursor":{"type":"string","maxLength":256},
                    "timeout_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS}
                },
                "required":["targets"],
                "additionalProperties":false
            }),
            AgentControlOperation::Interrupt => json!({
                "type":"object",
                "properties": {
                    "target":target_schema,
                    "reason":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS},
                    "command_id":{"type":"string","minLength":1,"maxLength":128}
                },
                "required":["target","reason","command_id"],
                "additionalProperties":false
            }),
            AgentControlOperation::Spawn => json!({
                "type":"object",
                "properties": {
                    "goal":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS},
                    "title":{"type":"string","minLength":1,"maxLength":160},
                    "workspace_id":{"type":"string"},
                    "first_message":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS},
                    "start":{"type":"boolean"}
                },
                "required":["goal"],
                "additionalProperties":false
            }),
            AgentControlOperation::Resume => json!({
                "type":"object",
                "properties": {
                    "workspace_id":{"type":"string"},
                    "conversation_id":{"type":"string"},
                    "resume_policy":{"type":"string","enum":["followup","task_run"]},
                    "run_id":{"type":"string","description":"Paused TaskRun id; required when resume_policy is task_run."},
                    "text":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS}
                },
                "required":["workspace_id","conversation_id"],
                "additionalProperties":false
            }),
            AgentControlOperation::Handoff => json!({
                "type":"object",
                "properties": {
                    "workspace_id":{"type":"string"},
                    "conversation_id":{"type":"string"},
                    "destination_workspace_id":{"type":"string"},
                    "follow_up":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS}
                },
                "required":["workspace_id","conversation_id","destination_workspace_id"],
                "additionalProperties":false
            }),
            AgentControlOperation::Group => json!({
                "type":"object",
                "properties": {
                    "action":{"type":"string","enum":["list","create","update","delete"]},
                    "group_id":{"type":"string"},
                    "name":{"type":"string","minLength":1,"maxLength":160},
                    "leader":conversation_target_schema,
                    "members":{"type":"array","minItems":1,"maxItems":32,"items":{
                        "type":"object",
                        "required":["workspace_id","conversation_id","subagent_role"],
                        "properties":{
                            "workspace_id":{"type":"string"},
                            "conversation_id":{"type":"string"},
                            "subagent_role":{"type":"string","minLength":1,"maxLength":128},
                            "label":{"type":"string","maxLength":160}
                        },
                        "additionalProperties":false
                    }}
                },
                "required":["action"],
                "additionalProperties":false
            }),
        }
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        ctx: &'a ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let value = Value::Object(parameters.into_iter().collect());
            let result = match self.operation {
                AgentControlOperation::List => {
                    let request = serde_json::from_value::<AgentListRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.list(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Inspect => {
                    let request = serde_json::from_value::<InspectToolRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.inspect(request.target).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Message | AgentControlOperation::Followup => {
                    let request = serde_json::from_value::<AgentMessageRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) if matches!(self.operation, AgentControlOperation::Message) => {
                            operation_result(self.service.message(request).await)
                        }
                        Ok(request) => operation_result(self.service.followup(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Wait => {
                    let request = serde_json::from_value::<AgentWaitRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => {
                            operation_result(self.service.wait(request, ctx.cancel.clone()).await)
                        }
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Interrupt => {
                    let request = serde_json::from_value::<AgentInterruptRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.interrupt(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Spawn => {
                    let request = serde_json::from_value::<AgentSpawnRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.spawn(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Resume => {
                    let request = serde_json::from_value::<AgentResumeRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.resume(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Handoff => {
                    let request = serde_json::from_value::<AgentHandoffRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.handoff(request).await),
                        Err(error) => Err(error),
                    }
                }
                AgentControlOperation::Group => {
                    let request = serde_json::from_value::<AgentGroupToolRequest>(value)
                        .map_err(|error| AgentControlError::Invalid(error.to_string()));
                    match request {
                        Ok(request) => operation_result(self.service.group(request).await),
                        Err(error) => Err(error),
                    }
                }
            };
            match result {
                Ok(result) => Ok(result),
                Err(error) => Ok(tool_error(error)),
            }
        })
    }
}

#[derive(Debug, Deserialize)]
struct InspectToolRequest {
    target: AgentTarget,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSpawnRequest {
    /// Goal for the new conversation Agent (also becomes its first message).
    pub goal: String,
    /// Optional conversation title. Defaults to a bounded goal preview.
    pub title: Option<String>,
    /// Target workspace id. Defaults to the service's bound workspace.
    pub workspace_id: Option<String>,
    /// Optional first message body; defaults to the goal text.
    pub first_message: Option<String>,
    /// When true, start the first turn immediately (cold delivery).
    #[serde(default = "default_true")]
    pub start: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentResumeRequest {
    /// Conversation to wake (follow-up delivery), or a TaskRun-bearing
    /// conversation whose paused run should resume.
    pub workspace_id: String,
    pub conversation_id: String,
    /// `followup` (default) queues a follow-up message; `task_run` resumes a
    /// paused TaskRun bound to this conversation.
    #[serde(default)]
    pub resume_policy: AgentResumePolicy,
    /// Required with `task_run`: the exact paused run to resume.
    pub run_id: Option<String>,
    /// Optional instruction text (follow-up message or run resume note).
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentResumePolicy {
    #[default]
    Followup,
    TaskRun,
}

#[derive(Debug, Deserialize)]
pub struct AgentGroupToolRequest {
    pub action: AgentGroupAction,
    pub group_id: Option<String>,
    pub name: Option<String>,
    pub leader: Option<ConversationTarget>,
    pub members: Option<Vec<AgentGroupMemberInput>>,
    #[serde(default)]
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentGroupAction {
    List,
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentGroupMemberInput {
    pub workspace_id: String,
    pub conversation_id: String,
    pub subagent_role: String,
    pub label: Option<String>,
}

impl AgentGroupMemberInput {
    fn into_member(self) -> crate::agent_router::AgentGroupMember {
        crate::agent_router::AgentGroupMember {
            address: {
                let workspace = crate::workspace::WorkspaceId::from_raw(self.workspace_id);
                crate::agent_router::AgentAddress::new(workspace, self.conversation_id)
            },
            subagent_role: self.subagent_role,
            label: self.label,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentHandoffRequest {
    pub workspace_id: String,
    pub conversation_id: String,
    pub destination_workspace_id: String,
    /// Optional follow-up message delivered in the destination workspace.
    pub follow_up: Option<String>,
}

/// Register all six control tools on the existing shared ToolManager. The
/// helper is idempotent with respect to repeated post-hoc setup calls.
pub async fn register_agent_control_tools_on_agent(
    agent_handle: &AgentHandle,
    service: Arc<AgentControlService>,
) {
    let replace_existing = service.delivery_wake.is_some();
    let operations = [
        AgentControlOperation::List,
        AgentControlOperation::Inspect,
        AgentControlOperation::Message,
        AgentControlOperation::Followup,
        AgentControlOperation::Wait,
        AgentControlOperation::Interrupt,
        AgentControlOperation::Spawn,
        AgentControlOperation::Resume,
        AgentControlOperation::Handoff,
        AgentControlOperation::Group,
    ];
    let added = agent_handle
        .write(|agent| {
            let mut added = 0_usize;
            for operation in operations {
                let name = match operation {
                    AgentControlOperation::List => "agent_list",
                    AgentControlOperation::Inspect => "agent_inspect",
                    AgentControlOperation::Message => "agent_message",
                    AgentControlOperation::Followup => "agent_followup",
                    AgentControlOperation::Wait => "agent_wait",
                    AgentControlOperation::Interrupt => "agent_interrupt",
                    AgentControlOperation::Spawn => "agent_spawn",
                    AgentControlOperation::Resume => "agent_resume",
                    AgentControlOperation::Handoff => "agent_handoff",
                    AgentControlOperation::Group => "agent_group",
                };
                if replace_existing
                    && agent
                        .tool_names()
                        .iter()
                        .any(|registered| registered == name)
                {
                    // AppState-bound registration replaces the bootstrap
                    // fallback so Conversation messages also wake the sole
                    // delivery supervisor. The ToolManager remains the
                    // canonical registry; no second tool path is created.
                    let _ = agent.remove_tool(name);
                }
                if !agent
                    .tool_names()
                    .iter()
                    .any(|registered| registered == name)
                {
                    agent.add_tool(Box::new(AgentControlTool::new(
                        Arc::clone(&service),
                        operation,
                    )));
                    added = added.saturating_add(1);
                }
            }
            added
        })
        .await;
    if added > 0 {
        tracing::info!(added, "Registered unified model Agent control tools");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::SubagentReleaseRecord;
    use crate::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, ExecutionUsage, PlanTask, SubagentOutcome,
        SubagentStatus, TaskPlan, task_goal_sha256,
    };
    use echo_agent::memory::{ConversationStore, FileConversationStore, NewConversation};

    fn service(root: &std::path::Path) -> Result<AgentControlService, String> {
        let router = Arc::new(AgentRouter::new(root.join("router")));
        let task_runtime = Arc::new(
            TaskRuntimeStore::open_for_workspace(root.join("tasks"), "global")
                .map_err(|error| error.to_string())?,
        );
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(root.join("workspaces"))
                .map_err(|error| error.to_string())?,
        );
        let conversation_store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(root.join("conversation-store"))
                .map_err(|error| error.to_string())?,
        );
        Ok(AgentControlService::new(router, task_runtime, registry)
            .with_conversation_store(conversation_store, "global".to_string()))
    }

    async fn seed_conversations(
        service: &AgentControlService,
        conversation_ids: &[&str],
    ) -> Result<(), String> {
        let store = service
            .conversation_store
            .as_ref()
            .ok_or_else(|| "conversation store fixture is missing".to_string())?;
        for conversation_id in conversation_ids {
            store
                .create_conversation(NewConversation {
                    conversation_id: (*conversation_id).to_string(),
                    user_id: "default".to_string(),
                    agent_type: None,
                    title: None,
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn conversation(workspace_id: &str, conversation_id: &str) -> AgentTarget {
        AgentTarget::Conversation {
            target: ConversationTarget {
                workspace_id: workspace_id.to_string(),
                conversation_id: conversation_id.to_string(),
                workspace_generation: None,
            },
        }
    }

    fn task_subagent_target(run_id: &str) -> AgentTarget {
        AgentTarget::TaskSubagent {
            target: TaskSubagentTarget {
                workspace_id: "global".to_string(),
                run_id: run_id.to_string(),
                task_id: "task-a".to_string(),
                plan_revision: 1,
                execution_id: "execution-a".to_string(),
                attempt: 1,
                workspace_generation: None,
            },
        }
    }

    fn seed_task_plan(store: &TaskRuntimeStore, run_id: &str) -> Result<(), String> {
        store
            .create_run(
                run_id,
                "global",
                "conversation-a",
                "message-a",
                DomainProfile::General,
                "goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "task-a".to_string(),
                    title: "Task A".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())
    }

    #[test]
    fn agent_target_uses_flat_internal_tag_and_typescript_discriminator() -> Result<(), String> {
        let target = conversation("global", "conversation-a");
        let value = serde_json::to_value(&target).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("type").and_then(Value::as_str),
            Some("conversation")
        );
        assert_eq!(
            value.get("workspace_id").and_then(Value::as_str),
            Some("global")
        );
        assert_eq!(
            value.get("conversation_id").and_then(Value::as_str),
            Some("conversation-a")
        );
        assert!(value.get("target").is_none());
        let decoded: AgentTarget =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert_eq!(decoded, target);
        let declaration = AgentTarget::decl();
        assert!(declaration.contains("type"));
        assert!(declaration.contains("conversation"));
        assert!(declaration.contains("task_subagent"));
        Ok(())
    }

    #[test]
    fn multi_target_wait_cursor_round_trips_without_cross_target_rejection() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        let targets = vec![conversation("global", "a"), conversation("global", "b")];
        let mut cursors = HashMap::new();
        let first = targets.first().ok_or("first target missing")?;
        let second = targets.get(1).ok_or("second target missing")?;
        cursors.insert(service.cursor_prefix(first), 4);
        cursors.insert(service.cursor_prefix(second), 9);
        let encoded = service.next_wait_cursor(&targets, &cursors);
        let decoded = service
            .parse_wait_cursors(&targets, Some(&encoded))
            .map_err(|error| error.to_string())?;
        assert_eq!(decoded, cursors);
        Ok(())
    }

    #[tokio::test]
    async fn conversation_message_is_exactly_once_and_cursor_is_target_bound() -> Result<(), String>
    {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_conversations(&service, &["conversation-a", "conversation-b"]).await?;
        let target = conversation("global", "conversation-a");
        let request = AgentMessageRequest {
            target: target.clone(),
            text: "hello".to_string(),
            command_id: None,
            message_id: Some("message-1".to_string()),
            correlation_id: Some("corr-1".to_string()),
            delivery: AgentMessageDelivery::Live,
            from: None,
        };
        let first = service
            .message(request.clone())
            .await
            .map_err(|e| e.to_string())?;
        let records = service
            .router
            .records(&AgentAddress::new(
                WorkspaceId::from_raw("global".to_string()),
                "conversation-a",
            ))
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(
            records.first().map(|record| record.payload.origin),
            Some(crate::agent_router::AgentMessageOrigin::Agent)
        );
        let second = service.message(request).await.map_err(|e| e.to_string())?;
        assert!(!first.duplicate);
        assert!(second.duplicate);
        let listed = service
            .list(AgentListRequest {
                scope: AgentListScope::Conversation,
                group_id: None,
                workspace_id: None,
                status: None,
                limit: 8,
            })
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(listed.count, 2);
        assert!(listed.entries.iter().all(|entry| {
            matches!(
                &entry.target,
                AgentTarget::Conversation { target }
                    if target.workspace_generation.as_deref() == Some("global")
            )
        }));
        let inspected = service
            .inspect(target.clone())
            .await
            .map_err(|e| e.to_string())?;
        assert!(inspected.cursor.contains(':'));
        let wrong_target = conversation("global", "conversation-b");
        service
            .router
            .enqueue(crate::agent_router::AgentMessage::agent_text(
                None,
                AgentAddress::new(
                    WorkspaceId::from_raw("global".to_string()),
                    "conversation-b",
                ),
                "seed target",
            ))
            .await
            .map_err(|e| e.to_string())?;
        let error = match service
            .wait(
                AgentWaitRequest {
                    targets: vec![wrong_target],
                    after_cursor: Some(inspected.cursor),
                    timeout_ms: 0,
                },
                None,
            )
            .await
        {
            Ok(_) => return Err("a cursor from another target was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, AgentControlError::InvalidCursor));
        Ok(())
    }

    #[tokio::test]
    async fn conversation_message_is_information_only_and_followup_invokes_delivery_wake()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&wakes);
        let service = service(root.path())?.with_delivery_wake(Arc::new(move |_target| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        seed_conversations(&service, &["conversation-a"]).await?;
        service
            .message(AgentMessageRequest {
                target: conversation("global", "conversation-a"),
                text: "wake the target".to_string(),
                command_id: None,
                message_id: Some("message-wake".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 0);
        service
            .followup(AgentMessageRequest {
                target: conversation("global", "conversation-a"),
                text: "wake the target".to_string(),
                command_id: None,
                message_id: Some("message-followup".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 1);
        Ok(())
    }

    fn task_run_count(service: &AgentControlService) -> Result<usize, String> {
        use crate::tasks::task_runtime::TaskRunStatus;

        service
            .task_runtime
            .list_runs_in(&[
                TaskRunStatus::Pending,
                TaskRunStatus::Running,
                TaskRunStatus::Paused,
                TaskRunStatus::Cancelled,
                TaskRunStatus::Failed,
                TaskRunStatus::Completed,
            ])
            .map(|runs| runs.len())
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn task_agent_queries_remain_exact_after_long_unrelated_history() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_task_plan(service.task_runtime.as_ref(), "run-bounded")
            .map_err(|error| format!("failed to seed bounded Agent-control plan: {error}"))?;
        service
            .task_runtime
            .record_subagent_assigned(
                "run-bounded",
                "task-a",
                "execution-a",
                "implementer",
                "Task A",
                1,
                1,
                false,
                false,
            )
            .map_err(|error| error.to_string())?;
        let assigned = service
            .task_runtime
            .query_events_bounded(
                "run-bounded",
                RuntimeEventQuery::new(0, 1)
                    .for_execution("execution-a")
                    .with_event_types(vec![RuntimeEventKind::SubagentAssigned]),
            )
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "assigned event missing".to_string())?;
        for index in 0..300 {
            service
                .task_runtime
                .note("run-bounded", None, &format!("unrelated event {index}"))
                .map_err(|error| error.to_string())?;
        }
        let result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "完成 ✅ adapter result",
            Vec::new(),
        );
        let usage = ExecutionUsage {
            duration_ms: Some(17),
            tokens_used: Some(23),
            iterations: Some(2),
        };
        service
            .task_runtime
            .record_subagent_released(SubagentReleaseRecord {
                run_id: "run-bounded",
                task_id: "task-a",
                execution_id: "execution-a",
                agent_name: "implementer",
                task_subject: "Task A",
                plan_revision: 1,
                attempt: 1,
                status: "completed",
                outcome: Some(&result),
                full_output: None,
                usage: Some(&usage),
                dispatch_hook: false,
            })
            .map_err(|error| error.to_string())?;

        let listed = service
            .list(AgentListRequest {
                scope: AgentListScope::TaskSubagent,
                group_id: None,
                workspace_id: Some("global".to_string()),
                status: None,
                limit: 1,
            })
            .await
            .map_err(|error| error.to_string())?;
        let entry = listed
            .entries
            .first()
            .ok_or_else(|| "bounded Agent list entry missing".to_string())?;
        assert_eq!(entry.attempt, Some(1));
        assert_eq!(entry.status, "completed");
        assert_eq!(entry.summary.as_deref(), Some("完成 ✅ adapter result"));

        let target = task_subagent_target("run-bounded");
        let inspected = service
            .inspect(target.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(inspected.phase.as_deref(), Some("subagent_released"));
        assert_eq!(inspected.outcome.as_deref(), Some("completed"));
        assert_eq!(inspected.summary.as_deref(), Some("完成 ✅ adapter result"));
        let waited = service
            .wait(
                AgentWaitRequest {
                    targets: vec![target.clone()],
                    after_cursor: Some(service.cursor_token(&target, assigned.seq)),
                    timeout_ms: 0,
                },
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(waited.status, AgentWaitStatus::Changed);
        assert_eq!(waited.events.len(), 1);
        assert_eq!(
            waited.events.first().map(|event| event.kind.as_str()),
            Some("subagent_released")
        );
        Ok(())
    }

    async fn settle_claim(
        router: &AgentRouter,
        claim: &crate::agent_router::AgentDeliveryClaim,
        turn_id: &str,
    ) -> Result<AgentDeliveryReceipt, String> {
        router
            .begin_injection(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .mailbox_accepted(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .drained(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .turn_settled(
                claim,
                Some(turn_id.to_string()),
                crate::agent_router::AgentDeliveryOutcome::Completed,
                true,
                None,
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn f5_conversation_multi_turns_preserve_fifo_turn_identity_without_task_run()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_conversations(&service, &["conversation-multi"]).await?;
        let target = conversation("global", "conversation-multi");
        let address = AgentAddress::new(
            WorkspaceId::from_raw("global".to_string()),
            "conversation-multi",
        );

        let first_request = AgentMessageRequest {
            target: target.clone(),
            text: "first turn".to_string(),
            command_id: None,
            message_id: Some("f5-first-turn".to_string()),
            correlation_id: None,
            delivery: AgentMessageDelivery::Live,
            from: None,
        };
        let second_request = AgentMessageRequest {
            target,
            text: "second turn".to_string(),
            command_id: None,
            message_id: Some("f5-second-turn".to_string()),
            correlation_id: None,
            delivery: AgentMessageDelivery::Live,
            from: None,
        };
        let first_receipt = service
            .message(first_request)
            .await
            .map_err(|error| error.to_string())?;
        let second_receipt = service
            .message(second_request)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(first_receipt.phase, "persisted");
        assert_eq!(second_receipt.phase, "persisted");

        let pending = service
            .router
            .pending(&address)
            .await
            .map_err(|error| error.to_string())?;
        let first = pending
            .first()
            .ok_or_else(|| "first Conversation turn was not queued".to_string())?;
        let second = pending
            .get(1)
            .ok_or_else(|| "second Conversation turn was not queued".to_string())?;
        assert_eq!(first.message_id, "f5-first-turn");
        assert_eq!(second.message_id, "f5-second-turn");
        let first_turn_id = first.delivery_turn_id();
        let second_turn_id = second.delivery_turn_id();
        assert_ne!(first_turn_id, second_turn_id);

        let first_claim = service
            .router
            .claim_next(&address)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first Conversation claim was not admitted".to_string())?;
        assert_eq!(first_claim.payload.message_id, "f5-first-turn");
        settle_claim(&service.router, &first_claim, &first_turn_id).await?;

        let second_claim = service
            .router
            .claim_next(&address)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second Conversation claim was not admitted".to_string())?;
        assert_eq!(second_claim.payload.message_id, "f5-second-turn");
        settle_claim(&service.router, &second_claim, &second_turn_id).await?;

        let records = service
            .router
            .records(&address)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(records.len(), 2);
        let first_record = records
            .iter()
            .find(|record| record.message_id == "f5-first-turn")
            .ok_or_else(|| "first Conversation terminal receipt is missing".to_string())?;
        let second_record = records
            .iter()
            .find(|record| record.message_id == "f5-second-turn")
            .ok_or_else(|| "second Conversation terminal receipt is missing".to_string())?;
        assert_eq!(
            first_record.phase,
            crate::agent_router::AgentDeliveryPhase::TurnSettled
        );
        assert_eq!(
            second_record.phase,
            crate::agent_router::AgentDeliveryPhase::TurnSettled
        );
        assert_eq!(
            first_record.turn_id.as_deref(),
            Some(first_turn_id.as_str())
        );
        assert_eq!(
            second_record.turn_id.as_deref(),
            Some(second_turn_id.as_str())
        );
        assert_eq!(task_run_count(&service)?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn f5_followup_distinguishes_idle_information_from_running_delivery() -> Result<(), String>
    {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&wakes);
        let service = service(root.path())?.with_delivery_wake(Arc::new(move |_target| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        seed_conversations(&service, &["conversation-followup"]).await?;
        let target = conversation("global", "conversation-followup");
        let address = AgentAddress::new(
            WorkspaceId::from_raw("global".to_string()),
            "conversation-followup",
        );

        service
            .message(AgentMessageRequest {
                target: target.clone(),
                text: "idle information".to_string(),
                command_id: None,
                message_id: Some("f5-idle-message".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "plain Conversation message is informational and does not wake an idle target"
        );

        let claim = service
            .router
            .claim_next(&address)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "running Conversation claim is missing".to_string())?;
        let running_turn_id = claim.payload.delivery_turn_id();
        service
            .router
            .begin_injection(&claim, running_turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .router
            .mailbox_accepted(&claim, running_turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;

        let followup = service
            .followup(AgentMessageRequest {
                target,
                text: "running follow-up".to_string(),
                command_id: None,
                message_id: Some("f5-running-followup".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(followup.operation, "followup");
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "follow-up wakes the existing running target exactly once"
        );
        let in_flight = service
            .router
            .in_flight_claim(&address)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "follow-up replaced the running Conversation claim".to_string())?;
        assert_eq!(in_flight.claim.payload.message_id, "f5-idle-message");
        assert_eq!(in_flight.turn_id, running_turn_id);
        let pending = service
            .router
            .pending(&address)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .filter(|message| message.message_id == "f5-running-followup")
                .count(),
            1
        );
        assert_eq!(
            pending
                .iter()
                .position(|message| message.message_id == "f5-idle-message"),
            Some(0)
        );
        assert_eq!(task_run_count(&service)?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn f5_terminal_receipt_keeps_exact_turn_and_rejects_stale_claim() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(root.path().join("router"));
        let target = AgentAddress::new(
            WorkspaceId::from_raw("global".to_string()),
            "conversation-terminal",
        );
        let mut message =
            crate::agent_router::AgentMessage::agent_text(None, target.clone(), "terminal receipt");
        message.message_id = "f5-terminal-message".to_string();
        let turn_id = message.delivery_turn_id();
        router
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "terminal claim is missing".to_string())?;
        router
            .begin_injection(&claim, turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            router.mailbox_accepted(&claim, "f5-wrong-turn").await,
            Err(crate::agent_router::AgentRouterError::StaleClaim { .. })
        ));
        router
            .mailbox_accepted(&claim, turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        router
            .drained(&claim, turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        let terminal = router
            .turn_settled(
                &claim,
                Some(turn_id.clone()),
                crate::agent_router::AgentDeliveryOutcome::Completed,
                true,
                None,
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            terminal.phase,
            crate::agent_router::AgentDeliveryPhase::TurnSettled
        );
        assert_eq!(
            terminal.outcome,
            Some(crate::agent_router::AgentDeliveryOutcome::Completed)
        );
        assert!(terminal.drained);

        let duplicate = router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.phase, terminal.phase);
        assert_eq!(duplicate.outcome, terminal.outcome);
        assert_eq!(duplicate.drained, terminal.drained);
        assert!(matches!(
            router.begin_injection(&claim, turn_id).await,
            Err(crate::agent_router::AgentRouterError::StaleClaim { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn f5_late_or_unknown_conversation_message_does_not_create_task_run() -> Result<(), String>
    {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_conversations(&service, &["conversation-late"]).await?;
        let target = conversation("global", "conversation-late");
        let address = AgentAddress::new(
            WorkspaceId::from_raw("global".to_string()),
            "conversation-late",
        );
        let request = AgentMessageRequest {
            target: target.clone(),
            text: "late message".to_string(),
            command_id: None,
            message_id: Some("f5-late-message".to_string()),
            correlation_id: None,
            delivery: AgentMessageDelivery::Live,
            from: None,
        };
        service
            .message(request.clone())
            .await
            .map_err(|error| error.to_string())?;
        let claim = service
            .router
            .claim_next(&address)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "late Conversation claim is missing".to_string())?;
        let turn_id = claim.payload.delivery_turn_id();
        settle_claim(&service.router, &claim, &turn_id).await?;

        let late = service
            .message(request)
            .await
            .map_err(|error| error.to_string())?;
        assert!(late.duplicate);
        assert_eq!(late.phase, "turn_settled");
        let unknown = conversation("global", "conversation-stale");
        let error = match service
            .message(AgentMessageRequest {
                target: unknown,
                text: "stale target".to_string(),
                command_id: None,
                message_id: Some("f5-stale-target".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
        {
            Ok(_) => return Err("unknown Conversation message was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, AgentControlError::TargetUnavailable(_)));
        let targets = service
            .router
            .list_targets()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(targets, vec![address]);
        assert_eq!(task_run_count(&service)?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn inspect_unknown_conversation_is_read_only_and_does_not_create_target()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        let target = conversation("global", "unknown");
        // A router manifest alone is not a persisted Conversation. This
        // simulates a stale/phantom inbox and ensures Store-first validation
        // cannot be bypassed by the router target_exists fast path.
        service
            .router
            .enqueue(crate::agent_router::AgentMessage::agent_text(
                None,
                AgentAddress::new(WorkspaceId::from_raw("global".to_string()), "unknown"),
                "phantom",
            ))
            .await
            .map_err(|error| error.to_string())?;
        let error = match service.inspect(target).await {
            Ok(_) => return Err("unknown target was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, AgentControlError::TargetUnavailable(_)));
        let listed = service
            .list(AgentListRequest {
                scope: AgentListScope::Conversation,
                group_id: None,
                workspace_id: None,
                status: None,
                limit: 8,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(listed.count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn followup_rejects_task_subagent_targets_before_runtime_access() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        let error = match service
            .followup(AgentMessageRequest {
                target: AgentTarget::TaskSubagent {
                    target: TaskSubagentTarget {
                        workspace_id: "global".to_string(),
                        run_id: "run".to_string(),
                        task_id: "task".to_string(),
                        plan_revision: 1,
                        execution_id: "execution".to_string(),
                        attempt: 1,
                        workspace_generation: None,
                    },
                },
                text: "follow up".to_string(),
                command_id: Some("command".to_string()),
                message_id: None,
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
        {
            Ok(_) => return Err("Conversation follow-up addressed a TaskSubagent".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, AgentControlError::WrongTargetKind { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn wait_honors_cancellation_without_claiming_terminal_state() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_conversations(&service, &["conversation-a"]).await?;
        service
            .router
            .enqueue(crate::agent_router::AgentMessage::agent_text(
                None,
                AgentAddress::new(
                    WorkspaceId::from_raw("global".to_string()),
                    "conversation-a",
                ),
                "seed target",
            ))
            .await
            .map_err(|error| error.to_string())?;
        let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
        cancel.cancel();
        let response = service
            .wait(
                AgentWaitRequest {
                    targets: vec![conversation("global", "conversation-a")],
                    after_cursor: None,
                    timeout_ms: 10_000,
                },
                Some(cancel),
            )
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(response.status, AgentWaitStatus::Cancelled);
        assert!(response.events.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn task_subagent_next_attempt_command_replays_exactly_once() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_task_plan(&service.task_runtime, "run-a")?;
        let mut target = task_subagent_target("run-a");
        if let AgentTarget::TaskSubagent { target } = &mut target {
            target.execution_id = "pending:run-a:task-a:1:1".to_string();
        }
        let request = AgentMessageRequest {
            target,
            text: "use the latest fixture".to_string(),
            command_id: Some("command-a".to_string()),
            message_id: None,
            correlation_id: None,
            delivery: AgentMessageDelivery::NextAttempt,
            from: None,
        };
        let first = service
            .message(request.clone())
            .await
            .map_err(|e| e.to_string())?;
        let second = service.message(request).await.map_err(|e| e.to_string())?;
        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(first.command_id, second.command_id);
        Ok(())
    }

    #[tokio::test]
    async fn inspect_and_wait_allow_a_settled_subagent_attempt() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_task_plan(&service.task_runtime, "run-terminal")?;
        service
            .task_runtime
            .record_subagent_assigned(
                "run-terminal",
                "task-a",
                "execution-a",
                "reviewer",
                "Task A",
                1,
                1,
                true,
                false,
            )
            .map_err(|error| error.to_string())?;
        service
            .task_runtime
            .record_subagent_released(crate::tasks::task_runtime::store::SubagentReleaseRecord {
                run_id: "run-terminal",
                task_id: "task-a",
                execution_id: "execution-a",
                agent_name: "reviewer",
                task_subject: "Task A",
                plan_revision: 1,
                attempt: 1,
                status: "completed",
                outcome: None,
                full_output: None,
                usage: None,
                dispatch_hook: false,
            })
            .map_err(|error| error.to_string())?;
        let target = task_subagent_target("run-terminal");
        let inspected = service
            .inspect(target.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(inspected.status, "completed");
        let waited = service
            .wait(
                AgentWaitRequest {
                    targets: vec![target],
                    after_cursor: Some(inspected.cursor),
                    timeout_ms: 0,
                },
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(waited.status, AgentWaitStatus::Timeout);
        Ok(())
    }

    #[tokio::test]
    async fn task_subagent_target_rejects_a_different_workspace_runtime() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = service(root.path())?;
        seed_task_plan(&service.task_runtime, "run-a")?;
        let mut target = task_subagent_target("run-a");
        if let AgentTarget::TaskSubagent { target } = &mut target {
            target.workspace_id = "workspace-b".to_string();
        }
        let error = match service
            .message(AgentMessageRequest {
                target,
                text: "must stay in the owning workspace".to_string(),
                command_id: Some("command-b".to_string()),
                message_id: None,
                correlation_id: None,
                delivery: AgentMessageDelivery::NextAttempt,
                from: None,
            })
            .await
        {
            Ok(_) => return Err("cross-workspace TaskSubagent target was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, AgentControlError::TargetUnavailable(_)));
        Ok(())
    }

    #[tokio::test]
    async fn wrong_workspace_generation_is_rejected_before_enqueue() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = Arc::new(AgentRouter::new(root.path().join("router")));
        let task_runtime = Arc::new(
            TaskRuntimeStore::open_for_workspace(root.path().join("tasks"), "workspace-a")
                .map_err(|error| error.to_string())?,
        );
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(root.path().join("workspaces"))
                .map_err(|error| error.to_string())?,
        );
        let workspace = registry
            .create("workspace-a", crate::workspace::WorkspaceKind::General)
            .map_err(|error| error.to_string())?;
        let service = AgentControlService::new(router, task_runtime, registry);
        let error = match service
            .message(AgentMessageRequest {
                target: AgentTarget::Conversation {
                    target: ConversationTarget {
                        workspace_id: workspace.id.to_string(),
                        conversation_id: "conversation-a".to_string(),
                        workspace_generation: Some("stale-generation".to_string()),
                    },
                },
                text: "must fail closed".to_string(),
                command_id: None,
                message_id: Some("message-stale".to_string()),
                correlation_id: None,
                delivery: AgentMessageDelivery::Live,
                from: None,
            })
            .await
        {
            Ok(_) => return Err("wrong workspace generation was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AgentControlError::WrongWorkspaceGeneration { .. }
        ));
        Ok(())
    }

    #[test]
    fn six_control_tool_schemas_are_bounded_and_distinct() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = Arc::new(service(root.path())?);
        let operations = [
            (AgentControlOperation::List, "agent_list"),
            (AgentControlOperation::Inspect, "agent_inspect"),
            (AgentControlOperation::Message, "agent_message"),
            (AgentControlOperation::Followup, "agent_followup"),
            (AgentControlOperation::Wait, "agent_wait"),
            (AgentControlOperation::Interrupt, "agent_interrupt"),
        ];
        for (operation, name) in operations {
            let tool = AgentControlTool::new(Arc::clone(&service), operation);
            assert_eq!(tool.name(), name);
            jsonschema::validator_for(&tool.parameters())
                .map_err(|error| format!("{name} schema is invalid: {error}"))?;
            assert!(!tool.description().is_empty());
        }
        let message_schema =
            AgentControlTool::new(Arc::clone(&service), AgentControlOperation::Message)
                .parameters();
        let message_validator = jsonschema::validator_for(&message_schema)
            .map_err(|error| format!("agent_message schema is invalid: {error}"))?;
        let valid_source = json!({
            "target": {
                "type": "conversation",
                "workspace_id": "global",
                "conversation_id": "target"
            },
            "text": "hello",
            "from": {
                "workspace_id": "global",
                "conversation_id": "source"
            }
        });
        assert!(message_validator.validate(&valid_source).is_ok());
        Ok(())
    }
}
