//! Product-layer evolution integration.
//!
//! Bridges analysis-only framework reviewers into EKO's workspace-scoped
//! Review Inbox. Semantic mutations require an explicit inbox action; scheduled
//! deterministic maintenance remains owned by Dreaming.

pub mod dashboard;
pub mod evidence;
pub mod hook_fire;
pub mod review_integration;
pub mod rule_promoter;

pub use dashboard::{ActivityEntry, Dashboard, DashboardMetrics, MemoryStats, ToolDiagnostics};
pub use evidence::{
    EvidenceAction, EvidenceCandidate, EvidenceCandidateDraft, EvidenceCandidateStatus,
    EvidenceInteractionAction, EvidenceInteractionEvent, EvidenceInteractionFailureKind,
    EvidenceKind, EvidenceRef, EvidenceReviewFilter, EvidenceReviewItem, EvidenceScope,
    EvidenceSource, EvidenceStore, EvidenceTarget, capture_memory_conflict, capture_review_outcome,
};
pub use hook_fire::{evolution_hook_observer, fire_evolution_hook};
pub use review_integration::{
    BackgroundReviewPass, BackgroundReviewSettlement, MemoryProjectionSettlementReceipt,
    MemoryProjectionSettlementStatus, ReviewGenerationError, ReviewGenerationLease,
    ReviewIntegration, TriggerDeliveryStatus, discover_echo_agent_dir, format_review_report,
    workspace_curator,
};
pub use rule_promoter::{
    PromotionCriteria, RulePromoter, RulePromotionError, RulePromotionPhase, RulePromotionReceipt,
    RuleProposal,
};
