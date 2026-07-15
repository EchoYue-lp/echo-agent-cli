//! Review scheduling — wires [`MemoryReviewer`] into the product lifecycle.
//!
//! Trigger paths (stage4 F1):
//! 1. **Optional — session end**: Disabled by default; callers may opt in.
//! 2. **Manual — `/memory-review` command**: User-initiated full review.
//! 3. **Scheduled — Dreaming**: A cron-driven `Dreaming::run` pass does
//!    recall-frequency-driven promotion + staleness demote (replaces the old
//!    "every-N-writes" write-counter trigger, which coupled review cadence to
//!    write volume instead of recall value).
//!
//! The struct is self-contained: given a `Store`, an `echo_agent_dir`, and a
//! [`ReviewConfig`], it creates analysis and audit dependencies on demand and
//! persists semantic proposals into the workspace Review Inbox.

use echo_agent::evolution::{
    Curator, CuratorConfig, MemoryLayerManager, MemoryReviewer, MemoryRuntimeIntegrationBuilder,
    ReviewChange, ReviewConfig, ReviewReport, SkillCandidateDetector, SkillDraftGenerator,
};
use echo_agent::memory::Store;
use echo_agent::memory::TypedMemoryStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use super::evidence::{
    EvidenceCandidate, EvidenceCandidateDraft, EvidenceKind, EvidenceRef, EvidenceSource,
    EvidenceStore, capture_memory_conflict, capture_review_outcome,
};

/// Bridges the framework's review system into the product lifecycle.
///
/// Self-contained: callers only need a `Store`, the `.echo-agent/` directory
/// path, and an optional [`ReviewConfig`]. All framework-level plumbing
/// (`MemoryLayerManager`, `ChangeLog`, `TypedMemoryStore`) is created
/// internally on each review pass.
pub struct ReviewIntegration {
    config: ReviewConfig,
    /// Path to the `.echo-agent/` (or `.eko/`) directory for the change log
    /// and MEMORY.md. Wrapped in `RwLock` so a workspace switch can rebind
    /// this and the `store` atomically without recreating the whole
    /// `ReviewIntegration` (which is shared via `Arc` across many callers).
    echo_agent_dir: RwLock<PathBuf>,
    /// The underlying Store for creating TypedMemoryStore on demand.
    store: RwLock<Arc<dyn Store>>,
}

impl ReviewIntegration {
    /// Create a new review integration with the given config.
    pub fn new(config: ReviewConfig, echo_agent_dir: PathBuf, store: Arc<dyn Store>) -> Self {
        Self {
            config,
            echo_agent_dir: RwLock::new(echo_agent_dir),
            store: RwLock::new(store),
        }
    }

    /// Rebind to a new project directory + Store (used on workspace switch).
    ///
    /// After this call, subsequent review passes, `create_layer_manager`,
    /// and dreaming all use the new `echo_agent_dir` and `store`. The shared
    /// `Arc<ReviewIntegration>` held across the app picks this up automatically.
    pub fn rebind(&self, echo_agent_dir: PathBuf, store: Arc<dyn Store>) {
        if let Ok(mut dir) = self.echo_agent_dir.write() {
            *dir = echo_agent_dir;
        }
        if let Ok(mut s) = self.store.write() {
            *s = store;
        }
        tracing::info!("ReviewIntegration rebound to new workspace memory store");
    }

    /// Get the current review config.
    pub fn config(&self) -> &ReviewConfig {
        &self.config
    }

    /// Workspace-scoped evidence store bound to the current memory root.
    pub fn evidence_store(&self) -> EvidenceStore {
        EvidenceStore::new(self.current_echo_agent_dir())
    }

    /// Current workspace-local `.eko` root used by evolution artifacts.
    pub fn echo_agent_dir(&self) -> PathBuf {
        self.current_echo_agent_dir()
    }

    /// Workspace-scoped curator bound to the current memory root.
    pub fn curator(&self) -> Curator {
        workspace_curator(&self.current_echo_agent_dir())
    }

    /// Persist a structured background-review proposal in the unified inbox.
    pub fn capture_review_outcome(
        &self,
        outcome: &echo_agent::evolution::ReviewOutcome,
    ) -> Result<Option<EvidenceCandidate>, String> {
        capture_review_outcome(&self.evidence_store(), outcome)
    }

    /// Called at session end. Runs a full review if configured.
    pub async fn on_session_end(&self) -> Option<Result<ReviewReport, String>> {
        if !self.config.review_on_session_end {
            return None;
        }

        tracing::info!("Review triggered by session end");
        Some(self.run_review_inner().await)
    }

    /// Manual trigger for `/memory-review` command.
    pub async fn run_review(&self) -> Result<ReviewReport, String> {
        tracing::info!("Review triggered by /memory-review command");
        self.run_review_inner().await
    }

    /// Internal: run the review and return the report.
    ///
    /// Creates framework plumbing through `MemoryRuntimeIntegrationBuilder` on
    /// each call. Reviews are manual by default, so this overhead is negligible.
    async fn run_review_inner(&self) -> Result<ReviewReport, String> {
        // Snapshot the current (echo_agent_dir, store) once per review pass.
        // `rebind` may fire between passes (workspace switch), but within a
        // single review we want a consistent pair. Briefly holding the read
        // lock to clone is fine — reviews are infrequent.
        let (echo_agent_dir, store) = {
            let dir = self
                .echo_agent_dir
                .read()
                .map_err(|e| format!("echo_agent_dir lock poisoned: {e}"))?
                .clone();
            let st = self
                .store
                .read()
                .map_err(|e| format!("store lock poisoned: {e}"))?
                .clone();
            (dir, st)
        };
        let typed_store = TypedMemoryStore::new(store.clone());
        let runtime_builder = MemoryRuntimeIntegrationBuilder::new(echo_agent_dir.clone(), store);
        let change_log = runtime_builder.create_change_log();
        let reviewer = MemoryReviewer::new();
        let mut report = reviewer
            .review(&typed_store, &self.config)
            .await
            .map_err(|e| format!("Memory review failed: {e}"))?;

        let evidence_store = EvidenceStore::new(echo_agent_dir.clone());
        for proposal in &report.conflict_proposals {
            capture_memory_conflict(&evidence_store, proposal)?;
        }

        // ── Skill candidate detection ──────────────────────────────
        if self.config.detect_skill_candidates {
            let detector =
                SkillCandidateDetector::new().with_curator(workspace_curator(&echo_agent_dir));
            match detector.detect(&typed_store, change_log.as_ref()).await {
                Ok(candidate_report) => {
                    for candidate in &candidate_report.new_candidates {
                        report.candidates_proposed += 1;
                        report.changes.push(ReviewChange::CandidateProposed {
                            name: candidate.name.clone(),
                            sample_count: candidate.sample_count,
                        });
                    }

                    // Optionally auto-generate drafts for new candidates.
                    if self.config.auto_generate_drafts
                        && !candidate_report.new_candidates.is_empty()
                    {
                        let generator =
                            SkillDraftGenerator::new(echo_agent_dir.clone(), change_log.as_ref())
                                .with_curator(workspace_curator(&echo_agent_dir));
                        for candidate in &candidate_report.new_candidates {
                            match generator.generate_from_candidate(candidate).await {
                                Ok(draft_result) => {
                                    report.drafts_generated += 1;
                                    report.changes.push(ReviewChange::DraftGenerated {
                                        name: draft_result.name,
                                        path: draft_result
                                            .skill_md_path
                                            .to_string_lossy()
                                            .to_string(),
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        name = %candidate.name,
                                        error = %e,
                                        "review: failed to generate draft for candidate"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "review: skill candidate detection failed");
                }
            }
        }

        Ok(report)
    }

    /// Create a `MemoryLayerManager` for the current workspace's memory store.
    ///
    /// Reads the current `(echo_agent_dir, store)` from the inner locks, so
    /// after a workspace `rebind` this manager uses the new workspace.
    pub fn create_layer_manager(&self) -> MemoryLayerManager {
        self.runtime_builder().build_layer_manager()
    }

    /// Create framework runtime wiring without owning product lifecycle policy.
    fn runtime_builder(&self) -> MemoryRuntimeIntegrationBuilder {
        // Read current values; on lock poisoning fall back to whatever we can
        // get (empty path / clone-of-poisoned-err). Lock poisoning only happens
        // on panic, so this is a best-effort degradation path.
        let echo_agent_dir = self
            .echo_agent_dir
            .read()
            .map(|g| g.clone())
            .unwrap_or_default();
        let store = self
            .store
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        MemoryRuntimeIntegrationBuilder::new(echo_agent_dir, store)
    }

    fn current_echo_agent_dir(&self) -> PathBuf {
        self.echo_agent_dir
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|error| error.into_inner().clone())
    }
}

impl echo_agent::evolution::MemoryTriggerSink for ReviewIntegration {
    fn on_trigger<'a>(
        &'a self,
        trigger: &'a echo_agent::evolution::TriggerMatch,
    ) -> futures::future::BoxFuture<
        'a,
        std::result::Result<echo_agent::evolution::MemoryTriggerDisposition, String>,
    > {
        Box::pin(async move {
            let kind = match trigger.memory_type {
                echo_agent::memory::MemoryType::UserPreference => EvidenceKind::UserPreference,
                echo_agent::memory::MemoryType::DebuggingLesson => EvidenceKind::DebuggingLesson,
                echo_agent::memory::MemoryType::ErrorResolution => EvidenceKind::ErrorResolution,
                echo_agent::memory::MemoryType::WorkflowPattern => EvidenceKind::WorkflowPattern,
                _ => EvidenceKind::ProjectFact,
            };
            let evidence = trigger
                .evidence
                .iter()
                .map(|item| EvidenceRef {
                    source: EvidenceSource::TriggerDetector,
                    source_run_id: None,
                    source_role: Some(item.source_role.clone()),
                    source_turn: None,
                    source_memory_key: None,
                    quote: item.quote.clone(),
                })
                .collect();
            if let Err(error) = self.evidence_store().upsert(EvidenceCandidateDraft {
                kind,
                scope: matches!(kind, EvidenceKind::UserPreference)
                    .then(|| super::evidence::EvidenceScope::User("local-user".to_string())),
                content: trigger.content.clone(),
                evidence,
                action: None,
                confidence: trigger.confidence,
            }) {
                // EKO treats inferred memory as review-only. Do not let an inbox
                // storage failure fall through to the framework's direct durable
                // write path and silently bypass that review gate.
                tracing::warn!(%error, "failed to queue trigger evidence; candidate dropped");
            }
            Ok(echo_agent::evolution::MemoryTriggerDisposition::Captured)
        })
    }
}

impl echo_agent::skills::external::SkillLoadPolicy for ReviewIntegration {
    fn allows(&self, descriptor: &echo_agent::skills::external::SkillDescriptor) -> bool {
        if descriptor
            .location
            .ancestors()
            .any(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some("_drafts"))
        {
            return false;
        }
        let current_root = self.current_echo_agent_dir().join("skills");
        if let Some(skill_root) = workspace_skill_root(&descriptor.location)
            && normalize_path(&skill_root) != normalize_path(&current_root)
        {
            return false;
        }
        let Some(meta) = self.curator().skill_for_path(&descriptor.location) else {
            return true;
        };
        matches!(
            meta.lifecycle,
            echo_agent::evolution::SkillLifecycle::Active
                | echo_agent::evolution::SkillLifecycle::Stale
        )
    }
}

fn workspace_skill_root(path: &std::path::Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let is_skills = ancestor.file_name().and_then(|name| name.to_str()) == Some("skills");
        let parent_is_eko = ancestor
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some(".eko");
        (is_skills && parent_is_eko).then(|| ancestor.to_path_buf())
    })
}

fn normalize_path(path: &std::path::Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Construct the authoritative curator for one `.eko`/framework state directory.
pub fn workspace_curator(echo_agent_dir: &std::path::Path) -> Curator {
    Curator::new(
        CuratorConfig::default(),
        echo_agent_dir.join("evolution").join("curator-state.json"),
    )
}

/// Discover the `.eko/` directory for the current project.
///
/// Walks up from the current working directory to find a directory containing
/// `.eko/` or `.git/`. Falls back to `$HOME/.eko/` if no project root is found.
pub fn discover_echo_agent_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let home_dir = PathBuf::from(&home);

    // Try to find project root by walking up from cwd
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            if dir.join(".eko").is_dir() || dir.join(".git").is_dir() {
                return dir.join(".eko");
            }
            dir = match dir.parent() {
                Some(p) => p,
                None => break,
            };
        }
    }

    // Fallback to global directory
    home_dir.join(".eko")
}

/// Format a [`ReviewReport`] for display to the user.
pub fn format_review_report(report: &ReviewReport) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "📋 Memory Review Report — {} entries scanned",
        report.total_scanned
    ));

    if report.total_scanned == 0 {
        lines.push("  No memories to review.".to_string());
        return lines.join("\n");
    }

    lines.push(format!(
        "  🕐 Stale entries (flagged): {}",
        report.stale_count
    ));
    lines.push(format!(
        "  ⚠️  Conflict groups found: {}",
        report.conflict_groups
    ));
    lines.push(format!(
        "  🔀 Conflict proposals queued: {}",
        report.conflict_proposals.len()
    ));
    if report.candidates_proposed > 0 {
        lines.push(format!(
            "  🎯 Skill candidates proposed: {}",
            report.candidates_proposed
        ));
    }
    if report.drafts_generated > 0 {
        lines.push(format!(
            "  📝 Draft SKILL.md generated: {}",
            report.drafts_generated
        ));
    }

    if !report.changes.is_empty() {
        lines.push(String::new());
        lines.push("Proposals:".to_string());
        for change in &report.changes {
            match change {
                echo_agent::evolution::ReviewChange::StalenessSuggested {
                    key,
                    recommended_status,
                    staleness,
                } => {
                    lines.push(format!(
                        "  🕐 '{}' → suggested {:?} (staleness: {:.2})",
                        key, recommended_status, staleness
                    ));
                }
                echo_agent::evolution::ReviewChange::ConflictProposed {
                    topic,
                    recommended_primary_key,
                    member_keys,
                } => {
                    lines.push(format!(
                        "  🔀 Conflict '{}' among [{}]; recommended primary '{}'",
                        topic,
                        member_keys.join(", "),
                        recommended_primary_key
                    ));
                }
                echo_agent::evolution::ReviewChange::CandidateProposed { name, sample_count } => {
                    lines.push(format!(
                        "  🎯 Candidate '{}' proposed ({} observations)",
                        name, sample_count
                    ));
                }
                echo_agent::evolution::ReviewChange::DraftGenerated { name, path } => {
                    lines.push(format!("  📝 Draft '{}' → {}", name, path));
                }
            }
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_echo_agent_dir_returns_path() {
        let dir = discover_echo_agent_dir();
        // Should return a non-empty path ending in .eko
        assert!(dir.to_string_lossy().ends_with(".eko"));
    }

    #[test]
    fn test_format_review_report_empty() {
        let report = ReviewReport::default();
        let text = format_review_report(&report);
        assert!(text.contains("No memories to review"));
    }

    #[test]
    fn test_format_review_report_with_data() {
        let report = ReviewReport {
            total_scanned: 10,
            stale_count: 3,
            conflict_groups: 2,
            staleness_suggestions: Vec::new(),
            conflict_proposals: Vec::new(),
            candidates_proposed: 0,
            drafts_generated: 0,
            changes: vec![echo_agent::evolution::ReviewChange::StalenessSuggested {
                key: "old_fact".to_string(),
                recommended_status: echo_agent::memory::MemoryStatus::Archived,
                staleness: 0.75,
            }],
        };
        let text = format_review_report(&report);
        assert!(text.contains("10 entries scanned"));
        assert!(text.contains("Stale entries (flagged): 3"));
        assert!(text.contains("'old_fact' → suggested Archived"));
    }

    #[test]
    fn skill_policy_blocks_drafts_and_foreign_workspaces() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace-a/.eko");
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = ReviewIntegration::new(ReviewConfig::default(), echo_dir.clone(), store);
        let mut descriptor = echo_agent::skills::external::parse_skill_md(
            "---\nname: test-skill\ndescription: test skill\n---\nbody",
        )
        .map_err(|error| error.to_string())?;

        descriptor.location = echo_dir.join("skills/_drafts/test-skill/SKILL.md");
        assert!(!echo_agent::skills::external::SkillLoadPolicy::allows(
            &integration,
            &descriptor,
        ));

        descriptor.location = echo_dir.join("skills/test-skill/SKILL.md");
        assert!(echo_agent::skills::external::SkillLoadPolicy::allows(
            &integration,
            &descriptor,
        ));

        descriptor.location = temp
            .path()
            .join("workspace-b/.eko/skills/test-skill/SKILL.md");
        assert!(!echo_agent::skills::external::SkillLoadPolicy::allows(
            &integration,
            &descriptor,
        ));
        Ok(())
    }

    #[tokio::test]
    async fn memory_conflicts_are_queued_without_mutating_the_store() -> Result<(), String> {
        use echo_agent::evolution::ReviewConfig;
        use echo_agent::memory::{
            InMemoryStore, MemoryMeta, MemorySource, MemoryStatus, MemoryType,
        };

        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let ri = ReviewIntegration::new(ReviewConfig::default(), temp.path().join(".eko"), store);
        let layer_manager = ri.create_layer_manager();
        let high = MemoryMeta::new(MemoryType::ProjectFact, MemorySource::ExplicitSave, "build")
            .with_confidence(0.9);
        let low = MemoryMeta::new(
            MemoryType::ProjectFact,
            MemorySource::AutoExtracted,
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

        let report = ri.run_review().await?;
        assert_eq!(report.conflict_proposals.len(), 1);
        let queued = ri.evidence_store().list()?;
        assert_eq!(queued.len(), 1);
        let candidate = queued
            .first()
            .ok_or_else(|| "review did not queue a conflict candidate".to_string())?;
        assert!(matches!(
            candidate.action,
            crate::evolution::EvidenceAction::MergeMemories { .. }
        ));
        let secondary = layer_manager
            .locate("make")
            .await
            .ok_or_else(|| "secondary memory disappeared during review".to_string())?;
        assert_eq!(secondary.1.meta.status, MemoryStatus::Active);
        Ok(())
    }

    #[tokio::test]
    async fn test_review_integration_session_end_disabled() {
        use echo_agent::evolution::ReviewConfig;
        use echo_agent::memory::InMemoryStore;

        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let config = ReviewConfig {
            review_on_session_end: false,
            ..Default::default()
        };

        let ri = ReviewIntegration::new(config, dir, store);
        let result = ri.on_session_end().await;
        assert!(result.is_none(), "should not review when disabled");
    }
}
