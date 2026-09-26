//! Caller-owned replacement and recovery of managed conversation history.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use echo_agent::agent::Agent;
use echo_agent::llm::types::Message;
use echo_agent::memory::{
    ConversationProjectionAuthority, ConversationProjectionLifecycle, ConversationStore,
    EnsureConversationProjectionRequest, ManagedConversationImport,
    ManagedConversationMetadataUpdate, ManagedConversationMetadataUpdateStatus, NewConversation,
    PersistenceCallContext, StoredMessage, TranscriptProjectionApplyReceipt,
    TranscriptProjectionApplyStatus, TranscriptProjectionSettlementStatus,
    managed_import_projection_digests, restore_messages,
};
use echo_agent::state::{
    AgentCheckpoint, ManagedImportEpochTransition, ManagedRuntimeStateSnapshot,
    RuntimeCheckpointCasRequest, RuntimeCheckpointCasStatus, RuntimeStateExpectedVersion,
    RuntimeStateStore, RuntimeStateVersion, TranscriptProjectionCheckpoint,
    TranscriptProjectionMessage, settle_pending_transcript_projection,
};

use crate::agent_handle::AgentHandle;

/// Held across import, runtime CAS and Agent hydration by direct UI commands.
#[must_use]
pub struct ManagedConversationAdmission {
    workspace_id: String,
    conversation_id: String,
    _identity: crate::conversation_deletion::ConversationIdentityGuard,
    _suspension: crate::foreground_turn::ForegroundConversationSuspension,
}

impl ManagedConversationAdmission {
    pub(crate) fn new(
        workspace_id: &str,
        conversation_id: &str,
        identity: crate::conversation_deletion::ConversationIdentityGuard,
        suspension: crate::foreground_turn::ForegroundConversationSuspension,
    ) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
            _identity: identity,
            _suspension: suspension,
        }
    }

    pub(crate) fn matches(&self, workspace_id: &str, conversation_id: &str) -> bool {
        self.workspace_id == workspace_id && self.conversation_id == conversation_id
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        crate::conversation_deletion::ConversationIdentityGuard,
        crate::foreground_turn::ForegroundConversationSuspension,
    ) {
        (self._identity, self._suspension)
    }
}

/// Remove the last complete user turn, including its tool calls and results.
pub fn take_last_user_turn(messages: &mut Vec<StoredMessage>) -> Option<StoredMessage> {
    let index = messages
        .iter()
        .rposition(|message| message.role == "user")?;
    let removed_user = messages.get(index).cloned();
    messages.truncate(index);
    removed_user
}

pub(crate) fn canonical_rows(
    conversation_id: &str,
    messages: &[StoredMessage],
) -> Vec<StoredMessage> {
    messages
        .iter()
        .cloned()
        .map(|mut message| {
            message.id = None;
            message.conversation_id = conversation_id.to_string();
            message
        })
        .collect()
}

fn validate_import_history(conversation_id: &str, messages: &[StoredMessage]) -> Result<()> {
    let restored = restore_messages(messages)?;
    let digests = managed_import_projection_digests(conversation_id, messages)?;
    let projected = digests
        .into_iter()
        .enumerate()
        .map(|(ordinal, digest)| {
            Ok(TranscriptProjectionMessage {
                ordinal: u64::try_from(ordinal).context("import ordinal capacity exhausted")?,
                digest,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let checkpoint = AgentCheckpoint {
        conversation_id: conversation_id.to_string(),
        messages_json: AgentCheckpoint::serialize_payload(
            restored,
            Some(TranscriptProjectionCheckpoint {
                generation_id: conversation_id.to_string(),
                next_ordinal: u64::try_from(projected.len())
                    .context("import cursor capacity exhausted")?,
                projected,
            }),
        )?,
        current_plan: None,
        active_skills: Vec::new(),
        blocked_reason: None,
        working_dir: None,
        timestamp: Utc::now(),
    };
    checkpoint.restore_managed_runtime_payload()?;
    Ok(())
}

async fn authority(
    store: &dyn ConversationStore,
    conversation_id: &str,
) -> Result<ConversationProjectionAuthority> {
    if let Some(authority) = store.get_projection_authority(conversation_id).await? {
        if authority.lifecycle != ConversationProjectionLifecycle::Live {
            bail!("conversation {conversation_id} is not live");
        }
        return Ok(authority);
    }
    let conversation = store
        .get_conversation(conversation_id)
        .await?
        .with_context(|| format!("conversation {conversation_id} does not exist"))?;
    let receipt = store
        .ensure_projection_epoch(EnsureConversationProjectionRequest {
            conversation: NewConversation {
                conversation_id: conversation.conversation_id,
                user_id: conversation.user_id,
                agent_type: conversation.agent_type,
                title: conversation.title,
            },
            expected_tombstone_epoch: None,
        })
        .await?;
    if receipt.authority.lifecycle != ConversationProjectionLifecycle::Live {
        bail!("conversation {conversation_id} could not acquire a live epoch");
    }
    Ok(receipt.authority)
}

async fn settled_state(
    store: &dyn ConversationStore,
    runtime_state: &dyn RuntimeStateStore,
    conversation_id: &str,
) -> Result<Option<ManagedRuntimeStateSnapshot>> {
    let existing = runtime_state
        .load_runtime_state(conversation_id, conversation_id)
        .await?;
    if existing.is_none() {
        return Ok(None);
    }
    let context = PersistenceCallContext::with_timeout(Duration::from_secs(30))?;
    let settlement = settle_pending_transcript_projection(
        store,
        runtime_state,
        conversation_id,
        conversation_id,
        context,
    )
    .await?;
    if settlement.status != TranscriptProjectionSettlementStatus::Settled {
        bail!("conversation {conversation_id} has an unsettled transcript projection");
    }
    Ok(runtime_state
        .load_runtime_state(conversation_id, conversation_id)
        .await?)
}

fn expected_version(version: &RuntimeStateVersion) -> Result<RuntimeStateExpectedVersion> {
    match version {
        RuntimeStateVersion::Absent => Ok(RuntimeStateExpectedVersion::Absent),
        RuntimeStateVersion::Unmanaged { digest } => Ok(RuntimeStateExpectedVersion::Unmanaged {
            digest: digest.clone(),
        }),
        RuntimeStateVersion::Managed { revision } => Ok(RuntimeStateExpectedVersion::Managed {
            revision: *revision,
        }),
        RuntimeStateVersion::Retired { .. } => bail!("runtime generation is retired"),
    }
}

async fn checkpoint_import(
    runtime_state: &dyn RuntimeStateStore,
    agent: &AgentHandle,
    import: ManagedConversationImport,
    receipt: TranscriptProjectionApplyReceipt,
    state_before: Option<ManagedRuntimeStateSnapshot>,
) -> Result<usize> {
    if !matches!(
        receipt.status,
        TranscriptProjectionApplyStatus::Applied | TranscriptProjectionApplyStatus::AlreadyApplied
    ) {
        bail!(
            "managed transcript import did not commit: {:?}",
            receipt.status
        );
    }
    let conversation_id = import.conversation_id.clone();
    let scope = match state_before.as_ref() {
        Some(state) => Some(state.scope.clone()),
        None => runtime_state.load_scope_authority(&conversation_id).await?,
    };
    let expected_scope_revision = scope.as_ref().map_or(0, |scope| scope.revision);
    let expected_state_version = state_before
        .as_ref()
        .map(|state| expected_version(&state.version))
        .transpose()?
        .unwrap_or(RuntimeStateExpectedVersion::Absent);
    let digests = managed_import_projection_digests(&conversation_id, &import.messages)?;
    let next_ordinal = u64::try_from(digests.len()).context("import cursor capacity exhausted")?;
    let projected = digests
        .into_iter()
        .enumerate()
        .map(|(ordinal, digest)| {
            Ok(TranscriptProjectionMessage {
                ordinal: u64::try_from(ordinal).context("import ordinal capacity exhausted")?,
                digest,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut messages = restore_messages(&import.messages)?;
    let message_count = messages.len();
    let system_prompt = agent.read(|agent| agent.system_prompt().to_string()).await;
    if !system_prompt.is_empty() {
        messages.insert(0, Message::system(system_prompt));
    }
    let previous = state_before.and_then(|state| state.checkpoint);
    let checkpoint = AgentCheckpoint {
        conversation_id: conversation_id.clone(),
        messages_json: AgentCheckpoint::serialize_payload(
            messages,
            Some(TranscriptProjectionCheckpoint {
                generation_id: conversation_id.clone(),
                next_ordinal,
                projected,
            }),
        )?,
        current_plan: None,
        active_skills: previous
            .as_ref()
            .map(|checkpoint| checkpoint.active_skills.clone())
            .unwrap_or_default(),
        blocked_reason: None,
        working_dir: previous.and_then(|checkpoint| checkpoint.working_dir),
        timestamp: Utc::now(),
    };
    let managed_import = Some(ManagedImportEpochTransition {
        request: import,
        receipt: receipt.clone(),
    });
    let saved = runtime_state
        .compare_and_save_checkpoint(RuntimeCheckpointCasRequest {
            scope_id: conversation_id.clone(),
            runtime_state_id: conversation_id.clone(),
            conversation_epoch: Some(receipt.authority.epoch),
            expected_scope_revision,
            expected_state_version,
            checkpoint,
            managed_import,
        })
        .await?;
    if !matches!(
        saved.status,
        RuntimeCheckpointCasStatus::Applied | RuntimeCheckpointCasStatus::AlreadyCurrent
    ) {
        bail!(
            "managed import checkpoint was not committed: {:?}",
            saved.status
        );
    }
    let restored = agent
        .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
        .await?;
    if restored.is_none() {
        bail!("managed import checkpoint disappeared before Agent hydration");
    }
    Ok(message_count)
}

async fn fresh_import_and_resume(
    store: &dyn ConversationStore,
    runtime_state: &dyn RuntimeStateStore,
    agent: &AgentHandle,
    conversation_id: &str,
    rows: Vec<StoredMessage>,
    state_before: Option<ManagedRuntimeStateSnapshot>,
    current: ConversationProjectionAuthority,
) -> Result<usize> {
    validate_import_history(conversation_id, &rows)?;
    let import = ManagedConversationImport::prepare_for_generation(
        conversation_id,
        current.epoch,
        current.revision,
        conversation_id,
        rows,
    )?;
    let receipt = store.import_managed_messages(import.clone()).await?;
    checkpoint_import(runtime_state, agent, import, receipt, state_before).await
}

/// Replace one idle conversation's visible history, then hydrate its Agent.
/// The caller must hold the conversation's turn admission for this entire call.
pub async fn replace_and_resume(
    store: &dyn ConversationStore,
    runtime_state: &dyn RuntimeStateStore,
    agent: &AgentHandle,
    conversation_id: &str,
    messages: &[StoredMessage],
) -> Result<usize> {
    let rows = canonical_rows(conversation_id, messages);
    validate_import_history(conversation_id, &rows)?;
    let mut state_before = settled_state(store, runtime_state, conversation_id).await?;
    let mut current = authority(store, conversation_id).await?;
    let scope_epoch = state_before
        .as_ref()
        .and_then(|state| state.scope.conversation_epoch);
    if scope_epoch.is_some_and(|epoch| epoch != current.epoch)
        || (state_before.is_none()
            && store
                .get_latest_managed_import(conversation_id)
                .await?
                .is_some())
    {
        resume_or_import(store, runtime_state, agent, conversation_id).await?;
        let committed =
            canonical_rows(conversation_id, &store.get_messages(conversation_id).await?);
        if committed == rows {
            return Ok(rows.len());
        }
        state_before = settled_state(store, runtime_state, conversation_id).await?;
        current = authority(store, conversation_id).await?;
    }
    fresh_import_and_resume(
        store,
        runtime_state,
        agent,
        conversation_id,
        rows,
        state_before,
        current,
    )
    .await
}

/// Resume a matching checkpoint or repair an import committed before its CAS.
pub async fn resume_or_import(
    store: &dyn ConversationStore,
    runtime_state: &dyn RuntimeStateStore,
    agent: &AgentHandle,
    conversation_id: &str,
) -> Result<usize> {
    let state_before = settled_state(store, runtime_state, conversation_id).await?;
    let current = authority(store, conversation_id).await?;
    let stored = store.get_messages(conversation_id).await?;
    if let Some(state) = state_before.as_ref()
        && state.scope.conversation_epoch == Some(current.epoch)
        && state.checkpoint.is_some()
    {
        let restored = agent
            .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
            .await?;
        let checkpoint = restored.context("managed runtime checkpoint disappeared")?;
        return Ok(checkpoint.restore_messages()?.len().saturating_sub(1));
    }
    let rows = canonical_rows(conversation_id, &stored);
    let scope_epoch = state_before
        .as_ref()
        .and_then(|state| state.scope.conversation_epoch);
    let locator = store.get_latest_managed_import(conversation_id).await?;
    if let Some(locator) = locator
        && locator.applied_epoch == current.epoch
        && scope_epoch != Some(current.epoch)
    {
        if scope_epoch.is_some_and(|epoch| epoch != locator.expected_epoch)
            || locator.generation_id != conversation_id
        {
            bail!("conversation {conversation_id} import locator does not match runtime scope");
        }
        let import = ManagedConversationImport::prepare_for_generation(
            conversation_id,
            locator.expected_epoch,
            locator.expected_revision,
            conversation_id,
            rows,
        )?;
        if !locator.matches(&import) {
            bail!("conversation {conversation_id} import locator does not match durable rows");
        }
        let receipt = store.import_managed_messages(import.clone()).await?;
        if receipt.status != TranscriptProjectionApplyStatus::AlreadyApplied {
            bail!("conversation {conversation_id} has no matching committed import receipt");
        }
        return checkpoint_import(runtime_state, agent, import, receipt, state_before).await;
    }
    if scope_epoch.is_some_and(|epoch| epoch != current.epoch) {
        bail!("conversation {conversation_id} has no matching committed import locator");
    }
    if stored.is_empty() && state_before.is_none() {
        return Ok(0);
    }
    fresh_import_and_resume(
        store,
        runtime_state,
        agent,
        conversation_id,
        rows,
        state_before,
        current,
    )
    .await
}

pub async fn update_title(
    store: &dyn ConversationStore,
    conversation_id: &str,
    title: &str,
) -> Result<()> {
    let current = authority(store, conversation_id).await?;
    let request = ManagedConversationMetadataUpdate::prepare(
        conversation_id,
        current.epoch,
        current.revision,
        Some(title.to_string()),
        None,
        None,
    )?;
    let receipt = store.update_managed_conversation(request).await?;
    if !matches!(
        receipt.status,
        ManagedConversationMetadataUpdateStatus::Updated
            | ManagedConversationMetadataUpdateStatus::AlreadyUpdated
    ) {
        bail!("conversation title update conflicted: {:?}", receipt.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::memory::{ConversationStore, FileConversationStore};
    use echo_agent::state::{FileRuntimeStateStore, RuntimeStateStore};

    use super::*;

    fn test_agent(
        conversation_id: &str,
        store: Arc<FileConversationStore>,
        runtime_state: Arc<FileRuntimeStateStore>,
    ) -> Result<AgentHandle> {
        Ok(AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                .system_prompt("current system prompt")
                .conversation_id(conversation_id)
                .conversation_store(store)
                .state_store(runtime_state)
                .build()?,
        ))
    }

    async fn test_conversation(store: &FileConversationStore, conversation_id: &str) -> Result<()> {
        store
            .ensure_projection_epoch(EnsureConversationProjectionRequest {
                conversation: NewConversation {
                    conversation_id: conversation_id.to_string(),
                    user_id: "test".to_string(),
                    agent_type: None,
                    title: None,
                },
                expected_tombstone_epoch: None,
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn branch_import_hydrates_current_prompt_and_prefix() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
        let runtime_state = Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?);
        test_conversation(store.as_ref(), "branch").await?;
        let agent = test_agent("branch", store.clone(), runtime_state.clone())?;
        let row = echo_agent::memory::project_message(
            "branch",
            &Message::user("parent context".to_string()),
        )?;
        assert_eq!(
            replace_and_resume(
                store.as_ref(),
                runtime_state.as_ref(),
                &agent,
                "branch",
                &[row]
            )
            .await?,
            1
        );
        let messages = agent
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .first()
                .and_then(|message| message.content.as_text())
                .is_some_and(|prompt| prompt.ends_with("current system prompt"))
        );
        assert_eq!(
            messages
                .get(1)
                .and_then(|message| message.content.as_text()),
            Some("parent context".to_string())
        );
        assert_eq!(store.get_messages("branch").await?.len(), 1);
        assert_eq!(
            runtime_state
                .load_runtime_state("branch", "branch")
                .await?
                .context("branch checkpoint missing")?
                .scope
                .conversation_epoch,
            Some(2)
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_side_snapshot_is_imported_before_first_turn() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
        let runtime_state = Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?);
        store
            .create_conversation(NewConversation {
                conversation_id: "side-child".to_string(),
                user_id: "test".to_string(),
                agent_type: None,
                title: Some("Side".to_string()),
            })
            .await?;
        store
            .save_messages(
                "side-child",
                &[echo_agent::memory::project_message(
                    "side-child",
                    &Message::user("parent snapshot".to_string()),
                )?],
            )
            .await?;
        let agent = test_agent("side-child", store.clone(), runtime_state.clone())?;
        assert_eq!(
            resume_or_import(store.as_ref(), runtime_state.as_ref(), &agent, "side-child").await?,
            1
        );
        let messages = agent
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .get(1)
                .and_then(|message| message.content.as_text()),
            Some("parent snapshot".to_string())
        );
        assert_eq!(
            store
                .get_projection_authority("side-child")
                .await?
                .context("Side projection epoch missing")?
                .epoch,
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_tool_history_cannot_commit_import() -> Result<()> {
        use echo_agent::llm::types::{FunctionCall, ToolCall};

        let root = tempfile::tempdir()?;
        let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
        let runtime_state = Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?);
        test_conversation(store.as_ref(), "invalid-history").await?;
        let agent = test_agent("invalid-history", store.clone(), runtime_state.clone())?;
        let incomplete = echo_agent::memory::project_message(
            "invalid-history",
            &Message::assistant_with_tools(vec![ToolCall {
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "inspect".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
        )?;
        assert!(
            replace_and_resume(
                store.as_ref(),
                runtime_state.as_ref(),
                &agent,
                "invalid-history",
                &[incomplete],
            )
            .await
            .is_err()
        );
        assert!(store.get_messages("invalid-history").await?.is_empty());
        assert_eq!(
            store
                .get_projection_authority("invalid-history")
                .await?
                .context("projection epoch missing")?
                .epoch,
            1
        );
        Ok(())
    }

    #[test]
    fn undo_removes_the_entire_tool_turn() -> Result<()> {
        use echo_agent::llm::types::{FunctionCall, ToolCall};

        let messages = vec![
            Message::user("earlier".to_string()),
            Message::assistant("done".to_string()),
            Message::user("latest".to_string()),
            Message::assistant_with_tools(vec![ToolCall {
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "inspect".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            Message::tool_result(
                "call-1".to_string(),
                "inspect".to_string(),
                "result".to_string(),
            ),
            Message::assistant("final".to_string()),
        ];
        let mut stored = echo_agent::memory::project_messages("undo", &messages)?;
        let removed = take_last_user_turn(&mut stored).context("user turn missing")?;
        assert_eq!(removed.content.as_deref(), Some("latest"));
        assert_eq!(stored.len(), 2);
        validate_import_history("undo", &stored)?;
        Ok(())
    }

    #[tokio::test]
    async fn committed_import_without_checkpoint_is_recovered_exactly() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
        let runtime_state = Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?);
        test_conversation(store.as_ref(), "recovery").await?;
        let agent = test_agent("recovery", store.clone(), runtime_state.clone())?;
        let initial = AgentCheckpoint {
            conversation_id: "recovery".to_string(),
            messages_json: AgentCheckpoint::serialize_payload(
                vec![Message::system("old prompt".to_string())],
                None,
            )?,
            current_plan: None,
            active_skills: Vec::new(),
            blocked_reason: None,
            working_dir: None,
            timestamp: Utc::now(),
        };
        runtime_state
            .compare_and_save_checkpoint(RuntimeCheckpointCasRequest {
                scope_id: "recovery".to_string(),
                runtime_state_id: "recovery".to_string(),
                conversation_epoch: Some(1),
                expected_scope_revision: 0,
                expected_state_version: RuntimeStateExpectedVersion::Absent,
                checkpoint: initial,
                managed_import: None,
            })
            .await?;
        let current = authority(store.as_ref(), "recovery").await?;
        let row = echo_agent::memory::project_message(
            "recovery",
            &Message::user("durable fork".to_string()),
        )?;
        let import = ManagedConversationImport::prepare_for_generation(
            "recovery",
            current.epoch,
            current.revision,
            "recovery",
            vec![row],
        )?;
        let applied = store.import_managed_messages(import.clone()).await?;
        assert_eq!(applied.status, TranscriptProjectionApplyStatus::Applied);
        let renamed = store
            .update_managed_conversation(ManagedConversationMetadataUpdate::prepare(
                "recovery",
                applied.authority.epoch,
                applied.authority.revision,
                Some("Renamed after import".to_string()),
                None,
                None,
            )?)
            .await?;
        assert_eq!(
            renamed.status,
            ManagedConversationMetadataUpdateStatus::Updated
        );
        assert_eq!(
            replace_and_resume(
                store.as_ref(),
                runtime_state.as_ref(),
                &agent,
                "recovery",
                &import.messages,
            )
            .await?,
            1
        );
        assert_eq!(
            store
                .get_projection_authority("recovery")
                .await?
                .context("recovered projection authority missing")?
                .epoch,
            2
        );
        assert_eq!(
            store
                .get_conversation("recovery")
                .await?
                .context("recovered conversation missing")?
                .title
                .as_deref(),
            Some("Renamed after import")
        );
        assert_eq!(
            resume_or_import(store.as_ref(), runtime_state.as_ref(), &agent, "recovery").await?,
            1
        );
        let messages = agent
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .get(1)
                .and_then(|message| message.content.as_text()),
            Some("durable fork".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn new_branch_import_crash_replays_without_advancing_epoch_again() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
        let runtime_state = Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?);
        test_conversation(store.as_ref(), "new-branch").await?;
        let agent = test_agent("new-branch", store.clone(), runtime_state.clone())?;
        let current = authority(store.as_ref(), "new-branch").await?;
        let row = echo_agent::memory::project_message(
            "new-branch",
            &Message::user("copied prefix".to_string()),
        )?;
        let import = ManagedConversationImport::prepare_for_generation(
            "new-branch",
            current.epoch,
            current.revision,
            "new-branch",
            vec![row],
        )?;
        assert_eq!(
            store.import_managed_messages(import.clone()).await?.status,
            TranscriptProjectionApplyStatus::Applied
        );
        assert_eq!(
            replace_and_resume(
                store.as_ref(),
                runtime_state.as_ref(),
                &agent,
                "new-branch",
                &import.messages,
            )
            .await?,
            1
        );
        assert_eq!(
            store
                .get_projection_authority("new-branch")
                .await?
                .context("new branch authority missing")?
                .epoch,
            2
        );
        Ok(())
    }
}
