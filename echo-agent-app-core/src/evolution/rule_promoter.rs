//! Rule promotion — promotes high-confidence memories to agent rules.
//!
//! RulePromoter scans typed memories for high-confidence entries that meet
//! promotion criteria and writes them to learned-rules.md as permanent rules.

use crate::instruction_provider::InstructionProvider;
use chrono::{DateTime, Utc};
use echo_agent::evolution::{
    ChangeEntry, ChangeLog, ChangeType, EntityType, EvolutionSecurityGuard, InputTrustLevel,
    layer::WARM_NAMESPACE,
};
use echo_agent::memory::{
    MemoryFilter, MemoryStatus, MemoryType, Store, TypedMemoryEntry, TypedMemoryStore,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PROMOTION_RECEIPT_VERSION: u32 = 1;
const MAX_PROMOTION_RECEIPTS: usize = 4096;
const MAX_PROMOTION_RECEIPT_BYTES: u64 = 64 * 1024;
const PROMOTION_LOCK_ATTEMPTS: usize = 100;
const PROMOTION_LOCK_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePromotionPhase {
    Prepared,
    EffectsApplied,
    Committed,
}

/// Durable coordination receipt for one memory-to-rule promotion.
///
/// This record deliberately stores identities and digests, not a second copy
/// of memory or rule content. The canonical memory Store and learned-rules.md
/// remain the only content authorities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePromotionReceipt {
    pub version: u32,
    pub promotion_id: String,
    pub namespace: Vec<String>,
    pub memory_key: String,
    pub source_sha256: String,
    pub rule_sha256: String,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub prepared_at: DateTime<Utc>,
    pub phase: RulePromotionPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulePromotionError {
    Security(String),
    MissingMemory {
        namespace: Vec<String>,
        key: String,
    },
    Conflict {
        promotion_id: String,
        reason: String,
    },
    CorruptReceipt {
        path: PathBuf,
        reason: String,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        reason: String,
    },
    Memory(String),
    Audit(String),
    LockTimeout(PathBuf),
    Projection(String),
    Injected(&'static str),
}

impl std::fmt::Display for RulePromotionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Security(reason) => write!(formatter, "rule promotion rejected: {reason}"),
            Self::MissingMemory { namespace, key } => write!(
                formatter,
                "memory {key:?} was not found in namespace {namespace:?}"
            ),
            Self::Conflict {
                promotion_id,
                reason,
            } => write!(
                formatter,
                "rule promotion {promotion_id} conflicts with current authority: {reason}"
            ),
            Self::CorruptReceipt { path, reason } => write!(
                formatter,
                "invalid rule-promotion receipt {}: {reason}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                reason,
            } => write!(
                formatter,
                "failed to {operation} {}: {reason}",
                path.display()
            ),
            Self::Memory(reason) => {
                write!(formatter, "rule-promotion memory update failed: {reason}")
            }
            Self::Audit(reason) => write!(formatter, "rule-promotion audit failed: {reason}"),
            Self::LockTimeout(path) => write!(
                formatter,
                "timed out waiting for rule-promotion lock {}",
                path.display()
            ),
            Self::Projection(reason) => {
                write!(formatter, "rule-promotion projection failed: {reason}")
            }
            Self::Injected(stage) => {
                write!(formatter, "injected rule-promotion fault after {stage}")
            }
        }
    }
}

impl std::error::Error for RulePromotionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromotionFault {
    Prepared,
    Rule,
    Memory,
    Audit,
}

/// Criteria for memory promotion to a rule.
pub struct PromotionCriteria {
    /// Minimum confidence score (0.0-1.0).
    pub min_confidence: f32,
    /// Required memory types (empty = any type).
    pub allowed_types: Vec<MemoryType>,
    /// Minimum age in days.
    pub min_age_days: u32,
}

impl Default for PromotionCriteria {
    fn default() -> Self {
        Self {
            min_confidence: 0.95,
            allowed_types: vec![
                MemoryType::ProjectFact,
                MemoryType::WorkflowPattern,
                MemoryType::UserPreference,
            ],
            min_age_days: 7,
        }
    }
}

/// A proposed rule for promotion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleProposal {
    /// Unique key of the memory being promoted.
    pub memory_key: String,
    /// Namespace where the memory is stored.
    pub namespace: Vec<String>,
    /// The rule text to add to learned-rules.md.
    pub rule_text: String,
    /// Confidence score of the source memory.
    pub confidence: f32,
    /// Memory type of the source.
    pub memory_type: MemoryType,
    /// When this proposal was created.
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub proposed_at: DateTime<Utc>,
    /// Reason for promotion.
    pub reason: String,
}

/// Promotes high-confidence memories to agent rules in learned-rules.md.
pub struct RulePromoter {
    store: Arc<dyn Store>,
    security_guard: EvolutionSecurityGuard,
    criteria: PromotionCriteria,
    rules_path: PathBuf,
    #[cfg(test)]
    fault: Option<PromotionFault>,
}

impl RulePromoter {
    /// Create a new RulePromoter with default criteria.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            security_guard: EvolutionSecurityGuard::new(
                echo_agent::evolution::SecurityConfig::default(),
            ),
            criteria: PromotionCriteria::default(),
            rules_path: InstructionProvider::agents_instructions_path(),
            #[cfg(test)]
            fault: None,
        }
    }

    /// Create a new RulePromoter with custom criteria.
    pub fn with_criteria(store: Arc<dyn Store>, criteria: PromotionCriteria) -> Self {
        Self {
            store,
            security_guard: EvolutionSecurityGuard::new(
                echo_agent::evolution::SecurityConfig::default(),
            ),
            criteria,
            rules_path: InstructionProvider::agents_instructions_path(),
            #[cfg(test)]
            fault: None,
        }
    }

    /// Bind rule publication to an explicitly admitted workspace root.
    pub fn with_rules_path(mut self, rules_path: PathBuf) -> Self {
        self.rules_path = rules_path;
        self
    }

    /// Scan memories and generate rule promotion proposals.
    ///
    /// Returns a list of RuleProposal for memories that meet the promotion criteria.
    pub async fn scan_for_proposals(&self) -> Vec<RuleProposal> {
        let typed_store = TypedMemoryStore::new(self.store.clone());
        // 扫描与写入主路径统一:用 WARM_NAMESPACE(= ["agent","memories"]),
        // 与 MemoryLayerManager::write_memory / MemoryRecaller / Dreaming 同源。
        // 此前硬编码 ["agent","typed_memories"] 是死 namespace(无生产写入),
        // 导致晋升扫描永远命中空集。
        let filter = MemoryFilter::new().with_status(MemoryStatus::Active);

        let memories = match typed_store.list_typed(WARM_NAMESPACE, &filter).await {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut proposals = Vec::new();
        let now = Utc::now();

        for entry in memories {
            // Check confidence threshold
            if entry.meta.confidence < self.criteria.min_confidence {
                continue;
            }

            // Check memory type
            if !self.criteria.allowed_types.is_empty()
                && !self
                    .criteria
                    .allowed_types
                    .contains(&entry.meta.memory_type)
            {
                continue;
            }

            // Check age
            let age_days = (now.timestamp() - entry.raw.updated_at as i64) / 86400;
            if age_days < self.criteria.min_age_days as i64 {
                continue;
            }

            // Check if already promoted (look for marker in content).
            // 用前缀匹配:promote_rule 写入的格式是
            // `<!-- PROMOTED_TO_RULE: {timestamp} -->`,故按前缀判定,兼容带/不带
            // 时间戳,避免 scan 与 write 标记不一致导致重复晋升。
            if entry.content.contains("<!-- PROMOTED_TO_RULE") {
                continue;
            }

            // Create proposal
            let rule_text = self.format_rule_text(&entry);
            let reason = format!(
                "High confidence ({:.2}), age {} days, type {:?}",
                entry.meta.confidence, age_days, entry.meta.memory_type
            );

            proposals.push(RuleProposal {
                memory_key: entry.key.clone(),
                namespace: WARM_NAMESPACE.iter().map(|s| s.to_string()).collect(),
                rule_text,
                confidence: entry.meta.confidence,
                memory_type: entry.meta.memory_type,
                proposed_at: now,
                reason,
            });
        }

        proposals
    }

    /// Format a memory entry as a rule text for learned-rules.md.
    fn format_rule_text(&self, entry: &TypedMemoryEntry) -> String {
        self.format_rule_text_from_parts(&entry.content, entry.meta.memory_type)
    }

    /// Promote a rule through a durable, retryable application transaction.
    ///
    /// A durable Prepared receipt is written before either content authority is
    /// changed. Recovery then converges learned-rules.md, the source memory and
    /// the canonical ChangeLog forward by stable promotion ID. This is not a
    /// claim of cross-file atomicity; it is an idempotent local outbox/finalizer
    /// contract that makes every crash window recoverable.
    pub async fn promote_rule(
        &self,
        proposal: &RuleProposal,
        change_log: &dyn ChangeLog,
    ) -> Result<RulePromotionReceipt, RulePromotionError> {
        let verdict = self
            .security_guard
            .check_rule_promotion(&proposal.rule_text, InputTrustLevel::Trusted);
        if !verdict.allowed {
            return Err(RulePromotionError::Security(
                verdict
                    .reason
                    .unwrap_or_else(|| "unknown reason".to_string()),
            ));
        }
        let _lock = self.acquire_transaction_lock().await?;
        self.reconcile_pending_locked(change_log).await?;
        let typed_store = TypedMemoryStore::new(self.store.clone());
        let namespace_refs: Vec<&str> = proposal.namespace.iter().map(|s| s.as_str()).collect();
        let entry = typed_store
            .get_typed(&namespace_refs, &proposal.memory_key)
            .await
            .map_err(|error| RulePromotionError::Memory(error.to_string()))?
            .ok_or_else(|| RulePromotionError::MissingMemory {
                namespace: proposal.namespace.clone(),
                key: proposal.memory_key.clone(),
            })?;
        let (source_content, existing_marker_id) = split_promotion_marker(&entry.content)?;
        let canonical_rule =
            self.format_rule_text_from_parts(&source_content, entry.meta.memory_type);
        if canonical_rule != proposal.rule_text {
            return Err(RulePromotionError::Conflict {
                promotion_id: "unprepared".to_string(),
                reason: "proposal rule text no longer matches its source memory".to_string(),
            });
        }
        let source_sha256 = digest_text(&source_content);
        let rule_sha256 = digest_text(&canonical_rule);
        let promotion_id = stable_promotion_id(
            &proposal.namespace,
            &proposal.memory_key,
            &source_sha256,
            &rule_sha256,
        );
        if existing_marker_id
            .as_deref()
            .is_some_and(|existing| existing != promotion_id)
        {
            return Err(RulePromotionError::Conflict {
                promotion_id,
                reason: "source memory is marked by a different promotion".to_string(),
            });
        }
        let receipt_path = self.receipt_path(&promotion_id);
        let mut receipt = if receipt_path.exists() {
            let existing = read_receipt(&receipt_path)?;
            validate_receipt(&receipt_path, &existing)?;
            if existing.namespace != proposal.namespace
                || existing.memory_key != proposal.memory_key
                || existing.source_sha256 != source_sha256
                || existing.rule_sha256 != rule_sha256
            {
                return Err(RulePromotionError::Conflict {
                    promotion_id,
                    reason: "stable receipt identity resolves to different content".to_string(),
                });
            }
            existing
        } else {
            let prepared = RulePromotionReceipt {
                version: PROMOTION_RECEIPT_VERSION,
                promotion_id,
                namespace: proposal.namespace.clone(),
                memory_key: proposal.memory_key.clone(),
                source_sha256,
                rule_sha256,
                prepared_at: Utc::now(),
                phase: RulePromotionPhase::Prepared,
            };
            write_receipt(&receipt_path, &prepared)?;
            self.inject_fault(PromotionFault::Prepared)?;
            prepared
        };
        if receipt.phase == RulePromotionPhase::Prepared {
            self.reconcile_receipt(&mut receipt, change_log).await?;
        }
        Ok(receipt)
    }

    /// Resume every prepared transaction in this workspace.
    ///
    /// All receipts are decoded and validated before the first mutation, so an
    /// interior corrupt receipt fails the whole pass closed instead of allowing
    /// a partial, order-dependent recovery.
    pub async fn reconcile_pending(
        &self,
        change_log: &dyn ChangeLog,
    ) -> Result<Vec<RulePromotionReceipt>, RulePromotionError> {
        let _lock = self.acquire_transaction_lock().await?;
        self.reconcile_pending_locked(change_log).await
    }

    async fn reconcile_pending_locked(
        &self,
        change_log: &dyn ChangeLog,
    ) -> Result<Vec<RulePromotionReceipt>, RulePromotionError> {
        let mut receipts = self.load_receipts()?;
        let mut reconciled = Vec::new();
        for receipt in &mut receipts {
            if receipt.phase == RulePromotionPhase::Prepared {
                self.reconcile_receipt(receipt, change_log).await?;
            }
            if receipt.phase == RulePromotionPhase::EffectsApplied {
                reconciled.push(receipt.clone());
            }
        }
        Ok(reconciled)
    }

    async fn reconcile_receipt(
        &self,
        receipt: &mut RulePromotionReceipt,
        change_log: &dyn ChangeLog,
    ) -> Result<(), RulePromotionError> {
        if receipt.phase == RulePromotionPhase::Committed {
            return Ok(());
        }
        let typed_store = TypedMemoryStore::new(self.store.clone());
        let namespace_refs: Vec<&str> = receipt.namespace.iter().map(String::as_str).collect();
        let entry = typed_store
            .get_typed(&namespace_refs, &receipt.memory_key)
            .await
            .map_err(|error| RulePromotionError::Memory(error.to_string()))?
            .ok_or_else(|| RulePromotionError::MissingMemory {
                namespace: receipt.namespace.clone(),
                key: receipt.memory_key.clone(),
            })?;
        let (source_content, existing_marker_id) = split_promotion_marker(&entry.content)?;
        if existing_marker_id
            .as_deref()
            .is_some_and(|existing| existing != receipt.promotion_id)
        {
            return Err(RulePromotionError::Conflict {
                promotion_id: receipt.promotion_id.clone(),
                reason: "source memory contains a different promotion marker".to_string(),
            });
        }
        if digest_text(&source_content) != receipt.source_sha256 {
            return Err(RulePromotionError::Conflict {
                promotion_id: receipt.promotion_id.clone(),
                reason: "source memory changed after the promotion was prepared".to_string(),
            });
        }
        let canonical_rule =
            self.format_rule_text_from_parts(&source_content, entry.meta.memory_type);
        if digest_text(&canonical_rule) != receipt.rule_sha256 {
            return Err(RulePromotionError::Conflict {
                promotion_id: receipt.promotion_id.clone(),
                reason: "source memory type no longer reconstructs the prepared rule".to_string(),
            });
        }

        self.ensure_rule_block(receipt, &canonical_rule)?;
        self.inject_fault(PromotionFault::Rule)?;

        let marker = promotion_marker(&receipt.promotion_id);
        if !entry.content.contains(&marker) {
            let updated_content = format!("{}\n\n{marker}", source_content.trim_end());
            typed_store
                .put_typed(
                    &namespace_refs,
                    &receipt.memory_key,
                    &updated_content,
                    entry.meta,
                )
                .await
                .map_err(|error| RulePromotionError::Memory(error.to_string()))?;
        }
        self.inject_fault(PromotionFault::Memory)?;

        record_promotion_audit(change_log, receipt)?;
        self.inject_fault(PromotionFault::Audit)?;
        receipt.phase = RulePromotionPhase::EffectsApplied;
        write_receipt(&self.receipt_path(&receipt.promotion_id), receipt)
    }

    /// Mark one promotion complete only after the application projection
    /// transaction has published the exact instruction snapshot everywhere.
    pub async fn commit_projection(
        &self,
        expected: &RulePromotionReceipt,
    ) -> Result<RulePromotionReceipt, RulePromotionError> {
        let _lock = self.acquire_transaction_lock().await?;
        let path = self.receipt_path(&expected.promotion_id);
        let mut receipt = read_receipt(&path)?;
        validate_receipt(&path, &receipt)?;
        if receipt.namespace != expected.namespace
            || receipt.memory_key != expected.memory_key
            || receipt.source_sha256 != expected.source_sha256
            || receipt.rule_sha256 != expected.rule_sha256
            || receipt.prepared_at != expected.prepared_at
        {
            return Err(RulePromotionError::Conflict {
                promotion_id: expected.promotion_id.clone(),
                reason: "projection commit receipt no longer matches prepared identity".to_string(),
            });
        }
        match receipt.phase {
            RulePromotionPhase::Prepared => Err(RulePromotionError::Conflict {
                promotion_id: receipt.promotion_id,
                reason: "projection cannot commit before durable effects and audit".to_string(),
            }),
            RulePromotionPhase::EffectsApplied => {
                receipt.phase = RulePromotionPhase::Committed;
                write_receipt(&path, &receipt)?;
                Ok(receipt)
            }
            RulePromotionPhase::Committed => Ok(receipt),
        }
    }

    fn ensure_rule_block(
        &self,
        receipt: &RulePromotionReceipt,
        canonical_rule: &str,
    ) -> Result<(), RulePromotionError> {
        let existing = read_optional_text(&self.rules_path)?;
        let begin = promotion_rule_begin(&receipt.promotion_id);
        let end = promotion_rule_end(&receipt.promotion_id);
        let begin_count = existing.matches(&begin).count();
        let end_count = existing.matches(&end).count();
        if begin_count > 1 || end_count > 1 || begin_count != end_count {
            return Err(RulePromotionError::Conflict {
                promotion_id: receipt.promotion_id.clone(),
                reason: "learned-rules.md contains a duplicated or incomplete promotion block"
                    .to_string(),
            });
        }
        if begin_count == 1 {
            let block = extract_rule_block(&existing, &begin, &end).ok_or_else(|| {
                RulePromotionError::Conflict {
                    promotion_id: receipt.promotion_id.clone(),
                    reason: "learned-rules.md promotion block is malformed".to_string(),
                }
            })?;
            if digest_text(block) != receipt.rule_sha256 {
                return Err(RulePromotionError::Conflict {
                    promotion_id: receipt.promotion_id.clone(),
                    reason: "prepared rule block was edited before commit".to_string(),
                });
            }
            return Ok(());
        }

        let block = format!("{begin}\n{canonical_rule}\n{end}");
        let content = if existing.trim().is_empty() {
            format!(
                "# Agent Rules\n\nAuto-promoted rules from high-confidence memories.\n\n## Rules\n\n{block}"
            )
        } else if existing.contains("## Rules") {
            format!("{}\n{block}", existing.trim_end())
        } else {
            format!("{}\n\n## Rules\n\n{block}", existing.trim_end())
        };
        InstructionProvider::save_agents_instructions_at(&self.rules_path, &content).map_err(
            |error| RulePromotionError::Io {
                operation: "write",
                path: self.rules_path.clone(),
                reason: error.to_string(),
            },
        )
    }

    fn load_receipts(&self) -> Result<Vec<RulePromotionReceipt>, RulePromotionError> {
        let dir = self.receipts_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(RulePromotionError::Io {
                    operation: "read directory",
                    path: dir,
                    reason: error.to_string(),
                });
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| RulePromotionError::Io {
                operation: "read directory entry",
                path: dir.clone(),
                reason: error.to_string(),
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            paths.push(path);
            if paths.len() > MAX_PROMOTION_RECEIPTS {
                return Err(RulePromotionError::CorruptReceipt {
                    path: dir,
                    reason: format!(
                        "receipt count exceeds the bounded limit of {MAX_PROMOTION_RECEIPTS}"
                    ),
                });
            }
        }
        paths.sort();
        let mut receipts = Vec::with_capacity(paths.len());
        for path in paths {
            let receipt = read_receipt(&path)?;
            validate_receipt(&path, &receipt)?;
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    async fn acquire_transaction_lock(&self) -> Result<std::fs::File, RulePromotionError> {
        let lock_path = self.receipts_dir().join("promotion.lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RulePromotionError::Io {
                operation: "create directory",
                path: parent.to_path_buf(),
                reason: error.to_string(),
            })?;
        }
        if std::fs::symlink_metadata(&lock_path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(RulePromotionError::Io {
                operation: "open lock",
                path: lock_path,
                reason: "lock path is a symlink".to_string(),
            });
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| RulePromotionError::Io {
                operation: "open lock",
                path: lock_path.clone(),
                reason: error.to_string(),
            })?;
        for _ in 0..PROMOTION_LOCK_ATTEMPTS {
            match lock.try_lock_exclusive() {
                Ok(()) => return Ok(lock),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(PROMOTION_LOCK_BACKOFF).await;
                }
                Err(error) => {
                    return Err(RulePromotionError::Io {
                        operation: "lock",
                        path: lock_path,
                        reason: error.to_string(),
                    });
                }
            }
        }
        Err(RulePromotionError::LockTimeout(lock_path))
    }

    fn receipts_dir(&self) -> PathBuf {
        self.rules_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("evolution")
            .join("rule-promotions")
    }

    fn receipt_path(&self, promotion_id: &str) -> PathBuf {
        self.receipts_dir().join(format!("{promotion_id}.json"))
    }

    fn format_rule_text_from_parts(&self, content: &str, memory_type: MemoryType) -> String {
        let prefix = match memory_type {
            MemoryType::ProjectFact => "Project fact",
            MemoryType::WorkflowPattern => "Workflow pattern",
            MemoryType::UserPreference => "User preference",
            MemoryType::ToolUsage => "Tool usage",
            _ => "Rule",
        };
        format!("- **{prefix}**: {}", content.trim_end())
    }

    #[cfg(test)]
    fn with_fault(mut self, fault: PromotionFault) -> Self {
        self.fault = Some(fault);
        self
    }

    #[cfg(test)]
    fn inject_fault(&self, stage: PromotionFault) -> Result<(), RulePromotionError> {
        if self.fault == Some(stage) {
            return Err(RulePromotionError::Injected(match stage {
                PromotionFault::Prepared => "prepared receipt",
                PromotionFault::Rule => "rule write",
                PromotionFault::Memory => "memory marker",
                PromotionFault::Audit => "audit write",
            }));
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn inject_fault(&self, _stage: PromotionFault) -> Result<(), RulePromotionError> {
        Ok(())
    }

    /// Get the current promotion criteria.
    pub fn criteria(&self) -> &PromotionCriteria {
        &self.criteria
    }
}

fn digest_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn stable_promotion_id(
    namespace: &[String],
    memory_key: &str,
    source_sha256: &str,
    rule_sha256: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"eko-rule-promotion-v1");
    for value in
        namespace
            .iter()
            .map(String::as_str)
            .chain([memory_key, source_sha256, rule_sha256])
    {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("rp_{}", hex::encode(hasher.finalize()))
}

fn promotion_marker(promotion_id: &str) -> String {
    format!("<!-- PROMOTED_TO_RULE: {promotion_id} -->")
}

fn promotion_rule_begin(promotion_id: &str) -> String {
    format!("<!-- EKO_RULE_PROMOTION:{promotion_id}:BEGIN -->")
}

fn promotion_rule_end(promotion_id: &str) -> String {
    format!("<!-- EKO_RULE_PROMOTION:{promotion_id}:END -->")
}

fn split_promotion_marker(current: &str) -> Result<(String, Option<String>), RulePromotionError> {
    let marker_prefix = "<!-- PROMOTED_TO_RULE";
    let marker_count = current.matches(marker_prefix).count();
    if marker_count == 0 {
        return Ok((current.trim_end().to_string(), None));
    }
    let trimmed = current.trim_end();
    let marker_start =
        trimmed
            .rfind(marker_prefix)
            .ok_or_else(|| RulePromotionError::Conflict {
                promotion_id: "unknown".to_string(),
                reason: "source memory promotion marker cannot be located".to_string(),
            })?;
    let marker = trimmed
        .get(marker_start..)
        .ok_or_else(|| RulePromotionError::Conflict {
            promotion_id: "unknown".to_string(),
            reason: "source memory promotion marker is not UTF-8 aligned".to_string(),
        })?;
    let id = marker
        .strip_prefix("<!-- PROMOTED_TO_RULE: ")
        .and_then(|value| value.strip_suffix(" -->"))
        .filter(|value| {
            value.strip_prefix("rp_").is_some_and(|digest| {
                digest.chars().count() == 64 && digest.chars().all(|ch| ch.is_ascii_hexdigit())
            })
        })
        .ok_or_else(|| RulePromotionError::Conflict {
            promotion_id: "unknown".to_string(),
            reason: "source memory contains a legacy or malformed promotion marker".to_string(),
        })?;
    if marker_count != 1 {
        return Err(RulePromotionError::Conflict {
            promotion_id: id.to_string(),
            reason: "source memory contains duplicate promotion markers".to_string(),
        });
    }
    let source = trimmed
        .get(..marker_start)
        .ok_or_else(|| RulePromotionError::Conflict {
            promotion_id: id.to_string(),
            reason: "source memory marker offset is invalid".to_string(),
        })?;
    Ok((source.trim_end().to_string(), Some(id.to_string())))
}

fn extract_rule_block<'a>(content: &'a str, begin: &str, end: &str) -> Option<&'a str> {
    let after_begin = content.split_once(begin)?.1.strip_prefix('\n')?;
    let (block, after_end) = after_begin.split_once(end)?;
    if !after_end.contains(begin) && !after_end.contains(end) {
        Some(block.trim_end_matches('\n'))
    } else {
        None
    }
}

fn read_optional_text(path: &Path) -> Result<String, RulePromotionError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => {
            return Err(RulePromotionError::Io {
                operation: "inspect",
                path: path.to_path_buf(),
                reason: error.to_string(),
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(RulePromotionError::Io {
            operation: "read",
            path: path.to_path_buf(),
            reason: "learned-rules authority must be a regular non-symlink file".to_string(),
        });
    }
    std::fs::read_to_string(path).map_err(|error| RulePromotionError::Io {
        operation: "read",
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

fn read_receipt(path: &Path) -> Result<RulePromotionReceipt, RulePromotionError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| RulePromotionError::Io {
        operation: "inspect",
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RulePromotionError::CorruptReceipt {
            path: path.to_path_buf(),
            reason: "receipt is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_PROMOTION_RECEIPT_BYTES {
        return Err(RulePromotionError::CorruptReceipt {
            path: path.to_path_buf(),
            reason: format!("receipt exceeds {MAX_PROMOTION_RECEIPT_BYTES} bytes"),
        });
    }
    let bytes = std::fs::read(path).map_err(|error| RulePromotionError::Io {
        operation: "read",
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| RulePromotionError::CorruptReceipt {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

fn validate_receipt(path: &Path, receipt: &RulePromotionReceipt) -> Result<(), RulePromotionError> {
    if receipt.version != PROMOTION_RECEIPT_VERSION {
        return Err(RulePromotionError::CorruptReceipt {
            path: path.to_path_buf(),
            reason: format!("unsupported version {}", receipt.version),
        });
    }
    let expected = stable_promotion_id(
        &receipt.namespace,
        &receipt.memory_key,
        &receipt.source_sha256,
        &receipt.rule_sha256,
    );
    let expected_name = format!("{expected}.json");
    if receipt.promotion_id != expected
        || path.file_name().and_then(|value| value.to_str()) != Some(expected_name.as_str())
    {
        return Err(RulePromotionError::CorruptReceipt {
            path: path.to_path_buf(),
            reason: "promotion identity does not match receipt content or filename".to_string(),
        });
    }
    Ok(())
}

fn write_receipt(path: &Path, receipt: &RulePromotionReceipt) -> Result<(), RulePromotionError> {
    let bytes =
        serde_json::to_vec_pretty(receipt).map_err(|error| RulePromotionError::CorruptReceipt {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    echo_agent::utils::fs::atomic_write(path, &bytes).map_err(|error| RulePromotionError::Io {
        operation: "write",
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

fn record_promotion_audit(
    change_log: &dyn ChangeLog,
    receipt: &RulePromotionReceipt,
) -> Result<(), RulePromotionError> {
    let entity_key = format!("{}#{}", receipt.memory_key, receipt.promotion_id);
    let entry = ChangeEntry {
        change_id: receipt.promotion_id.clone(),
        timestamp: receipt.prepared_at,
        entity_type: EntityType::Memory,
        entity_key,
        change_type: ChangeType::Promote,
        before: Some(serde_json::json!({
            "status": "active",
            "source_sha256": receipt.source_sha256,
        })),
        after: Some(serde_json::json!({
            "status": "promoted_to_rule",
            "rule_sha256": receipt.rule_sha256,
            "promotion_id": receipt.promotion_id,
        })),
        reason: format!(
            "Promoted memory {} to learned-rules.md as {}",
            receipt.memory_key, receipt.promotion_id
        ),
        trigger: "rule_promoter".to_string(),
    };
    // Framework owns the durable audit authority. Its idempotent stable-ID
    // append rejects conflicting duplicates and fails closed on corrupt history.
    change_log
        .record_idempotent(entry)
        .map(|_| ())
        .map_err(|error| RulePromotionError::Audit(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct RecordingChangeLog {
        fail: AtomicBool,
        entries: Mutex<Vec<ChangeEntry>>,
    }

    impl ChangeLog for RecordingChangeLog {
        fn record(
            &self,
            entry: echo_agent::evolution::ChangeEntry,
        ) -> echo_agent::error::Result<()> {
            self.record_idempotent(entry).map(|_| ())
        }

        fn record_idempotent(
            &self,
            entry: echo_agent::evolution::ChangeEntry,
        ) -> echo_agent::error::Result<echo_agent::evolution::ChangeRecordOutcome> {
            if self.fail.load(Ordering::SeqCst) {
                Err(echo_agent::error::ReactError::Other(
                    "injected audit failure".to_string(),
                ))
            } else {
                let mut entries = self
                    .entries
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(existing) = entries
                    .iter()
                    .find(|existing| existing.change_id == entry.change_id)
                {
                    return if existing == &entry {
                        Ok(echo_agent::evolution::ChangeRecordOutcome::AlreadyRecorded)
                    } else {
                        Err(echo_agent::error::ReactError::Other(
                            "conflicting stable audit identity".to_string(),
                        ))
                    };
                }
                entries.push(entry);
                Ok(echo_agent::evolution::ChangeRecordOutcome::Appended)
            }
        }

        fn query(
            &self,
            _filter: &echo_agent::evolution::ChangeFilter,
        ) -> echo_agent::error::Result<Vec<echo_agent::evolution::ChangeEntry>> {
            Ok(Vec::new())
        }

        fn latest_for(
            &self,
            _entity_type: EntityType,
            _entity_key: &str,
        ) -> echo_agent::error::Result<Option<echo_agent::evolution::ChangeEntry>> {
            Ok(None)
        }

        fn len(&self) -> usize {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    fn high_conf_project_fact() -> MemoryMeta {
        MemoryMeta::new(MemoryType::ProjectFact, MemorySource::ExplicitSave, "test")
            .with_confidence(0.97)
    }

    async fn seed_memory(store: Arc<dyn Store>, key: &str, content: &str) -> Result<(), String> {
        TypedMemoryStore::new(store)
            .put_typed(WARM_NAMESPACE, key, content, high_conf_project_fact())
            .await
            .map_err(|error| error.to_string())
    }

    fn test_promoter(store: Arc<dyn Store>, rules_path: PathBuf) -> RulePromoter {
        RulePromoter::with_criteria(
            store,
            PromotionCriteria {
                min_confidence: 0.95,
                allowed_types: vec![MemoryType::ProjectFact],
                min_age_days: 0,
            },
        )
        .with_rules_path(rules_path)
    }

    async fn only_proposal(promoter: &RulePromoter) -> Result<RuleProposal, String> {
        let proposals = promoter.scan_for_proposals().await;
        if proposals.len() != 1 {
            return Err(format!("expected one proposal, got {}", proposals.len()));
        }
        proposals
            .into_iter()
            .next()
            .ok_or_else(|| "proposal disappeared".to_string())
    }

    async fn assert_effects_applied_authorities(
        store: Arc<dyn Store>,
        rules_path: &Path,
        receipt: &RulePromotionReceipt,
    ) -> Result<(), String> {
        let memory = TypedMemoryStore::new(store)
            .get_typed(WARM_NAMESPACE, &receipt.memory_key)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source memory disappeared".to_string())?;
        assert_eq!(
            memory
                .content
                .matches(&promotion_marker(&receipt.promotion_id))
                .count(),
            1
        );
        let rules = std::fs::read_to_string(rules_path).map_err(|error| error.to_string())?;
        assert_eq!(
            rules
                .matches(&promotion_rule_begin(&receipt.promotion_id))
                .count(),
            1
        );
        assert_eq!(
            rules
                .matches(&promotion_rule_end(&receipt.promotion_id))
                .count(),
            1
        );
        assert_eq!(receipt.phase, RulePromotionPhase::EffectsApplied);
        Ok(())
    }

    #[tokio::test]
    async fn scan_hits_memories_written_to_warm_namespace() -> Result<(), String> {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "Build requires JAVA_HOME=JDK8").await?;
        let promoter = test_promoter(store, PathBuf::from("unused-rules.md"));
        let proposals = promoter.scan_for_proposals().await;
        assert_eq!(proposals.len(), 1, "应命中 WARM namespace 的 1 条记忆");
        let proposal = proposals
            .first()
            .ok_or_else(|| "proposal disappeared".to_string())?;
        assert_eq!(proposal.memory_key, "eligible");
        assert_eq!(
            proposal.namespace,
            vec!["agent".to_string(), "memories".to_string()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn scan_ignores_low_confidence_memory() -> Result<(), String> {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let typed = TypedMemoryStore::new(store.clone());
        let low_conf = MemoryMeta::new(MemoryType::ProjectFact, MemorySource::ExplicitSave, "test")
            .with_confidence(0.50);
        typed
            .put_typed(WARM_NAMESPACE, "low", "maybe a fact", low_conf)
            .await
            .map_err(|error| error.to_string())?;
        let promoter = test_promoter(store, PathBuf::from("unused-rules.md"));
        let proposals = promoter.scan_for_proposals().await;
        assert!(proposals.is_empty(), "低置信记忆不应被晋升");
        Ok(())
    }

    #[tokio::test]
    async fn scan_skips_already_promoted_memory() -> Result<(), String> {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let content = "Already promoted\n\n<!-- PROMOTED_TO_RULE: 2026-07-01 -->";
        seed_memory(store.clone(), "done", content).await?;
        let promoter = test_promoter(store, PathBuf::from("unused-rules.md"));
        let proposals = promoter.scan_for_proposals().await;
        assert!(proposals.is_empty());
        Ok(())
    }

    async fn recover_after_fault(fault: PromotionFault) -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "Build uses cargo").await?;
        let promoter = test_promoter(store.clone(), rules_path.clone());
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let failed = test_promoter(store.clone(), rules_path.clone()).with_fault(fault);
        assert!(failed.promote_rule(&proposal, &log).await.is_err());

        let recovered = test_promoter(store.clone(), rules_path.clone())
            .reconcile_pending(&log)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = recovered
            .first()
            .ok_or_else(|| "prepared promotion was not reconciled".to_string())?;
        assert_effects_applied_authorities(store, &rules_path, receipt).await?;
        assert_eq!(
            log.len(),
            1,
            "stable audit ID must be appended exactly once"
        );
        Ok(())
    }

    #[tokio::test]
    async fn restart_recovers_crash_after_prepared_receipt() -> Result<(), String> {
        recover_after_fault(PromotionFault::Prepared).await
    }

    #[tokio::test]
    async fn restart_recovers_crash_after_rule_write() -> Result<(), String> {
        recover_after_fault(PromotionFault::Rule).await
    }

    #[tokio::test]
    async fn restart_recovers_crash_after_memory_marker() -> Result<(), String> {
        recover_after_fault(PromotionFault::Memory).await
    }

    #[tokio::test]
    async fn restart_recovers_crash_after_audit_without_duplicate() -> Result<(), String> {
        recover_after_fault(PromotionFault::Audit).await
    }

    #[tokio::test]
    async fn duplicate_retry_returns_same_effects_receipt() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "Use UTF-8: 中文 😀").await?;
        let promoter = test_promoter(store.clone(), rules_path.clone());
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let first = promoter
            .promote_rule(&proposal, &log)
            .await
            .map_err(|error| error.to_string())?;
        let second = promoter
            .promote_rule(&proposal, &log)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(first, second);
        assert_effects_applied_authorities(store, &rules_path, &second).await?;
        assert_eq!(log.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn committed_is_written_only_after_projection_acknowledgement() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "Use the canonical transaction").await?;
        let promoter = test_promoter(store, rules_path);
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let effects = promoter
            .promote_rule(&proposal, &log)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(effects.phase, RulePromotionPhase::EffectsApplied);

        let committed = promoter
            .commit_projection(&effects)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(committed.phase, RulePromotionPhase::Committed);
        let retry = promoter
            .commit_projection(&effects)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(retry, committed);
        Ok(())
    }

    #[tokio::test]
    async fn changed_source_keeps_prepared_receipt_and_fails_closed() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "original").await?;
        let promoter = test_promoter(store.clone(), rules_path.clone());
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let failed =
            test_promoter(store.clone(), rules_path.clone()).with_fault(PromotionFault::Prepared);
        assert!(failed.promote_rule(&proposal, &log).await.is_err());
        seed_memory(store.clone(), "eligible", "changed concurrently").await?;

        let error = promoter
            .reconcile_pending(&log)
            .await
            .err()
            .ok_or_else(|| "changed source was accepted".to_string())?;
        assert!(matches!(error, RulePromotionError::Conflict { .. }));
        assert!(!rules_path.exists());
        assert_eq!(log.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_interior_receipt_blocks_all_recovery_before_mutation() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "original").await?;
        let promoter = test_promoter(store.clone(), rules_path.clone());
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let failed = test_promoter(store, rules_path.clone()).with_fault(PromotionFault::Prepared);
        assert!(failed.promote_rule(&proposal, &log).await.is_err());
        let corrupt = promoter.receipts_dir().join("bad.json");
        std::fs::write(&corrupt, b"{not-json").map_err(|error| error.to_string())?;

        let error = promoter
            .reconcile_pending(&log)
            .await
            .err()
            .ok_or_else(|| "corrupt receipt was ignored".to_string())?;
        assert!(matches!(error, RulePromotionError::CorruptReceipt { .. }));
        assert!(!rules_path.exists());
        assert_eq!(log.len(), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_rules_authority_is_rejected_without_mutating_target() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = dir.path().join("outside-rules.md");
        std::fs::write(&outside, "outside authority").map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("workspace/.eko/learned-rules.md");
        let rules_parent = rules_path
            .parent()
            .ok_or_else(|| "rules path has no parent".to_string())?;
        std::fs::create_dir_all(rules_parent).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside, &rules_path).map_err(|error| error.to_string())?;
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "never follow the symlink").await?;
        let promoter = test_promoter(store, rules_path);
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();

        let error = promoter
            .promote_rule(&proposal, &log)
            .await
            .err()
            .ok_or_else(|| "symlinked learned-rules authority was accepted".to_string())?;

        assert!(matches!(error, RulePromotionError::Io { .. }));
        assert_eq!(
            std::fs::read_to_string(outside).map_err(|error| error.to_string())?,
            "outside authority"
        );
        assert_eq!(log.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn audit_failure_is_retried_forward_without_rollback() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store.clone(), "eligible", "original").await?;
        let promoter = test_promoter(store.clone(), rules_path.clone());
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        log.fail.store(true, Ordering::SeqCst);
        assert!(promoter.promote_rule(&proposal, &log).await.is_err());
        assert!(rules_path.exists(), "rule effect must remain recoverable");
        log.fail.store(false, Ordering::SeqCst);

        let reconciled = promoter
            .reconcile_pending(&log)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = reconciled
            .first()
            .ok_or_else(|| "audit failure did not leave a prepared receipt".to_string())?;
        assert_effects_applied_authorities(store, &rules_path, receipt).await?;
        assert_eq!(log.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_receipts_reconcile_only_their_bound_store() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store_a: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let store_b: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        seed_memory(store_a.clone(), "a", "workspace a").await?;
        seed_memory(store_b.clone(), "b", "workspace b").await?;
        let rules_a = dir.path().join("a/.eko/learned-rules.md");
        let rules_b = dir.path().join("b/.eko/learned-rules.md");
        let promoter_a = test_promoter(store_a.clone(), rules_a.clone());
        let promoter_b = test_promoter(store_b.clone(), rules_b.clone());
        let proposal_a = only_proposal(&promoter_a).await?;
        let log = RecordingChangeLog::default();
        assert!(
            test_promoter(store_a.clone(), rules_a.clone())
                .with_fault(PromotionFault::Prepared)
                .promote_rule(&proposal_a, &log)
                .await
                .is_err()
        );
        assert!(
            promoter_b
                .reconcile_pending(&log)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(!rules_b.exists());
        let reconciled = promoter_a
            .reconcile_pending(&log)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = reconciled
            .first()
            .ok_or_else(|| "workspace-a receipt was not recovered".to_string())?;
        assert_effects_applied_authorities(store_a, &rules_a, receipt).await?;
        Ok(())
    }

    #[tokio::test]
    async fn receipt_contains_digests_but_not_rule_or_memory_content() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let rules_path = dir.path().join("learned-rules.md");
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let secret = "unique-memory-content-not-copied";
        seed_memory(store.clone(), "eligible", secret).await?;
        let promoter = test_promoter(store, rules_path);
        let proposal = only_proposal(&promoter).await?;
        let log = RecordingChangeLog::default();
        let receipt = promoter
            .promote_rule(&proposal, &log)
            .await
            .map_err(|error| error.to_string())?;
        let receipt_text = std::fs::read_to_string(promoter.receipt_path(&receipt.promotion_id))
            .map_err(|error| error.to_string())?;
        assert!(!receipt_text.contains(secret));
        assert!(!receipt_text.contains(&proposal.rule_text));
        assert!(receipt_text.contains(&receipt.source_sha256));
        assert!(receipt_text.contains(&receipt.rule_sha256));
        Ok(())
    }
}
