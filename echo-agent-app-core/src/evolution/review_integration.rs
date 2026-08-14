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
    Curator, CuratorConfig, EvolutionObserver, MemoryLayerManager, MemoryReviewer,
    MemoryRuntimeIntegrationBuilder, ReviewChange, ReviewConfig, ReviewReport,
    SkillCandidateDetector, SkillDraftGenerator,
};
use echo_agent::memory::Store;
use echo_agent::memory::TypedMemoryStore;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use super::evidence::{
    EvidenceCandidate, EvidenceCandidateDraft, EvidenceKind, EvidenceRef, EvidenceSource,
    EvidenceStore, capture_memory_conflict, capture_review_outcome,
};

#[derive(Clone)]
struct ReviewBinding {
    echo_agent_dir: PathBuf,
    store: Arc<dyn Store>,
    generation: u64,
}

struct ReviewBindingState {
    current: ReviewBinding,
    active_passes: usize,
    rebind_in_progress: bool,
    pending_triggers: VecDeque<QueuedTrigger>,
    trigger_delivery_failures: u64,
    rejected_triggers: u64,
    last_trigger_delivery_error: Option<String>,
}

struct ReviewBindingControl {
    state: Mutex<ReviewBindingState>,
    background_reviews: Mutex<BackgroundReviewRegistry>,
}

struct BackgroundReviewRegistry {
    accepting: bool,
    tasks: Vec<OwnedBackgroundReview>,
}

struct OwnedBackgroundReview {
    abort_handle: tokio::task::AbortHandle,
    release: echo_agent::agent::CancellationToken,
    supervisor: tokio::task::JoinHandle<Result<(), String>>,
}

#[derive(Clone)]
struct QueuedTrigger {
    echo_agent_dir: PathBuf,
    draft: EvidenceCandidateDraft,
}

const MAX_PENDING_TRIGGERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TriggerDeliveryStatus {
    pub pending: usize,
    pub failures: u64,
    pub rejected: u64,
    pub last_error: Option<String>,
}

/// Result of publishing a prepared memory generation. Binding publication is
/// infallible after the workspace transition crosses its commit boundary;
/// trigger settlement can still make the owning transition degraded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MemoryRebindReceipt {
    pub generation: u64,
    pub pending_old: usize,
    pub pending_roots: Vec<PathBuf>,
    pub delivery_error: Option<String>,
}

impl MemoryRebindReceipt {
    pub fn is_degraded(&self) -> bool {
        self.pending_old != 0 || !self.pending_roots.is_empty() || self.delivery_error.is_some()
    }
}

/// A workspace memory generation is busy running a review/evolution pass or
/// is already being rebound by the canonical workspace transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewGenerationError {
    Busy {
        active_passes: usize,
        rebind_in_progress: bool,
    },
    CounterExhausted(&'static str),
    TriggerSettlement {
        pending: usize,
        last_error: String,
    },
}

impl std::fmt::Display for ReviewGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy {
                active_passes,
                rebind_in_progress,
            } => write!(
                formatter,
                "memory evolution generation is busy (active passes: {active_passes}, rebind in progress: {rebind_in_progress})"
            ),
            Self::CounterExhausted(counter) => {
                write!(formatter, "memory evolution {counter} counter exhausted")
            }
            Self::TriggerSettlement {
                pending,
                last_error,
            } => write!(
                formatter,
                "memory trigger settlement failed with {pending} candidate(s) still pending: {last_error}"
            ),
        }
    }
}

impl std::error::Error for ReviewGenerationError {}

/// Pins one review or evolution pass to an immutable workspace binding.
/// Dropping the lease, including through future cancellation, releases the
/// workspace transition admission automatically.
#[must_use]
#[derive(Clone)]
pub struct ReviewGenerationLease {
    receipt: Arc<ReviewGenerationReceipt>,
}

struct ReviewGenerationReceipt {
    control: Arc<ReviewBindingControl>,
    binding: ReviewBinding,
    evolution_observer: Option<Arc<dyn EvolutionObserver>>,
}

impl ReviewGenerationLease {
    pub fn create_layer_manager(&self) -> MemoryLayerManager {
        let mut builder = MemoryRuntimeIntegrationBuilder::new(
            self.receipt.binding.echo_agent_dir.clone(),
            self.receipt.binding.store.clone(),
        );
        if let Some(observer) = self.receipt.evolution_observer.clone() {
            builder = builder.evolution_observer(observer);
        }
        builder.build_layer_manager()
    }

    /// Evidence inbox pinned to the same workspace as this pass.
    pub fn evidence_store(&self) -> EvidenceStore {
        EvidenceStore::new(self.receipt.binding.echo_agent_dir.clone())
    }

    /// Framework memory store pinned to the same workspace generation.
    pub fn memory_store(&self) -> Arc<dyn Store> {
        self.receipt.binding.store.clone()
    }

    /// Transfer a spawned framework review into the integration's owned
    /// settlement registry. The returned value only observes the outcome;
    /// caller cancellation aborts the inner task while the registry retains and
    /// awaits the supervisor that owns this generation lease.
    pub async fn track_background_review(
        self,
        handle: tokio::task::JoinHandle<echo_agent::evolution::ReviewOutcome>,
    ) -> Result<BackgroundReviewPass, String> {
        let abort_handle = handle.abort_handle();
        let evidence_store = self.evidence_store();
        let control = self.receipt.control.clone();
        let admission = {
            let mut registry = control
                .background_reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !registry.accepting {
                Err(handle)
            } else {
                let (completed, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut registry.tasks)
                    .into_iter()
                    .partition(|task| task.supervisor.is_finished());
                registry.tasks = pending;
                let (outcome_sender, outcome_receiver) = tokio::sync::oneshot::channel();
                let release = echo_agent::agent::CancellationToken::new();
                let supervisor_release = release.clone();
                let supervisor = tokio::spawn(async move {
                    let (settlement, owner_error) = match handle.await {
                        Ok(outcome) => match capture_review_outcome(&evidence_store, &outcome) {
                            Ok(evidence_candidate) => (
                                Ok(BackgroundReviewSettlement {
                                    outcome,
                                    evidence_candidate,
                                }),
                                None,
                            ),
                            Err(error) => (Err(error.clone()), Some(error)),
                        },
                        Err(error) => {
                            let was_cancelled = error.is_cancelled();
                            let message = format!("Background review task failed to join: {error}");
                            let owner_error = (!was_cancelled).then(|| message.clone());
                            (Err(message), owner_error)
                        }
                    };
                    if let Some(error) = owner_error.as_ref() {
                        tracing::error!(%error, "integration-owned background review settlement failed");
                    }
                    let _delivered = outcome_sender.send(settlement);
                    supervisor_release.cancelled().await;
                    drop(self);
                    match owner_error {
                        Some(error) => Err(error),
                        None => Ok(()),
                    }
                });
                registry.tasks.push(OwnedBackgroundReview {
                    abort_handle: abort_handle.clone(),
                    release: release.clone(),
                    supervisor,
                });
                Ok((completed, outcome_receiver, release))
            }
        };
        let (completed, outcome_receiver, release) = match admission {
            Ok(accepted) => accepted,
            Err(handle) => {
                abort_handle.abort();
                let _settled = handle.await;
                return Err("background review admission is closed".to_string());
            }
        };
        // Admission owns incremental collection of finished supervisors. This
        // keeps long-lived sessions bounded without detaching a cleanup task;
        // shutdown drains the same registry when no later review is admitted.
        let _historical_error = await_owned_background_reviews(completed).await;
        Ok(BackgroundReviewPass {
            outcome: Some(outcome_receiver),
            abort_handle,
            release,
            settled: false,
        })
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.receipt.binding.generation
    }

    /// Workspace-local `.eko` root pinned by this generation.
    pub fn echo_agent_dir(&self) -> &std::path::Path {
        &self.receipt.binding.echo_agent_dir
    }
}

impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for ReviewGenerationLease {
    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            drop(self);
        })
    }
}

impl Drop for ReviewGenerationReceipt {
    fn drop(&mut self) {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_passes = state.active_passes.saturating_sub(1);
    }
}

/// Completed application settlement for one framework background review.
pub struct BackgroundReviewSettlement {
    pub outcome: echo_agent::evolution::ReviewOutcome,
    pub evidence_candidate: Option<EvidenceCandidate>,
}

/// Observes one integration-owned background review settlement. A dropped
/// caller aborts the framework task; the registry still awaits the owner that
/// retains the generation through evidence persistence.
#[must_use]
pub struct BackgroundReviewPass {
    outcome: Option<tokio::sync::oneshot::Receiver<Result<BackgroundReviewSettlement, String>>>,
    abort_handle: tokio::task::AbortHandle,
    release: echo_agent::agent::CancellationToken,
    settled: bool,
}

impl BackgroundReviewPass {
    pub async fn settle(&mut self) -> Result<BackgroundReviewSettlement, String> {
        let outcome = self
            .outcome
            .take()
            .ok_or_else(|| "background review pass was already settled".to_string())?;
        let outcome = outcome
            .await
            .map_err(|_| "background review supervisor dropped its outcome".to_string())??;
        self.settled = true;
        Ok(outcome)
    }
}

impl Drop for BackgroundReviewPass {
    fn drop(&mut self) {
        if !self.settled {
            self.abort_handle.abort();
        }
        self.release.cancel();
    }
}

async fn await_owned_background_reviews(tasks: Vec<OwnedBackgroundReview>) -> Option<String> {
    let mut first_error = None;
    for task in tasks {
        let error = match task.supervisor.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(error) => Some(format!(
                "background review supervisor failed to join: {error}"
            )),
        };
        if let Some(error) = error {
            tracing::error!(%error, "owned background review settlement failed");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error
}

/// Exclusive workspace-rebind admission owned by the existing workspace
/// transition. Preparing it is fallible and happens before the transition's
/// commit boundary; committing the already-prepared binding is infallible.
#[must_use]
pub struct ReviewRebindPermit {
    control: Arc<ReviewBindingControl>,
    next: Option<ReviewBinding>,
}

impl ReviewRebindPermit {
    /// Settle triggers captured against the old workspace, then publish the
    /// prepared directory, Store, and generation in one lock acquisition.
    /// Publication is infallible; an incomplete old-root flush is reported in
    /// the receipt so the canonical workspace transition can settle Degraded.
    /// The permit keeps blocking passes until projection settlement completes.
    pub fn commit(&mut self) -> MemoryRebindReceipt {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        flush_queued_triggers_locked(&mut state);
        let pending_old = state.pending_triggers.len();
        let mut pending_roots = Vec::new();
        for queued in &state.pending_triggers {
            if !pending_roots.contains(&queued.echo_agent_dir) {
                pending_roots.push(queued.echo_agent_dir.clone());
            }
        }
        let delivery_error = if pending_old == 0 {
            None
        } else {
            state.last_trigger_delivery_error.clone()
        };
        if let Some(next) = self.next.take() {
            state.current = next;
        }
        MemoryRebindReceipt {
            generation: state.current.generation,
            pending_old,
            pending_roots,
            delivery_error,
        }
    }
}

impl Drop for ReviewRebindPermit {
    fn drop(&mut self) {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.rebind_in_progress = false;
    }
}

fn flush_queued_triggers_locked(state: &mut ReviewBindingState) {
    while let Some(queued) = state.pending_triggers.front().cloned() {
        match EvidenceStore::new(queued.echo_agent_dir).upsert(queued.draft) {
            Ok(_) => {
                state.pending_triggers.pop_front();
            }
            Err(error) => {
                state.trigger_delivery_failures = state.trigger_delivery_failures.saturating_add(1);
                let failures = state.trigger_delivery_failures;
                state.last_trigger_delivery_error = Some(error.clone());
                tracing::error!(%error, failures, "queued memory trigger delivery failed");
                break;
            }
        }
    }
}

/// Bridges the framework's review system into the product lifecycle.
///
/// Self-contained: callers only need a `Store`, the `.echo-agent/` directory
/// path, and an optional [`ReviewConfig`]. All framework-level plumbing
/// (`MemoryLayerManager`, `ChangeLog`, `TypedMemoryStore`) is created
/// internally on each review pass.
pub struct ReviewIntegration {
    config: ReviewConfig,
    /// Single authority for workspace directory, Store and generation. The
    /// pair must never be read or published through independent locks.
    binding: Arc<ReviewBindingControl>,
    /// Framework observer wired to the shared agent HookRegistry.
    evolution_observer: RwLock<Option<Arc<dyn EvolutionObserver>>>,
}

impl ReviewIntegration {
    /// Create a new review integration with the given config.
    pub fn new(config: ReviewConfig, echo_agent_dir: PathBuf, store: Arc<dyn Store>) -> Self {
        Self {
            config,
            binding: Arc::new(ReviewBindingControl {
                state: Mutex::new(ReviewBindingState {
                    current: ReviewBinding {
                        echo_agent_dir,
                        store,
                        generation: 0,
                    },
                    active_passes: 0,
                    rebind_in_progress: false,
                    pending_triggers: VecDeque::new(),
                    trigger_delivery_failures: 0,
                    rejected_triggers: 0,
                    last_trigger_delivery_error: None,
                }),
                background_reviews: Mutex::new(BackgroundReviewRegistry {
                    accepting: true,
                    tasks: Vec::new(),
                }),
            }),
            evolution_observer: RwLock::new(None),
        }
    }

    /// Attach the runtime observer used by memory, candidate and health paths.
    pub fn set_evolution_observer(&self, observer: Arc<dyn EvolutionObserver>) {
        let mut current = self
            .evolution_observer
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *current = Some(observer);
    }

    /// Reserve the memory/evolution generation for the canonical workspace
    /// transition. EKO deliberately returns Busy instead of synchronously
    /// waiting inside async code when a review or Dreaming pass is active.
    pub fn prepare_rebind(
        &self,
        echo_agent_dir: PathBuf,
        store: Arc<dyn Store>,
    ) -> Result<ReviewRebindPermit, ReviewGenerationError> {
        let mut state = self
            .binding
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active_passes != 0 || state.rebind_in_progress {
            return Err(ReviewGenerationError::Busy {
                active_passes: state.active_passes,
                rebind_in_progress: state.rebind_in_progress,
            });
        }
        flush_queued_triggers_locked(&mut state);
        if !state.pending_triggers.is_empty() {
            return Err(ReviewGenerationError::TriggerSettlement {
                pending: state.pending_triggers.len(),
                last_error: state
                    .last_trigger_delivery_error
                    .clone()
                    .unwrap_or_else(|| "unknown trigger delivery failure".to_string()),
            });
        }
        let generation = state
            .current
            .generation
            .checked_add(1)
            .ok_or(ReviewGenerationError::CounterExhausted("generation"))?;
        state.rebind_in_progress = true;
        drop(state);
        Ok(ReviewRebindPermit {
            control: self.binding.clone(),
            next: Some(ReviewBinding {
                echo_agent_dir,
                store,
                generation,
            }),
        })
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

    /// Observable delivery state for triggers that arrived during the short
    /// workspace rebind settlement window.
    pub fn trigger_delivery_status(&self) -> TriggerDeliveryStatus {
        let state = self
            .binding
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        TriggerDeliveryStatus {
            pending: state.pending_triggers.len(),
            failures: state.trigger_delivery_failures,
            rejected: state.rejected_triggers,
            last_error: state.last_trigger_delivery_error.clone(),
        }
    }

    /// Close background-review admission, cancel every accepted inner task,
    /// and await the owned supervisors that retain generation leases until the
    /// inner framework JoinHandle and evidence settlement have both ended.
    pub async fn shutdown_background_reviews(&self) -> Result<(), String> {
        let tasks = {
            let mut registry = self
                .binding
                .background_reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            registry.accepting = false;
            std::mem::take(&mut registry.tasks)
        };
        for task in &tasks {
            task.abort_handle.abort();
            task.release.cancel();
        }
        match await_owned_background_reviews(tasks).await {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    fn background_review_registry_counts(&self) -> (usize, usize) {
        let registry = self
            .binding
            .background_reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let completed = registry
            .tasks
            .iter()
            .filter(|task| task.supervisor.is_finished())
            .count();
        (registry.tasks.len(), completed)
    }

    /// Workspace-scoped curator bound to the current memory root.
    pub fn curator(&self) -> Curator {
        workspace_curator(&self.current_echo_agent_dir())
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
        let lease = self.lease_generation().map_err(|error| error.to_string())?;
        let echo_agent_dir = lease.receipt.binding.echo_agent_dir.clone();
        let store = lease.receipt.binding.store.clone();
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
            let mut detector =
                SkillCandidateDetector::new().with_curator(workspace_curator(&echo_agent_dir));
            if let Some(observer) = self.current_evolution_observer() {
                detector = detector.with_evolution_observer(observer);
            }
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
    /// Reads one atomic `(echo_agent_dir, store, generation)` binding, so after
    /// a workspace `rebind` this manager uses the new workspace.
    pub fn create_layer_manager(&self) -> MemoryLayerManager {
        self.runtime_builder().build_layer_manager()
    }

    /// Create a manager with caller-specific event correlation metadata.
    /// Pool agents use this so shared storage does not imply a shared session ID.
    pub fn create_layer_manager_with_observer(
        &self,
        observer: Arc<dyn EvolutionObserver>,
    ) -> MemoryLayerManager {
        self.runtime_builder()
            .evolution_observer(observer)
            .build_layer_manager()
    }

    /// Create framework runtime wiring without owning product lifecycle policy.
    fn runtime_builder(&self) -> MemoryRuntimeIntegrationBuilder {
        let binding = self.binding_snapshot();
        let mut builder =
            MemoryRuntimeIntegrationBuilder::new(binding.echo_agent_dir, binding.store);
        if let Some(observer) = self.current_evolution_observer() {
            builder = builder.evolution_observer(observer);
        }
        builder
    }

    fn current_evolution_observer(&self) -> Option<Arc<dyn EvolutionObserver>> {
        self.evolution_observer
            .read()
            .map(|observer| observer.clone())
            .unwrap_or_else(|error| error.into_inner().clone())
    }

    fn current_echo_agent_dir(&self) -> PathBuf {
        self.binding_snapshot().echo_agent_dir
    }

    fn binding_snapshot(&self) -> ReviewBinding {
        self.binding
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .clone()
    }

    pub fn lease_generation(&self) -> Result<ReviewGenerationLease, ReviewGenerationError> {
        let binding = {
            let mut state = self
                .binding
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.rebind_in_progress {
                return Err(ReviewGenerationError::Busy {
                    active_passes: state.active_passes,
                    rebind_in_progress: true,
                });
            }
            flush_queued_triggers_locked(&mut state);
            state.active_passes = state
                .active_passes
                .checked_add(1)
                .ok_or(ReviewGenerationError::CounterExhausted("active pass"))?;
            state.current.clone()
        };
        let lease = ReviewGenerationLease {
            receipt: Arc::new(ReviewGenerationReceipt {
                control: self.binding.clone(),
                binding,
                evolution_observer: self.current_evolution_observer(),
            }),
        };
        Ok(lease)
    }

    fn queue_trigger(&self, draft: EvidenceCandidateDraft, delivery_error: Option<String>) {
        let (pending, failures, overflowed) = {
            let mut state = self
                .binding
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.pending_triggers.len() >= MAX_PENDING_TRIGGERS {
                state.trigger_delivery_failures = state.trigger_delivery_failures.saturating_add(1);
                state.rejected_triggers = state.rejected_triggers.saturating_add(1);
                state.last_trigger_delivery_error = Some(delivery_error.unwrap_or_else(|| {
                    format!("memory trigger queue reached its capacity of {MAX_PENDING_TRIGGERS}")
                }));
                (
                    state.pending_triggers.len(),
                    state.trigger_delivery_failures,
                    true,
                )
            } else {
                let echo_agent_dir = state.current.echo_agent_dir.clone();
                if let Some(error) = delivery_error {
                    state.trigger_delivery_failures =
                        state.trigger_delivery_failures.saturating_add(1);
                    state.last_trigger_delivery_error = Some(error);
                }
                state.pending_triggers.push_back(QueuedTrigger {
                    echo_agent_dir,
                    draft,
                });
                (
                    state.pending_triggers.len(),
                    state.trigger_delivery_failures,
                    false,
                )
            }
        };
        if overflowed {
            tracing::error!(
                pending,
                failures,
                capacity = MAX_PENDING_TRIGGERS,
                "memory trigger queue is full; candidate delivery rejected"
            );
        } else {
            tracing::warn!(pending, "memory trigger queued during workspace transition");
        }
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
            let draft = EvidenceCandidateDraft {
                kind,
                scope: matches!(kind, EvidenceKind::UserPreference)
                    .then(|| super::evidence::EvidenceScope::User("local-user".to_string())),
                content: trigger.content.clone(),
                evidence,
                action: None,
                confidence: trigger.confidence,
            };
            let lease = match self.lease_generation() {
                Ok(lease) => lease,
                Err(ReviewGenerationError::Busy { .. }) => {
                    self.queue_trigger(draft, None);
                    return Ok(echo_agent::evolution::MemoryTriggerDisposition::Captured);
                }
                Err(error) => {
                    self.queue_trigger(draft, Some(error.to_string()));
                    tracing::error!(%error, "memory trigger could not acquire generation lease");
                    return Ok(echo_agent::evolution::MemoryTriggerDisposition::Captured);
                }
            };
            if let Err(error) = lease.evidence_store().upsert(draft.clone()) {
                self.queue_trigger(draft, Some(error.clone()));
                let failures = self.trigger_delivery_status().failures;
                // EKO treats inferred memory as review-only. Do not let an inbox
                // storage failure fall through to the framework's direct durable
                // write path and silently bypass that review gate.
                tracing::error!(%error, failures, "failed to persist trigger evidence");
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
        // One synchronous snapshot keeps the policy root and curator state on
        // the same generation even if a workspace transition publishes next.
        let binding = self.binding_snapshot();
        let current_root = binding.echo_agent_dir.join("skills");
        if let Some(skill_root) = workspace_skill_root(&descriptor.location)
            && normalize_path(&skill_root) != normalize_path(&current_root)
        {
            return false;
        }
        match workspace_curator(&binding.echo_agent_dir).skill_for_path(&descriptor.location) {
            Ok(Some(meta)) => matches!(
                meta.lifecycle,
                echo_agent::evolution::SkillLifecycle::Active
                    | echo_agent::evolution::SkillLifecycle::Stale
            ),
            Ok(None) => true,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %descriptor.location.display(),
                    "refusing to load skill because curator state is unreadable"
                );
                false
            }
        }
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
    use futures::future::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingEvolutionObserver {
        writes: Arc<AtomicUsize>,
    }

    impl EvolutionObserver for CountingEvolutionObserver {
        fn on_memory_write<'a>(&'a self, _key: &'a str, _source: &'a str) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.writes.fetch_add(1, Ordering::Relaxed);
            })
        }
    }

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

    #[test]
    fn skill_policy_rejects_unreadable_curator_state() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace/.eko");
        let state_path = echo_dir.join("evolution/curator-state.json");
        let state_parent = state_path
            .parent()
            .ok_or_else(|| "curator state path has no parent".to_string())?;
        std::fs::create_dir_all(state_parent).map_err(|error| error.to_string())?;
        std::fs::write(&state_path, b"{not-json").map_err(|error| error.to_string())?;

        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = ReviewIntegration::new(ReviewConfig::default(), echo_dir.clone(), store);
        let mut descriptor = echo_agent::skills::external::parse_skill_md(
            "---\nname: test-skill\ndescription: test skill\n---\nbody",
        )
        .map_err(|error| error.to_string())?;
        descriptor.location = echo_dir.join("skills/test-skill/SKILL.md");

        assert!(!echo_agent::skills::external::SkillLoadPolicy::allows(
            &integration,
            &descriptor,
        ));
        assert_eq!(
            std::fs::read_to_string(state_path).map_err(|error| error.to_string())?,
            "{not-json"
        );
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
    async fn evolution_observer_survives_rebind_and_manager_rebuild() -> Result<(), String> {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first_store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let ri = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("first/.eko"),
            first_store,
        );
        let writes = Arc::new(AtomicUsize::new(0));
        ri.set_evolution_observer(Arc::new(CountingEvolutionObserver {
            writes: writes.clone(),
        }));

        let meta = || {
            MemoryMeta::new(
                MemoryType::ProjectFact,
                MemorySource::AutoExtracted,
                "observer-test",
            )
            .with_confidence(0.6)
        };
        ri.create_layer_manager()
            .write_memory("first", "first write", meta())
            .await
            .map_err(|error| error.to_string())?;

        let second_store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let mut permit = ri
            .prepare_rebind(temp.path().join("second/.eko"), second_store)
            .map_err(|error| error.to_string())?;
        assert_eq!(permit.commit().generation, 1);
        drop(permit);
        ri.create_layer_manager()
            .write_memory("second", "second write", meta())
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(writes.load(Ordering::Relaxed), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_review_integration_session_end_disabled() -> Result<(), String> {
        use echo_agent::evolution::ReviewConfig;
        use echo_agent::memory::InMemoryStore;

        let store = Arc::new(InMemoryStore::new()) as Arc<dyn Store>;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let dir = temp.path().to_path_buf();
        let config = ReviewConfig {
            review_on_session_end: false,
            ..Default::default()
        };

        let ri = ReviewIntegration::new(config, dir, store);
        let result = ri.on_session_end().await;
        assert!(result.is_none(), "should not review when disabled");
        Ok(())
    }

    #[test]
    fn binding_snapshot_is_atomic_and_rebind_is_busy_until_settlement() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let dir_a = temp.path().join("workspace-a/.eko");
        let dir_b = temp.path().join("workspace-b/.eko");
        let store_a = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let store_b = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration =
            ReviewIntegration::new(ReviewConfig::default(), dir_a.clone(), store_a.clone());

        let pass_a = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(pass_a.generation(), 0);
        assert_eq!(pass_a.echo_agent_dir(), dir_a);
        assert!(Arc::ptr_eq(&pass_a.memory_store(), &store_a));
        assert!(matches!(
            integration.prepare_rebind(dir_b.clone(), store_b.clone()),
            Err(ReviewGenerationError::Busy {
                active_passes: 1,
                rebind_in_progress: false,
            })
        ));

        drop(pass_a);
        let mut permit = integration
            .prepare_rebind(dir_b.clone(), store_b.clone())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            integration.lease_generation(),
            Err(ReviewGenerationError::Busy {
                active_passes: 0,
                rebind_in_progress: true,
            })
        ));
        assert_eq!(permit.commit().generation, 1);
        // Projection settlement still owns the permit after binding publish.
        assert!(matches!(
            integration.lease_generation(),
            Err(ReviewGenerationError::Busy {
                active_passes: 0,
                rebind_in_progress: true,
            })
        ));
        drop(permit);

        let pass_b = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(pass_b.generation(), 1);
        assert_eq!(pass_b.echo_agent_dir(), dir_b);
        assert!(Arc::ptr_eq(&pass_b.memory_store(), &store_b));
        Ok(())
    }

    #[tokio::test]
    async fn aborting_a_parked_pass_releases_generation_admission() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store_a = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store_a,
        ));
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let parked_integration = integration.clone();
        let parked = tokio::spawn(async move {
            let _lease = parked_integration
                .lease_generation()
                .map_err(|error| error.to_string())?;
            let _ignored = started_sender.send(());
            futures::future::pending::<Result<(), String>>().await
        });
        started_receiver
            .await
            .map_err(|error| format!("parked pass did not start: {error}"))?;

        parked.abort();
        let aborted = parked.await;
        assert!(
            aborted
                .as_ref()
                .is_err_and(tokio::task::JoinError::is_cancelled),
            "parked pass should be cancelled"
        );

        let store_b = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let mut permit = integration
            .prepare_rebind(temp.path().join("workspace-b/.eko"), store_b)
            .map_err(|error| error.to_string())?;
        assert_eq!(permit.commit().generation, 1);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_background_review_caller_aborts_child_and_releases_lease()
    -> Result<(), String> {
        struct BlockingDrop {
            entered: Option<tokio::sync::oneshot::Sender<()>>,
            release: std::sync::mpsc::Receiver<()>,
        }

        impl Drop for BlockingDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.entered.take() {
                    let _ignored = sender.send(());
                }
                let _released = self.release.recv();
            }
        }

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store,
        ));
        let (child_started_sender, child_started_receiver) = tokio::sync::oneshot::channel();
        let (drop_entered_sender, drop_entered_receiver) = tokio::sync::oneshot::channel();
        let (drop_release_sender, drop_release_receiver) = std::sync::mpsc::channel();
        let child = tokio::spawn(async move {
            let _drop_signal = BlockingDrop {
                entered: Some(drop_entered_sender),
                release: drop_release_receiver,
            };
            let _ignored = child_started_sender.send(());
            futures::future::pending::<echo_agent::evolution::ReviewOutcome>().await
        });
        child_started_receiver
            .await
            .map_err(|error| format!("background review child did not start: {error}"))?;

        let caller_integration = integration.clone();
        let (pass_started_sender, pass_started_receiver) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            let lease = caller_integration
                .lease_generation()
                .map_err(|error| error.to_string())?;
            let _pass = lease.track_background_review(child).await?;
            let _ignored = pass_started_sender.send(());
            futures::future::pending::<Result<(), String>>().await
        });
        pass_started_receiver
            .await
            .map_err(|error| format!("background review receipt was not installed: {error}"))?;
        caller.abort();
        let _caller_result = caller.await;
        drop_entered_receiver
            .await
            .map_err(|error| format!("background review child did not begin abort: {error}"))?;

        let blocked_store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        assert!(matches!(
            integration.prepare_rebind(temp.path().join("workspace-b/.eko"), blocked_store),
            Err(ReviewGenerationError::Busy {
                active_passes: 1,
                ..
            })
        ));

        drop_release_sender
            .send(())
            .map_err(|error| format!("failed to release background review child: {error}"))?;
        let mut permit = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let next_store =
                    Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
                match integration.prepare_rebind(temp.path().join("workspace-b/.eko"), next_store) {
                    Ok(permit) => return Ok(permit),
                    Err(ReviewGenerationError::Busy { .. }) => tokio::task::yield_now().await,
                    Err(error) => return Err(error.to_string()),
                }
            }
        })
        .await
        .map_err(|_| "background review lease did not settle after child exit".to_string())??;
        assert_eq!(permit.commit().generation, 1);
        Ok(())
    }

    #[tokio::test]
    async fn dropped_observer_does_not_cancel_completed_evidence_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace-a/.eko");
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            echo_dir,
            store,
        ));
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
        let child = tokio::spawn(async move {
            let _released = release_receiver.await;
            echo_agent::evolution::ReviewOutcome {
                run_id: "run-a".to_string(),
                actions: vec!["capture project fact".to_string()],
                nothing_to_save: false,
                candidate: Some(echo_agent::evolution::ReviewCandidate {
                    kind: echo_agent::evolution::ReviewCandidateKind::ProjectFact,
                    content: "workspace uses Rust".to_string(),
                    evidence: "Cargo.toml declares Rust crates".to_string(),
                    confidence: 0.9,
                    persisted: false,
                }),
                error: None,
            }
        });
        let lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let pass = lease.track_background_review(child).await?;
        release_sender
            .send(())
            .map_err(|_| "failed to release background review".to_string())?;
        let mut evidence_settled = false;
        for _attempt in 0..128 {
            if integration.evidence_store().list()?.len() == 1 {
                evidence_settled = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            evidence_settled,
            "integration owner did not persist completed review evidence"
        );
        drop(pass);

        integration.shutdown_background_reviews().await?;
        let candidates = integration.evidence_store().list()?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates
                .first()
                .map(|candidate| candidate.content.as_str()),
            Some("workspace uses Rust")
        );
        Ok(())
    }

    #[tokio::test]
    async fn later_admission_collects_finished_background_review_supervisors() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store,
        ));
        let completed_child = tokio::spawn(async {
            echo_agent::evolution::ReviewOutcome {
                run_id: "completed-review".to_string(),
                actions: Vec::new(),
                nothing_to_save: true,
                candidate: None,
                error: None,
            }
        });
        let completed_lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let mut completed_pass = completed_lease
            .track_background_review(completed_child)
            .await?;
        let _settlement = completed_pass.settle().await?;
        drop(completed_pass);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if integration.background_review_registry_counts().1 == 1 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "completed background review supervisor did not settle".to_string())?;

        let active_child = tokio::spawn(async {
            futures::future::pending::<echo_agent::evolution::ReviewOutcome>().await
        });
        let active_lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let active_pass = active_lease.track_background_review(active_child).await?;
        assert_eq!(integration.background_review_registry_counts(), (1, 0));

        drop(active_pass);
        integration.shutdown_background_reviews().await?;
        assert_eq!(integration.background_review_registry_counts(), (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_closes_background_review_admission_and_awaits_supervisors()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store,
        ));
        let child = tokio::spawn(async {
            futures::future::pending::<echo_agent::evolution::ReviewOutcome>().await
        });
        let lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let mut pass = lease.track_background_review(child).await?;

        integration.shutdown_background_reviews().await?;
        assert!(pass.settle().await.is_err());

        let rejected_child = tokio::spawn(async {
            futures::future::pending::<echo_agent::evolution::ReviewOutcome>().await
        });
        let rejected_lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert!(
            rejected_lease
                .track_background_review(rejected_child)
                .await
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn failed_trigger_flush_stays_owned_and_retries_exactly_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace-a/.eko");
        std::fs::create_dir_all(&echo_dir).map_err(|error| error.to_string())?;
        let blocked_parent = echo_dir.join("evolution");
        std::fs::write(&blocked_parent, b"blocks directory creation")
            .map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = ReviewIntegration::new(ReviewConfig::default(), echo_dir.clone(), store);
        integration.queue_trigger(
            EvidenceCandidateDraft {
                kind: EvidenceKind::ProjectFact,
                scope: None,
                content: "workspace uses Rust".to_string(),
                evidence: vec![EvidenceRef {
                    source: EvidenceSource::TriggerDetector,
                    source_run_id: None,
                    source_role: Some("user".to_string()),
                    source_turn: None,
                    source_memory_key: None,
                    quote: "workspace uses Rust".to_string(),
                }],
                action: None,
                confidence: 0.9,
            },
            None,
        );

        let first = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let failed = integration.trigger_delivery_status();
        assert_eq!(failed.pending, 1);
        assert_eq!(failed.failures, 1);
        drop(first);

        std::fs::remove_file(&blocked_parent).map_err(|error| error.to_string())?;
        let retry = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(integration.trigger_delivery_status().pending, 0);
        assert_eq!(retry.evidence_store().list()?.len(), 1);
        drop(retry);

        let final_pass = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(final_pass.evidence_store().list()?.len(), 1);
        Ok(())
    }

    #[test]
    fn full_trigger_queue_reports_unowned_candidate() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store,
        );
        for index in 0..=MAX_PENDING_TRIGGERS {
            integration.queue_trigger(
                EvidenceCandidateDraft {
                    kind: EvidenceKind::ProjectFact,
                    scope: None,
                    content: format!("candidate {index}"),
                    evidence: Vec::new(),
                    action: None,
                    confidence: 0.9,
                },
                None,
            );
        }

        let delivery = integration.trigger_delivery_status();
        assert_eq!(delivery.pending, MAX_PENDING_TRIGGERS);
        assert_eq!(delivery.rejected, 1);
        assert_eq!(delivery.failures, 1);
        assert!(
            delivery
                .last_error
                .is_some_and(|error| error.contains("capacity"))
        );
        Ok(())
    }

    #[test]
    fn degraded_commit_publishes_next_binding_and_retries_old_root_only() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let dir_a = temp.path().join("workspace-a/.eko");
        let dir_b = temp.path().join("workspace-b/.eko");
        std::fs::create_dir_all(&dir_a).map_err(|error| error.to_string())?;
        let store_a = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let store_b = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = ReviewIntegration::new(ReviewConfig::default(), dir_a.clone(), store_a);
        let mut permit = integration
            .prepare_rebind(dir_b.clone(), store_b.clone())
            .map_err(|error| error.to_string())?;

        let blocked_parent = dir_a.join("evolution");
        std::fs::write(&blocked_parent, b"blocks directory creation")
            .map_err(|error| error.to_string())?;
        integration.queue_trigger(
            EvidenceCandidateDraft {
                kind: EvidenceKind::ProjectFact,
                scope: None,
                content: "old workspace fact".to_string(),
                evidence: vec![EvidenceRef {
                    source: EvidenceSource::TriggerDetector,
                    source_run_id: None,
                    source_role: Some("user".to_string()),
                    source_turn: None,
                    source_memory_key: None,
                    quote: "old workspace fact".to_string(),
                }],
                action: None,
                confidence: 0.9,
            },
            None,
        );

        let receipt = permit.commit();
        assert_eq!(receipt.generation, 1);
        assert_eq!(receipt.pending_old, 1);
        assert_eq!(receipt.pending_roots, vec![dir_a.clone()]);
        assert!(receipt.is_degraded());
        drop(permit);

        std::fs::remove_file(&blocked_parent).map_err(|error| error.to_string())?;
        let lease_b = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(lease_b.generation(), 1);
        assert_eq!(lease_b.echo_agent_dir(), dir_b);
        assert!(Arc::ptr_eq(&lease_b.memory_store(), &store_b));
        assert_eq!(lease_b.evidence_store().list()?.len(), 0);
        assert_eq!(EvidenceStore::new(dir_a.clone()).list()?.len(), 1);
        assert_eq!(integration.trigger_delivery_status().pending, 0);
        drop(lease_b);

        let final_lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            EvidenceStore::new(final_lease.echo_agent_dir().to_path_buf())
                .list()?
                .len(),
            0
        );
        assert_eq!(EvidenceStore::new(dir_a).list()?.len(), 1);
        assert_eq!(integration.trigger_delivery_status().pending, 0);
        Ok(())
    }
}
