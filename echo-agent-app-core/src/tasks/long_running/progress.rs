//! Real-time progress reporting for long-running tasks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tokio::sync::watch;

use super::phases::PhasePlan;

/// Real-time progress information for a long-running task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProgress {
    pub task_id: String,
    /// Overall percentage (0-100)
    pub percentage: f64,
    /// Current phase name
    pub current_phase: String,
    /// Current phase index
    pub phase_index: usize,
    /// Total phases
    pub total_phases: usize,
    /// Phase-internal message (e.g., "Searching papers: 12/20 found")
    pub phase_message: Option<String>,
    /// Estimated time remaining in seconds
    pub eta_secs: Option<u64>,
    /// Timestamp of this update
    pub updated_at: DateTime<Utc>,
}

/// Broadcasts progress updates to subscribers.
pub struct ProgressReporter {
    sender: watch::Sender<TaskProgress>,
    receiver: watch::Receiver<TaskProgress>,
    plan: PhasePlan,
    task_start: Instant,
    #[allow(dead_code)]
    phase_start: Instant,
}

impl ProgressReporter {
    pub fn new(task_id: String, plan: PhasePlan) -> Self {
        let initial = TaskProgress {
            task_id: task_id.clone(),
            percentage: 0.0,
            current_phase: plan.phase_name(0).to_string(),
            phase_index: 0,
            total_phases: plan.phases.len(),
            phase_message: None,
            eta_secs: None,
            updated_at: Utc::now(),
        };
        let (sender, receiver) = watch::channel(initial);
        let now = Instant::now();
        Self {
            sender,
            receiver,
            plan,
            task_start: now,
            phase_start: now,
        }
    }

    /// Called when entering a new phase.
    pub fn enter_phase(&mut self, phase_idx: usize, message: Option<String>) {
        self.phase_start = Instant::now();
        let pct = self.plan.progress_pct(phase_idx, 0.0);
        let task_id = self.sender.borrow().task_id.clone();
        let _ = self.sender.send(TaskProgress {
            task_id,
            percentage: pct,
            current_phase: self.plan.phase_name(phase_idx).to_string(),
            phase_index: phase_idx,
            total_phases: self.plan.phases.len(),
            phase_message: message,
            eta_secs: self.calculate_eta(pct),
            updated_at: Utc::now(),
        });
    }

    /// Called for intra-phase progress updates.
    pub fn update_phase_progress(&self, phase_pct: f64, message: Option<String>) {
        let current = self.sender.borrow();
        let pct = self.plan.progress_pct(current.phase_index, phase_pct);
        let task_id = current.task_id.clone();
        let current_phase = current.current_phase.clone();
        let phase_index = current.phase_index;
        let total_phases = current.total_phases;
        drop(current);
        let _ = self.sender.send(TaskProgress {
            task_id,
            percentage: pct,
            current_phase,
            phase_index,
            total_phases,
            phase_message: message,
            eta_secs: self.calculate_eta(pct),
            updated_at: Utc::now(),
        });
    }

    /// Subscribe to progress updates (for SSE/WebSocket streaming).
    pub fn subscribe(&self) -> watch::Receiver<TaskProgress> {
        self.receiver.clone()
    }

    /// Get current progress snapshot.
    pub fn current(&self) -> TaskProgress {
        self.sender.borrow().clone()
    }

    /// Calculate ETA based on elapsed time and progress percentage.
    fn calculate_eta(&self, pct: f64) -> Option<u64> {
        if pct <= 0.0 {
            return None;
        }
        let elapsed = self.task_start.elapsed().as_secs();
        let total_estimated = (elapsed as f64 / pct * 100.0) as u64;
        Some(total_estimated.saturating_sub(elapsed))
    }
}
