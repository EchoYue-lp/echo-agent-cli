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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use super::evidence::{
    EvidenceCandidate, EvidenceCandidateDraft, EvidenceKind, EvidenceRef, EvidenceSource,
    EvidenceStore, capture_memory_conflict, capture_review_outcome,
};
#[cfg(test)]
use super::rule_promoter::RulePromotionPhase;
use super::rule_promoter::{RulePromoter, RulePromotionError, RulePromotionReceipt, RuleProposal};

#[derive(Default)]
struct RuleProjectionTargets {
    primary: Option<crate::agent_handle::AgentHandle>,
    pool: Option<Weak<crate::agent_pool::AgentPool>>,
}

struct HotMemoryProjectionControl {
    gate: Arc<tokio::sync::Mutex<()>>,
    repair: tokio::sync::Mutex<HotMemoryProjectionRepair>,
    snapshot: RwLock<Option<crate::unified_memory::HotMemoryProjectionSnapshot>>,
    targets: Arc<RwLock<RuleProjectionTargets>>,
    source: Arc<crate::unified_memory::HotMemoryProjectionSource>,
    active_generation: AtomicU64,
    settled_dirty_epoch: AtomicU64,
    #[cfg(test)]
    projection_reads: AtomicU64,
}

struct HotMemoryProjectionRepair {
    accepting: bool,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleProjectionFault {
    Primary,
    Pool,
}

#[derive(Clone)]
struct ReviewBinding {
    authority_scope: String,
    workspace_generation: String,
    echo_agent_dir: PathBuf,
    store: Arc<dyn Store>,
    generation: u64,
    layer_manager: Arc<Mutex<Option<Arc<MemoryLayerManager>>>>,
    projection_observer: Arc<MemoryProjectionObserver>,
}

struct MemoryProjectionObserver {
    revision: AtomicU64,
    downstream: Arc<RwLock<Option<Arc<dyn EvolutionObserver>>>>,
    projection_gate: Arc<tokio::sync::Mutex<()>>,
}

impl MemoryProjectionObserver {
    fn new(
        downstream: Arc<RwLock<Option<Arc<dyn EvolutionObserver>>>>,
        projection_gate: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        Self {
            revision: AtomicU64::new(0),
            downstream,
            projection_gate,
        }
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn mark_dirty(&self) {
        let _ = self
            .revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
                revision.checked_add(1)
            });
    }

    fn downstream(&self) -> Option<Arc<dyn EvolutionObserver>> {
        self.downstream
            .read()
            .map(|observer| observer.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }
}

impl EvolutionObserver for MemoryProjectionObserver {
    fn on_memory_write<'a>(
        &'a self,
        key: &'a str,
        source: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(observer) = self.downstream() {
                observer.on_memory_write(key, source).await;
            }
        })
    }

    fn on_memory_layer_change<'a>(
        &'a self,
        key: &'a str,
        from_layer: &'a str,
        to_layer: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            if from_layer == "hot" || to_layer == "hot" {
                let _gate = self.projection_gate.lock().await;
                self.mark_dirty();
            }
            if let Some(observer) = self.downstream() {
                observer
                    .on_memory_layer_change(key, from_layer, to_layer)
                    .await;
            }
        })
    }

    fn on_skill_candidate_detected<'a>(
        &'a self,
        skill_name: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(observer) = self.downstream() {
                observer.on_skill_candidate_detected(skill_name).await;
            }
        })
    }

    fn on_skill_health_check<'a>(
        &'a self,
        skill_name: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(observer) = self.downstream() {
                observer.on_skill_health_check(skill_name).await;
            }
        })
    }
}

struct ReviewBindingState {
    current: ReviewBinding,
    active_passes: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProjectionSettlementStatus {
    Settled,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MemoryProjectionSettlementReceipt {
    pub authority_scope: String,
    pub workspace_generation: String,
    pub revision: String,
    pub changed: bool,
    pub status: MemoryProjectionSettlementStatus,
    pub primary_bound: bool,
    pub pool_bound: bool,
    pub future_bound: bool,
    pub pending_revision: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewGenerationError {
    CounterExhausted(&'static str),
    TriggerSettlement { pending: usize, last_error: String },
}

impl std::fmt::Display for ReviewGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
    projection_observer: Arc<MemoryProjectionObserver>,
    hot_memory_projection: Arc<HotMemoryProjectionControl>,
}

impl ReviewGenerationLease {
    pub fn layer_manager(&self) -> echo_agent::error::Result<Arc<MemoryLayerManager>> {
        layer_manager_for_binding(&self.receipt.binding, &self.receipt.projection_observer)
    }

    /// Evidence inbox pinned to the same workspace as this pass.
    pub fn evidence_store(&self) -> EvidenceStore {
        EvidenceStore::new(self.receipt.binding.echo_agent_dir.clone())
    }

    /// Framework memory store pinned to the same workspace generation.
    pub fn memory_store(&self) -> Arc<dyn Store> {
        self.receipt.binding.store.clone()
    }

    pub async fn settle_hot_memory_projection(&self) -> MemoryProjectionSettlementReceipt {
        let receipt = settle_hot_memory_projection_for_binding(
            &self.receipt.binding,
            &self.receipt.hot_memory_projection,
        )
        .await;
        if receipt.status == MemoryProjectionSettlementStatus::Degraded {
            self.schedule_hot_memory_projection_repair().await;
        }
        receipt
    }

    async fn schedule_hot_memory_projection_repair(&self) {
        let mut repair = self.receipt.hot_memory_projection.repair.lock().await;
        if !repair.accepting {
            return;
        }
        if let Some(existing) = repair.task.take() {
            if !existing.is_finished() {
                repair.task = Some(existing);
                return;
            }
            if let Err(error) = existing.await {
                tracing::error!(%error, "hot-memory projection repair task failed to join");
            }
        }
        let binding = self.receipt.binding.clone();
        let control = self.receipt.hot_memory_projection.clone();
        repair.task = Some(tokio::spawn(async move {
            let mut last_receipt = None;
            for delay_ms in [25_u64, 50, 100] {
                if control.active_generation.load(Ordering::Acquire) != binding.generation {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let receipt = settle_hot_memory_projection_for_binding(&binding, &control).await;
                if receipt.status == MemoryProjectionSettlementStatus::Settled {
                    return;
                }
                last_receipt = Some(receipt);
            }
            if let Some(receipt) = last_receipt {
                tracing::warn!(
                    authority_scope = %receipt.authority_scope,
                    workspace_generation = %receipt.workspace_generation,
                    revision = %receipt.revision,
                    pending_revision = ?receipt.pending_revision,
                    error = ?receipt.error,
                    "bounded hot-memory projection repair remains degraded"
                );
            }
        }));
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

    /// Workspace-local `.eko` root pinned by this generation.
    pub fn echo_agent_dir(&self) -> &std::path::Path {
        &self.receipt.binding.echo_agent_dir
    }
}

fn layer_manager_for_binding(
    binding: &ReviewBinding,
    observer: &Arc<MemoryProjectionObserver>,
) -> echo_agent::error::Result<Arc<MemoryLayerManager>> {
    let mut manager = binding
        .layer_manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(manager) = manager.as_ref() {
        return Ok(Arc::clone(manager));
    }
    let created = Arc::new(
        MemoryRuntimeIntegrationBuilder::new(binding.echo_agent_dir.clone(), binding.store.clone())
            .evolution_observer(observer.clone())
            .build_layer_manager()?,
    );
    *manager = Some(Arc::clone(&created));
    Ok(created)
}

fn projection_target_bindings(control: &HotMemoryProjectionControl) -> (bool, bool, bool) {
    let targets = control
        .targets
        .read()
        .map(|targets| (targets.primary.is_some(), targets.pool.clone()))
        .unwrap_or_else(|poisoned| {
            let targets = poisoned.into_inner();
            (targets.primary.is_some(), targets.pool.clone())
        });
    let pool_bound = targets.1.and_then(|pool| pool.upgrade()).is_some();
    (targets.0, pool_bound, pool_bound)
}

async fn settle_hot_memory_projection_for_binding(
    binding: &ReviewBinding,
    control: &HotMemoryProjectionControl,
) -> MemoryProjectionSettlementReceipt {
    let _gate = control.gate.lock().await;
    let dirty_epoch = binding.projection_observer.revision();
    let previous = control
        .snapshot
        .read()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
    let previous_revision = previous
        .as_ref()
        .map(|snapshot| snapshot.revision().to_string())
        .unwrap_or_else(|| "unpublished".to_string());
    let degraded = |error: String,
                    pending_revision: Option<String>,
                    changed: bool|
     -> MemoryProjectionSettlementReceipt {
        let (primary_bound, pool_bound, future_bound) = projection_target_bindings(control);
        MemoryProjectionSettlementReceipt {
            authority_scope: binding.authority_scope.clone(),
            workspace_generation: binding.workspace_generation.clone(),
            revision: previous_revision.clone(),
            changed,
            status: MemoryProjectionSettlementStatus::Degraded,
            primary_bound,
            pool_bound,
            future_bound,
            pending_revision,
            error: Some(error),
        }
    };
    if control.active_generation.load(Ordering::Acquire) != binding.generation {
        return degraded(
            "stale memory generation cannot publish projection".to_string(),
            Some(format!("dirty:{dirty_epoch}")),
            false,
        );
    }
    if previous.is_some() && control.settled_dirty_epoch.load(Ordering::Acquire) == dirty_epoch {
        let (primary_bound, pool_bound, future_bound) = projection_target_bindings(control);
        return MemoryProjectionSettlementReceipt {
            authority_scope: binding.authority_scope.clone(),
            workspace_generation: binding.workspace_generation.clone(),
            revision: previous_revision,
            changed: false,
            status: MemoryProjectionSettlementStatus::Settled,
            primary_bound,
            pool_bound,
            future_bound,
            pending_revision: None,
            error: None,
        };
    }
    if let Err(error) = layer_manager_for_binding(binding, &binding.projection_observer) {
        return degraded(
            format!("memory layer manager unavailable: {error}"),
            Some(format!("dirty:{dirty_epoch}")),
            false,
        );
    }
    #[cfg(test)]
    control.projection_reads.fetch_add(1, Ordering::AcqRel);
    let snapshot = match crate::unified_memory::load_hot_memory_projection_snapshot(
        binding.echo_agent_dir.clone(),
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return degraded(error, Some(format!("dirty:{dirty_epoch}")), false);
        }
    };
    let changed = previous
        .as_ref()
        .is_none_or(|previous| !previous.same_content(&snapshot));
    if control.active_generation.load(Ordering::Acquire) != binding.generation {
        return degraded(
            "memory generation retired during projection read".to_string(),
            Some(snapshot.revision().to_string()),
            changed,
        );
    }
    let latest_dirty_epoch = binding.projection_observer.revision();
    if latest_dirty_epoch != dirty_epoch {
        return degraded(
            "hot memory changed during projection read".to_string(),
            Some(format!("dirty:{latest_dirty_epoch}")),
            false,
        );
    }
    let (primary_bound, pool_bound, future_bound) = projection_target_bindings(control);
    if !primary_bound {
        return degraded(
            "primary hot-memory projection target is not bound".to_string(),
            Some(snapshot.revision().to_string()),
            changed,
        );
    }
    let revision = snapshot.revision().to_string();
    control.source.publish(snapshot.clone());
    let final_dirty_epoch = binding.projection_observer.revision();
    if control.active_generation.load(Ordering::Acquire) != binding.generation
        || final_dirty_epoch != dirty_epoch
    {
        return degraded(
            "hot-memory projection changed before settlement commit".to_string(),
            Some(format!("dirty:{final_dirty_epoch}")),
            false,
        );
    }
    *control
        .snapshot
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
    control
        .settled_dirty_epoch
        .store(dirty_epoch, Ordering::Release);
    MemoryProjectionSettlementReceipt {
        authority_scope: binding.authority_scope.clone(),
        workspace_generation: binding.workspace_generation.clone(),
        revision,
        changed,
        status: MemoryProjectionSettlementStatus::Settled,
        primary_bound: true,
        pool_bound,
        future_bound,
        pending_revision: None,
        error: None,
    }
}

impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for ReviewGenerationLease {
    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            let receipt = self.settle_hot_memory_projection().await;
            if receipt.status == MemoryProjectionSettlementStatus::Degraded {
                tracing::warn!(
                    authority_scope = %receipt.authority_scope,
                    workspace_generation = %receipt.workspace_generation,
                    revision = %receipt.revision,
                    pending_revision = ?receipt.pending_revision,
                    error = ?receipt.error,
                    "run release retained committed hot-memory projection debt"
                );
            }
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
    /// One observer shared by the generation-bound layer manager. It marks the
    /// latest hot-memory revision dirty and forwards framework hook events.
    forwarding_evolution_observer: Arc<RwLock<Option<Arc<dyn EvolutionObserver>>>>,
    /// Serializes promotion recovery, publication, and projection settlement.
    promotion_gate: tokio::sync::Mutex<()>,
    /// Primary plus current pool are the one projection target set used by all
    /// surfaces. A successful promotion refreshes both before returning.
    rule_projection_targets: Arc<RwLock<RuleProjectionTargets>>,
    /// Last generation confirmed on primary and pool. Instruction files remain
    /// the content authority; this is only a runtime publication receipt.
    rule_projection_snapshot: RwLock<Option<crate::unified_memory::InstructionProjectionSnapshot>>,
    hot_memory_projection: Arc<HotMemoryProjectionControl>,
    #[cfg(test)]
    rule_projection_fault: Mutex<Option<RuleProjectionFault>>,
}

impl ReviewIntegration {
    /// Create a new review integration with the given config.
    pub fn new(config: ReviewConfig, echo_agent_dir: PathBuf, store: Arc<dyn Store>) -> Self {
        Self::new_scoped(
            config,
            echo_agent_dir,
            store,
            "global".to_string(),
            "global".to_string(),
        )
    }

    pub fn new_scoped(
        config: ReviewConfig,
        echo_agent_dir: PathBuf,
        store: Arc<dyn Store>,
        authority_scope: String,
        workspace_generation: String,
    ) -> Self {
        let forwarding_evolution_observer = Arc::new(RwLock::new(None));
        let projection_gate = Arc::new(tokio::sync::Mutex::new(()));
        let memory_projection_observer = Arc::new(MemoryProjectionObserver::new(
            forwarding_evolution_observer.clone(),
            projection_gate.clone(),
        ));
        let rule_projection_targets = Arc::new(RwLock::new(RuleProjectionTargets::default()));
        let hot_memory_projection = Arc::new(HotMemoryProjectionControl {
            gate: projection_gate,
            repair: tokio::sync::Mutex::new(HotMemoryProjectionRepair {
                accepting: true,
                task: None,
            }),
            snapshot: RwLock::new(None),
            targets: rule_projection_targets.clone(),
            source: Arc::new(crate::unified_memory::HotMemoryProjectionSource::new()),
            active_generation: AtomicU64::new(0),
            settled_dirty_epoch: AtomicU64::new(0),
            #[cfg(test)]
            projection_reads: AtomicU64::new(0),
        });
        Self {
            config,
            binding: Arc::new(ReviewBindingControl {
                state: Mutex::new(ReviewBindingState {
                    current: ReviewBinding {
                        authority_scope,
                        workspace_generation,
                        echo_agent_dir,
                        store,
                        generation: 0,
                        layer_manager: Arc::new(Mutex::new(None)),
                        projection_observer: memory_projection_observer,
                    },
                    active_passes: 0,
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
            forwarding_evolution_observer,
            promotion_gate: tokio::sync::Mutex::new(()),
            rule_projection_targets,
            rule_projection_snapshot: RwLock::new(None),
            hot_memory_projection,
            #[cfg(test)]
            rule_projection_fault: Mutex::new(None),
        }
    }

    pub fn bind_rule_projection_primary(&self, primary: crate::agent_handle::AgentHandle) {
        self.rule_projection_targets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .primary = Some(primary);
    }

    pub async fn bind_rule_projection_pool(
        &self,
        pool: &Arc<crate::agent_pool::AgentPool>,
    ) -> Result<(), RulePromotionError> {
        self.rule_projection_targets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pool = Some(Arc::downgrade(pool));
        let _gate = self.promotion_gate.lock().await;
        let lease = self
            .lease_generation()
            .map_err(|error| RulePromotionError::Projection(error.to_string()))?;
        let snapshot = self.current_or_load_rule_projection(&lease)?;
        self.publish_rule_projection(snapshot).await
    }

    /// Reconcile prepared rule promotions for the bootstrap binding before any
    /// review or promotion admission is published.
    pub async fn initialize_rule_promotions(&self) -> Result<(), RulePromotionError> {
        let _gate = self.promotion_gate.lock().await;
        let lease = self
            .lease_generation()
            .map_err(|error| RulePromotionError::Projection(error.to_string()))?;
        let pending = self.reconcile_rule_promotions_for_lease(&lease).await?;
        if !pending.is_empty() || self.has_rule_projection_primary() {
            let snapshot = self.current_or_load_rule_projection(&lease)?;
            self.publish_rule_projection(snapshot).await?;
            self.commit_rule_projection_receipts(&lease, &pending)
                .await?;
        }
        Ok(())
    }

    pub async fn scan_rule_proposals(&self) -> Result<Vec<RuleProposal>, RulePromotionError> {
        let _gate = self.promotion_gate.lock().await;
        let lease = self
            .lease_generation()
            .map_err(|error| RulePromotionError::Projection(error.to_string()))?;
        let pending = self.reconcile_rule_promotions_for_lease(&lease).await?;
        let snapshot = self.current_or_load_rule_projection(&lease)?;
        self.publish_rule_projection(snapshot).await?;
        self.commit_rule_projection_receipts(&lease, &pending)
            .await?;
        Ok(RulePromoter::new(lease.memory_store())
            .with_rules_path(lease.echo_agent_dir().join("learned-rules.md"))
            .scan_for_proposals()
            .await)
    }

    pub async fn promote_rule(
        &self,
        proposal: &RuleProposal,
    ) -> Result<RulePromotionReceipt, RulePromotionError> {
        let _gate = self.promotion_gate.lock().await;
        let lease = self
            .lease_generation()
            .map_err(|error| RulePromotionError::Projection(error.to_string()))?;
        let promoter = RulePromoter::new(lease.memory_store())
            .with_rules_path(lease.echo_agent_dir().join("learned-rules.md"));
        let change_log = self.rule_promotion_change_log(&lease)?;
        let recovered = promoter.reconcile_pending(&change_log).await?;
        if !recovered.is_empty() {
            let recovered_snapshot = self.current_or_load_rule_projection(&lease)?;
            self.publish_rule_projection(recovered_snapshot).await?;
            self.commit_rule_projection_receipts(&lease, &recovered)
                .await?;
        }
        let effects = promoter.promote_rule(proposal, &change_log).await?;
        let snapshot = crate::unified_memory::load_instruction_projection_strict(
            lease.echo_agent_dir().parent(),
        )
        .map_err(|error| RulePromotionError::Projection(error.to_string()))?;
        self.publish_rule_projection(snapshot).await?;
        let receipt = promoter.commit_projection(&effects).await?;
        let primary = self
            .rule_projection_targets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .primary
            .clone()
            .ok_or_else(|| {
                RulePromotionError::Projection(
                    "primary rule projection target is not bound during promotion".to_string(),
                )
            })?;
        super::fire_evolution_hook(
            &primary,
            echo_agent::hooks::HookEvent::RulePromoted,
            &receipt.memory_key,
        )
        .await;
        Ok(receipt)
    }

    async fn reconcile_rule_promotions_for_lease(
        &self,
        lease: &ReviewGenerationLease,
    ) -> Result<Vec<RulePromotionReceipt>, RulePromotionError> {
        let promoter = RulePromoter::new(lease.memory_store())
            .with_rules_path(lease.echo_agent_dir().join("learned-rules.md"));
        let change_log = self.rule_promotion_change_log(lease)?;
        promoter.reconcile_pending(&change_log).await
    }

    async fn commit_rule_projection_receipts(
        &self,
        lease: &ReviewGenerationLease,
        receipts: &[RulePromotionReceipt],
    ) -> Result<(), RulePromotionError> {
        if receipts.is_empty() {
            return Ok(());
        }
        let promoter = RulePromoter::new(lease.memory_store())
            .with_rules_path(lease.echo_agent_dir().join("learned-rules.md"));
        for receipt in receipts {
            promoter.commit_projection(receipt).await?;
        }
        Ok(())
    }

    fn rule_promotion_change_log(
        &self,
        lease: &ReviewGenerationLease,
    ) -> Result<echo_agent::evolution::JsonlChangeLog, RulePromotionError> {
        echo_agent::evolution::JsonlChangeLog::new(
            lease
                .echo_agent_dir()
                .join("evolution")
                .join("change-log.jsonl"),
        )
        .map_err(|error| RulePromotionError::Audit(error.to_string()))
    }

    fn current_or_load_rule_projection(
        &self,
        lease: &ReviewGenerationLease,
    ) -> Result<crate::unified_memory::InstructionProjectionSnapshot, RulePromotionError> {
        crate::unified_memory::load_instruction_projection_strict(lease.echo_agent_dir().parent())
            .map_err(|error| RulePromotionError::Projection(error.to_string()))
    }

    fn has_rule_projection_primary(&self) -> bool {
        self.rule_projection_targets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .primary
            .is_some()
    }

    async fn publish_rule_projection(
        &self,
        snapshot: crate::unified_memory::InstructionProjectionSnapshot,
    ) -> Result<(), RulePromotionError> {
        self.publish_rule_projection_with_transition(snapshot, None)
            .await
    }

    async fn publish_rule_projection_with_transition(
        &self,
        snapshot: crate::unified_memory::InstructionProjectionSnapshot,
        workspace_transition: Option<&crate::agent_pool::AgentPoolWorkspaceTransition<'_>>,
    ) -> Result<(), RulePromotionError> {
        let targets = {
            let targets = self
                .rule_projection_targets
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (targets.primary.clone(), targets.pool.clone())
        };
        let primary = targets.0.ok_or_else(|| {
            RulePromotionError::Projection(
                "primary rule projection target is not bound during bootstrap".to_string(),
            )
        })?;
        let primary_execution = primary
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(primary.inner());
        let mut primary_agent = primary_owner.write_owned().await;
        let pool = targets.1.and_then(|pool| pool.upgrade());
        let mut pool_publication = match (pool.as_ref(), workspace_transition) {
            (Some(pool), None) => Some(
                pool.begin_instruction_publication()
                    .await
                    .map_err(RulePromotionError::Projection)?,
            ),
            _ => None,
        };
        #[cfg(test)]
        if self.take_rule_projection_fault(RuleProjectionFault::Primary) {
            return Err(RulePromotionError::Projection(
                "injected primary instruction projection failure".to_string(),
            ));
        }
        #[cfg(test)]
        if self.take_rule_projection_fault(RuleProjectionFault::Pool) {
            return Err(RulePromotionError::Projection(
                "injected pool instruction projection failure".to_string(),
            ));
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.prepare(snapshot.clone()).await
        {
            return Err(RulePromotionError::Projection(error));
        }
        if let (Some(pool), Some(transition)) = (pool.as_ref(), workspace_transition) {
            transition
                .publish_instruction_snapshot(pool, snapshot.clone())
                .await
                .map_err(RulePromotionError::Projection)?;
        }
        crate::unified_memory::apply_instruction_projection_snapshot(&mut primary_agent, &snapshot)
            .await;
        if workspace_transition.is_none()
            && let Some(publication) = pool_publication
        {
            publication
                .commit()
                .await
                .map_err(RulePromotionError::Projection)?;
        }
        *self
            .rule_projection_snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
        Ok(())
    }

    #[cfg(test)]
    fn take_rule_projection_fault(&self, expected: RuleProjectionFault) -> bool {
        let mut fault = self
            .rule_projection_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *fault == Some(expected) {
            *fault = None;
            true
        } else {
            false
        }
    }

    /// Attach the runtime observer used by memory, candidate and health paths.
    pub fn set_evolution_observer(&self, observer: Arc<dyn EvolutionObserver>) {
        *self
            .forwarding_evolution_observer
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(observer);
    }

    fn current_evolution_observer(&self) -> Option<Arc<dyn EvolutionObserver>> {
        self.forwarding_evolution_observer
            .read()
            .map(|observer| observer.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub(crate) fn hot_memory_projection_source(
        &self,
    ) -> Arc<crate::unified_memory::HotMemoryProjectionSource> {
        self.hot_memory_projection.source.clone()
    }

    #[cfg(test)]
    fn hot_memory_projection_read_count(&self) -> u64 {
        self.hot_memory_projection
            .projection_reads
            .load(Ordering::Acquire)
    }

    pub async fn settle_hot_memory_projection(&self) -> MemoryProjectionSettlementReceipt {
        match self.lease_generation() {
            Ok(lease) => lease.settle_hot_memory_projection().await,
            Err(error) => {
                let binding = self.binding_snapshot();
                let targets = self
                    .rule_projection_targets
                    .read()
                    .map(|targets| (targets.primary.is_some(), targets.pool.clone()))
                    .unwrap_or_else(|poisoned| {
                        let targets = poisoned.into_inner();
                        (targets.primary.is_some(), targets.pool.clone())
                    });
                let pool_bound = targets.1.and_then(|pool| pool.upgrade()).is_some();
                let revision = self
                    .hot_memory_projection
                    .snapshot
                    .read()
                    .map(|snapshot| {
                        snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.revision().to_string())
                            .unwrap_or_else(|| "unpublished".to_string())
                    })
                    .unwrap_or_else(|poisoned| {
                        poisoned
                            .into_inner()
                            .as_ref()
                            .map(|snapshot| snapshot.revision().to_string())
                            .unwrap_or_else(|| "unpublished".to_string())
                    });
                MemoryProjectionSettlementReceipt {
                    authority_scope: binding.authority_scope,
                    workspace_generation: binding.workspace_generation,
                    revision,
                    changed: false,
                    status: MemoryProjectionSettlementStatus::Degraded,
                    primary_bound: targets.0,
                    pool_bound,
                    future_bound: pool_bound,
                    pending_revision: Some(format!(
                        "dirty:{}",
                        binding.projection_observer.revision()
                    )),
                    error: Some(error.to_string()),
                }
            }
        }
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

    /// Observable delivery state for queued trigger persistence.
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
    pub fn begin_background_review_shutdown(&self) {
        let mut registry = self
            .binding
            .background_reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.accepting = false;
        for task in &registry.tasks {
            task.abort_handle.abort();
            task.release.cancel();
        }
    }

    pub async fn shutdown_background_reviews(&self) -> Result<(), String> {
        self.begin_background_review_shutdown();
        {
            let _projection_gate = self.hot_memory_projection.gate.lock().await;
            let mut targets = self
                .rule_projection_targets
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            targets.primary = None;
            targets.pool = None;
            let _advanced = self.hot_memory_projection.active_generation.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |generation| generation.checked_add(1),
            );
        }
        let tasks = {
            let mut registry = self
                .binding
                .background_reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut registry.tasks)
        };
        for task in &tasks {
            task.abort_handle.abort();
            task.release.cancel();
        }
        let projection_repair = {
            let mut repair = self.hot_memory_projection.repair.lock().await;
            repair.accepting = false;
            repair.task.take()
        };
        let mut errors = Vec::new();
        if let Some(error) = await_owned_background_reviews(tasks).await {
            errors.push(error);
        }
        if let Some(repair) = projection_repair
            && let Err(error) = repair.await
        {
            errors.push(format!(
                "hot-memory projection repair failed to join: {error}"
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
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
        let _promotion_gate = self.promotion_gate.lock().await;
        let lease = self.lease_generation().map_err(|error| error.to_string())?;
        let pending = self
            .reconcile_rule_promotions_for_lease(&lease)
            .await
            .map_err(|error| error.to_string())?;
        if !pending.is_empty() || self.has_rule_projection_primary() {
            let snapshot = self
                .current_or_load_rule_projection(&lease)
                .map_err(|error| error.to_string())?;
            self.publish_rule_projection(snapshot)
                .await
                .map_err(|error| error.to_string())?;
            self.commit_rule_projection_receipts(&lease, &pending)
                .await
                .map_err(|error| error.to_string())?;
        }
        let echo_agent_dir = lease.receipt.binding.echo_agent_dir.clone();
        let store = lease.receipt.binding.store.clone();
        let typed_store = TypedMemoryStore::new(store.clone());
        let runtime_builder = MemoryRuntimeIntegrationBuilder::new(echo_agent_dir.clone(), store);
        let change_log = runtime_builder
            .create_change_log()
            .map_err(|error| format!("Failed to open evolution change log: {error}"))?;
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
            let mut detector = SkillCandidateDetector::new(workspace_curator(&echo_agent_dir));
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
            flush_queued_triggers_locked(&mut state);
            state.active_passes = state
                .active_passes
                .checked_add(1)
                .ok_or(ReviewGenerationError::CounterExhausted("active pass"))?;
            state.current.clone()
        };
        let projection_observer = binding.projection_observer.clone();
        let lease = ReviewGenerationLease {
            receipt: Arc::new(ReviewGenerationReceipt {
                control: self.binding.clone(),
                binding,
                projection_observer,
                hot_memory_projection: self.hot_memory_projection.clone(),
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

    fn projection_test_agent() -> Result<crate::agent_handle::AgentHandle, String> {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;

        let model = Arc::new(MockLlmClient::new().with_model_name("projection-test"));
        ReactAgentBuilder::new()
            .model("projection-test")
            .llm_client(model)
            .build()
            .map(crate::agent_handle::AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    async fn projected_instruction_text(handle: &crate::agent_handle::AgentHandle) -> String {
        let context = handle.read(|agent| agent.context().clone()).await;
        context
            .lock()
            .await
            .messages()
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn inject_projection_fault(integration: &ReviewIntegration, fault: RuleProjectionFault) {
        *integration
            .rule_projection_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fault);
    }

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
        let mut descriptor = echo_agent::skills::external::SkillDocument::parse(
            "---\nname: test-skill\ndescription: test skill\n---\nbody",
        )
        .map_err(|error| error.to_string())?
        .into_descriptor();

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
        let mut descriptor = echo_agent::skills::external::SkillDocument::parse(
            "---\nname: test-skill\ndescription: test skill\n---\nbody",
        )
        .map_err(|error| error.to_string())?
        .into_descriptor();
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
        let layer_manager = ri
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
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
    async fn projection_failure_preserves_the_previous_primary_and_pool_generation()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        let echo_dir = root.join(".eko");
        std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&echo_dir).map_err(|error| error.to_string())?;
        let rules_path = echo_dir.join("learned-rules.md");
        std::fs::write(&rules_path, "old projection").map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        let integration = Arc::new(ReviewIntegration::new(
            ReviewConfig::default(),
            echo_dir,
            store,
        ));
        let primary = projection_test_agent()?;
        integration.bind_rule_projection_primary(primary.clone());
        integration
            .initialize_rule_promotions()
            .await
            .map_err(|error| error.to_string())?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false).await,
        );
        let existing = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;
        drop(existing);
        integration
            .bind_rule_projection_pool(&pool)
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(&rules_path, "new projection").map_err(|error| error.to_string())?;
        inject_projection_fault(&integration, RuleProjectionFault::Pool);
        assert!(integration.initialize_rule_promotions().await.is_err());
        assert!(
            projected_instruction_text(&primary)
                .await
                .contains("old projection")
        );
        let existing = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            projected_instruction_text(&existing.agent())
                .await
                .contains("old projection")
        );
        drop(existing);

        integration
            .initialize_rule_promotions()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            projected_instruction_text(&primary)
                .await
                .contains("new projection")
        );
        let future = pool
            .acquire("future")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            projected_instruction_text(&future.agent())
                .await
                .contains("new projection")
        );
        let primary_revision = integration
            .rule_projection_snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|snapshot| snapshot.revision().to_string())
            .ok_or_else(|| "primary projection revision missing".to_string())?;
        assert_eq!(
            pool.instruction_projection_revision_for_test().await,
            Some(primary_revision)
        );
        Ok(())
    }

    #[tokio::test]
    async fn promotion_receipt_commits_only_after_projection_retry() -> Result<(), String> {
        use echo_agent::memory::{MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        let echo_dir = root.join(".eko");
        std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;
        let store = Arc::new(echo_agent::memory::InMemoryStore::new()) as Arc<dyn Store>;
        TypedMemoryStore::new(store.clone())
            .put_typed(
                echo_agent::evolution::layer::WARM_NAMESPACE,
                "promotion-projection",
                "Run the canonical projection transaction",
                MemoryMeta::new(
                    MemoryType::ProjectFact,
                    MemorySource::ExplicitSave,
                    "projection-test",
                )
                .with_confidence(0.99),
            )
            .await
            .map_err(|error| error.to_string())?;
        let integration =
            ReviewIntegration::new(ReviewConfig::default(), echo_dir.clone(), store.clone());
        let primary = projection_test_agent()?;
        integration.bind_rule_projection_primary(primary);
        integration
            .initialize_rule_promotions()
            .await
            .map_err(|error| error.to_string())?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                projection_test_agent()?,
                None,
                None,
                2,
                false,
            )
            .await,
        );
        integration
            .bind_rule_projection_pool(&pool)
            .await
            .map_err(|error| error.to_string())?;
        let proposal = RuleProposal {
            memory_key: "promotion-projection".to_string(),
            namespace: echo_agent::evolution::layer::WARM_NAMESPACE
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            rule_text: "- **Project fact**: Run the canonical projection transaction".to_string(),
            confidence: 0.99,
            memory_type: MemoryType::ProjectFact,
            proposed_at: chrono::Utc::now(),
            reason: "projection transaction test".to_string(),
        };

        inject_projection_fault(&integration, RuleProjectionFault::Pool);
        assert!(integration.promote_rule(&proposal).await.is_err());
        let receipts_dir = echo_dir.join("evolution/rule-promotions");
        let receipt_path = std::fs::read_dir(&receipts_dir)
            .map_err(|error| error.to_string())?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .ok_or_else(|| "effects receipt was not persisted".to_string())?;
        let effects: RulePromotionReceipt = serde_json::from_slice(
            &std::fs::read(&receipt_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(effects.phase, RulePromotionPhase::EffectsApplied);

        let committed = integration
            .promote_rule(&proposal)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(committed.phase, RulePromotionPhase::Committed);

        let restarted = ReviewIntegration::new(ReviewConfig::default(), echo_dir, store);
        let restarted_primary = projection_test_agent()?;
        restarted.bind_rule_projection_primary(restarted_primary.clone());
        restarted
            .initialize_rule_promotions()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            projected_instruction_text(&restarted_primary)
                .await
                .contains("Run the canonical projection transaction")
        );
        Ok(())
    }

    #[tokio::test]
    async fn generation_manager_is_shared_and_forwards_observer_events() -> Result<(), String> {
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
        let first_lease = ri.lease_generation().map_err(|error| error.to_string())?;
        let first_manager = first_lease
            .layer_manager()
            .map_err(|error| error.to_string())?;
        first_manager
            .write_memory("first", "first write", meta())
            .await
            .map_err(|error| error.to_string())?;
        let second_lease = ri.lease_generation().map_err(|error| error.to_string())?;
        let second_manager = second_lease
            .layer_manager()
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(&first_manager, &second_manager));
        second_manager
            .write_memory("second", "second write", meta())
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(writes.load(Ordering::Relaxed), 2);
        Ok(())
    }

    #[tokio::test]
    async fn run_release_publishes_dirty_snapshot_and_unchanged_turn_is_noop() -> Result<(), String>
    {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace/.eko");
        std::fs::create_dir_all(&echo_dir).map_err(|error| error.to_string())?;
        let integration = Arc::new(ReviewIntegration::new_scoped(
            ReviewConfig::default(),
            echo_dir,
            Arc::new(InMemoryStore::new()),
            "workspace:alpha".to_string(),
            "generation-alpha".to_string(),
        ));
        integration.bind_rule_projection_primary(projection_test_agent()?);
        let lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        lease
            .layer_manager()
            .map_err(|error| error.to_string())?
            .write_memory(
                "release-fact",
                "the release path publishes this fact",
                MemoryMeta::new(
                    MemoryType::ProjectFact,
                    MemorySource::ExplicitSave,
                    "release-test",
                )
                .with_confidence(0.99)
                .with_stability(0.90),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            integration
                .hot_memory_projection_source()
                .snapshot()
                .is_none()
        );

        crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(lease))
            .await;
        let published = integration
            .hot_memory_projection_source()
            .snapshot()
            .ok_or_else(|| "run release did not publish hot memory".to_string())?;
        assert!(!published.revision().is_empty());
        assert_eq!(integration.hot_memory_projection_read_count(), 1);

        let unchanged = integration
            .lease_generation()
            .map_err(|error| error.to_string())?
            .settle_hot_memory_projection()
            .await;
        assert_eq!(unchanged.status, MemoryProjectionSettlementStatus::Settled);
        assert!(!unchanged.changed);
        assert_eq!(unchanged.authority_scope, "workspace:alpha");
        assert_eq!(unchanged.workspace_generation, "generation-alpha");
        assert_eq!(unchanged.revision, published.revision());
        assert_eq!(integration.hot_memory_projection_read_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cloned_review_owner_preserves_generation_and_original_settles_once()
    -> Result<(), String> {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new_scoped(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(InMemoryStore::new()),
            "workspace:review".to_string(),
            "generation-review".to_string(),
        );
        integration.bind_rule_projection_primary(projection_test_agent()?);
        let initial = integration.settle_hot_memory_projection().await;
        assert_eq!(initial.status, MemoryProjectionSettlementStatus::Settled);
        let review_lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        review_lease
            .layer_manager()
            .map_err(|error| error.to_string())?
            .write_memory(
                "review-hot-fact",
                "review completion settles this hot-memory generation once",
                MemoryMeta::new(
                    MemoryType::ArchitectureDecision,
                    MemorySource::ExplicitSave,
                    "review-clone-test",
                )
                .with_confidence(0.99)
                .with_stability(0.90),
            )
            .await
            .map_err(|error| error.to_string())?;
        let child = tokio::spawn(async {
            echo_agent::evolution::ReviewOutcome {
                run_id: "review-clone-run".to_string(),
                actions: vec!["capture reusable decision".to_string()],
                nothing_to_save: false,
                candidate: Some(echo_agent::evolution::ReviewCandidate {
                    kind: echo_agent::evolution::ReviewCandidateKind::ProjectFact,
                    content: "review generation remains stable".to_string(),
                    evidence: "the background owner captured this candidate".to_string(),
                    confidence: 0.95,
                    persisted: false,
                }),
                error: None,
            }
        });
        let mut pass = review_lease.clone().track_background_review(child).await?;
        let settlement = pass.settle().await?;
        assert!(settlement.evidence_candidate.is_some());
        drop(pass);
        assert_eq!(review_lease.evidence_store().list()?.len(), 1);

        let receipt = review_lease.settle_hot_memory_projection().await;
        assert_eq!(receipt.status, MemoryProjectionSettlementStatus::Settled);
        assert_eq!(receipt.authority_scope, "workspace:review");
        assert_eq!(receipt.workspace_generation, "generation-review");
        assert!(receipt.changed);
        assert_eq!(integration.hot_memory_projection_read_count(), 2);
        let unchanged = review_lease.settle_hot_memory_projection().await;
        assert!(!unchanged.changed);
        assert_eq!(integration.hot_memory_projection_read_count(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn warm_only_write_settles_without_hot_projection_read() -> Result<(), String> {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(InMemoryStore::new()),
        );
        integration.bind_rule_projection_primary(projection_test_agent()?);
        let initial = integration.settle_hot_memory_projection().await;
        assert_eq!(initial.status, MemoryProjectionSettlementStatus::Settled);
        assert_eq!(integration.hot_memory_projection_read_count(), 1);

        let generation = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        generation
            .layer_manager()
            .map_err(|error| error.to_string())?
            .write_memory(
                "warm-only",
                "this memory intentionally remains in the warm layer",
                MemoryMeta::new(
                    MemoryType::ProjectFact,
                    MemorySource::ExplicitSave,
                    "warm-only-test",
                )
                .with_confidence(0.99),
            )
            .await
            .map_err(|error| error.to_string())?;
        let settled = generation.settle_hot_memory_projection().await;
        assert_eq!(settled.status, MemoryProjectionSettlementStatus::Settled);
        assert!(!settled.changed);
        assert_eq!(settled.revision, initial.revision);
        assert_eq!(integration.hot_memory_projection_read_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn durable_write_degrades_without_primary_and_repairs_on_retry() -> Result<(), String> {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(InMemoryStore::new()),
        );
        let lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        lease
            .layer_manager()
            .map_err(|error| error.to_string())?
            .write_memory(
                "durable-before-projection",
                "durable write remains committed while projection is degraded",
                MemoryMeta::new(
                    MemoryType::ProjectFact,
                    MemorySource::ExplicitSave,
                    "degraded-repair-test",
                )
                .with_confidence(0.99)
                .with_stability(0.90),
            )
            .await
            .map_err(|error| error.to_string())?;
        let degraded = lease.settle_hot_memory_projection().await;
        assert_eq!(degraded.status, MemoryProjectionSettlementStatus::Degraded);
        assert!(
            degraded
                .pending_revision
                .as_deref()
                .is_some_and(|revision| !revision.starts_with("dirty:"))
        );
        assert!(
            integration
                .hot_memory_projection_source()
                .snapshot()
                .is_none()
        );

        integration.bind_rule_projection_primary(projection_test_agent()?);
        let repaired = lease.settle_hot_memory_projection().await;
        assert_eq!(repaired.status, MemoryProjectionSettlementStatus::Settled);
        assert!(repaired.changed);
        assert!(repaired.pending_revision.is_none());
        assert!(
            integration
                .hot_memory_projection_source()
                .snapshot()
                .is_some()
        );
        assert_eq!(integration.hot_memory_projection_read_count(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn strict_read_failure_preserves_last_good_and_owned_repair_publishes_next()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_dir = temp.path().join("workspace/.eko");
        std::fs::create_dir_all(&echo_dir).map_err(|error| error.to_string())?;
        let memory_path = echo_dir.join("MEMORY.md");
        std::fs::write(&memory_path, "last known good").map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            echo_dir,
            Arc::new(echo_agent::memory::InMemoryStore::new()),
        );
        integration.bind_rule_projection_primary(projection_test_agent()?);
        let initial = integration.settle_hot_memory_projection().await;
        assert_eq!(initial.status, MemoryProjectionSettlementStatus::Settled);
        let initial_revision = initial.revision.clone();

        std::fs::remove_file(&memory_path).map_err(|error| error.to_string())?;
        std::fs::create_dir(&memory_path).map_err(|error| error.to_string())?;
        let binding = integration.binding_snapshot();
        {
            let _gate = binding.projection_observer.projection_gate.lock().await;
            binding.projection_observer.mark_dirty();
        }
        let degraded = integration.settle_hot_memory_projection().await;
        assert_eq!(degraded.status, MemoryProjectionSettlementStatus::Degraded);
        assert_eq!(degraded.revision, initial_revision);
        assert!(
            degraded
                .error
                .as_deref()
                .is_some_and(|error| error.contains("read failed"))
        );
        assert_eq!(
            integration
                .hot_memory_projection_source()
                .snapshot()
                .map(|snapshot| snapshot.revision().to_string())
                .as_deref(),
            Some(initial_revision.as_str())
        );

        std::fs::remove_dir(&memory_path).map_err(|error| error.to_string())?;
        std::fs::write(&memory_path, "repaired projection").map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let repaired = integration
                    .hot_memory_projection_source()
                    .snapshot()
                    .is_some_and(|snapshot| snapshot.revision() != initial_revision);
                if repaired {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "owned hot-memory projection repair did not publish".to_string())?;
        integration.shutdown_background_reviews().await?;
        Ok(())
    }

    #[tokio::test]
    async fn settlement_does_not_wait_for_live_primary_agent_lock() -> Result<(), String> {
        use echo_agent::memory::{InMemoryStore, MemoryMeta, MemorySource, MemoryType};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(InMemoryStore::new()),
        );
        let primary = projection_test_agent()?;
        integration.bind_rule_projection_primary(primary.clone());
        let lease = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        lease
            .layer_manager()
            .map_err(|error| error.to_string())?
            .write_memory(
                "live-agent-write",
                "publication cannot wait for the agent that initiated this write",
                MemoryMeta::new(
                    MemoryType::ProjectFact,
                    MemorySource::ExplicitSave,
                    "active-agent-test",
                )
                .with_confidence(0.99)
                .with_stability(0.90),
            )
            .await
            .map_err(|error| error.to_string())?;
        let primary_owner = Arc::clone(primary.inner());
        let _primary_guard = primary_owner.write_owned().await;
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lease.settle_hot_memory_projection(),
        )
        .await
        .map_err(|_| "hot-memory settlement waited for live primary agent".to_string())?;
        assert_eq!(receipt.status, MemoryProjectionSettlementStatus::Settled);
        Ok(())
    }

    #[tokio::test]
    async fn receipt_reports_current_pool_lifetime_without_stale_bound_flags() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(echo_agent::memory::InMemoryStore::new()),
        );
        let primary = projection_test_agent()?;
        integration.bind_rule_projection_primary(primary.clone());
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary, None, None, 2, false).await,
        );
        integration
            .rule_projection_targets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pool = Some(Arc::downgrade(&pool));

        let bound = integration.settle_hot_memory_projection().await;
        assert!(bound.primary_bound);
        assert!(bound.pool_bound);
        assert!(bound.future_bound);
        drop(pool);

        let retired = integration.settle_hot_memory_projection().await;
        assert!(retired.primary_bound);
        assert!(!retired.pool_bound);
        assert!(!retired.future_bound);
        assert_eq!(integration.hot_memory_projection_read_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn retired_generation_cannot_publish_or_report_stale_targets() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let integration = ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("workspace/.eko"),
            Arc::new(echo_agent::memory::InMemoryStore::new()),
        );
        integration.bind_rule_projection_primary(projection_test_agent()?);
        let stale = integration.binding_snapshot();
        stale.projection_observer.mark_dirty();
        integration.shutdown_background_reviews().await?;

        let rejected =
            settle_hot_memory_projection_for_binding(&stale, &integration.hot_memory_projection)
                .await;
        assert_eq!(rejected.status, MemoryProjectionSettlementStatus::Degraded);
        assert!(
            rejected
                .error
                .as_deref()
                .is_some_and(|error| error.contains("stale memory generation"))
        );
        assert!(!rejected.primary_bound);
        assert!(!rejected.pool_bound);
        assert!(!rejected.future_bound);
        assert!(
            integration
                .hot_memory_projection_source()
                .snapshot()
                .is_none()
        );
        assert_eq!(integration.hot_memory_projection_read_count(), 0);
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

        let next = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        assert_eq!(next.echo_agent_dir(), temp.path().join("workspace-a/.eko"));
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

        drop_release_sender
            .send(())
            .map_err(|error| format!("failed to release background review child: {error}"))?;
        integration.shutdown_background_reviews().await?;
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
}
