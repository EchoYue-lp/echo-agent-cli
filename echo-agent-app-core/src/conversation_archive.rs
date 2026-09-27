//! EKO-owned visibility projection for persisted conversations.
//!
//! Archive and internal-turn provenance are product-level visibility facts,
//! not transcript state. Keeping them in the same existing file avoids EKO
//! fields in the reusable framework ConversationStore.

use chrono::{DateTime, Utc};
use echo_agent::memory::StoredMessage;
use echo_agent::utils::fs::atomic_write;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ArchiveFile {
    #[serde(default)]
    workspaces: HashMap<String, BTreeSet<String>>,
    #[serde(default)]
    internal_turns: HashMap<String, HashMap<String, InternalTurnProjection>>,
    #[serde(default)]
    handoffs: BTreeMap<String, ConversationHandoffIntent>,
    #[serde(default)]
    completed_handoffs: BTreeMap<String, CompletedConversationHandoff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationHandoffPhase {
    Prepared,
    TargetImported,
    SourceRetired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationHandoffIntent {
    pub operation_id: String,
    pub source_workspace_id: String,
    pub destination_workspace_id: String,
    pub conversation_id: String,
    pub source_epoch: u64,
    pub target_base_epoch: u64,
    pub source_fingerprint: String,
    pub follow_up: Option<String>,
    pub phase: ConversationHandoffPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletedConversationHandoff {
    pub operation_id: String,
    pub source_epoch: u64,
    pub follow_up: Option<String>,
    pub message_count: usize,
    pub follow_up_delivered: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InternalTurnProjection {
    #[serde(default)]
    epoch: Option<u64>,
    #[serde(default)]
    transfer: Option<VisibilityTransfer>,
    #[serde(default)]
    pending: BTreeMap<String, InternalTurnIntent>,
    #[serde(default)]
    settled: BTreeMap<String, BTreeSet<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct VisibilityTransfer {
    pub expected_epoch: u64,
    pub hidden_rows: Vec<(usize, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InternalTurnIntent {
    pub start_index: usize,
    pub instruction_digest: String,
}

pub(crate) fn committed_internal_turn_ids(
    messages: &[StoredMessage],
    intent: &InternalTurnIntent,
    instruction: &str,
    effect_cutoff: DateTime<Utc>,
) -> Result<Option<BTreeSet<i64>>, String> {
    let digest = format!("{:x}", Sha256::digest(instruction.as_bytes()));
    if digest != intent.instruction_digest {
        return Err("internal turn instruction differs from durable intent".to_string());
    }
    let mut matching = Vec::new();
    for (index, message) in messages.iter().enumerate().skip(intent.start_index) {
        if message.role != "user" || message.content.as_deref() != Some(instruction) {
            continue;
        }
        let created_at = DateTime::parse_from_rfc3339(&message.created_at)
            .map_err(|error| format!("internal turn message timestamp is invalid: {error}"))?;
        if created_at <= effect_cutoff {
            matching.push(index);
        }
    }
    let Some(start) = matching.first().copied() else {
        return Ok(None);
    };
    if matching.len() > 1 {
        return Err("internal turn transcript boundary is ambiguous".to_string());
    }
    let end = messages
        .iter()
        .enumerate()
        .skip(start.saturating_add(1))
        .find_map(|(index, message)| (message.role == "user").then_some(index))
        .unwrap_or(messages.len());
    let ids = messages
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|message| {
            message
                .id
                .filter(|id| *id > 0)
                .ok_or_else(|| "committed internal message has no stable ID".to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(Some(ids))
}

/// Durable workspace-scoped conversation archive projection.
pub struct ConversationArchiveStore {
    path: PathBuf,
    state: Mutex<ArchiveFile>,
    #[cfg(test)]
    fail_next_persist: AtomicBool,
}

impl ConversationArchiveStore {
    pub(crate) fn handoff_operation_id(
        source_workspace_id: &str,
        destination_workspace_id: &str,
        conversation_id: &str,
    ) -> Result<String, String> {
        if source_workspace_id.trim().is_empty()
            || destination_workspace_id.trim().is_empty()
            || conversation_id.trim().is_empty()
            || source_workspace_id == destination_workspace_id
        {
            return Err("handoff addresses must be distinct and non-empty".to_string());
        }
        let bytes = serde_json::to_vec(&(
            source_workspace_id,
            destination_workspace_id,
            conversation_id,
        ))
        .map_err(|error| error.to_string())?;
        Ok(format!("handoff-{:x}", Sha256::digest(bytes)))
    }

    pub(crate) fn handoff(
        &self,
        operation_id: &str,
    ) -> Result<Option<ConversationHandoffIntent>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "conversation visibility lock poisoned".to_string())?;
        Ok(state.handoffs.get(operation_id).cloned())
    }

    pub(crate) fn pending_handoffs(&self) -> Result<Vec<ConversationHandoffIntent>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "conversation visibility lock poisoned".to_string())?;
        Ok(state.handoffs.values().cloned().collect())
    }

    pub(crate) fn has_pending_handoff_for(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<bool, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "conversation visibility lock poisoned".to_string())?;
        Ok(state.handoffs.values().any(|intent| {
            intent.conversation_id == conversation_id
                && (intent.source_workspace_id == workspace_id
                    || intent.destination_workspace_id == workspace_id)
        }))
    }

    pub(crate) fn completed_handoff(
        &self,
        operation_id: &str,
    ) -> Result<Option<CompletedConversationHandoff>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "conversation visibility lock poisoned".to_string())?;
        Ok(state.completed_handoffs.get(operation_id).cloned())
    }

    pub(crate) fn prepare_handoff(&self, intent: ConversationHandoffIntent) -> Result<(), String> {
        let expected = Self::handoff_operation_id(
            &intent.source_workspace_id,
            &intent.destination_workspace_id,
            &intent.conversation_id,
        )?;
        if intent.operation_id != expected
            || intent.source_fingerprint.len() != 64
            || intent.source_epoch == 0
            || intent.target_base_epoch == 0
            || intent.phase != ConversationHandoffPhase::Prepared
        {
            return Err("handoff intent identity is invalid".to_string());
        }
        self.update(|next| match next.handoffs.get(&intent.operation_id) {
            Some(existing) if existing == &intent => Ok(()),
            Some(_) => Err("handoff identity conflicts with a durable operation".to_string()),
            None => {
                if next.completed_handoffs.contains_key(&intent.operation_id) {
                    return Err("handoff identity already completed".to_string());
                }
                next.handoffs.insert(intent.operation_id.clone(), intent);
                Ok(())
            }
        })
    }

    pub(crate) fn advance_handoff(
        &self,
        operation_id: &str,
        phase: ConversationHandoffPhase,
    ) -> Result<(), String> {
        self.update(|next| {
            let intent = next
                .handoffs
                .get_mut(operation_id)
                .ok_or_else(|| "handoff intent is missing".to_string())?;
            let valid = matches!(
                (intent.phase, phase),
                (
                    ConversationHandoffPhase::Prepared,
                    ConversationHandoffPhase::TargetImported
                ) | (
                    ConversationHandoffPhase::TargetImported,
                    ConversationHandoffPhase::SourceRetired
                )
            ) || intent.phase == phase;
            if !valid {
                return Err("handoff phase transition is invalid".to_string());
            }
            intent.phase = phase;
            Ok(())
        })
    }

    pub(crate) fn finish_handoff(
        &self,
        completed: CompletedConversationHandoff,
    ) -> Result<(), String> {
        self.update(|next| {
            let intent = next
                .handoffs
                .get(&completed.operation_id)
                .ok_or_else(|| "handoff intent is missing".to_string())?;
            if intent.phase != ConversationHandoffPhase::SourceRetired {
                return Err("handoff source has not retired".to_string());
            }
            if intent.follow_up != completed.follow_up
                || intent.source_epoch != completed.source_epoch
                || completed.follow_up_delivered != completed.follow_up.is_some()
            {
                return Err("handoff completion differs from its durable intent".to_string());
            }
            next.handoffs.remove(&completed.operation_id);
            next.completed_handoffs
                .insert(completed.operation_id.clone(), completed);
            Ok(())
        })
    }

    pub(crate) fn abandon_handoff(&self, operation_id: &str) -> Result<(), String> {
        self.update(|next| {
            next.handoffs.remove(operation_id);
            Ok(())
        })
    }

    pub fn at_default_path() -> Result<Self, String> {
        Self::open(crate::data_root::user_data_path(
            "conversation-archives.json",
        ))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<ArchiveFile>(&bytes)
                .map_err(|error| format!("invalid conversation visibility projection: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ArchiveFile::default(),
            Err(error) => return Err(format!("conversation visibility projection I/O: {error}")),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
            #[cfg(test)]
            fail_next_persist: AtomicBool::new(false),
        })
    }

    pub fn archived_ids(&self, workspace_id: &str) -> Result<Vec<String>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "conversation archive store lock poisoned".to_string())?;
        Ok(state
            .workspaces
            .get(workspace_id)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default())
    }

    pub fn set_archived(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        archived: bool,
    ) -> Result<(), String> {
        if workspace_id.trim().is_empty() || conversation_id.trim().is_empty() {
            return Err("workspace_id and conversation_id must not be empty".to_string());
        }
        self.update(|next| {
            if archived {
                next.workspaces
                    .entry(workspace_id.to_string())
                    .or_default()
                    .insert(conversation_id.to_string());
            } else if let Some(ids) = next.workspaces.get_mut(workspace_id) {
                ids.remove(conversation_id);
                if ids.is_empty() {
                    next.workspaces.remove(workspace_id);
                }
            }
            Ok(())
        })
    }

    pub fn remove(&self, workspace_id: &str, conversation_id: &str) -> Result<(), String> {
        self.update(|next| {
            if let Some(ids) = next.workspaces.get_mut(workspace_id) {
                ids.remove(conversation_id);
                if ids.is_empty() {
                    next.workspaces.remove(workspace_id);
                }
            }
            if let Some(conversations) = next.internal_turns.get_mut(workspace_id) {
                conversations.remove(conversation_id);
                if conversations.is_empty() {
                    next.internal_turns.remove(workspace_id);
                }
            }
            Ok(())
        })
    }

    pub fn prepare_internal_turn(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        conversation_epoch: u64,
        delivery_id: &str,
        start_index: usize,
        instruction: &str,
    ) -> Result<(), String> {
        if workspace_id.trim().is_empty()
            || conversation_id.trim().is_empty()
            || conversation_epoch == 0
            || delivery_id.trim().is_empty()
        {
            return Err("internal turn identity must not be empty".to_string());
        }
        let intent = InternalTurnIntent {
            start_index,
            instruction_digest: format!("{:x}", Sha256::digest(instruction.as_bytes())),
        };
        self.update(|next| {
            let projection = next
                .internal_turns
                .entry(workspace_id.to_string())
                .or_default()
                .entry(conversation_id.to_string())
                .or_default();
            if projection.epoch != Some(conversation_epoch) {
                *projection = InternalTurnProjection {
                    epoch: Some(conversation_epoch),
                    ..InternalTurnProjection::default()
                };
            }
            if projection.settled.contains_key(delivery_id) {
                return Ok(());
            }
            match projection.pending.get(delivery_id) {
                Some(existing) if existing != &intent => {
                    Err("internal turn identity was reused with another intent".to_string())
                }
                Some(_) => Ok(()),
                None => {
                    projection.pending.insert(delivery_id.to_string(), intent);
                    Ok(())
                }
            }
        })
    }

    pub fn settle_internal_turn(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        delivery_id: &str,
        message_ids: BTreeSet<i64>,
    ) -> Result<(), String> {
        if message_ids.iter().any(|id| *id <= 0) {
            return Err("internal turn message IDs must be positive".to_string());
        }
        self.update(|next| {
            let projection = next
                .internal_turns
                .get_mut(workspace_id)
                .and_then(|conversations| conversations.get_mut(conversation_id))
                .ok_or_else(|| "internal turn has no durable intent".to_string())?;
            if let Some(existing) = projection.settled.get(delivery_id) {
                return if existing == &message_ids {
                    Ok(())
                } else {
                    Err("internal turn settlement conflicts with its receipt".to_string())
                };
            }
            if projection.pending.remove(delivery_id).is_none() {
                return Err("internal turn has no pending intent".to_string());
            }
            projection
                .settled
                .insert(delivery_id.to_string(), message_ids);
            Ok(())
        })
    }

    pub(crate) fn prepare_visibility_transfer(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        transfer: VisibilityTransfer,
    ) -> Result<(), String> {
        if workspace_id.trim().is_empty()
            || conversation_id.trim().is_empty()
            || transfer.expected_epoch == 0
            || transfer
                .hidden_rows
                .iter()
                .any(|(_, digest)| digest.len() != 64)
        {
            return Err("visibility transfer identity is invalid".to_string());
        }
        self.update(|next| {
            let projection = next
                .internal_turns
                .entry(workspace_id.to_string())
                .or_default()
                .entry(conversation_id.to_string())
                .or_default();
            if projection
                .epoch
                .is_some_and(|epoch| epoch != transfer.expected_epoch)
                || !projection.pending.is_empty()
                || !projection.settled.is_empty()
            {
                return Err("visibility transfer target is not empty".to_string());
            }
            match projection.transfer.as_ref() {
                Some(existing) if existing != &transfer => {
                    Err("visibility transfer conflicts with an existing intent".to_string())
                }
                Some(_) => Ok(()),
                None => {
                    projection.epoch = Some(transfer.expected_epoch);
                    projection.transfer = Some(transfer);
                    Ok(())
                }
            }
        })
    }

    pub(crate) fn pending_visibility_transfer(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Option<VisibilityTransfer>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "visibility projection lock poisoned".to_string())?;
        Ok(state
            .internal_turns
            .get(workspace_id)
            .and_then(|conversations| conversations.get(conversation_id))
            .and_then(|projection| projection.transfer.clone()))
    }

    pub fn settle_visibility_transfer(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        epoch: u64,
        messages: &[StoredMessage],
    ) -> Result<(), String> {
        self.update(|next| {
            let projection = next
                .internal_turns
                .get_mut(workspace_id)
                .and_then(|conversations| conversations.get_mut(conversation_id))
                .ok_or_else(|| "visibility transfer has no target intent".to_string())?;
            let transfer = projection
                .transfer
                .as_ref()
                .ok_or_else(|| "visibility transfer was already settled".to_string())?;
            if transfer.expected_epoch != epoch || projection.epoch != Some(epoch) {
                return Err("visibility transfer target epoch is not committed".to_string());
            }
            let mut ids = BTreeSet::new();
            for (index, expected_digest) in &transfer.hidden_rows {
                let row = messages
                    .get(*index)
                    .ok_or_else(|| "visibility transfer target row is missing".to_string())?;
                let digest = echo_agent::memory::transcript_projection_message_digest(row)
                    .map_err(|error| error.to_string())?;
                if &digest != expected_digest {
                    return Err("visibility transfer target row changed".to_string());
                }
                let id = row
                    .id
                    .filter(|id| *id > 0)
                    .ok_or_else(|| "visibility transfer target row has no stable ID".to_string())?;
                ids.insert(id);
            }
            projection.settled.insert("handoff".to_string(), ids);
            projection.transfer = None;
            Ok(())
        })
    }

    pub(crate) fn pending_internal_turns(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<BTreeMap<String, InternalTurnIntent>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "visibility projection lock poisoned".to_string())?;
        Ok(state
            .internal_turns
            .get(workspace_id)
            .and_then(|conversations| conversations.get(conversation_id))
            .map(|projection| projection.pending.clone())
            .unwrap_or_default())
    }

    pub fn ensure_internal_epoch(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        epoch: Option<u64>,
    ) -> Result<(), String> {
        let stale = {
            let state = self
                .state
                .lock()
                .map_err(|_| "visibility projection lock poisoned".to_string())?;
            let projection = state
                .internal_turns
                .get(workspace_id)
                .and_then(|conversations| conversations.get(conversation_id));
            if let Some(transfer) = projection.and_then(|projection| projection.transfer.as_ref())
                && epoch != Some(transfer.expected_epoch)
            {
                return Err("visibility transfer target is not yet committed".to_string());
            }
            projection.is_some_and(|projection| projection.epoch != epoch)
        };
        if !stale {
            return Ok(());
        }
        self.update(|next| {
            if let Some(conversations) = next.internal_turns.get_mut(workspace_id) {
                if conversations
                    .get(conversation_id)
                    .is_some_and(|projection| projection.epoch != epoch)
                {
                    conversations.remove(conversation_id);
                }
                if conversations.is_empty() {
                    next.internal_turns.remove(workspace_id);
                }
            }
            Ok(())
        })
    }

    pub fn internal_message_ids(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<BTreeSet<i64>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "visibility projection lock poisoned".to_string())?;
        Ok(state
            .internal_turns
            .get(workspace_id)
            .and_then(|conversations| conversations.get(conversation_id))
            .map(|projection| {
                projection
                    .settled
                    .values()
                    .flat_map(|ids| ids.iter().copied())
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn internal_turn_is_settled(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        delivery_id: &str,
    ) -> Result<bool, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "visibility projection lock poisoned".to_string())?;
        Ok(state
            .internal_turns
            .get(workspace_id)
            .and_then(|conversations| conversations.get(conversation_id))
            .is_some_and(|projection| projection.settled.contains_key(delivery_id)))
    }

    fn update(
        &self,
        mutate: impl FnOnce(&mut ArchiveFile) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "visibility projection lock poisoned".to_string())?;
        let mut next = state.clone();
        mutate(&mut next)?;
        self.persist(&next)?;
        *state = next;
        Ok(())
    }

    fn persist(&self, state: &ArchiveFile) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_next_persist.swap(false, Ordering::SeqCst) {
            return Err("injected visibility persistence failure".to_string());
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("failed to encode conversation archive: {error}"))?;
        atomic_write(&self.path, &bytes)
            .map_err(|error| format!("failed to persist {}: {error}", self.path.display()))
    }

    #[cfg(test)]
    pub(crate) fn fail_next_persist_for_test(&self) {
        self.fail_next_persist.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_updates_are_workspace_scoped_and_durable() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("archives.json");
        let store = ConversationArchiveStore::open(&path)?;
        store.set_archived("workspace-a", "conversation-1", true)?;
        store.set_archived("workspace-b", "conversation-1", true)?;
        assert_eq!(
            store.archived_ids("workspace-a")?,
            vec!["conversation-1".to_string()]
        );
        assert_eq!(
            store.archived_ids("workspace-b")?,
            vec!["conversation-1".to_string()]
        );

        let reopened = ConversationArchiveStore::open(&path)?;
        assert_eq!(
            reopened.archived_ids("workspace-a")?,
            vec!["conversation-1".to_string()]
        );
        reopened.remove("workspace-a", "conversation-1")?;
        assert!(reopened.archived_ids("workspace-a")?.is_empty());
        Ok(())
    }

    #[test]
    fn internal_turn_intent_recovers_exact_rows_and_survives_child_removal() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("visibility.json");
        let instruction = "[eko_agent_message] private delivery";
        let store = ConversationArchiveStore::open(&path)?;
        store.prepare_internal_turn("workspace", "primary", 1, "delivery-1", 0, instruction)?;
        drop(store);
        let reopened = ConversationArchiveStore::open(&path)?;
        let intent = reopened
            .pending_internal_turns("workspace", "primary")?
            .remove("delivery-1")
            .ok_or_else(|| "durable internal turn intent missing".to_string())?;
        let rows = [
            (1, "user", instruction, "2026-01-01T00:00:00Z"),
            (2, "assistant", "internal reply", "2026-01-01T00:00:01Z"),
            (3, "user", instruction, "2026-01-01T00:02:00Z"),
        ]
        .into_iter()
        .map(|(id, role, content, created_at)| StoredMessage {
            id: Some(id),
            conversation_id: "primary".to_string(),
            role: role.to_string(),
            content: Some(content.to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: created_at.to_string(),
        })
        .collect::<Vec<_>>();
        let cutoff = DateTime::parse_from_rfc3339("2026-01-01T00:01:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let ids = committed_internal_turn_ids(&rows, &intent, instruction, cutoff)?
            .ok_or_else(|| "committed turn rows missing".to_string())?;
        assert_eq!(ids, BTreeSet::from([1, 2]));
        reopened.settle_internal_turn("workspace", "primary", "delivery-1", ids.clone())?;
        reopened.settle_internal_turn("workspace", "primary", "delivery-1", ids)?;
        reopened.prepare_internal_turn("workspace", "side", 1, "delivery-2", 0, instruction)?;
        reopened.settle_internal_turn("workspace", "side", "delivery-2", BTreeSet::from([4]))?;
        reopened.remove("workspace", "side")?;
        let restarted = ConversationArchiveStore::open(&path)?;
        assert_eq!(
            restarted.internal_message_ids("workspace", "primary")?,
            BTreeSet::from([1, 2])
        );
        assert!(
            restarted
                .internal_message_ids("workspace", "side")?
                .is_empty()
        );
        restarted.ensure_internal_epoch("workspace", "primary", Some(2))?;
        assert!(
            restarted
                .internal_message_ids("workspace", "primary")?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn corrupt_visibility_file_fails_closed() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("visibility.json");
        std::fs::write(&path, b"not json").map_err(|error| error.to_string())?;
        assert!(ConversationArchiveStore::open(path).is_err());
        Ok(())
    }

    #[test]
    fn settlement_write_failure_reopens_with_pending_intent() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("visibility.json");
        let store = ConversationArchiveStore::open(&path)?;
        store.prepare_internal_turn("workspace", "primary", 1, "delivery-1", 0, "instruction")?;
        store.fail_next_persist_for_test();
        assert!(
            store
                .settle_internal_turn("workspace", "primary", "delivery-1", BTreeSet::from([1, 2]),)
                .is_err()
        );
        assert!(
            store
                .internal_message_ids("workspace", "primary")?
                .is_empty()
        );
        drop(store);
        let reopened = ConversationArchiveStore::open(&path)?;
        assert!(
            reopened
                .pending_internal_turns("workspace", "primary")?
                .contains_key("delivery-1")
        );
        reopened.settle_internal_turn(
            "workspace",
            "primary",
            "delivery-1",
            BTreeSet::from([1, 2]),
        )?;
        assert_eq!(
            reopened.internal_message_ids("workspace", "primary")?,
            BTreeSet::from([1, 2])
        );
        Ok(())
    }

    #[test]
    fn handoff_transfer_recovers_new_ids_only_after_exact_import_epoch() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root.path().join("visibility.json");
        let source = StoredMessage {
            id: Some(3),
            conversation_id: "source".to_string(),
            role: "user".to_string(),
            content: Some("internal turn".to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let expected_digest = echo_agent::memory::transcript_projection_message_digest(&source)
            .map_err(|error| error.to_string())?;
        let store = ConversationArchiveStore::open(&path)?;
        store.prepare_visibility_transfer(
            "target-workspace",
            "conversation",
            VisibilityTransfer {
                expected_epoch: 2,
                hidden_rows: vec![(0, expected_digest)],
            },
        )?;
        drop(store);
        let reopened = ConversationArchiveStore::open(&path)?;
        assert!(
            reopened
                .ensure_internal_epoch("target-workspace", "conversation", Some(1))
                .is_err()
        );
        reopened.ensure_internal_epoch("target-workspace", "conversation", Some(2))?;
        let mut target = source;
        target.id = Some(10);
        target.conversation_id = "conversation".to_string();
        reopened.settle_visibility_transfer("target-workspace", "conversation", 2, &[target])?;
        assert_eq!(
            reopened.internal_message_ids("target-workspace", "conversation")?,
            BTreeSet::from([10])
        );
        assert!(
            reopened
                .pending_visibility_transfer("target-workspace", "conversation")?
                .is_none()
        );
        Ok(())
    }
}
