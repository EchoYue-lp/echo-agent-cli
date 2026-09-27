//! EKO's durable admission and settlement owner for lazy framework reviews.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use echo_agent::evolution::{MemoryLayerManager, ReviewIdentity, ReviewOutcome};
use echo_agent::state::journal::{EventJournal, FileEventJournal, JournalDurabilityStatus};
use echo_agent::utils::fs::FileDurability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evidence::{EvidenceCandidate, EvidenceStore, capture_review_outcome};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReviewEvent {
    Admitted {
        operation_id: String,
        identity: ReviewIdentity,
        authority_scope: String,
        workspace_generation: String,
        memory_before: Option<String>,
    },
    Outcome {
        operation_id: String,
        outcome_json: String,
    },
    Terminal {
        operation_id: String,
        result: ReviewTerminal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewTerminal {
    Settled {
        evidence_candidate_id: Option<String>,
    },
    Interrupted {
        /// None means a pre-existing key makes attribution indeterminate.
        memory_persisted: Option<bool>,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct ReviewRecoveryReceipt {
    pub operation_id: String,
    pub identity: ReviewIdentity,
    pub authority_scope: String,
    pub workspace_generation: String,
    pub result: ReviewTerminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundReviewReceipt {
    pub operation_id: String,
    pub identity: ReviewIdentity,
    pub authority_scope: String,
    pub workspace_generation: String,
    pub outcome_recorded: bool,
    pub terminal: Option<ReviewTerminal>,
}

#[derive(Default)]
struct ReviewRecord {
    identity: Option<ReviewIdentity>,
    authority_scope: Option<String>,
    workspace_generation: Option<String>,
    memory_before: Option<String>,
    outcome: Option<ReviewOutcome>,
    terminal: Option<ReviewTerminal>,
}

#[derive(Clone)]
pub struct ReviewReceiptStore {
    journal: Arc<FileEventJournal<ReviewEvent>>,
    #[cfg(test)]
    path: PathBuf,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewAppendFault {
    Outcome,
    Terminal,
}

#[cfg(test)]
fn append_faults() -> &'static Mutex<HashMap<PathBuf, ReviewAppendFault>> {
    static FAULTS: OnceLock<Mutex<HashMap<PathBuf, ReviewAppendFault>>> = OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl ReviewReceiptStore {
    pub fn open(echo_agent_dir: &Path) -> Result<Self, String> {
        let path = echo_agent_dir
            .join("evolution")
            .join("background-review.jsonl");
        let journal = FileEventJournal::open(path.clone(), FileDurability::SyncData)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            journal: Arc::new(journal),
            #[cfg(test)]
            path,
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append_for_test(echo_agent_dir: &Path, fault: ReviewAppendFault) {
        let path = echo_agent_dir
            .join("evolution")
            .join("background-review.jsonl");
        let mut faults = append_faults()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        faults.insert(path, fault);
    }

    fn append(&self, event: ReviewEvent) -> Result<(), String> {
        #[cfg(test)]
        {
            let kind = match &event {
                ReviewEvent::Outcome { .. } => Some(ReviewAppendFault::Outcome),
                ReviewEvent::Terminal { .. } => Some(ReviewAppendFault::Terminal),
                ReviewEvent::Admitted { .. } => None,
            };
            let mut faults = append_faults()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if kind.is_some_and(|kind| faults.get(&self.path) == Some(&kind)) {
                faults.remove(&self.path);
                return Err("injected background review receipt append failure".to_string());
            }
        }
        let receipt = self
            .journal
            .append(event)
            .map_err(|error| error.to_string())?;
        match receipt.durability {
            JournalDurabilityStatus::Confirmed => Ok(()),
            other => Err(format!(
                "background review receipt was not durably confirmed: {other:?}"
            )),
        }
    }

    pub fn admit(
        &self,
        operation_id: String,
        identity: ReviewIdentity,
        authority_scope: &str,
        workspace_generation: &str,
        memory_before: Option<String>,
    ) -> Result<(), String> {
        if authority_scope.is_empty() || workspace_generation.is_empty() {
            return Err("background review admission requires workspace identity".to_string());
        }
        self.append(ReviewEvent::Admitted {
            operation_id,
            identity,
            authority_scope: authority_scope.to_string(),
            workspace_generation: workspace_generation.to_string(),
            memory_before,
        })
    }

    pub fn record_outcome(&self, operation_id: &str, outcome: ReviewOutcome) -> Result<(), String> {
        // The framework journal canonicalizes generic serde Values. Keep the
        // f32 confidence inside a JSON string so replay preserves its number
        // representation under serde_json's arbitrary-precision feature.
        let outcome_json = serde_json::to_string(&outcome).map_err(|error| error.to_string())?;
        self.append(ReviewEvent::Outcome {
            operation_id: operation_id.to_string(),
            outcome_json,
        })
    }

    pub fn settle(&self, operation_id: &str, result: ReviewTerminal) -> Result<(), String> {
        self.append(ReviewEvent::Terminal {
            operation_id: operation_id.to_string(),
            result,
        })
    }

    fn records(&self) -> Result<HashMap<String, ReviewRecord>, String> {
        let mut records: HashMap<String, ReviewRecord> = HashMap::new();
        let mut cursor = 0;
        loop {
            let events = self
                .journal
                .replay_after(cursor, 256)
                .map_err(|error| error.to_string())?;
            if events.is_empty() {
                break;
            }
            for record in events {
                cursor = record.sequence;
                match record.event.as_ref() {
                    ReviewEvent::Admitted {
                        operation_id,
                        identity,
                        authority_scope,
                        workspace_generation,
                        memory_before,
                    } => {
                        let state = records.entry(operation_id.clone()).or_default();
                        if state.identity.is_some() {
                            return Err(format!(
                                "duplicate background review admission {operation_id}"
                            ));
                        }
                        state.identity = Some(identity.clone());
                        state.authority_scope = Some(authority_scope.clone());
                        state.workspace_generation = Some(workspace_generation.clone());
                        state.memory_before = memory_before.clone();
                    }
                    ReviewEvent::Outcome {
                        operation_id,
                        outcome_json,
                    } => {
                        let state = records.get_mut(operation_id).ok_or_else(|| {
                            format!("background review outcome has no admission {operation_id}")
                        })?;
                        state.outcome =
                            Some(serde_json::from_str(outcome_json).map_err(|error| {
                                format!("invalid persisted review outcome: {error}")
                            })?);
                    }
                    ReviewEvent::Terminal {
                        operation_id,
                        result,
                    } => {
                        let state = records.get_mut(operation_id).ok_or_else(|| {
                            format!("background review terminal has no admission {operation_id}")
                        })?;
                        state.terminal = Some(result.clone());
                    }
                }
            }
        }
        Ok(records)
    }

    pub fn receipt(&self, operation_id: &str) -> Result<Option<BackgroundReviewReceipt>, String> {
        let mut records = self.records()?;
        let Some(record) = records.remove(operation_id) else {
            return Ok(None);
        };
        Ok(Some(BackgroundReviewReceipt {
            operation_id: operation_id.to_string(),
            identity: record.identity.ok_or_else(|| {
                format!("background review {operation_id} has no durable identity")
            })?,
            authority_scope: record.authority_scope.ok_or_else(|| {
                format!("background review {operation_id} has no authority scope")
            })?,
            workspace_generation: record.workspace_generation.ok_or_else(|| {
                format!("background review {operation_id} has no workspace generation")
            })?,
            outcome_recorded: record.outcome.is_some(),
            terminal: record.terminal,
        }))
    }

    pub fn pending_for_identity(
        &self,
        identity: &ReviewIdentity,
    ) -> Result<Option<String>, String> {
        for (operation_id, record) in self.records()? {
            if record.terminal.is_none() && record.identity.as_ref() == Some(identity) {
                return Ok(Some(operation_id));
            }
        }
        Ok(None)
    }

    pub async fn settle_interrupted(
        &self,
        operation_id: &str,
        identity: &ReviewIdentity,
        memory_before: Option<&str>,
        layer_manager: &MemoryLayerManager,
        reason: String,
    ) -> Result<ReviewTerminal, String> {
        layer_manager
            .reconcile_pending()
            .await
            .map_err(|error| error.to_string())?;
        let memory_after = memory_fingerprint(layer_manager, &identity.persistence_key).await?;
        let memory_persisted = match (memory_before, memory_after.as_deref()) {
            (None, None) => Some(false),
            (None, Some(_)) => Some(true),
            (Some(before), Some(after)) if before != after => Some(true),
            (Some(_), _) => None,
        };
        let result = ReviewTerminal::Interrupted {
            memory_persisted,
            reason,
        };
        self.settle(operation_id, result.clone())?;
        Ok(result)
    }

    pub async fn recover(
        &self,
        layer_manager: &MemoryLayerManager,
        evidence_store: &EvidenceStore,
        authority_scope: &str,
        workspace_generation: &str,
    ) -> Result<Vec<ReviewRecoveryReceipt>, String> {
        layer_manager
            .reconcile_pending()
            .await
            .map_err(|error| error.to_string())?;
        let mut recovered = Vec::new();
        for (operation_id, record) in self.records()? {
            if record.terminal.is_some() {
                continue;
            }
            let identity = record.identity.ok_or_else(|| {
                format!("background review {operation_id} has no durable identity")
            })?;
            if record.authority_scope.as_deref() != Some(authority_scope)
                || record.workspace_generation.as_deref() != Some(workspace_generation)
            {
                return Err(format!(
                    "background review {operation_id} belongs to a different workspace generation"
                ));
            }
            let result = if let Some(outcome) = record.outcome {
                if outcome.run_id != identity.run_id {
                    return Err(format!(
                        "background review {operation_id} outcome run ID does not match admission"
                    ));
                }
                let candidate = capture_review_outcome(evidence_store, &outcome)?;
                let result = ReviewTerminal::Settled {
                    evidence_candidate_id: candidate.map(|item| item.candidate_id),
                };
                self.settle(&operation_id, result.clone())?;
                result
            } else {
                self.settle_interrupted(
                    &operation_id,
                    &identity,
                    record.memory_before.as_deref(),
                    layer_manager,
                    "process exited before review outcome was recorded".to_string(),
                )
                .await?
            };
            recovered.push(ReviewRecoveryReceipt {
                operation_id,
                identity,
                authority_scope: authority_scope.to_string(),
                workspace_generation: workspace_generation.to_string(),
                result,
            });
        }
        Ok(recovered)
    }
}

pub async fn memory_fingerprint(
    manager: &MemoryLayerManager,
    key: &str,
) -> Result<Option<String>, String> {
    let entry = manager
        .locate(key)
        .await
        .map_err(|error| error.to_string())?;
    entry
        .map(|(_, entry)| {
            let bytes = serde_json::to_vec(&(entry.content, entry.meta))
                .map_err(|error| error.to_string())?;
            Ok(format!("{:x}", Sha256::digest(bytes)))
        })
        .transpose()
}

pub fn evidence_terminal(candidate: &Option<EvidenceCandidate>) -> ReviewTerminal {
    ReviewTerminal::Settled {
        evidence_candidate_id: candidate.as_ref().map(|item| item.candidate_id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::evolution::{
        MemoryRuntimeIntegrationBuilder, ReviewCandidate, ReviewCandidateKind,
    };
    use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType, Store};

    fn manager(echo_agent_dir: &Path, store: Arc<dyn Store>) -> Result<MemoryLayerManager, String> {
        MemoryRuntimeIntegrationBuilder::new(echo_agent_dir.to_path_buf(), store)
            .build_layer_manager()
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn recovery_replays_outcome_into_inbox_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join(".eko");
        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let manager = manager(&root, store)?;
        let receipts = ReviewReceiptStore::open(&root)?;
        let identity = ReviewIdentity::for_run("review-replay");
        receipts.admit(
            "operation-a".to_string(),
            identity.clone(),
            "scope-a",
            "generation-a",
            None,
        )?;
        receipts.record_outcome(
            "operation-a",
            ReviewOutcome {
                run_id: identity.run_id.clone(),
                actions: vec!["candidate proposed".to_string()],
                nothing_to_save: false,
                candidate: Some(ReviewCandidate {
                    kind: ReviewCandidateKind::ProjectFact,
                    content: "workspace uses Rust".to_string(),
                    evidence: "Cargo.toml declares Rust crates".to_string(),
                    confidence: 0.9,
                    persisted: Some(false),
                }),
                error: None,
            },
        )?;
        let inbox = EvidenceStore::new(root.clone());
        let reopened = ReviewReceiptStore::open(&root)?;
        let recovered = reopened
            .recover(&manager, &inbox, "scope-a", "generation-a")
            .await?;
        assert_eq!(recovered.len(), 1);
        assert!(matches!(
            recovered.first().map(|item| &item.result),
            Some(ReviewTerminal::Settled { .. })
        ));
        assert_eq!(inbox.list()?.len(), 1);
        assert!(
            reopened
                .recover(&manager, &inbox, "scope-a", "generation-a")
                .await?
                .is_empty()
        );
        assert_eq!(inbox.list()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn recovery_exposes_persisted_memory_without_inventing_outcome() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join(".eko");
        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let manager = manager(&root, store)?;
        let receipts = ReviewReceiptStore::open(&root)?;
        let identity = ReviewIdentity::for_run("review-interrupted");
        receipts.admit(
            "operation-b".to_string(),
            identity.clone(),
            "scope-b",
            "generation-b",
            None,
        )?;
        manager
            .write_memory(
                &identity.persistence_key,
                "explicit durable preference",
                MemoryMeta::new(
                    MemoryType::UserPreference,
                    MemorySource::ExplicitSave,
                    "background-review-test",
                ),
            )
            .await
            .map_err(|error| error.to_string())?;
        let inbox = EvidenceStore::new(root.clone());
        let reopened = ReviewReceiptStore::open(&root)?;
        assert!(
            reopened
                .recover(&manager, &inbox, "scope-b", "generation-other")
                .await
                .is_err()
        );
        let recovered = reopened
            .recover(&manager, &inbox, "scope-b", "generation-b")
            .await?;
        assert_eq!(recovered.len(), 1);
        assert!(matches!(
            recovered.first().map(|item| &item.result),
            Some(ReviewTerminal::Interrupted {
                memory_persisted: Some(true),
                ..
            })
        ));
        assert!(inbox.list()?.is_empty());
        assert!(
            reopened
                .recover(&manager, &inbox, "scope-b", "generation-b")
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn preexisting_run_memory_is_not_attributed_to_interrupted_review() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join(".eko");
        let manager = manager(&root, Arc::new(InMemoryStore::new()))?;
        let identity = ReviewIdentity::for_run("review-repeated");
        manager
            .write_memory(
                &identity.persistence_key,
                "previous review memory",
                MemoryMeta::new(
                    MemoryType::UserPreference,
                    MemorySource::ExplicitSave,
                    "background-review-test",
                ),
            )
            .await
            .map_err(|error| error.to_string())?;
        let before = memory_fingerprint(&manager, &identity.persistence_key).await?;
        let receipts = ReviewReceiptStore::open(&root)?;
        receipts.admit(
            "operation-repeat".to_string(),
            identity,
            "scope-repeat",
            "generation-repeat",
            before,
        )?;
        let recovered = ReviewReceiptStore::open(&root)?
            .recover(
                &manager,
                &EvidenceStore::new(root),
                "scope-repeat",
                "generation-repeat",
            )
            .await?;
        assert!(matches!(
            recovered.first().map(|item| &item.result),
            Some(ReviewTerminal::Interrupted {
                memory_persisted: None,
                ..
            })
        ));
        Ok(())
    }
}
