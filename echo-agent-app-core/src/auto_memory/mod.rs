//! Auto-memory product integration.
//!
//! Coding-oriented observation extraction and inbox routing are EKO product
//! policy. The framework owns only generic memory contracts and stores.

mod policy;

use crate::evolution::{
    EvidenceCandidate, EvidenceCandidateDraft, EvidenceKind, EvidenceRef, EvidenceSource,
    EvidenceStore,
};
pub use policy::{
    AutoMemoryConfig, Observation, ObservationCategory, extract_observations,
    format_observations_for_memory,
};

/// Queue inferred observations in the unified workspace inbox.
pub fn queue_observations(
    store: &EvidenceStore,
    observations: &[Observation],
    messages: &[(String, String)],
) -> Result<Vec<EvidenceCandidate>, String> {
    let mut candidates = Vec::new();
    for observation in observations {
        let source = observation.source_turn.and_then(|turn| {
            messages
                .get(turn)
                .map(|(role, content)| (turn, role, content))
        });
        let (source_turn, source_role, quote) = match source {
            Some((turn, role, content)) => (Some(turn), Some(role.clone()), content.clone()),
            None => (observation.source_turn, None, observation.text.clone()),
        };
        let kind = match observation.category {
            ObservationCategory::User => EvidenceKind::UserPreference,
            ObservationCategory::Bug => EvidenceKind::DebuggingLesson,
            ObservationCategory::Decision | ObservationCategory::Project => {
                EvidenceKind::ProjectFact
            }
            ObservationCategory::FilePath => EvidenceKind::ProjectFact,
        };
        candidates.push(
            store.upsert(EvidenceCandidateDraft {
                kind,
                scope: matches!(kind, EvidenceKind::UserPreference)
                    .then(|| crate::evolution::EvidenceScope::User("local-user".to_string())),
                content: observation.text.clone(),
                evidence: vec![EvidenceRef {
                    source: EvidenceSource::AutoMemory,
                    source_run_id: None,
                    source_role,
                    source_turn,
                    source_memory_key: None,
                    quote,
                }],
                action: None,
                confidence: observation.confidence as f32,
            })?,
        );
    }
    Ok(candidates)
}
