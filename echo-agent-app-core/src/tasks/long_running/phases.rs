//! Pipeline phase definitions and progress calculation.

use serde::{Deserialize, Serialize};

/// A single phase in a long-running pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePhase {
    /// Unique phase identifier (e.g., "research_search", "writing_draft")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Weight for progress calculation (relative to total)
    pub weight: f64,
    /// Whether this phase requires human approval before proceeding
    pub human_checkpoint: bool,
    /// Maximum retries for this phase
    pub max_retries: u32,
    /// Timeout in seconds (0 = no timeout)
    pub timeout_secs: u64,
}

/// Ordered list of phases with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhasePlan {
    pub phases: Vec<PipelinePhase>,
    /// Total weight (sum of all phase weights, cached)
    pub total_weight: f64,
}

impl PhasePlan {
    pub fn new(phases: Vec<PipelinePhase>) -> Self {
        let total_weight = phases.iter().map(|p| p.weight).sum();
        Self {
            phases,
            total_weight,
        }
    }

    /// Get overall progress percentage given current phase index and phase-internal progress.
    pub fn progress_pct(&self, current_phase_idx: usize, phase_internal_pct: f64) -> f64 {
        if self.total_weight <= 0.0 || self.phases.is_empty() {
            return 0.0;
        }
        let completed_weight: f64 = self.phases[..current_phase_idx.min(self.phases.len())]
            .iter()
            .map(|p| p.weight)
            .sum();
        let current_weight = self
            .phases
            .get(current_phase_idx)
            .map(|p| p.weight * phase_internal_pct.clamp(0.0, 1.0))
            .unwrap_or(0.0);
        ((completed_weight + current_weight) / self.total_weight * 100.0).min(100.0)
    }

    /// Get the phase name for display.
    pub fn phase_name(&self, idx: usize) -> &str {
        self.phases
            .get(idx)
            .map(|p| p.name.as_str())
            .unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plan() -> PhasePlan {
        PhasePlan::new(vec![
            PipelinePhase {
                id: "init".into(),
                name: "Initialize".into(),
                weight: 1.0,
                human_checkpoint: false,
                max_retries: 0,
                timeout_secs: 30,
            },
            PipelinePhase {
                id: "outline".into(),
                name: "Generate Outline".into(),
                weight: 3.0,
                human_checkpoint: true,
                max_retries: 2,
                timeout_secs: 300,
            },
            PipelinePhase {
                id: "draft".into(),
                name: "Write Draft".into(),
                weight: 5.0,
                human_checkpoint: false,
                max_retries: 2,
                timeout_secs: 600,
            },
            PipelinePhase {
                id: "finalize".into(),
                name: "Finalize".into(),
                weight: 1.0,
                human_checkpoint: true,
                max_retries: 0,
                timeout_secs: 120,
            },
        ])
    }

    #[test]
    fn test_progress_at_start() {
        let plan = test_plan();
        let pct = plan.progress_pct(0, 0.0);
        assert!((pct - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_progress_mid_phase_0() {
        let plan = test_plan();
        let pct = plan.progress_pct(0, 0.5);
        // Phase 0 weight=1.0, total=10.0, 50% of phase 0 = 0.5/10 = 5%
        assert!((pct - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_progress_after_phase_0() {
        let plan = test_plan();
        let pct = plan.progress_pct(1, 0.0);
        // Phase 0 complete (weight 1.0), total=10.0 -> 10%
        assert!((pct - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_progress_mid_phase_1() {
        let plan = test_plan();
        let pct = plan.progress_pct(1, 0.5);
        // Phase 0 complete (1.0) + 50% of phase 1 (1.5) = 2.5/10 = 25%
        assert!((pct - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_progress_all_complete() {
        let plan = test_plan();
        let pct = plan.progress_pct(4, 0.0);
        // All phases complete
        assert!((pct - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_phase_name() {
        let plan = test_plan();
        assert_eq!(plan.phase_name(0), "Initialize");
        assert_eq!(plan.phase_name(2), "Write Draft");
        assert_eq!(plan.phase_name(99), "unknown");
    }
}
