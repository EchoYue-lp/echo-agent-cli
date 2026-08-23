//! Workspace-scoped evidence candidates backed by an append-only JSONL log.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 3;
const MAX_EVIDENCE_ITEMS: usize = 16;
const MAX_EVIDENCE_CHARS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    BackgroundReviewer,
    TriggerDetector,
    AutoMemory,
    MemoryReviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    UserPreference,
    ProjectFact,
    DebuggingLesson,
    ErrorResolution,
    WorkflowPattern,
    Skill,
    MemoryConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EvidenceScope {
    User(String),
    Workspace(String),
    Session(String),
}

impl EvidenceScope {
    fn fingerprint_key(&self) -> String {
        match self {
            Self::User(id) => format!("user:{id}"),
            Self::Workspace(id) => format!("workspace:{id}"),
            Self::Session(id) => format!("session:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub source: EvidenceSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_memory_key: Option<String>,
    pub quote: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceAction {
    #[default]
    SaveMemory,
    MergeMemories {
        proposal: echo_agent::evolution::MemoryConflictProposal,
    },
}

impl EvidenceAction {
    fn fingerprint_key(&self) -> String {
        match self {
            Self::SaveMemory => "save_memory".to_string(),
            Self::MergeMemories { proposal } => {
                let members = proposal
                    .members
                    .iter()
                    .map(|member| format!("{}:{}", member.key, normalize_content(&member.content)))
                    .collect::<Vec<_>>()
                    .join("|");
                format!(
                    "merge:{:?}:{}:{}:{}",
                    proposal.memory_type, proposal.topic, proposal.recommended_primary_key, members
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCandidateStatus {
    Pending,
    Applied,
    Rejected,
}

/// User interaction recorded alongside candidate snapshots in the Evidence JSONL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "failure_kind", rename_all = "snake_case")]
pub enum EvidenceInteractionAction {
    AcceptAttempt,
    AcceptSucceeded,
    AcceptFailed(EvidenceInteractionFailureKind),
    Rejected,
    UndoAttempt,
    UndoSucceeded,
    UndoFailed(EvidenceInteractionFailureKind),
}

/// Stable failure classes used by Review Inbox diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceInteractionFailureKind {
    StaleProposal,
    Validation,
    Mutation,
    Persistence,
    Rollback,
}

/// Append-only interaction event. Candidate snapshots remain backward compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceInteractionEvent {
    pub schema_version: u32,
    pub record_type: String,
    pub event_id: String,
    pub candidate_id: String,
    pub action: EvidenceInteractionAction,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceTarget {
    Memory {
        key: String,
    },
    MemoryMerge {
        primary_key: String,
        superseded_keys: Vec<String>,
        before: Vec<echo_agent::evolution::MemoryMergeSnapshot>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCandidate {
    pub schema_version: u32,
    pub candidate_id: String,
    pub fingerprint: String,
    pub kind: EvidenceKind,
    pub scope: EvidenceScope,
    pub content: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub action: EvidenceAction,
    pub confidence: f32,
    pub status: EvidenceCandidateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EvidenceTarget>,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Candidate plus transient Review Inbox state derived from interaction events.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceReviewItem {
    #[serde(flatten)]
    pub candidate: EvidenceCandidate,
    pub expired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceReviewFilter {
    Pending,
    Expired,
    Undoable,
}

impl EvidenceReviewFilter {
    pub fn matches(self, item: &EvidenceReviewItem) -> bool {
        match self {
            Self::Pending => {
                item.candidate.status == EvidenceCandidateStatus::Pending && !item.expired
            }
            Self::Expired => {
                item.candidate.status == EvidenceCandidateStatus::Pending && item.expired
            }
            Self::Undoable => item.candidate.status == EvidenceCandidateStatus::Applied,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceCandidateDraft {
    pub kind: EvidenceKind,
    pub scope: Option<EvidenceScope>,
    pub content: String,
    pub evidence: Vec<EvidenceRef>,
    pub action: Option<EvidenceAction>,
    pub confidence: f32,
}

#[derive(Debug)]
enum EvidenceLogRecord {
    Candidate(Box<EvidenceCandidate>),
    Interaction(EvidenceInteractionEvent),
}

fn decode_log_record(line: &str) -> Result<EvidenceLogRecord, String> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    match value.get("record_type") {
        None => serde_json::from_value::<EvidenceCandidate>(value)
            .map(|candidate| EvidenceLogRecord::Candidate(Box::new(candidate)))
            .map_err(|error| error.to_string()),
        Some(serde_json::Value::String(record_type)) if record_type == "interaction" => {
            serde_json::from_value::<EvidenceInteractionEvent>(value)
                .map(EvidenceLogRecord::Interaction)
                .map_err(|error| error.to_string())
        }
        Some(record_type) => Err(format!("unknown evidence record_type: {record_type}")),
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceStore {
    path: PathBuf,
    scope: EvidenceScope,
}

/// Convert and persist a framework background-review proposal.
pub fn capture_review_outcome(
    store: &EvidenceStore,
    outcome: &echo_agent::evolution::ReviewOutcome,
) -> Result<Option<EvidenceCandidate>, String> {
    let Some(candidate) = &outcome.candidate else {
        return Ok(None);
    };
    let kind = match candidate.kind {
        echo_agent::evolution::ReviewCandidateKind::UserPreference => EvidenceKind::UserPreference,
        echo_agent::evolution::ReviewCandidateKind::ProjectFact => EvidenceKind::ProjectFact,
        echo_agent::evolution::ReviewCandidateKind::DebuggingLesson => {
            EvidenceKind::DebuggingLesson
        }
        echo_agent::evolution::ReviewCandidateKind::Skill => EvidenceKind::Skill,
    };
    store
        .upsert(EvidenceCandidateDraft {
            kind,
            scope: matches!(kind, EvidenceKind::UserPreference)
                .then(|| EvidenceScope::User("local-user".to_string())),
            content: candidate.content.clone(),
            evidence: vec![EvidenceRef {
                source: EvidenceSource::BackgroundReviewer,
                source_run_id: Some(outcome.run_id.clone()),
                source_role: None,
                source_turn: None,
                source_memory_key: None,
                quote: candidate.evidence.clone(),
            }],
            action: None,
            confidence: candidate.confidence,
        })
        .map(Some)
}

/// Persist an analysis-only memory conflict as an actionable inbox proposal.
pub fn capture_memory_conflict(
    store: &EvidenceStore,
    proposal: &echo_agent::evolution::MemoryConflictProposal,
) -> Result<EvidenceCandidate, String> {
    let member_keys = proposal
        .members
        .iter()
        .map(|member| member.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let confidence = proposal
        .members
        .iter()
        .find(|member| member.key == proposal.recommended_primary_key)
        .map(|member| member.confidence)
        .unwrap_or(0.5);
    let evidence = proposal
        .members
        .iter()
        .map(|member| EvidenceRef {
            source: EvidenceSource::MemoryReviewer,
            source_run_id: None,
            source_role: None,
            source_turn: None,
            source_memory_key: Some(member.key.clone()),
            quote: member.content.clone(),
        })
        .collect();
    store.upsert(EvidenceCandidateDraft {
        kind: EvidenceKind::MemoryConflict,
        scope: None,
        content: format!(
            "Resolve {:?} conflict for topic '{}'; recommended primary '{}' among [{}]",
            proposal.memory_type, proposal.topic, proposal.recommended_primary_key, member_keys
        ),
        evidence,
        action: Some(EvidenceAction::MergeMemories {
            proposal: proposal.clone(),
        }),
        confidence,
    })
}

impl EvidenceStore {
    pub fn new(echo_agent_dir: impl Into<PathBuf>) -> Self {
        let echo_agent_dir = echo_agent_dir.into();
        let scope_root = echo_agent_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| echo_agent_dir.clone());
        let normalized_root = scope_root
            .canonicalize()
            .unwrap_or_else(|_| scope_root.clone());
        Self {
            path: echo_agent_dir
                .join("evolution")
                .join("evidence-candidates.jsonl"),
            scope: EvidenceScope::Workspace(normalized_root.display().to_string()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn scope(&self) -> &EvidenceScope {
        &self.scope
    }

    pub fn upsert(&self, mut draft: EvidenceCandidateDraft) -> Result<EvidenceCandidate, String> {
        draft.content = normalize_content(&draft.content);
        if draft.content.is_empty() {
            return Err("evidence candidate content cannot be empty".to_string());
        }
        draft.confidence = draft.confidence.clamp(0.0, 1.0);
        draft.evidence = sanitize_evidence(draft.evidence);
        if draft.evidence.is_empty() {
            return Err("evidence candidate requires at least one source excerpt".to_string());
        }

        self.with_locked_log(|latest, file| {
            let scope = draft.scope.clone().unwrap_or_else(|| self.scope.clone());
            let action = draft.action.clone().unwrap_or_default();
            let fingerprint = candidate_fingerprint(draft.kind, &scope, &draft.content, &action);
            if let Some(existing) = latest
                .values()
                .find(|candidate| candidate.fingerprint == fingerprint)
            {
                let mut updated = existing.clone();
                let mut changed = false;
                if updated.status == EvidenceCandidateStatus::Pending && updated.action != action {
                    updated.action = action;
                    changed = true;
                }
                for evidence in &draft.evidence {
                    if updated.evidence.len() >= MAX_EVIDENCE_ITEMS {
                        break;
                    }
                    if !updated.evidence.contains(evidence) {
                        updated.evidence.push(evidence.clone());
                        changed = true;
                    }
                }
                let confidence = updated.confidence.max(draft.confidence);
                if (confidence - updated.confidence).abs() > f32::EPSILON {
                    updated.confidence = confidence;
                    changed = true;
                }
                if changed {
                    updated.revision = updated.revision.saturating_add(1);
                    updated.updated_at = Utc::now();
                    append_snapshot(file, &updated)?;
                }
                return Ok(updated);
            }

            let now = Utc::now();
            let candidate = EvidenceCandidate {
                schema_version: SCHEMA_VERSION,
                candidate_id: format!("ec_{}", uuid::Uuid::new_v4().simple()),
                fingerprint,
                kind: draft.kind,
                scope,
                content: draft.content,
                evidence: draft.evidence,
                action,
                confidence: draft.confidence,
                status: EvidenceCandidateStatus::Pending,
                target: None,
                revision: 1,
                created_at: now,
                updated_at: now,
            };
            append_snapshot(file, &candidate)?;
            Ok(candidate)
        })
    }

    pub fn list(&self) -> Result<Vec<EvidenceCandidate>, String> {
        let mut candidates: Vec<_> = self.read_latest()?.into_values().collect();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.updated_at));
        Ok(candidates)
    }

    /// Build the small, on-demand Review Inbox projection.
    pub fn review_items(&self) -> Result<Vec<EvidenceReviewItem>, String> {
        let stale_failures = self.latest_stale_failures()?;
        Ok(self
            .list()?
            .into_iter()
            .map(|candidate| {
                let expired = candidate.status == EvidenceCandidateStatus::Pending
                    && stale_failures
                        .get(&candidate.candidate_id)
                        .is_some_and(|timestamp| *timestamp >= candidate.updated_at);
                EvidenceReviewItem { candidate, expired }
            })
            .collect())
    }

    pub fn review_item(&self, candidate_id: &str) -> Result<Option<EvidenceReviewItem>, String> {
        Ok(self
            .review_items()?
            .into_iter()
            .find(|item| item.candidate.candidate_id == candidate_id))
    }

    pub fn get(&self, candidate_id: &str) -> Result<Option<EvidenceCandidate>, String> {
        Ok(self.read_latest()?.remove(candidate_id))
    }

    pub fn edit(&self, candidate_id: &str, content: &str) -> Result<EvidenceCandidate, String> {
        let normalized = normalize_content(content);
        if normalized.is_empty() {
            return Err("edited candidate content cannot be empty".to_string());
        }
        self.update(candidate_id, |candidate, latest| {
            if candidate.status == EvidenceCandidateStatus::Applied {
                return Err("undo an applied candidate before editing it".to_string());
            }
            if !matches!(candidate.action, EvidenceAction::SaveMemory) {
                return Err("semantic action proposals cannot edit their summary".to_string());
            }
            let fingerprint = candidate_fingerprint(
                candidate.kind,
                &candidate.scope,
                &normalized,
                &candidate.action,
            );
            if latest.values().any(|other| {
                other.candidate_id != candidate.candidate_id && other.fingerprint == fingerprint
            }) {
                return Err("edited content duplicates another candidate".to_string());
            }
            candidate.content = normalized;
            candidate.fingerprint = fingerprint;
            Ok(())
        })
    }

    pub fn reject(&self, candidate_id: &str) -> Result<EvidenceCandidate, String> {
        let candidate = self.update(candidate_id, |candidate, _| {
            if candidate.status == EvidenceCandidateStatus::Applied {
                return Err("undo an applied candidate before rejecting it".to_string());
            }
            candidate.status = EvidenceCandidateStatus::Rejected;
            Ok(())
        })?;
        self.record_interaction_best_effort(candidate_id, EvidenceInteractionAction::Rejected);
        Ok(candidate)
    }

    pub async fn accept(
        &self,
        candidate_id: &str,
        edited_content: Option<&str>,
        layer_manager: &Arc<echo_agent::evolution::MemoryLayerManager>,
    ) -> Result<EvidenceCandidate, String> {
        let mut candidate = self
            .get(candidate_id)?
            .ok_or_else(|| format!("evidence candidate '{candidate_id}' not found"))?;
        if candidate.status == EvidenceCandidateStatus::Applied {
            return Ok(candidate);
        }
        self.record_interaction(candidate_id, EvidenceInteractionAction::AcceptAttempt)?;
        if let Some(content) = edited_content {
            candidate = match self.edit(candidate_id, content) {
                Ok(candidate) => candidate,
                Err(error) => {
                    self.record_interaction_best_effort(
                        candidate_id,
                        EvidenceInteractionAction::AcceptFailed(
                            EvidenceInteractionFailureKind::Validation,
                        ),
                    );
                    return Err(error);
                }
            };
        }
        match candidate.action.clone() {
            EvidenceAction::SaveMemory => {
                let memory_type = memory_type_for_kind(candidate.kind)?;
                let key = format!("evidence_{}", candidate.candidate_id);
                let meta = echo_agent::memory::MemoryMeta::new(
                    memory_type,
                    echo_agent::memory::MemorySource::ExplicitSave,
                    "evidence_inbox",
                )
                .with_confidence(candidate.confidence);
                if let Err(error) = layer_manager
                    .write_memory(&key, &candidate.content, meta)
                    .await
                {
                    self.record_interaction_best_effort(
                        candidate_id,
                        EvidenceInteractionAction::AcceptFailed(
                            EvidenceInteractionFailureKind::Mutation,
                        ),
                    );
                    return Err(format!("failed to apply evidence candidate: {error}"));
                }

                let update_result = self.update(candidate_id, |candidate, _| {
                    candidate.status = EvidenceCandidateStatus::Applied;
                    candidate.target = Some(EvidenceTarget::Memory { key: key.clone() });
                    Ok(())
                });
                match update_result {
                    Ok(candidate) => {
                        self.record_interaction_best_effort(
                            candidate_id,
                            EvidenceInteractionAction::AcceptSucceeded,
                        );
                        Ok(candidate)
                    }
                    Err(update_error) => match layer_manager.delete_memory(&key).await {
                        Ok(_) => {
                            self.record_interaction_best_effort(
                                candidate_id,
                                EvidenceInteractionAction::AcceptFailed(
                                    EvidenceInteractionFailureKind::Persistence,
                                ),
                            );
                            Err(format!(
                                "failed to record applied evidence candidate; memory write was rolled back: {update_error}"
                            ))
                        }
                        Err(rollback_error) => {
                            self.record_interaction_best_effort(
                                candidate_id,
                                EvidenceInteractionAction::AcceptFailed(
                                    EvidenceInteractionFailureKind::Rollback,
                                ),
                            );
                            Err(format!(
                                "failed to record applied evidence candidate ({update_error}); failed to roll back memory write ({rollback_error})"
                            ))
                        }
                    },
                }
            }
            EvidenceAction::MergeMemories { proposal } => {
                if edited_content.is_some() {
                    return Err("memory merge proposals cannot be edited during accept".to_string());
                }
                let applied = match layer_manager.apply_merge_proposal(&proposal).await {
                    Ok(applied) => applied,
                    Err(error) => {
                        let failure_kind =
                            if echo_agent::evolution::is_stale_memory_proposal_error(&error) {
                                EvidenceInteractionFailureKind::StaleProposal
                            } else {
                                EvidenceInteractionFailureKind::Mutation
                            };
                        self.record_interaction_best_effort(
                            candidate_id,
                            EvidenceInteractionAction::AcceptFailed(failure_kind),
                        );
                        return Err(format!("failed to apply memory merge proposal: {error}"));
                    }
                };
                let before = applied.before.clone();
                let primary_key = applied.primary_key.clone();
                let superseded_keys = applied.superseded_keys.clone();
                let update_result = self.update(candidate_id, |candidate, _| {
                    candidate.status = EvidenceCandidateStatus::Applied;
                    candidate.target = Some(EvidenceTarget::MemoryMerge {
                        primary_key,
                        superseded_keys,
                        before: before.clone(),
                    });
                    Ok(())
                });
                match update_result {
                    Ok(candidate) => {
                        self.record_interaction_best_effort(
                            candidate_id,
                            EvidenceInteractionAction::AcceptSucceeded,
                        );
                        Ok(candidate)
                    }
                    Err(update_error) => {
                        match layer_manager.restore_merge_snapshots(&applied.before).await {
                            Ok(()) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::AcceptFailed(
                                        EvidenceInteractionFailureKind::Persistence,
                                    ),
                                );
                                Err(format!(
                                    "failed to record applied memory merge; merge was rolled back: {update_error}"
                                ))
                            }
                            Err(rollback_error) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::AcceptFailed(
                                        EvidenceInteractionFailureKind::Rollback,
                                    ),
                                );
                                Err(format!(
                                    "failed to record applied memory merge ({update_error}); failed to roll back merge ({rollback_error})"
                                ))
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn undo(
        &self,
        candidate_id: &str,
        layer_manager: &Arc<echo_agent::evolution::MemoryLayerManager>,
    ) -> Result<EvidenceCandidate, String> {
        let candidate = self
            .get(candidate_id)?
            .ok_or_else(|| format!("evidence candidate '{candidate_id}' not found"))?;
        self.record_interaction(candidate_id, EvidenceInteractionAction::UndoAttempt)?;
        let target = match candidate.target.clone() {
            Some(target) => target,
            None => {
                self.record_interaction_best_effort(
                    candidate_id,
                    EvidenceInteractionAction::UndoFailed(
                        EvidenceInteractionFailureKind::Validation,
                    ),
                );
                return Err("candidate has no applied action to undo".to_string());
            }
        };
        match target {
            EvidenceTarget::Memory { key } => {
                let memory_type = memory_type_for_kind(candidate.kind)?;
                if let Err(error) = layer_manager.delete_memory(&key).await {
                    self.record_interaction_best_effort(
                        candidate_id,
                        EvidenceInteractionAction::UndoFailed(
                            EvidenceInteractionFailureKind::Mutation,
                        ),
                    );
                    return Err(format!("failed to remove applied memory: {error}"));
                }
                let update_result = self.mark_pending(candidate_id);
                match update_result {
                    Ok(candidate) => {
                        self.record_interaction_best_effort(
                            candidate_id,
                            EvidenceInteractionAction::UndoSucceeded,
                        );
                        Ok(candidate)
                    }
                    Err(update_error) => {
                        let meta = echo_agent::memory::MemoryMeta::new(
                            memory_type,
                            echo_agent::memory::MemorySource::ExplicitSave,
                            "evidence_inbox",
                        )
                        .with_confidence(candidate.confidence);
                        match layer_manager
                            .write_memory(&key, &candidate.content, meta)
                            .await
                        {
                            Ok(_) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::UndoFailed(
                                        EvidenceInteractionFailureKind::Persistence,
                                    ),
                                );
                                Err(format!(
                                    "failed to record evidence undo; memory deletion was rolled back: {update_error}"
                                ))
                            }
                            Err(rollback_error) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::UndoFailed(
                                        EvidenceInteractionFailureKind::Rollback,
                                    ),
                                );
                                Err(format!(
                                    "failed to record evidence undo ({update_error}); failed to restore memory ({rollback_error})"
                                ))
                            }
                        }
                    }
                }
            }
            EvidenceTarget::MemoryMerge { before, .. } => {
                if let Err(error) = layer_manager.restore_merge_snapshots(&before).await {
                    self.record_interaction_best_effort(
                        candidate_id,
                        EvidenceInteractionAction::UndoFailed(
                            EvidenceInteractionFailureKind::Mutation,
                        ),
                    );
                    return Err(format!("failed to undo memory merge: {error}"));
                }
                let update_result = self.mark_pending(candidate_id);
                match update_result {
                    Ok(candidate) => {
                        self.record_interaction_best_effort(
                            candidate_id,
                            EvidenceInteractionAction::UndoSucceeded,
                        );
                        Ok(candidate)
                    }
                    Err(update_error) => {
                        let EvidenceAction::MergeMemories { proposal } = &candidate.action else {
                            return Err(format!(
                                "failed to record merge undo and candidate action is invalid: {update_error}"
                            ));
                        };
                        match layer_manager.apply_merge_proposal(proposal).await {
                            Ok(_) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::UndoFailed(
                                        EvidenceInteractionFailureKind::Persistence,
                                    ),
                                );
                                Err(format!(
                                    "failed to record merge undo; merge was re-applied: {update_error}"
                                ))
                            }
                            Err(rollback_error) => {
                                self.record_interaction_best_effort(
                                    candidate_id,
                                    EvidenceInteractionAction::UndoFailed(
                                        EvidenceInteractionFailureKind::Rollback,
                                    ),
                                );
                                Err(format!(
                                    "failed to record merge undo ({update_error}); failed to re-apply merge ({rollback_error})"
                                ))
                            }
                        }
                    }
                }
            }
        }
    }

    fn mark_pending(&self, candidate_id: &str) -> Result<EvidenceCandidate, String> {
        self.update(candidate_id, |candidate, _| {
            candidate.status = EvidenceCandidateStatus::Pending;
            candidate.target = None;
            Ok(())
        })
    }

    fn update<F>(&self, candidate_id: &str, mutate: F) -> Result<EvidenceCandidate, String>
    where
        F: FnOnce(
            &mut EvidenceCandidate,
            &HashMap<String, EvidenceCandidate>,
        ) -> Result<(), String>,
    {
        self.with_locked_log(|latest, file| {
            let mut candidate = latest
                .get(candidate_id)
                .cloned()
                .ok_or_else(|| format!("evidence candidate '{candidate_id}' not found"))?;
            mutate(&mut candidate, latest)?;
            candidate.revision = candidate.revision.saturating_add(1);
            candidate.updated_at = Utc::now();
            append_snapshot(file, &candidate)?;
            Ok(candidate)
        })
    }

    fn read_latest(&self) -> Result<HashMap<String, EvidenceCandidate>, String> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let lock_path = self.path.with_extension("jsonl.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("failed to open evidence lock: {error}"))?;
        lock.lock_shared()
            .map_err(|error| format!("failed to lock evidence log for reading: {error}"))?;
        let result = File::open(&self.path)
            .map_err(|error| format!("failed to open evidence log: {error}"))
            .and_then(read_latest_from);
        let _ = lock.unlock();
        result
    }

    fn read_interactions(&self) -> Result<Vec<EvidenceInteractionEvent>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let lock_path = self.path.with_extension("jsonl.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("failed to open evidence lock: {error}"))?;
        lock.lock_shared()
            .map_err(|error| format!("failed to lock evidence log for reading: {error}"))?;
        let result = File::open(&self.path)
            .map_err(|error| format!("failed to open evidence log: {error}"))
            .and_then(read_interactions_from);
        let _ = lock.unlock();
        result
    }

    fn latest_stale_failures(&self) -> Result<HashMap<String, DateTime<Utc>>, String> {
        let mut latest: HashMap<String, DateTime<Utc>> = HashMap::new();
        for event in self.read_interactions()? {
            if matches!(
                event.action,
                EvidenceInteractionAction::AcceptFailed(
                    EvidenceInteractionFailureKind::StaleProposal
                )
            ) {
                latest
                    .entry(event.candidate_id)
                    .and_modify(|timestamp| *timestamp = (*timestamp).max(event.timestamp))
                    .or_insert(event.timestamp);
            }
        }
        Ok(latest)
    }

    fn record_interaction(
        &self,
        candidate_id: &str,
        action: EvidenceInteractionAction,
    ) -> Result<(), String> {
        self.with_locked_log(|latest, file| {
            if !latest.contains_key(candidate_id) {
                return Err(format!("evidence candidate '{candidate_id}' not found"));
            }
            append_interaction(
                file,
                &EvidenceInteractionEvent {
                    schema_version: SCHEMA_VERSION,
                    record_type: "interaction".to_string(),
                    event_id: format!("ei_{}", uuid::Uuid::new_v4().simple()),
                    candidate_id: candidate_id.to_string(),
                    action,
                    timestamp: Utc::now(),
                },
            )
        })
    }

    fn record_interaction_best_effort(
        &self,
        candidate_id: &str,
        action: EvidenceInteractionAction,
    ) {
        if let Err(error) = self.record_interaction(candidate_id, action) {
            tracing::warn!(
                candidate_id,
                error = %error,
                "failed to append Evidence interaction event"
            );
        }
    }

    fn with_locked_log<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(&HashMap<String, EvidenceCandidate>, &mut File) -> Result<T, String>,
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create evidence directory: {error}"))?;
        }
        let lock_path = self.path.with_extension("jsonl.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("failed to open evidence lock: {error}"))?;
        lock.lock_exclusive()
            .map_err(|error| format!("failed to lock evidence log: {error}"))?;

        let latest = if self.path.exists() {
            let read_file = File::open(&self.path)
                .map_err(|error| format!("failed to open evidence log: {error}"))?;
            read_latest_from(read_file)?
        } else {
            HashMap::new()
        };
        let mut append_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| format!("failed to append evidence log: {error}"))?;
        let result = operation(&latest, &mut append_file);
        let _ = lock.unlock();
        result
    }
}

fn read_latest_from(file: File) -> Result<HashMap<String, EvidenceCandidate>, String> {
    let mut latest = HashMap::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| format!("failed to read evidence line: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let record = decode_log_record(&line).map_err(|error| {
            format!(
                "invalid evidence JSONL record at line {}: {error}",
                index.saturating_add(1)
            )
        })?;
        if let EvidenceLogRecord::Candidate(candidate) = record {
            let candidate = *candidate;
            latest.insert(candidate.candidate_id.clone(), candidate);
        }
    }
    Ok(latest)
}

fn read_interactions_from(file: File) -> Result<Vec<EvidenceInteractionEvent>, String> {
    let mut interactions = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| format!("failed to read evidence line: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let record = decode_log_record(&line).map_err(|error| {
            format!(
                "invalid evidence JSONL record at line {}: {error}",
                index.saturating_add(1)
            )
        })?;
        if let EvidenceLogRecord::Interaction(interaction) = record {
            interactions.push(interaction);
        }
    }
    Ok(interactions)
}

fn append_snapshot(file: &mut File, candidate: &EvidenceCandidate) -> Result<(), String> {
    let mut line = serde_json::to_vec(candidate)
        .map_err(|error| format!("failed to serialize evidence candidate: {error}"))?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|error| format!("failed to write evidence candidate: {error}"))?;
    file.sync_data()
        .map_err(|error| format!("failed to sync evidence candidate: {error}"))
}

fn append_interaction(file: &mut File, event: &EvidenceInteractionEvent) -> Result<(), String> {
    let mut line = serde_json::to_vec(event)
        .map_err(|error| format!("failed to serialize evidence interaction: {error}"))?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|error| format!("failed to write evidence interaction: {error}"))?;
    file.sync_data()
        .map_err(|error| format!("failed to sync evidence interaction: {error}"))
}

fn normalize_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitize_evidence(evidence: Vec<EvidenceRef>) -> Vec<EvidenceRef> {
    let mut result = Vec::new();
    for mut item in evidence {
        item.quote = item.quote.trim().chars().take(MAX_EVIDENCE_CHARS).collect();
        if !item.quote.is_empty() && !result.contains(&item) {
            result.push(item);
        }
        if result.len() >= MAX_EVIDENCE_ITEMS {
            break;
        }
    }
    result
}

fn candidate_fingerprint(
    kind: EvidenceKind,
    scope: &EvidenceScope,
    content: &str,
    action: &EvidenceAction,
) -> String {
    let normalized = normalize_content(content).to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "{kind:?}\n{}\n{normalized}\n{}",
        scope.fingerprint_key(),
        action.fingerprint_key()
    ));
    hex::encode(hasher.finalize())
}

fn memory_type_for_kind(kind: EvidenceKind) -> Result<echo_agent::memory::MemoryType, String> {
    match kind {
        EvidenceKind::UserPreference => Ok(echo_agent::memory::MemoryType::UserPreference),
        EvidenceKind::ProjectFact => Ok(echo_agent::memory::MemoryType::ProjectFact),
        EvidenceKind::DebuggingLesson => Ok(echo_agent::memory::MemoryType::DebuggingLesson),
        EvidenceKind::ErrorResolution => Ok(echo_agent::memory::MemoryType::ErrorResolution),
        EvidenceKind::WorkflowPattern => Ok(echo_agent::memory::MemoryType::WorkflowPattern),
        EvidenceKind::Skill => {
            Err("skill evidence must use the dedicated skill candidate review flow".to_string())
        }
        EvidenceKind::MemoryConflict => {
            Err("memory conflict evidence must use an explicit merge action".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict_proposal() -> echo_agent::evolution::MemoryConflictProposal {
        echo_agent::evolution::MemoryConflictProposal {
            topic: "build".to_string(),
            memory_type: echo_agent::memory::MemoryType::ProjectFact,
            recommended_primary_key: "cargo".to_string(),
            members: vec![
                echo_agent::evolution::MemoryConflictMember {
                    key: "cargo".to_string(),
                    content: "Build uses cargo".to_string(),
                    confidence: 0.9,
                    status: echo_agent::memory::MemoryStatus::Active,
                    recall_count: 0,
                    updated_at: 1,
                },
                echo_agent::evolution::MemoryConflictMember {
                    key: "make".to_string(),
                    content: "Build uses make".to_string(),
                    confidence: 0.5,
                    status: echo_agent::memory::MemoryStatus::Active,
                    recall_count: 0,
                    updated_at: 1,
                },
            ],
        }
    }

    async fn seed_conflict(
        layer_manager: &echo_agent::evolution::MemoryLayerManager,
    ) -> Result<(), String> {
        let high = echo_agent::memory::MemoryMeta::new(
            echo_agent::memory::MemoryType::ProjectFact,
            echo_agent::memory::MemorySource::ExplicitSave,
            "build",
        )
        .with_confidence(0.9);
        let low = echo_agent::memory::MemoryMeta::new(
            echo_agent::memory::MemoryType::ProjectFact,
            echo_agent::memory::MemorySource::AutoExtracted,
            "build",
        )
        .with_confidence(0.5);
        layer_manager
            .write_memory("cargo", "Build uses cargo", high)
            .await
            .map_err(|error| error.to_string())?;
        layer_manager
            .write_memory("make", "Build uses make", low)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn draft(content: &str, quote: &str) -> EvidenceCandidateDraft {
        EvidenceCandidateDraft {
            kind: EvidenceKind::ProjectFact,
            scope: None,
            content: content.to_string(),
            evidence: vec![EvidenceRef {
                source: EvidenceSource::AutoMemory,
                source_run_id: None,
                source_role: Some("assistant".to_string()),
                source_turn: Some(1),
                source_memory_key: None,
                quote: quote.to_string(),
            }],
            action: None,
            confidence: 0.8,
        }
    }

    #[test]
    fn deduplicates_by_scope_kind_and_normalized_content() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let first = store.upsert(draft("Project uses Rust", "first"))?;
        let second = store.upsert(draft("  project   uses RUST ", "second"))?;

        assert_eq!(first.candidate_id, second.candidate_id);
        assert_eq!(second.evidence.len(), 2);
        assert_eq!(store.list()?.len(), 1);
        Ok(())
    }

    #[test]
    fn candidate_snapshot_round_trips_through_log_record() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let candidate = store.upsert(draft("Round trip candidate", "source"))?;
        let encoded = serde_json::to_string(&candidate).map_err(|error| error.to_string())?;
        match decode_log_record(&encoded)? {
            EvidenceLogRecord::Candidate(decoded) => {
                assert_eq!(decoded.candidate_id, candidate.candidate_id);
                Ok(())
            }
            EvidenceLogRecord::Interaction(_) => {
                Err("candidate decoded as an interaction event".to_string())
            }
        }
    }

    #[test]
    fn interaction_snapshot_round_trips_and_unknown_record_types_fail_closed() -> Result<(), String>
    {
        let interaction = EvidenceInteractionEvent {
            schema_version: SCHEMA_VERSION,
            record_type: "interaction".to_string(),
            event_id: "interaction-round-trip".to_string(),
            candidate_id: "candidate-round-trip".to_string(),
            action: EvidenceInteractionAction::Rejected,
            timestamp: Utc::now(),
        };
        let encoded = serde_json::to_string(&interaction).map_err(|error| error.to_string())?;
        match decode_log_record(&encoded)? {
            EvidenceLogRecord::Interaction(decoded) => {
                assert_eq!(decoded.event_id, interaction.event_id);
            }
            EvidenceLogRecord::Candidate(_) => {
                return Err("interaction decoded as a candidate".to_string());
            }
        }
        for invalid in [
            r#"{"record_type":"candidate"}"#,
            r#"{"record_type":{"interaction":true}}"#,
            r#"{"record_type":"interaction","candidate_id":"missing-fields"}"#,
        ] {
            if decode_log_record(invalid).is_ok() {
                return Err(format!(
                    "invalid evidence record decoded successfully: {invalid}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn rejected_candidate_is_not_resurrected_by_duplicate_evidence() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let candidate = store.upsert(draft("Keep JSONL", "first"))?;
        store.reject(&candidate.candidate_id)?;
        let duplicate = store.upsert(draft("keep jsonl", "second"))?;

        assert_eq!(duplicate.status, EvidenceCandidateStatus::Rejected);
        assert_eq!(duplicate.evidence.len(), 2);
        let rejected_item = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "rejected candidate missing from audit log".to_string())?;
        assert!(!EvidenceReviewFilter::Pending.matches(&rejected_item));
        assert!(!EvidenceReviewFilter::Expired.matches(&rejected_item));
        assert!(!EvidenceReviewFilter::Undoable.matches(&rejected_item));
        Ok(())
    }

    #[test]
    fn pending_merge_candidate_refreshes_nonsemantic_proposal_metadata() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let proposal = conflict_proposal();
        let first = capture_memory_conflict(&store, &proposal)?;
        let mut refreshed = proposal;
        if let Some(member) = refreshed
            .members
            .iter_mut()
            .find(|member| member.key == "make")
        {
            member.confidence = 0.6;
            member.updated_at = 2;
        }
        let second = capture_memory_conflict(&store, &refreshed)?;

        assert_eq!(first.candidate_id, second.candidate_id);
        assert!(second.revision > first.revision);
        assert_eq!(
            second.action,
            EvidenceAction::MergeMemories {
                proposal: refreshed
            }
        );
        Ok(())
    }

    #[test]
    fn editing_does_not_reuse_fingerprint_derived_candidate_ids() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let original = store.upsert(draft("Original project fact", "first"))?;
        let edited = store.edit(&original.candidate_id, "Edited project fact")?;
        let repeated_original = store.upsert(draft("Original project fact", "second"))?;

        assert_eq!(edited.candidate_id, original.candidate_id);
        assert_ne!(repeated_original.candidate_id, original.candidate_id);
        assert_eq!(store.list()?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn accept_and_undo_are_backed_by_layered_memory() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let candidate = store.upsert(draft("Use append-only JSONL", "decision evidence"))?;
        let memory_store = Arc::new(echo_agent::memory::InMemoryStore::new());
        let change_log = echo_agent::evolution::JsonlChangeLog::new(
            temp.path().join(".eko/evolution/change-log.jsonl"),
        )
        .map_err(|error| error.to_string())?;
        let layer_manager = Arc::new(echo_agent::evolution::MemoryLayerManager::new(
            temp.path().join(".eko"),
            memory_store,
            Box::new(change_log),
        ));

        let accepted = store
            .accept(&candidate.candidate_id, None, &layer_manager)
            .await?;
        assert_eq!(accepted.status, EvidenceCandidateStatus::Applied);
        assert!(accepted.target.is_some());
        let accepted_item = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "accepted candidate missing from Review Inbox".to_string())?;
        assert!(EvidenceReviewFilter::Undoable.matches(&accepted_item));

        let undone = store.undo(&candidate.candidate_id, &layer_manager).await?;
        assert_eq!(undone.status, EvidenceCandidateStatus::Pending);
        assert!(undone.target.is_none());
        let review_item = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "undone candidate missing from Review Inbox".to_string())?;
        assert!(!review_item.expired);
        assert!(EvidenceReviewFilter::Pending.matches(&review_item));
        Ok(())
    }

    #[tokio::test]
    async fn memory_merge_candidate_accepts_and_restores_exact_metadata() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let memory_store = Arc::new(echo_agent::memory::InMemoryStore::new());
        let change_log = echo_agent::evolution::JsonlChangeLog::new(
            temp.path().join(".eko/evolution/change-log.jsonl"),
        )
        .map_err(|error| error.to_string())?;
        let layer_manager = Arc::new(echo_agent::evolution::MemoryLayerManager::new(
            temp.path().join(".eko"),
            memory_store,
            Box::new(change_log),
        ));
        seed_conflict(&layer_manager).await?;
        let proposal = conflict_proposal();
        let candidate = capture_memory_conflict(&store, &proposal)?;

        let accepted = store
            .accept(&candidate.candidate_id, None, &layer_manager)
            .await?;
        assert_eq!(accepted.status, EvidenceCandidateStatus::Applied);
        let secondary = layer_manager
            .locate("make")
            .await
            .ok_or_else(|| "merged secondary memory disappeared".to_string())?;
        assert_eq!(
            secondary.1.meta.status,
            echo_agent::memory::MemoryStatus::Superseded
        );
        assert_eq!(secondary.1.meta.superseded_by.as_deref(), Some("cargo"));

        let undone = store.undo(&candidate.candidate_id, &layer_manager).await?;
        assert_eq!(undone.status, EvidenceCandidateStatus::Pending);
        let restored = layer_manager
            .locate("make")
            .await
            .ok_or_else(|| "undone secondary memory disappeared".to_string())?;
        assert_eq!(
            restored.1.meta.status,
            echo_agent::memory::MemoryStatus::Active
        );
        assert_eq!(restored.1.meta.superseded_by, None);
        assert_eq!(restored.1.content, "Build uses make");
        Ok(())
    }

    #[tokio::test]
    async fn stale_memory_merge_candidate_fails_before_mutation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let memory_store = Arc::new(echo_agent::memory::InMemoryStore::new());
        let change_log = echo_agent::evolution::JsonlChangeLog::new(
            temp.path().join(".eko/evolution/change-log.jsonl"),
        )
        .map_err(|error| error.to_string())?;
        let layer_manager = Arc::new(echo_agent::evolution::MemoryLayerManager::new(
            temp.path().join(".eko"),
            memory_store,
            Box::new(change_log),
        ));
        seed_conflict(&layer_manager).await?;
        let proposal = conflict_proposal();
        let candidate = capture_memory_conflict(&store, &proposal)?;
        let changed_meta = echo_agent::memory::MemoryMeta::new(
            echo_agent::memory::MemoryType::ProjectFact,
            echo_agent::memory::MemorySource::ExplicitSave,
            "build",
        )
        .with_confidence(0.5);
        layer_manager
            .write_memory("make", "Build uses ninja", changed_meta)
            .await
            .map_err(|error| error.to_string())?;

        let error = store
            .accept(&candidate.candidate_id, None, &layer_manager)
            .await
            .err()
            .ok_or_else(|| "stale merge unexpectedly succeeded".to_string())?;
        assert!(error.contains("refresh Review Inbox"));
        let current = layer_manager
            .locate("make")
            .await
            .ok_or_else(|| "stale secondary memory disappeared".to_string())?;
        assert_eq!(current.1.content, "Build uses ninja");
        assert_eq!(
            current.1.meta.status,
            echo_agent::memory::MemoryStatus::Active
        );
        let review_item = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "stale candidate missing from Review Inbox".to_string())?;
        assert!(review_item.expired);
        assert!(EvidenceReviewFilter::Expired.matches(&review_item));
        assert!(!EvidenceReviewFilter::Pending.matches(&review_item));
        Ok(())
    }

    #[test]
    fn refreshed_candidate_clears_older_stale_marker() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = EvidenceStore::new(temp.path().join(".eko"));
        let proposal = conflict_proposal();
        let candidate = capture_memory_conflict(&store, &proposal)?;
        store.record_interaction(
            &candidate.candidate_id,
            EvidenceInteractionAction::AcceptFailed(EvidenceInteractionFailureKind::StaleProposal),
        )?;
        let expired = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "candidate missing after stale interaction".to_string())?;
        assert!(expired.expired);

        let mut refreshed = proposal;
        if let Some(member) = refreshed
            .members
            .iter_mut()
            .find(|member| member.key == "make")
        {
            member.updated_at = member.updated_at.saturating_add(1);
        }
        capture_memory_conflict(&store, &refreshed)?;
        let current = store
            .review_item(&candidate.candidate_id)?
            .ok_or_else(|| "refreshed candidate missing".to_string())?;
        assert!(!current.expired);
        Ok(())
    }
}
