//! Rule promotion — promotes high-confidence memories to agent rules.
//!
//! RulePromoter scans typed memories for high-confidence entries that meet
//! promotion criteria and writes them to learned-rules.md as permanent rules.

use crate::instruction_provider::InstructionProvider;
use chrono::{DateTime, Utc};
use echo_agent::evolution::{
    ChangeEntryBuilder, ChangeLog, ChangeType, EntityType, EvolutionSecurityGuard, InputTrustLevel,
    layer::WARM_NAMESPACE,
};
use echo_agent::memory::{
    MemoryFilter, MemoryStatus, MemoryType, Store, TypedMemoryEntry, TypedMemoryStore,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
        }
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
        let prefix = match entry.meta.memory_type {
            MemoryType::ProjectFact => "Project fact",
            MemoryType::WorkflowPattern => "Workflow pattern",
            MemoryType::UserPreference => "User preference",
            MemoryType::ToolUsage => "Tool usage",
            _ => "Rule",
        };

        format!("- **{}**: {}", prefix, entry.content)
    }

    /// Promote a rule by appending it to learned-rules.md.
    ///
    /// Returns Ok(()) if successful, Err if security check fails or file write fails.
    pub async fn promote_rule(
        &self,
        proposal: &RuleProposal,
        change_log: &dyn ChangeLog,
    ) -> Result<(), String> {
        // Security check
        let verdict = self
            .security_guard
            .check_rule_promotion(&proposal.rule_text, InputTrustLevel::Trusted);

        if !verdict.allowed {
            return Err(format!(
                "Security check failed: {}",
                verdict.reason.unwrap_or_else(|| "Unknown".to_string())
            ));
        }

        // Load existing learned-rules content (formerly AGENTS.md; renamed by
        // InstructionProvider::load_for on first load after upgrade).
        let existing_content =
            std::fs::read_to_string(InstructionProvider::agents_instructions_path())
                .unwrap_or_else(|_| String::new());

        // Append new rule
        let new_content = if existing_content.is_empty() {
            format!(
                "# Agent Rules\n\nAuto-promoted rules from high-confidence memories.\n\n## Rules\n\n{}",
                proposal.rule_text
            )
        } else {
            // Check if "## Rules" section exists
            if existing_content.contains("## Rules") {
                format!("{}\n{}", existing_content.trim_end(), proposal.rule_text)
            } else {
                format!(
                    "{}\n\n## Rules\n\n{}",
                    existing_content.trim_end(),
                    proposal.rule_text
                )
            }
        };

        // Write to learned-rules.md (auto-promoted rules; user-editable).
        InstructionProvider::save_agents_instructions(&new_content)
            .map_err(|e| format!("Failed to write learned-rules.md: {}", e))?;

        // Mark memory as promoted by updating its content
        let typed_store = TypedMemoryStore::new(self.store.clone());
        let namespace_refs: Vec<&str> = proposal.namespace.iter().map(|s| s.as_str()).collect();

        let entry = typed_store
            .get_typed(&namespace_refs, &proposal.memory_key)
            .await
            .map_err(|e| format!("Failed to get memory: {}", e))?
            .ok_or_else(|| format!("Memory {} not found", proposal.memory_key))?;

        let updated_content = format!(
            "{}\n\n<!-- PROMOTED_TO_RULE: {} -->",
            entry.content.trim_end(),
            echo_agent::utils::time::now_local().to_rfc3339()
        );

        // Update the memory with the marker
        let updated_meta = entry.meta.clone();
        typed_store
            .put_typed(
                &namespace_refs,
                &proposal.memory_key,
                &updated_content,
                updated_meta,
            )
            .await
            .map_err(|e| format!("Failed to mark memory as promoted: {}", e))?;

        // Log the change
        let change_entry =
            ChangeEntryBuilder::new(EntityType::Memory, &proposal.memory_key, ChangeType::Update)
                .reason(format!(
                    "Promoted memory {} to rule in learned-rules.md",
                    proposal.memory_key
                ))
                .before(serde_json::json!({"status": "active"}))
                .after(serde_json::json!({"status": "promoted_to_rule"}))
                .build(change_log);

        change_log
            .record(change_entry)
            .map_err(|e| format!("Failed to record promotion change: {}", e))?;

        Ok(())
    }

    /// Get the current promotion criteria.
    pub fn criteria(&self) -> &PromotionCriteria {
        &self.criteria
    }
}

#[cfg(test)]
mod tests {
    //! 回归测试:RulePromoter 必须扫描 WARM_NAMESPACE(`["agent","memories"]`),
    //! 与 MemoryLayerManager::write_memory / MemoryRecaller / Dreaming 同源。
    /// 此前 scan_for_proposals 硬编码 ["agent","typed_memories"](无生产写入的死
    /// namespace),导致晋升永远命中空集——"记忆 → learned-rules.md 规则"链路完全断开。
    use super::*;
    use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource};

    fn high_conf_project_fact() -> MemoryMeta {
        MemoryMeta::new(MemoryType::ProjectFact, MemorySource::ExplicitSave, "test")
            .with_confidence(0.97)
    }

    #[tokio::test]
    async fn scan_hits_memories_written_to_warm_namespace() {
        // 核心回归:写入 WARM_NAMESPACE 的高置信 ProjectFact 必须被 scan 命中。
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let typed = TypedMemoryStore::new(store.clone());

        typed
            .put_typed(
                WARM_NAMESPACE,
                "eligible",
                "Build requires JAVA_HOME=JDK8",
                high_conf_project_fact(),
            )
            .await
            .expect("seed warm memory");

        // min_age_days=0 跳过 age 检查,专注测 namespace 修复。
        let promoter = RulePromoter::with_criteria(
            store,
            PromotionCriteria {
                min_confidence: 0.95,
                allowed_types: vec![MemoryType::ProjectFact],
                min_age_days: 0,
            },
        );

        let proposals = promoter.scan_for_proposals().await;
        assert_eq!(proposals.len(), 1, "应命中 WARM namespace 的 1 条记忆");
        assert_eq!(proposals[0].memory_key, "eligible");
        assert_eq!(
            proposals[0].namespace,
            vec!["agent".to_string(), "memories".to_string()]
        );
    }

    #[tokio::test]
    async fn scan_ignores_low_confidence_memory() {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let typed = TypedMemoryStore::new(store.clone());

        let low_conf = MemoryMeta::new(MemoryType::ProjectFact, MemorySource::ExplicitSave, "test")
            .with_confidence(0.50); // 低于阈值 0.95

        typed
            .put_typed(WARM_NAMESPACE, "low", "maybe a fact", low_conf)
            .await
            .expect("seed low-conf memory");

        let promoter = RulePromoter::with_criteria(
            store,
            PromotionCriteria {
                min_confidence: 0.95,
                allowed_types: vec![MemoryType::ProjectFact],
                min_age_days: 0,
            },
        );

        let proposals = promoter.scan_for_proposals().await;
        assert!(proposals.is_empty(), "低置信记忆不应被晋升");
    }

    #[tokio::test]
    async fn scan_skips_already_promoted_memory() {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let typed = TypedMemoryStore::new(store.clone());

        // 内容已带 PROMOTED_TO_RULE 标记,应被跳过(防重复晋升)。
        let content = "Already promoted\n\n<!-- PROMOTED_TO_RULE: 2026-07-01 -->";
        typed
            .put_typed(WARM_NAMESPACE, "done", content, high_conf_project_fact())
            .await
            .expect("seed promoted memory");

        let promoter = RulePromoter::with_criteria(
            store,
            PromotionCriteria {
                min_confidence: 0.95,
                allowed_types: vec![MemoryType::ProjectFact],
                min_age_days: 0,
            },
        );

        let proposals = promoter.scan_for_proposals().await;
        assert!(
            proposals.is_empty(),
            "已带 PROMOTED_TO_RULE 标记的记忆不应重复晋升"
        );
    }
}
