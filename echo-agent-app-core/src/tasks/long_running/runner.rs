//! Long-running task runner -- executes pipelines with checkpoint/resume/progress/cancellation.

use super::checkpoint::{LongRunningCheckpoint, LongRunningCheckpointStore};
use super::human_gate::HumanCheckpointRequest;
use super::phases::PhasePlan;
use super::progress::ProgressReporter;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Outcome of running a phase.
pub enum PhaseOutcome {
    /// Phase completed successfully with output data.
    Completed(Value),
    /// Phase needs human input -- pipeline will pause.
    NeedsHumanInput(HumanCheckpointRequest),
    /// Phase was cancelled.
    Cancelled,
    /// Phase failed with an error message.
    Failed(String),
}

/// Context passed to each phase executor.
pub struct PhaseContext {
    pub task_id: String,
    pub phase_id: String,
    /// Outputs from all previously completed phases.
    pub previous_outputs: HashMap<String, Value>,
    /// Phase-internal mutable state (checkpointed).
    pub phase_state: HashMap<String, Value>,
    /// Cancellation token for this phase.
    pub cancel: CancellationToken,
    /// Progress reporter for intra-phase updates.
    pub progress: Arc<ProgressReporter>,
}

/// A phase executor function.
pub type PhaseExecutor =
    Box<dyn Fn(PhaseContext) -> BoxFuture<'static, PhaseOutcome> + Send + Sync>;

/// Executes a long-running pipeline with full lifecycle support.
pub struct LongRunningTaskRunner {
    task_id: String,
    plan: PhasePlan,
    checkpoint_store: LongRunningCheckpointStore,
    progress: ProgressReporter,
    cancel: CancellationToken,
    human_loop_provider: Option<Arc<dyn HumanLoopProvider>>,
}

impl LongRunningTaskRunner {
    pub fn new(
        task_id: String,
        plan: PhasePlan,
        checkpoint_store: LongRunningCheckpointStore,
        cancel: CancellationToken,
    ) -> Self {
        let progress = ProgressReporter::new(task_id.clone(), plan.clone());
        Self {
            task_id,
            plan,
            checkpoint_store,
            progress,
            cancel,
            human_loop_provider: None,
        }
    }

    /// Set the human loop provider for interactive pipelines.
    pub fn with_human_loop_provider(mut self, provider: Arc<dyn HumanLoopProvider>) -> Self {
        self.human_loop_provider = Some(provider);
        self
    }

    /// Get a subscriber for progress updates.
    pub fn progress_receiver(&self) -> tokio::sync::watch::Receiver<super::progress::TaskProgress> {
        self.progress.subscribe()
    }

    /// Run the pipeline from the given checkpoint (or from scratch if None).
    pub async fn run(
        mut self,
        phase_executors: Vec<PhaseExecutor>,
        resume_from: Option<LongRunningCheckpoint>,
    ) -> Result<HashMap<String, Value>, anyhow::Error> {
        let start_idx = resume_from
            .as_ref()
            .map(|cp| cp.last_completed_phase + 1)
            .unwrap_or(0);

        let mut phase_outputs = resume_from
            .as_ref()
            .map(|cp| cp.phase_outputs.clone())
            .unwrap_or_default();
        let mut phase_state = resume_from
            .as_ref()
            .map(|cp| cp.phase_state.clone())
            .unwrap_or_default();

        let resume_count = resume_from.as_ref().map(|cp| cp.resume_count).unwrap_or(0);

        if start_idx > 0 {
            tracing::info!(
                "Resuming task {} from phase {} (resume count: {})",
                self.task_id,
                start_idx,
                resume_count
            );
        }

        for (idx, executor) in phase_executors.into_iter().enumerate().skip(start_idx) {
            if self.cancel.is_cancelled() {
                return Err(anyhow::anyhow!("Task cancelled before phase {}", idx));
            }

            let phase = &self.plan.phases[idx];
            self.progress
                .enter_phase(idx, Some(format!("Starting: {}", phase.name)));

            tracing::info!(
                "Task {} entering phase {}: {} ({}/{})",
                self.task_id,
                idx,
                phase.name,
                idx + 1,
                self.plan.phases.len()
            );

            let ctx = PhaseContext {
                task_id: self.task_id.clone(),
                phase_id: phase.id.clone(),
                previous_outputs: phase_outputs.clone(),
                phase_state: phase_state.clone(),
                cancel: self.cancel.child_token(),
                progress: Arc::new(ProgressReporter::new(
                    self.task_id.clone(),
                    self.plan.clone(),
                )),
            };

            // Apply phase timeout if configured
            let outcome = if phase.timeout_secs > 0 {
                let timeout = std::time::Duration::from_secs(phase.timeout_secs);
                match tokio::time::timeout(timeout, executor(ctx)).await {
                    Ok(outcome) => outcome,
                    Err(_) => PhaseOutcome::Failed(format!(
                        "Phase '{}' timed out after {} seconds",
                        phase.name, phase.timeout_secs
                    )),
                }
            } else {
                executor(ctx).await
            };

            match outcome {
                PhaseOutcome::Completed(output) => {
                    phase_outputs.insert(phase.id.clone(), output);
                    tracing::info!(
                        "Task {} phase {} ({}) completed",
                        self.task_id,
                        idx,
                        phase.name
                    );

                    // Save checkpoint after each successful phase
                    self.checkpoint_store
                        .save(&LongRunningCheckpoint {
                            task_id: self.task_id.clone(),
                            last_completed_phase: idx,
                            phase_outputs: phase_outputs.clone(),
                            created_at: chrono::Utc::now(),
                            phase_state: phase_state.clone(),
                            resume_count,
                        })
                        .await?;

                    // Check for human checkpoint
                    if phase.human_checkpoint {
                        if let Some(ref provider) = self.human_loop_provider {
                            let request = HumanLoopRequest::selection(
                                &self.task_id,
                                format!(
                                    "Phase '{}' completed. Review and approve to continue.",
                                    phase.name
                                ),
                                vec![
                                    "approve".to_string(),
                                    "revise".to_string(),
                                    "cancel".to_string(),
                                ],
                            )
                            .with_context(
                                phase_outputs.get(&phase.id).cloned().unwrap_or(Value::Null),
                            )
                            .with_phase(&phase.id);

                            let response = tokio::select! {
                                result = provider.request(request) => result?,
                                _ = self.cancel.cancelled() => {
                                    return Err(anyhow::anyhow!("Task cancelled by user at checkpoint"));
                                }
                            };

                            match response {
                                HumanLoopResponse::Selection {
                                    selection,
                                    instructions,
                                } => {
                                    if selection == "cancel" {
                                        return Err(anyhow::anyhow!(
                                            "Task cancelled by user at checkpoint"
                                        ));
                                    }
                                    if let Some(inst) = instructions {
                                        phase_state.insert(
                                            format!("{}_human_feedback", phase.id),
                                            Value::String(inst),
                                        );
                                    }
                                }
                                _ => {
                                    return Err(anyhow::anyhow!(
                                        "Unexpected response type for checkpoint"
                                    ));
                                }
                            }
                        }
                    }
                }
                PhaseOutcome::NeedsHumanInput(request) => {
                    if let Some(ref provider) = self.human_loop_provider {
                        let response = tokio::select! {
                            result = provider.request(request) => result?,
                            _ = self.cancel.cancelled() => {
                                return Err(anyhow::anyhow!("Task cancelled by user"));
                            }
                        };

                        match response {
                            HumanLoopResponse::Selection {
                                selection,
                                instructions,
                            } => {
                                if selection == "cancel" {
                                    return Err(anyhow::anyhow!("Task cancelled by user"));
                                }
                                if let Some(inst) = instructions {
                                    phase_state.insert(
                                        format!("{}_human_feedback", phase.id),
                                        Value::String(inst),
                                    );
                                }
                            }
                            _ => {
                                return Err(anyhow::anyhow!(
                                    "Unexpected response type for checkpoint"
                                ));
                            }
                        }

                        // Save checkpoint with human feedback
                        self.checkpoint_store
                            .save(&LongRunningCheckpoint {
                                task_id: self.task_id.clone(),
                                last_completed_phase: idx,
                                phase_outputs: phase_outputs.clone(),
                                created_at: chrono::Utc::now(),
                                phase_state: phase_state.clone(),
                                resume_count,
                            })
                            .await?;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Phase '{}' requires human input but no HumanLoopProvider configured",
                            phase.name
                        ));
                    }
                }
                PhaseOutcome::Cancelled => {
                    tracing::info!(
                        "Task {} cancelled during phase {}",
                        self.task_id,
                        phase.name
                    );
                    return Err(anyhow::anyhow!("Phase '{}' cancelled", phase.name));
                }
                PhaseOutcome::Failed(err) => {
                    tracing::warn!(
                        "Task {} phase {} failed: {}. Retries: {}/{}",
                        self.task_id,
                        phase.name,
                        err,
                        0,
                        phase.max_retries
                    );
                    return Err(anyhow::anyhow!("Phase '{}' failed: {}", phase.name, err));
                }
            }
        }

        // All phases done -- clean up checkpoint
        let _ = self.checkpoint_store.delete(&self.task_id).await;
        tracing::info!(
            "Task {} completed all {} phases",
            self.task_id,
            self.plan.phases.len()
        );

        Ok(phase_outputs)
    }
}
