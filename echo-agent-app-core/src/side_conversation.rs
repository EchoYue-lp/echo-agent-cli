//! EKO Side Conversation composition over existing conversation authorities.
//!
//! This service owns no files, executor, queue, or event state. Transcript
//! data remains in `ConversationStore`; parent/member relationships remain in
//! `AgentRouter` groups. The Tauri adapter calls this service instead of owning
//! GUI-specific fork rules.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use echo_agent::memory::{Conversation, ConversationStore, NewConversation, StoredMessage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::agent_router::{
    AgentAddress, AgentGroup, AgentGroupMember, AgentRouter, AgentRouterError,
    SideConversationGroupMetadata,
};
use crate::workspace::WorkspaceId;

const SIDE_CONVERSATION_ROLE: &str = "side-conversation";
const SIDE_CONVERSATION_ID_PREFIX: &str = "side-";
const SIDE_CONVERSATION_GROUP_PREFIX: &str = "side-group-";
const DEFAULT_SIDE_TITLE: &str = "Side Conversation";
const MAX_REQUEST_ID_CHARS: usize = 256;
const MAX_PROMPT_CHARS: usize = 100_000;
const MAX_TITLE_CHARS: usize = 160;
const INTERNAL_VISIBILITY_FIELD: &str = "eko_visibility";
const INTERNAL_VISIBILITY_VALUE: &str = "internal_agent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SideConversationCreateRequest")]
pub struct SideConversationCreateRequest {
    pub workspace_id: String,
    pub parent_conversation_id: String,
    pub request_id: String,
    pub prompt: String,
    pub title: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SideConversationEntry")]
pub struct SideConversationEntry {
    pub group_id: String,
    pub workspace_id: String,
    pub parent_conversation_id: String,
    pub conversation_id: String,
    pub title: String,
    pub model_id: Option<String>,
    pub snapshot_message_count: usize,
    #[ts(type = "number | null")]
    pub snapshot_last_message_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub degraded: bool,
    pub status: SideConversationStatus,
    pub unread_count: usize,
    pub launch_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SideConversationStatus")]
pub enum SideConversationStatus {
    Idle,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SideConversationCreateReceipt")]
pub struct SideConversationCreateReceipt {
    pub entry: SideConversationEntry,
    pub duplicate: bool,
}

pub struct SideConversationLaunchPreparation {
    pub creation: SideConversationCreateReceipt,
    pub prompt: String,
    pub admission: SideConversationLaunchAdmission,
}

/// Opaque ownership carried from relation creation through the first GUI turn
/// admission. Holding both identities prevents parent or child deletion from
/// opening a gap between publishing the relation and registering the turn.
#[must_use]
pub struct SideConversationLaunchAdmission {
    pub(crate) workspace_id: String,
    pub(crate) parent_conversation_id: String,
    pub(crate) conversation_id: String,
    pub(crate) _parent_identity: crate::conversation_deletion::ConversationIdentityGuard,
    pub(crate) _child_identity: crate::conversation_deletion::ConversationIdentityGuard,
}

#[derive(Debug, thiserror::Error)]
pub enum SideConversationError {
    #[error("invalid Side Conversation request: {0}")]
    Validation(String),
    #[error("parent conversation was not found")]
    ParentNotFound,
    #[error("Side Conversation cannot create another Side Conversation")]
    NestedNotAllowed,
    #[error("Side Conversation idempotency conflict: {0}")]
    IdempotencyConflict(String),
    #[error("Side Conversation relation is malformed: {0}")]
    MalformedRelation(String),
    #[error("conversation persistence failed: {0}")]
    Conversation(String),
    #[error("AgentRouter operation failed: {0}")]
    Router(String),
    #[error("Side Conversation rollback failed after {operation}: {rollback}")]
    Rollback { operation: String, rollback: String },
}

impl From<AgentRouterError> for SideConversationError {
    fn from(error: AgentRouterError) -> Self {
        Self::Router(error.to_string())
    }
}

#[derive(Clone)]
pub struct SideConversationService {
    router: Arc<AgentRouter>,
}

impl SideConversationService {
    pub fn new(router: Arc<AgentRouter>) -> Self {
        Self { router }
    }

    pub(crate) fn child_id_for_request(
        request: &SideConversationCreateRequest,
    ) -> Result<String, SideConversationError> {
        validate_request(request)?;
        Ok(format!(
            "{SIDE_CONVERSATION_ID_PREFIX}{}",
            identity_digest(request)?
        ))
    }

    pub async fn create(
        &self,
        store: Arc<dyn ConversationStore>,
        request: SideConversationCreateRequest,
    ) -> Result<SideConversationCreateReceipt, SideConversationError> {
        validate_request(&request)?;
        let groups = self.router.list_group_records().await?;
        if relation_for_child_in(
            &groups,
            &request.workspace_id,
            &request.parent_conversation_id,
        )?
        .is_some()
        {
            return Err(SideConversationError::NestedNotAllowed);
        }

        let parent = store
            .get_conversation(&request.parent_conversation_id)
            .await
            .map_err(|error| SideConversationError::Conversation(error.to_string()))?
            .ok_or(SideConversationError::ParentNotFound)?;
        let identity_digest = identity_digest(&request)?;
        let child_id = Self::child_id_for_request(&request)?;
        let group_id = format!("{SIDE_CONVERSATION_GROUP_PREFIX}{identity_digest}");
        let payload_digest = request_payload_digest(&request)?;
        let initial_prompt = request.prompt.trim().to_string();

        if let Some(group) = groups.iter().find(|group| group.group_id == group_id) {
            let metadata = group.side_conversation.as_ref().ok_or_else(|| {
                SideConversationError::IdempotencyConflict(
                    "deterministic group id belongs to a non-Side Conversation group".to_string(),
                )
            })?;
            if metadata.request_payload_sha256 != payload_digest {
                return Err(SideConversationError::IdempotencyConflict(
                    "request id was reused with different title or model".to_string(),
                ));
            }
            return Ok(SideConversationCreateReceipt {
                entry: self.enriched_entry(&store, group).await?,
                duplicate: true,
            });
        }

        let source_messages = store
            .get_messages(&parent.conversation_id)
            .await
            .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
        let snapshot_source = source_messages
            .into_iter()
            .filter(|message| !is_internal_agent_message(message.attachments_json.as_deref()))
            .collect::<Vec<_>>();
        let snapshot_last_message_id = snapshot_source
            .iter()
            .filter_map(|message| message.id)
            .max();
        let snapshot_message_count = snapshot_source.len();
        let snapshot = copy_snapshot(&snapshot_source, &child_id);
        let title = normalized_title(request.title.as_deref(), &request.prompt);
        let existing_child = store
            .get_conversation(&child_id)
            .await
            .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
        let mut child_created = existing_child.is_none();
        if child_created {
            let created = store
                .create_conversation(NewConversation {
                    conversation_id: child_id.clone(),
                    user_id: parent.user_id,
                    agent_type: parent.agent_type,
                    title: Some(title.clone()),
                })
                .await;
            if let Err(error) = created {
                let concurrent_child =
                    store
                        .get_conversation(&child_id)
                        .await
                        .map_err(|read_error| {
                            SideConversationError::Conversation(format!(
                                "{error}; deterministic child re-read failed: {read_error}"
                            ))
                        })?;
                if concurrent_child.is_none() {
                    return Err(SideConversationError::Conversation(error.to_string()));
                }
                child_created = false;
            }
        }

        let existing_messages = store
            .get_messages(&child_id)
            .await
            .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
        if existing_messages.is_empty() {
            if let Err(error) = store.save_messages(&child_id, &snapshot).await {
                return Err(
                    rollback_child(&store, &child_id, child_created, error.to_string()).await,
                );
            }
        } else if !same_snapshot(&existing_messages, &snapshot) {
            return Err(SideConversationError::IdempotencyConflict(
                "deterministic child conversation contains different messages".to_string(),
            ));
        }

        let now = Utc::now();
        let group = AgentGroup {
            group_id,
            name: title.clone(),
            leader: AgentAddress::new(
                WorkspaceId::from_raw(request.workspace_id.clone()),
                request.parent_conversation_id,
            ),
            members: vec![AgentGroupMember {
                address: AgentAddress::new(
                    WorkspaceId::from_raw(request.workspace_id),
                    child_id.clone(),
                ),
                subagent_role: SIDE_CONVERSATION_ROLE.to_string(),
                label: Some(title),
            }],
            side_conversation: Some(SideConversationGroupMetadata {
                request_payload_sha256: payload_digest.clone(),
                initial_prompt,
                model_id: normalized_optional(request.model_id),
                snapshot_message_count,
                snapshot_last_message_id,
                viewed_at: Some(now),
                launch_error: None,
            }),
            created_at: now,
            updated_at: now,
        };
        let group = match self.router.create_group_record(group).await {
            Ok(group) => group,
            Err(AgentRouterError::IdCollision { .. }) => {
                let groups = self.router.list_group_records().await?;
                let existing = groups
                    .iter()
                    .find(|existing| existing.group_id == group_id_for_child(&child_id))
                    .ok_or_else(|| {
                        SideConversationError::IdempotencyConflict(
                            "deterministic group collided but could not be reloaded".to_string(),
                        )
                    })?;
                let metadata = existing.side_conversation.as_ref().ok_or_else(|| {
                    SideConversationError::IdempotencyConflict(
                        "deterministic group id belongs to another group kind".to_string(),
                    )
                })?;
                if metadata.request_payload_sha256 != payload_digest {
                    return Err(SideConversationError::IdempotencyConflict(
                        "concurrent request reused an id with different payload".to_string(),
                    ));
                }
                return Ok(SideConversationCreateReceipt {
                    entry: self.enriched_entry(&store, existing).await?,
                    duplicate: true,
                });
            }
            Err(error) => {
                return Err(
                    rollback_child(&store, &child_id, child_created, error.to_string()).await,
                );
            }
        };
        Ok(SideConversationCreateReceipt {
            entry: self.enriched_entry(&store, &group).await?,
            duplicate: false,
        })
    }

    pub async fn list_for_parent(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
        parent_conversation_id: &str,
    ) -> Result<Vec<SideConversationEntry>, SideConversationError> {
        let mut entries = Vec::new();
        for group in self.router.list_group_records().await? {
            if group.side_conversation.is_none()
                || group.leader.workspace_id.as_str() != workspace_id
                || group.leader.conversation_id != parent_conversation_id
            {
                continue;
            }
            entries.push(self.enriched_entry(&store, &group).await?);
        }
        entries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        Ok(entries)
    }

    pub async fn list_for_workspace(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
    ) -> Result<Vec<SideConversationEntry>, SideConversationError> {
        let mut entries = Vec::new();
        for group in self.router.list_group_records().await? {
            if group.side_conversation.is_none()
                || group.leader.workspace_id.as_str() != workspace_id
            {
                continue;
            }
            entries.push(self.enriched_entry(&store, &group).await?);
        }
        entries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        Ok(entries)
    }

    pub async fn relation_for_child(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Option<SideConversationEntry>, SideConversationError> {
        let groups = self.router.list_group_records().await?;
        let Some(group) = relation_for_child_in(&groups, workspace_id, conversation_id)? else {
            return Ok(None);
        };
        self.enriched_entry(&store, group).await.map(Some)
    }

    pub async fn is_side_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<bool, SideConversationError> {
        Ok(self
            .group_for_child(workspace_id, conversation_id)
            .await?
            .is_some())
    }

    pub async fn update_model(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
        conversation_id: &str,
        model_id: Option<String>,
    ) -> Result<SideConversationEntry, SideConversationError> {
        let model_id = normalized_optional(model_id);
        let group = self
            .mutate_group_for_child(workspace_id, conversation_id, move |group| {
                let metadata = group.side_conversation.as_mut().ok_or_else(|| {
                    AgentRouterError::Validation(
                        "group is missing Side Conversation metadata".to_string(),
                    )
                })?;
                metadata.model_id = model_id;
                Ok(())
            })
            .await?
            .ok_or_else(|| {
                SideConversationError::MalformedRelation(
                    "conversation is not a Side Conversation".to_string(),
                )
            })?;
        self.enriched_entry(&store, &group).await
    }

    pub async fn initial_prompt_for_child(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<String, SideConversationError> {
        let group = self
            .group_for_child(workspace_id, conversation_id)
            .await?
            .ok_or_else(|| {
                SideConversationError::MalformedRelation(
                    "conversation is not a Side Conversation".to_string(),
                )
            })?;
        group
            .side_conversation
            .map(|metadata| metadata.initial_prompt)
            .filter(|prompt| !prompt.trim().is_empty())
            .ok_or_else(|| {
                SideConversationError::MalformedRelation(
                    "Side Conversation initial prompt is unavailable".to_string(),
                )
            })
    }

    pub async fn set_launch_error(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
        conversation_id: &str,
        launch_error: Option<String>,
    ) -> Result<SideConversationEntry, SideConversationError> {
        let group = self
            .mutate_group_for_child(workspace_id, conversation_id, move |group| {
                let metadata = group.side_conversation.as_mut().ok_or_else(|| {
                    AgentRouterError::Validation(
                        "group is missing Side Conversation metadata".to_string(),
                    )
                })?;
                metadata.launch_error = launch_error;
                Ok(())
            })
            .await?
            .ok_or_else(|| {
                SideConversationError::MalformedRelation(
                    "conversation is not a Side Conversation".to_string(),
                )
            })?;
        self.enriched_entry(&store, &group).await
    }

    pub async fn rename(
        &self,
        store: Arc<dyn ConversationStore>,
        workspace_id: &str,
        conversation_id: &str,
        title: &str,
    ) -> Result<SideConversationEntry, SideConversationError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(SideConversationError::Validation(
                "title must not be empty".to_string(),
            ));
        }
        let title = title.chars().take(MAX_TITLE_CHARS).collect::<String>();
        store
            .update_conversation(conversation_id, Some(&title), None, None)
            .await
            .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
        let group = self
            .mutate_group_for_child(workspace_id, conversation_id, move |group| {
                group.name = title.clone();
                let member = group.members.first_mut().ok_or_else(|| {
                    AgentRouterError::Validation(
                        "Side Conversation group has no member".to_string(),
                    )
                })?;
                member.label = Some(title);
                Ok(())
            })
            .await?
            .ok_or_else(|| {
                SideConversationError::MalformedRelation(
                    "conversation is not a Side Conversation".to_string(),
                )
            })?;
        self.enriched_entry(&store, &group).await
    }

    pub async fn remove_relation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<bool, SideConversationError> {
        let Some(group) = self.group_for_child(workspace_id, conversation_id).await? else {
            return Ok(false);
        };
        self.router
            .delete_group_record(&group.group_id)
            .await
            .map_err(Into::into)
    }

    pub async fn mark_viewed(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<bool, SideConversationError> {
        let viewed_at = Utc::now();
        self.mutate_group_for_child(workspace_id, conversation_id, move |group| {
            let metadata = group.side_conversation.as_mut().ok_or_else(|| {
                AgentRouterError::Validation(
                    "group is missing Side Conversation metadata".to_string(),
                )
            })?;
            metadata.viewed_at = Some(viewed_at);
            Ok(())
        })
        .await
        .map(|group| group.is_some())
    }

    async fn mutate_group_for_child<F>(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        mutate: F,
    ) -> Result<Option<AgentGroup>, SideConversationError>
    where
        F: FnOnce(&mut AgentGroup) -> Result<(), AgentRouterError> + Send + 'static,
    {
        let Some(group) = self.group_for_child(workspace_id, conversation_id).await? else {
            return Ok(None);
        };
        let workspace_id = workspace_id.to_string();
        let conversation_id = conversation_id.to_string();
        self.router
            .mutate_group_record(group.group_id, move |latest| {
                let matches_child = latest.side_conversation.is_some()
                    && latest.leader.workspace_id.as_str() == workspace_id
                    && latest.members.len() == 1
                    && latest.members.first().is_some_and(|member| {
                        member.address.workspace_id.as_str() == workspace_id
                            && member.address.conversation_id == conversation_id
                            && member.subagent_role == SIDE_CONVERSATION_ROLE
                    });
                if !matches_child {
                    return Err(AgentRouterError::Validation(
                        "Side Conversation relation changed before update".to_string(),
                    ));
                }
                mutate(latest)
            })
            .await
            .map(Some)
            .map_err(Into::into)
    }

    async fn group_for_child(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Option<AgentGroup>, SideConversationError> {
        let groups = self.router.list_group_records().await?;
        Ok(relation_for_child_in(&groups, workspace_id, conversation_id)?.cloned())
    }

    async fn enriched_entry(
        &self,
        store: &Arc<dyn ConversationStore>,
        group: &AgentGroup,
    ) -> Result<SideConversationEntry, SideConversationError> {
        let mut entry = entry_from_group(store, group).await?;
        if entry.degraded {
            entry.status = SideConversationStatus::Degraded;
            return Ok(entry);
        }
        let target = AgentAddress::new(
            WorkspaceId::from_raw(entry.workspace_id.clone()),
            entry.conversation_id.clone(),
        );
        if self.router.target_exists(&target).await? {
            let records = self.router.records(&target).await?;
            if let Some(record) = records.iter().max_by_key(|record| record.persisted_at) {
                entry.status = match record.phase {
                    crate::agent_router::AgentDeliveryPhase::Persisted
                    | crate::agent_router::AgentDeliveryPhase::Deferred => {
                        SideConversationStatus::Queued
                    }
                    crate::agent_router::AgentDeliveryPhase::Claimed
                    | crate::agent_router::AgentDeliveryPhase::EffectStarted
                    | crate::agent_router::AgentDeliveryPhase::MailboxAccepted
                    | crate::agent_router::AgentDeliveryPhase::Drained => {
                        SideConversationStatus::Running
                    }
                    crate::agent_router::AgentDeliveryPhase::TurnSettled => match record.outcome {
                        Some(crate::agent_router::AgentDeliveryOutcome::Completed) => {
                            SideConversationStatus::Completed
                        }
                        Some(crate::agent_router::AgentDeliveryOutcome::Cancelled) => {
                            SideConversationStatus::Cancelled
                        }
                        Some(crate::agent_router::AgentDeliveryOutcome::Failed)
                        | Some(crate::agent_router::AgentDeliveryOutcome::Dropped)
                        | Some(crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown)
                        | None => SideConversationStatus::Failed,
                    },
                };
            }
        }
        Ok(entry)
    }
}

fn validate_request(request: &SideConversationCreateRequest) -> Result<(), SideConversationError> {
    AgentAddress::new(
        WorkspaceId::from_raw(request.workspace_id.clone()),
        request.parent_conversation_id.clone(),
    )
    .validate()
    .map_err(|error| SideConversationError::Validation(error.to_string()))?;
    if request.request_id.trim().is_empty()
        || request.request_id.chars().count() > MAX_REQUEST_ID_CHARS
    {
        return Err(SideConversationError::Validation(format!(
            "request_id must contain 1-{MAX_REQUEST_ID_CHARS} characters"
        )));
    }
    if request.prompt.trim().is_empty() {
        return Err(SideConversationError::Validation(
            "prompt is required".to_string(),
        ));
    }
    if request.prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(SideConversationError::Validation(format!(
            "prompt exceeds {MAX_PROMPT_CHARS} characters"
        )));
    }
    if request
        .title
        .as_deref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(SideConversationError::Validation(
            "title must not be empty when provided".to_string(),
        ));
    }
    if request
        .model_id
        .as_deref()
        .is_some_and(|model_id| model_id.trim().is_empty())
    {
        return Err(SideConversationError::Validation(
            "model_id must not be empty when provided".to_string(),
        ));
    }
    Ok(())
}

fn identity_digest(
    request: &SideConversationCreateRequest,
) -> Result<String, SideConversationError> {
    digest_value(&(
        request.workspace_id.as_str(),
        request.parent_conversation_id.as_str(),
        request.request_id.as_str(),
    ))
}

fn group_id_for_child(child_id: &str) -> String {
    let digest = child_id
        .strip_prefix(SIDE_CONVERSATION_ID_PREFIX)
        .unwrap_or(child_id);
    format!("{SIDE_CONVERSATION_GROUP_PREFIX}{digest}")
}

fn request_payload_digest(
    request: &SideConversationCreateRequest,
) -> Result<String, SideConversationError> {
    digest_value(&(
        request.workspace_id.as_str(),
        request.parent_conversation_id.as_str(),
        request.request_id.as_str(),
        request.prompt.as_str(),
        request.title.as_deref(),
        request.model_id.as_deref(),
    ))
}

fn digest_value(value: &impl Serialize) -> Result<String, SideConversationError> {
    let bytes = echo_agent::utils::canonical_json::canonical_json_bytes(value)
        .map_err(|error| SideConversationError::Validation(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalized_title(requested: Option<&str>, prompt: &str) -> String {
    requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(MAX_TITLE_CHARS).collect())
        .or_else(|| {
            let prompt = prompt.trim();
            (!prompt.is_empty()).then(|| prompt.chars().take(80).collect())
        })
        .unwrap_or_else(|| DEFAULT_SIDE_TITLE.to_string())
}

fn copy_snapshot(source: &[StoredMessage], child_id: &str) -> Vec<StoredMessage> {
    source
        .iter()
        .cloned()
        .map(|mut message| {
            message.id = None;
            message.conversation_id = child_id.to_string();
            message.attachments_json = strip_conversation_local_ui(message.attachments_json);
            message
        })
        .collect()
}

fn strip_conversation_local_ui(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Some(raw);
    };
    let Some(object) = value.as_object_mut() else {
        return Some(raw);
    };
    object.remove("message_id");
    object.remove("execution_steps");
    object.remove("execution_rounds");
    serde_json::to_string(&value).ok().or(Some(raw))
}

pub fn is_internal_agent_message(raw: Option<&str>) -> bool {
    raw.and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| {
            value
                .get(INTERNAL_VISIBILITY_FIELD)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|value| value == INTERNAL_VISIBILITY_VALUE)
}

pub async fn mark_internal_agent_turn_messages(
    store: &dyn ConversationStore,
    conversation_id: &str,
    start_index: usize,
    instruction: &str,
) -> Result<(), SideConversationError> {
    let mut messages = store
        .get_messages(conversation_id)
        .await
        .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
    let turn_start = messages
        .iter()
        .enumerate()
        .skip(start_index)
        .find_map(|(index, message)| {
            (message.role == "user" && message.content.as_deref() == Some(instruction))
                .then_some(index)
        })
        .ok_or_else(|| {
            SideConversationError::Conversation(
                "internal Agent turn was not found in the committed transcript".to_string(),
            )
        })?;
    let turn_end = messages
        .iter()
        .enumerate()
        .skip(turn_start.saturating_add(1))
        .find_map(|(index, message)| (message.role == "user").then_some(index))
        .unwrap_or(messages.len());
    for message in messages
        .iter_mut()
        .skip(turn_start)
        .take(turn_end.saturating_sub(turn_start))
    {
        let mut projection = message
            .attachments_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let object = projection.as_object_mut().ok_or_else(|| {
            SideConversationError::Conversation(
                "internal message projection is not an object".to_string(),
            )
        })?;
        object.insert(
            INTERNAL_VISIBILITY_FIELD.to_string(),
            serde_json::Value::String(INTERNAL_VISIBILITY_VALUE.to_string()),
        );
        message.attachments_json = Some(
            serde_json::to_string(&projection)
                .map_err(|error| SideConversationError::Conversation(error.to_string()))?,
        );
    }
    store
        .save_messages(conversation_id, &messages)
        .await
        .map_err(|error| SideConversationError::Conversation(error.to_string()))
}

fn same_snapshot(existing: &[StoredMessage], expected: &[StoredMessage]) -> bool {
    existing.len() == expected.len()
        && existing.iter().zip(expected).all(|(left, right)| {
            left.role == right.role
                && left.content == right.content
                && left.attachments_json == right.attachments_json
                && left.tool_calls_json == right.tool_calls_json
                && left.tool_result_json == right.tool_result_json
        })
}

fn relation_for_child_in<'a>(
    groups: &'a [AgentGroup],
    workspace_id: &str,
    conversation_id: &str,
) -> Result<Option<&'a AgentGroup>, SideConversationError> {
    let mut matches = groups.iter().filter(|group| {
        group.side_conversation.is_some()
            && group.members.iter().any(|member| {
                member.address.workspace_id.as_str() == workspace_id
                    && member.address.conversation_id == conversation_id
            })
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(SideConversationError::MalformedRelation(
            "child belongs to more than one Side Conversation group".to_string(),
        ));
    }
    Ok(first)
}

async fn entry_from_group(
    store: &Arc<dyn ConversationStore>,
    group: &AgentGroup,
) -> Result<SideConversationEntry, SideConversationError> {
    let metadata = group.side_conversation.as_ref().ok_or_else(|| {
        SideConversationError::MalformedRelation(
            "group is missing Side Conversation metadata".to_string(),
        )
    })?;
    let member = group.members.first().ok_or_else(|| {
        SideConversationError::MalformedRelation(
            "Side Conversation group has no member".to_string(),
        )
    })?;
    let conversation = store
        .get_conversation(&member.address.conversation_id)
        .await
        .map_err(|error| SideConversationError::Conversation(error.to_string()))?;
    let (title, updated_at, degraded) = match conversation {
        Some(Conversation {
            title, updated_at, ..
        }) => (
            title.unwrap_or_else(|| group.name.clone()),
            DateTime::parse_from_rfc3339(&updated_at)
                .map(|value| value.with_timezone(&Utc))
                .unwrap_or(group.updated_at),
            false,
        ),
        None => (group.name.clone(), group.updated_at, true),
    };
    let unread_count = usize::from(
        !degraded
            && metadata
                .viewed_at
                .is_none_or(|viewed_at| updated_at > viewed_at),
    );
    Ok(SideConversationEntry {
        group_id: group.group_id.clone(),
        workspace_id: member.address.workspace_id.to_string(),
        parent_conversation_id: group.leader.conversation_id.clone(),
        conversation_id: member.address.conversation_id.clone(),
        title,
        model_id: metadata.model_id.clone(),
        snapshot_message_count: metadata.snapshot_message_count,
        snapshot_last_message_id: metadata.snapshot_last_message_id,
        created_at: group.created_at,
        updated_at,
        degraded,
        status: if degraded {
            SideConversationStatus::Degraded
        } else if metadata.launch_error.is_some() {
            SideConversationStatus::Failed
        } else {
            SideConversationStatus::Idle
        },
        unread_count,
        launch_error: metadata.launch_error.clone(),
    })
}

async fn rollback_child(
    store: &Arc<dyn ConversationStore>,
    child_id: &str,
    child_created: bool,
    operation: String,
) -> SideConversationError {
    if !child_created {
        return SideConversationError::Conversation(operation);
    }
    match store.delete_conversation(child_id).await {
        Ok(()) => SideConversationError::Conversation(operation),
        Err(error) => SideConversationError::Rollback {
            operation,
            rollback: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use echo_agent::memory::{
        ConversationStore, FileConversationStore, NewConversation, StoredMessage,
    };

    use crate::agent_router::AgentRouter;

    fn stored(conversation_id: &str, role: &str, content: &str, id: i64) -> StoredMessage {
        StoredMessage {
            id: Some(id),
            conversation_id: conversation_id.to_string(),
            role: role.to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: echo_agent::utils::time::now_local().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn side_conversation_create_is_idempotent_and_rejects_nesting() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(root.path().join("conversation-store"))
                .map_err(|error| error.to_string())?,
        );
        store
            .create_conversation(NewConversation {
                conversation_id: "main-conversation".to_string(),
                user_id: "eko".to_string(),
                agent_type: None,
                title: Some("Main".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut hidden = stored("main-conversation", "assistant", "内部协调消息", 3);
        hidden.attachments_json = Some(
            serde_json::json!({
                "eko_visibility": "internal_agent",
            })
            .to_string(),
        );
        store
            .save_messages(
                "main-conversation",
                &[
                    stored("main-conversation", "user", "先分析现状", 1),
                    stored("main-conversation", "assistant", "现状已分析", 2),
                    hidden,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;

        let service = SideConversationService::new(Arc::new(AgentRouter::new(
            root.path().join("agent-router"),
        )));
        let request = SideConversationCreateRequest {
            workspace_id: "workspace-1".to_string(),
            parent_conversation_id: "main-conversation".to_string(),
            request_id: "request-1".to_string(),
            prompt: "验证替代方案".to_string(),
            title: Some("验证替代方案".to_string()),
            model_id: Some("provider:model".to_string()),
        };
        let mut oversized = request.clone();
        oversized.prompt = "界".repeat(MAX_PROMPT_CHARS.saturating_add(1));
        assert!(matches!(
            validate_request(&oversized),
            Err(SideConversationError::Validation(_))
        ));
        let created = service
            .create(Arc::clone(&store), request.clone())
            .await
            .map_err(|error| error.to_string())?;
        let duplicate = service
            .create(Arc::clone(&store), request)
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(created.entry, duplicate.entry);
        assert!(!created.duplicate);
        assert!(duplicate.duplicate);
        assert!(
            service
                .router
                .list_groups()
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(
            service
                .router
                .delete_group(&created.entry.group_id)
                .await
                .is_err()
        );
        assert!(
            service
                .router
                .create_group(
                    "invalid reserved role",
                    AgentAddress::new(
                        WorkspaceId::from_raw("workspace-1".to_string()),
                        "main-conversation",
                    ),
                    vec![AgentGroupMember {
                        address: AgentAddress::new(
                            WorkspaceId::from_raw("workspace-1".to_string()),
                            "other-conversation",
                        ),
                        subagent_role: SIDE_CONVERSATION_ROLE.to_string(),
                        label: None,
                    }],
                )
                .await
                .is_err()
        );
        let conflict = service
            .create(
                Arc::clone(&store),
                SideConversationCreateRequest {
                    workspace_id: "workspace-1".to_string(),
                    parent_conversation_id: "main-conversation".to_string(),
                    request_id: "request-1".to_string(),
                    prompt: "不同的请求内容".to_string(),
                    title: Some("验证替代方案".to_string()),
                    model_id: Some("provider:model".to_string()),
                },
            )
            .await;
        assert!(matches!(
            conflict,
            Err(SideConversationError::IdempotencyConflict(_))
        ));
        assert_eq!(created.entry.snapshot_message_count, 2);
        assert_eq!(created.entry.snapshot_last_message_id, Some(2));
        assert_eq!(created.entry.model_id.as_deref(), Some("provider:model"));
        assert_eq!(created.entry.launch_error, None);
        assert_eq!(
            service
                .initial_prompt_for_child("workspace-1", &created.entry.conversation_id)
                .await
                .map_err(|error| error.to_string())?,
            "验证替代方案"
        );
        let launch_failed = service
            .set_launch_error(
                Arc::clone(&store),
                "workspace-1",
                &created.entry.conversation_id,
                Some("model unavailable".to_string()),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(launch_failed.status, SideConversationStatus::Failed);
        assert_eq!(
            launch_failed.launch_error.as_deref(),
            Some("model unavailable")
        );
        let launch_retried = service
            .set_launch_error(
                Arc::clone(&store),
                "workspace-1",
                &created.entry.conversation_id,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(launch_retried.launch_error, None);
        assert!(
            service
                .is_side_conversation("workspace-1", &created.entry.conversation_id)
                .await
                .map_err(|error| error.to_string())?
        );
        let copied = store
            .get_messages(&created.entry.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(copied.len(), 2);
        assert!(
            copied
                .iter()
                .all(|message| message.conversation_id == created.entry.conversation_id)
        );
        assert_eq!(
            service
                .list_for_parent(Arc::clone(&store), "workspace-1", "main-conversation")
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let renamed = service
            .rename(
                Arc::clone(&store),
                "workspace-1",
                &created.entry.conversation_id,
                "新的支线标题",
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(renamed.title, "新的支线标题");
        let inherited = service
            .update_model(
                Arc::clone(&store),
                "workspace-1",
                &created.entry.conversation_id,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(inherited.model_id, None);
        let mut messages = store
            .get_messages(&created.entry.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        messages.push(stored(
            &created.entry.conversation_id,
            "user",
            "内部协调请求",
            3,
        ));
        messages.push(stored(
            &created.entry.conversation_id,
            "assistant",
            "内部协调结果",
            4,
        ));
        messages.push(stored(
            &created.entry.conversation_id,
            "user",
            "后续用户消息",
            5,
        ));
        store
            .save_messages(&created.entry.conversation_id, &messages)
            .await
            .map_err(|error| error.to_string())?;
        mark_internal_agent_turn_messages(
            store.as_ref(),
            &created.entry.conversation_id,
            2,
            "内部协调请求",
        )
        .await
        .map_err(|error| error.to_string())?;
        let marked = store
            .get_messages(&created.entry.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            marked
                .first()
                .is_some_and(|message| !is_internal_agent_message(
                    message.attachments_json.as_deref()
                ))
        );
        assert!(
            marked
                .get(2)
                .is_some_and(|message| is_internal_agent_message(
                    message.attachments_json.as_deref()
                ))
        );
        assert!(
            marked
                .get(3)
                .is_some_and(|message| is_internal_agent_message(
                    message.attachments_json.as_deref()
                ))
        );
        assert!(
            marked
                .get(4)
                .is_some_and(|message| !is_internal_agent_message(
                    message.attachments_json.as_deref()
                ))
        );

        let nested = service
            .create(
                Arc::clone(&store),
                SideConversationCreateRequest {
                    workspace_id: "workspace-1".to_string(),
                    parent_conversation_id: created.entry.conversation_id.clone(),
                    request_id: "nested-request".to_string(),
                    prompt: "继续创建".to_string(),
                    title: None,
                    model_id: None,
                },
            )
            .await;
        assert!(matches!(
            nested,
            Err(SideConversationError::NestedNotAllowed)
        ));
        store
            .delete_conversation(&created.entry.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        let degraded = service
            .list_for_parent(Arc::clone(&store), "workspace-1", "main-conversation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(degraded.len(), 1);
        assert!(degraded.first().is_some_and(|entry| entry.degraded));
        assert!(
            service
                .remove_relation("workspace-1", &created.entry.conversation_id)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(
            service
                .list_for_parent(Arc::clone(&store), "workspace-1", "main-conversation")
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_identical_create_publishes_one_relation() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(root.path().join("conversation-store"))
                .map_err(|error| error.to_string())?,
        );
        store
            .create_conversation(NewConversation {
                conversation_id: "main".to_string(),
                user_id: "eko".to_string(),
                agent_type: None,
                title: Some("Main".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;
        let service = SideConversationService::new(Arc::new(AgentRouter::new(
            root.path().join("agent-router"),
        )));
        let request = SideConversationCreateRequest {
            workspace_id: "workspace-1".to_string(),
            parent_conversation_id: "main".to_string(),
            request_id: "same-request".to_string(),
            prompt: "compare implementations".to_string(),
            title: None,
            model_id: None,
        };
        let first = service.create(Arc::clone(&store), request.clone());
        let second = service.create(Arc::clone(&store), request);
        let (first, second) = tokio::join!(first, second);
        let first = first.map_err(|error| error.to_string())?;
        let second = second.map_err(|error| error.to_string())?;
        assert_eq!(first.entry.conversation_id, second.entry.conversation_id);
        assert_ne!(first.duplicate, second.duplicate);
        assert_eq!(
            service
                .list_for_parent(Arc::clone(&store), "workspace-1", "main")
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_side_metadata_updates_preserve_independent_fields() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(root.path().join("conversation-store"))
                .map_err(|error| error.to_string())?,
        );
        store
            .create_conversation(NewConversation {
                conversation_id: "main".to_string(),
                user_id: "eko".to_string(),
                agent_type: None,
                title: Some("Main".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;
        let router = Arc::new(AgentRouter::new(root.path().join("agent-router")));
        let service = SideConversationService::new(Arc::clone(&router));
        let created = service
            .create(
                Arc::clone(&store),
                SideConversationCreateRequest {
                    workspace_id: "workspace-1".to_string(),
                    parent_conversation_id: "main".to_string(),
                    request_id: "metadata-race".to_string(),
                    prompt: "compare metadata updates".to_string(),
                    title: None,
                    model_id: None,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let child_id = created.entry.conversation_id;
        service
            .set_launch_error(
                Arc::clone(&store),
                "workspace-1",
                &child_id,
                Some("old launch error".to_string()),
            )
            .await
            .map_err(|error| error.to_string())?;

        let model = service.update_model(
            Arc::clone(&store),
            "workspace-1",
            &child_id,
            Some("provider:model-b".to_string()),
        );
        let viewed = service.mark_viewed("workspace-1", &child_id);
        let (model, viewed) = tokio::join!(model, viewed);
        model.map_err(|error| error.to_string())?;
        assert!(viewed.map_err(|error| error.to_string())?);

        let launch_error = service.set_launch_error(
            Arc::clone(&store),
            "workspace-1",
            &child_id,
            Some("new launch error".to_string()),
        );
        let viewed = service.mark_viewed("workspace-1", &child_id);
        let (launch_error, viewed) = tokio::join!(launch_error, viewed);
        launch_error.map_err(|error| error.to_string())?;
        assert!(viewed.map_err(|error| error.to_string())?);

        let metadata = router
            .list_group_records()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|group| group.group_id == created.entry.group_id)
            .and_then(|group| group.side_conversation)
            .ok_or_else(|| "Side Conversation metadata is missing".to_string())?;
        assert_eq!(metadata.model_id.as_deref(), Some("provider:model-b"));
        assert_eq!(metadata.launch_error.as_deref(), Some("new launch error"));
        assert!(metadata.viewed_at.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn restart_restores_relation_configuration_and_stable_first_input() -> Result<(), String>
    {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let conversation_root = root.path().join("conversations");
        let router_root = root.path().join("agent-router");
        let event_root = root.path().join("chat-events");
        let store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(&conversation_root).map_err(|error| error.to_string())?,
        );
        store
            .create_conversation(NewConversation {
                conversation_id: "main".to_string(),
                user_id: "eko".to_string(),
                agent_type: None,
                title: Some("Main".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;
        let router = Arc::new(AgentRouter::new(router_root.clone()));
        let service = SideConversationService::new(Arc::clone(&router));
        let created = service
            .create(
                Arc::clone(&store),
                SideConversationCreateRequest {
                    workspace_id: "workspace-1".to_string(),
                    parent_conversation_id: "main".to_string(),
                    request_id: "restart-request".to_string(),
                    prompt: "resume this prompt".to_string(),
                    title: Some("Restart proof".to_string()),
                    model_id: Some("provider:model".to_string()),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let child_id = created.entry.conversation_id.clone();
        let group_id = created.entry.group_id.clone();
        service
            .set_launch_error(
                Arc::clone(&store),
                "workspace-1",
                &child_id,
                Some("launch interrupted".to_string()),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .mark_viewed("workspace-1", &child_id)
            .await
            .map_err(|error| error.to_string())?;

        let input_address = crate::conversation_input::ConversationInputAddress {
            workspace_id: "workspace-1".to_string(),
            conversation_id: child_id.clone(),
        };
        let input_id = format!("side-start:{group_id}");
        let log = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                &event_root,
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let inputs = crate::conversation_input::ConversationInputService::new(Arc::clone(&log));
        inputs
            .submit(
                input_address.clone(),
                input_id.clone(),
                "resume this prompt".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        drop((inputs, log, service, router, store));

        let reopened_store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(&conversation_root).map_err(|error| error.to_string())?,
        );
        let reopened_router = Arc::new(AgentRouter::new(router_root));
        let reopened_service = SideConversationService::new(Arc::clone(&reopened_router));
        let restored = reopened_service
            .relation_for_child(Arc::clone(&reopened_store), "workspace-1", &child_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Side Conversation relation was not restored".to_string())?;
        assert_eq!(restored.model_id.as_deref(), Some("provider:model"));
        assert_eq!(restored.launch_error.as_deref(), Some("launch interrupted"));
        assert_eq!(
            reopened_service
                .initial_prompt_for_child("workspace-1", &child_id)
                .await
                .map_err(|error| error.to_string())?,
            "resume this prompt"
        );
        let metadata = reopened_router
            .list_group_records()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|group| group.group_id == group_id)
            .and_then(|group| group.side_conversation)
            .ok_or_else(|| "Side Conversation metadata was not restored".to_string())?;
        assert!(metadata.viewed_at.is_some());

        let reopened_log = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                &event_root,
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let reopened_inputs =
            crate::conversation_input::ConversationInputService::new(Arc::clone(&reopened_log));
        let duplicate = reopened_inputs
            .submit(
                input_address.clone(),
                input_id.clone(),
                "resume this prompt".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(
            duplicate.phase,
            crate::conversation_input::ConversationInputPhase::Persisted
        );
        let frontier = reopened_inputs
            .list(&input_address)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(frontier.items.len(), 1);
        let started = reopened_inputs
            .dispatch_selected(
                duplicate.identity,
                frontier.queue_revision,
                input_id.clone(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let attempt = started
            .active_attempt
            .ok_or_else(|| "first input attempt was not restored".to_string())?;
        reopened_inputs
            .mailbox_accepted(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        reopened_inputs
            .drained(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        reopened_inputs
            .turn_settled(
                attempt,
                crate::conversation_input::ConversationInputOutcome::Completed,
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        drop((reopened_inputs, reopened_log));

        let settled_inputs = crate::conversation_input::ConversationInputService::new(Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                &event_root,
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        ));
        let settled_duplicate = settled_inputs
            .submit(
                input_address.clone(),
                input_id,
                "resume this prompt".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(settled_duplicate.duplicate);
        assert_eq!(
            settled_duplicate.phase,
            crate::conversation_input::ConversationInputPhase::TurnSettled
        );
        assert!(
            settled_inputs
                .list(&input_address)
                .await
                .map_err(|error| error.to_string())?
                .items
                .is_empty()
        );
        Ok(())
    }
}
