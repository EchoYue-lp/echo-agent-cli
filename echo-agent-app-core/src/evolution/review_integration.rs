//! Review scheduling — wires [`MemoryReviewer`] into the product lifecycle.
//!
//! Three trigger paths:
//! 1. **Automatic — session end**: After REPL/TUI exit, alongside existing
//!    `run_auto_memory_on_exit()`.
//! 2. **Automatic — write counter**: Every N writes (configurable, default 50),
//!    trigger a lightweight review.
//! 3. **Manual — `/memory-review` command**: User-initiated full review.
//!
//! The struct is self-contained: given a `Store`, an `echo_agent_dir`, and a
//! [`ReviewConfig`], it creates its own [`MemoryLayerManager`] and
//! [`ChangeLog`] on each review pass, so callers never need to wire up the
//! framework internals.

use echo_agent::evolution::{
    MemoryLayerManager, MemoryReviewer, MemoryRuntimeIntegrationBuilder, MemoryWriteObserver,
    ReviewChange, ReviewConfig, ReviewReport, SkillCandidateDetector, SkillDraftGenerator,
};
use echo_agent::memory::Store;
use echo_agent::memory::TypedMemoryStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bridges the framework's review system into the product lifecycle.
///
/// Self-contained: callers only need a `Store`, the `.echo-agent/` directory
/// path, and an optional [`ReviewConfig`]. All framework-level plumbing
/// (`MemoryLayerManager`, `ChangeLog`, `TypedMemoryStore`) is created
/// internally on each review pass.
pub struct ReviewIntegration {
    config: ReviewConfig,
    write_counter: Arc<AtomicU64>,
    /// Path to the `.echo-agent/` directory for the change log and MEMORY.md.
    echo_agent_dir: PathBuf,
    /// The underlying Store for creating TypedMemoryStore on demand.
    store: Arc<dyn Store>,
}

impl ReviewIntegration {
    /// Create a new review integration with the given config.
    pub fn new(config: ReviewConfig, echo_agent_dir: PathBuf, store: Arc<dyn Store>) -> Self {
        Self {
            config,
            write_counter: Arc::new(AtomicU64::new(0)),
            echo_agent_dir,
            store,
        }
    }

    /// Get the current review config.
    pub fn config(&self) -> &ReviewConfig {
        &self.config
    }

    /// Get the write counter (for diagnostics / testing).
    pub fn write_count(&self) -> u64 {
        self.write_counter.load(Ordering::Relaxed)
    }

    /// Called after each real memory write. The counter is incremented by
    /// `MemoryLayerManager::write_memory`; this method only observes the shared
    /// count and triggers a review if the threshold is reached.
    ///
    /// Returns `None` if the threshold has not been reached, or
    /// `Some(Ok(report))` if a review was triggered, or `Some(Err(...))` if
    /// review was triggered but failed.
    pub async fn on_memory_write(&self) -> Option<Result<ReviewReport, String>> {
        let count = self.write_counter.load(Ordering::Relaxed);
        if count == 0 {
            return None;
        }
        if !count.is_multiple_of(self.config.review_every_n_writes) {
            return None;
        }

        tracing::info!(
            count,
            threshold = self.config.review_every_n_writes,
            "Review triggered by write counter"
        );

        Some(self.run_review_inner().await)
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
    /// each call. Reviews are infrequent enough (session end, every 50 writes,
    /// or manual) that this overhead is negligible.
    async fn run_review_inner(&self) -> Result<ReviewReport, String> {
        let typed_store = TypedMemoryStore::new(self.store.clone());
        let runtime_builder = self.runtime_builder();
        let change_log = runtime_builder.create_change_log();
        let layer_manager = self.create_layer_manager();
        let reviewer = MemoryReviewer::new();
        let mut report = reviewer
            .review(
                &typed_store,
                &layer_manager,
                change_log.as_ref(),
                &self.config,
            )
            .await
            .map_err(|e| format!("Memory review failed: {e}"))?;

        // ── Skill candidate detection ──────────────────────────────
        if self.config.detect_skill_candidates {
            let detector = SkillCandidateDetector::new();
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
                        let generator = SkillDraftGenerator::new(
                            self.echo_agent_dir.clone(),
                            change_log.as_ref(),
                        );
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

    /// Create a `MemoryLayerManager` for the current review pass.
    ///
    /// Wires the shared write counter so that every memory write through this
    /// layer manager increments the counter, and `on_memory_write()` can trigger
    /// periodic reviews without an explicit caller.
    pub fn create_layer_manager(&self) -> MemoryLayerManager {
        self.runtime_builder().build_layer_manager()
    }

    /// Create framework runtime wiring without owning product lifecycle policy.
    fn runtime_builder(&self) -> MemoryRuntimeIntegrationBuilder {
        MemoryRuntimeIntegrationBuilder::new(self.echo_agent_dir.clone(), self.store.clone())
            .write_counter(self.write_counter.clone())
            .review_every_n_writes(self.config.review_every_n_writes)
    }
}

impl MemoryWriteObserver for ReviewIntegration {
    fn on_memory_write<'a>(&'a self) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            let _ = ReviewIntegration::on_memory_write(self).await;
        })
    }
}

/// Discover the `.echo-agent/` directory for the current project.
///
/// Walks up from the current working directory to find a directory containing
/// `.echo-agent/` or `.git/`. Falls back to `$HOME/.echo-agent/` if no
/// project root is found.
pub fn discover_echo_agent_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let home_dir = PathBuf::from(&home);

    // Try to find project root by walking up from cwd
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            if dir.join(".echo-agent").is_dir() || dir.join(".git").is_dir() {
                return dir.join(".echo-agent");
            }
            dir = match dir.parent() {
                Some(p) => p,
                None => break,
            };
        }
    }

    // Fallback to global directory
    home_dir.join(".echo-agent")
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
    lines.push(format!("  🔀 Merges applied: {}", report.merges_applied));
    lines.push(format!(
        "  📦 Entries archived: {}",
        report.archives_applied
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
        lines.push("Changes:".to_string());
        for change in &report.changes {
            match change {
                echo_agent::evolution::ReviewChange::Archive { key, staleness } => {
                    lines.push(format!(
                        "  📦 Archived '{}' (staleness: {:.2})",
                        key, staleness
                    ));
                }
                echo_agent::evolution::ReviewChange::Merge {
                    primary_key,
                    superseded_keys,
                } => {
                    lines.push(format!(
                        "  🔀 Merged {} entries into '{}'",
                        superseded_keys.len() + 1,
                        primary_key
                    ));
                }
                echo_agent::evolution::ReviewChange::StatusTransition {
                    key,
                    from,
                    to,
                    staleness,
                } => {
                    lines.push(format!(
                        "  🔄 '{}' : {:?} → {:?} (staleness: {:.2})",
                        key, from, to, staleness
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
        // Should return a non-empty path ending in .echo-agent
        assert!(dir.to_string_lossy().ends_with(".echo-agent"));
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
            merges_applied: 1,
            archives_applied: 1,
            candidates_proposed: 0,
            drafts_generated: 0,
            changes: vec![echo_agent::evolution::ReviewChange::Archive {
                key: "old_fact".to_string(),
                staleness: 0.75,
            }],
        };
        let text = format_review_report(&report);
        assert!(text.contains("10 entries scanned"));
        assert!(text.contains("Stale entries (flagged): 3"));
        assert!(text.contains("Archived 'old_fact'"));
    }

    #[tokio::test]
    async fn test_review_integration_write_counter() {
        use echo_agent::evolution::ReviewConfig;
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let config = ReviewConfig {
            review_every_n_writes: 3,
            ..Default::default()
        };

        let ri = ReviewIntegration::new(config, dir, store);
        let layer_manager = ri.create_layer_manager();
        let meta = MemoryMeta::new(MemoryType::ProjectFact, MemorySource::AutoExtracted, "test");

        // Direct observer calls do not increment. Real MemoryLayerManager writes do.
        assert!(ri.on_memory_write().await.is_none());
        assert_eq!(ri.write_count(), 0);

        layer_manager
            .write_memory("one", "first project fact", meta.clone())
            .await
            .expect("write one");
        assert!(ri.on_memory_write().await.is_none());
        layer_manager
            .write_memory("two", "second project fact", meta.clone())
            .await
            .expect("write two");
        assert!(ri.on_memory_write().await.is_none());
        layer_manager
            .write_memory("three", "third project fact", meta)
            .await
            .expect("write three");
        // Third real write triggers review.
        let review_result = ri.on_memory_write().await;
        assert!(review_result.is_some());
        assert_eq!(ri.write_count(), 3);
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
