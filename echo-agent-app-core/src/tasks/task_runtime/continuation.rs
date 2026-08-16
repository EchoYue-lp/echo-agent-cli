//! EKO long-horizon continuation control plane.
//!
//! A finite RunTurn owns one TaskRuntime driver. This coordinator stays outside
//! that driver, waits for exact driver release, and requests the next turn only
//! when the event-folded TaskRun projection remains eligible. It does not own a
//! second task graph, executor, or completion state machine.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use echo_agent::agent::{AgentHandle, CancellationToken};

use crate::chat_resources::ChatResources;
use crate::prepared_turn::PreparedUserTurn;

use super::store::TaskRuntimeStore;
use super::types::{InteractionMode, RunTurnBinding, RunTurnOrigin, TaskRunStatus, TurnVisibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueRequestOutcome {
    Started,
    AlreadyRunning,
    MissingLauncher,
}

#[derive(Clone)]
struct ContinuationLauncher {
    fallback_agent: AgentHandle,
    resources: Arc<ChatResources>,
    root_message_id: String,
}

#[derive(Clone)]
struct RegisteredLauncher {
    generation: u64,
    launcher: ContinuationLauncher,
}

#[derive(Default)]
struct ContinuationState {
    launchers: HashMap<String, RegisteredLauncher>,
    active: HashMap<String, u64>,
    pending_wakeups: HashSet<String>,
    next_generation: u64,
}

pub(crate) struct TaskContinuationRuntime {
    store: Weak<TaskRuntimeStore>,
    state: Mutex<ContinuationState>,
}

impl TaskContinuationRuntime {
    fn new(store: Weak<TaskRuntimeStore>) -> Self {
        Self {
            store,
            state: Mutex::new(ContinuationState::default()),
        }
    }

    fn register_launcher(
        &self,
        run_id: &str,
        fallback_agent: AgentHandle,
        resources: Arc<ChatResources>,
        root_message_id: String,
    ) {
        // A launcher must not retain a workspace generation or the foreground
        // cancellation token between finite turns. Each turn reacquires both.
        let retained_sink = resources
            .sink
            .continuation_sink()
            .unwrap_or_else(|| resources.sink.clone());
        let retained = Arc::new(ChatResources {
            pool: resources.pool.clone(),
            store: None,
            sink: retained_sink,
            webhook_emitter: resources.webhook_emitter.clone(),
            conv_id: resources.conv_id.clone(),
            root_message_id: String::new(),
            attachments: resources.attachments.clone(),
            cancel: CancellationToken::new(),
            interaction_mode: InteractionMode::Task,
            review_integration: resources.review_integration.clone(),
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: resources.human_loop_provider.clone(),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_generation = state.next_generation.saturating_add(1).max(1);
        let generation = state.next_generation;
        state.launchers.insert(
            run_id.to_string(),
            RegisteredLauncher {
                generation,
                launcher: ContinuationLauncher {
                    fallback_agent,
                    resources: retained,
                    root_message_id,
                },
            },
        );
    }

    fn request(self: &Arc<Self>, run_id: &str, origin: RunTurnOrigin) -> ContinueRequestOutcome {
        let generation = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(generation) = state.launchers.get(run_id).map(|entry| entry.generation) else {
                return ContinueRequestOutcome::MissingLauncher;
            };
            if state.active.contains_key(run_id) {
                return ContinueRequestOutcome::AlreadyRunning;
            }
            state.active.insert(run_id.to_string(), generation);
            generation
        };
        let runtime = Arc::clone(self);
        let owned_run_id = run_id.to_string();
        tokio::spawn(async move {
            runtime
                .drive_until_deferred(owned_run_id, origin, generation)
                .await;
        });
        ContinueRequestOutcome::Started
    }

    async fn drive_until_deferred(
        self: Arc<Self>,
        run_id: String,
        mut origin: RunTurnOrigin,
        dispatch_generation: u64,
    ) {
        let Some(store) = self.store.upgrade() else {
            self.finish_dispatch(&run_id, dispatch_generation, true);
            return;
        };
        let mut consecutive_failures = 0_u8;
        loop {
            if !store.is_run_driver_admission_open() {
                self.finish_dispatch(&run_id, dispatch_generation, true);
                return;
            }
            store.wait_for_run_driver_idle(&run_id).await;
            if !store.is_run_driver_admission_open() {
                self.finish_dispatch(&run_id, dispatch_generation, true);
                return;
            }
            match continuation_eligibility(&store, &run_id) {
                ContinuationEligibility::Ready => {}
                ContinuationEligibility::Deferred => {
                    self.finish_dispatch(&run_id, dispatch_generation, false);
                    return;
                }
                ContinuationEligibility::Stop => {
                    self.finish_dispatch(&run_id, dispatch_generation, true);
                    return;
                }
            }
            let Some(launcher) = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .launchers
                .get(&run_id)
                .map(|entry| entry.launcher.clone())
            else {
                self.finish_dispatch(&run_id, dispatch_generation, false);
                return;
            };

            let turn_id = uuid::Uuid::new_v4().to_string();
            let binding = RunTurnBinding {
                run_id: Some(run_id.clone()),
                turn_id: turn_id.clone(),
                root_message_id: launcher.root_message_id.clone(),
                origin,
                transcript_visibility: TurnVisibility::Internal,
            };
            let resources = Arc::new(ChatResources {
                pool: launcher.resources.pool.clone(),
                store: Some(Arc::clone(&store)),
                sink: launcher.resources.sink.clone(),
                webhook_emitter: launcher.resources.webhook_emitter.clone(),
                conv_id: launcher.resources.conv_id.clone(),
                root_message_id: turn_id,
                attachments: launcher.resources.attachments.clone(),
                cancel: CancellationToken::new(),
                interaction_mode: InteractionMode::Task,
                review_integration: launcher.resources.review_integration.clone(),
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: launcher.resources.human_loop_provider.clone(),
            });
            let turn = PreparedUserTurn::runtime_instruction(format!(
                "Continue the existing TaskRun {run_id} toward its unchanged Goal. Reload the authoritative TaskRuntime projection, execute the next useful work, and use task_execute for the current revision when ready. This is internal continuation context, not a new user request."
            ));
            let human_loop_provider = resources.human_loop_provider.clone();
            let result = if let Some(pool) = resources.pool.clone() {
                crate::chat_driver::drive_pooled_chat_turn(
                    pool,
                    &format!("__continuation__:{run_id}"),
                    move |agent| async move {
                        if let Some(provider) = human_loop_provider {
                            agent
                                .write_async(|agent| {
                                    Box::pin(async move {
                                        agent
                                            .set_human_loop_provider_preserving_approvals(provider);
                                    })
                                })
                                .await;
                        }
                        Ok(())
                    },
                    &turn,
                    resources,
                    binding,
                )
                .await
            } else {
                if let Some(provider) = human_loop_provider {
                    launcher
                        .fallback_agent
                        .write_async(|agent| {
                            Box::pin(async move {
                                agent.set_human_loop_provider_preserving_approvals(provider);
                            })
                        })
                        .await;
                }
                crate::chat_driver::drive_chat_turn(
                    &launcher.fallback_agent,
                    &turn,
                    resources,
                    Some(binding),
                )
                .await
            };
            if let Err(error) = result {
                tracing::warn!(run_id, %error, "long-horizon continuation turn failed");
                if !store.is_run_driver_admission_open() {
                    self.finish_dispatch(&run_id, dispatch_generation, true);
                    return;
                }
                consecutive_failures = consecutive_failures.saturating_add(1);
                if consecutive_failures >= 3 {
                    if store
                        .get_run(&run_id)
                        .ok()
                        .flatten()
                        .is_some_and(|run| run.status == TaskRunStatus::Running)
                    {
                        let _paused = store.request_pause_with_reason(
                            &run_id,
                            super::types::RunPauseReason::ProviderUnavailable,
                            Some("three consecutive continuation turn admissions failed"),
                        );
                    }
                    self.finish_dispatch(&run_id, dispatch_generation, true);
                    return;
                }
                let backoff_millis = 250_u64.saturating_mul(1_u64 << consecutive_failures);
                tokio::time::sleep(std::time::Duration::from_millis(backoff_millis.min(2_000)))
                    .await;
            } else {
                consecutive_failures = 0;
            }
            origin = RunTurnOrigin::Continuation;
            tokio::task::yield_now().await;
        }
    }

    fn finish_dispatch(
        self: &Arc<Self>,
        run_id: &str,
        dispatch_generation: u64,
        remove_launcher: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let should_restart =
            settle_dispatch_state(&mut state, run_id, dispatch_generation, remove_launcher);
        drop(state);
        if should_restart {
            let outcome = self.request(run_id, RunTurnOrigin::Continuation);
            tracing::debug!(
                run_id,
                ?outcome,
                "new launcher or pending wake superseded a settling continuation"
            );
        }
    }

    fn wake(self: &Arc<Self>, run_id: &str) {
        let request_now = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.active.contains_key(run_id) {
                state.pending_wakeups.insert(run_id.to_string());
                false
            } else {
                true
            }
        };
        if request_now {
            let outcome = self.request(run_id, RunTurnOrigin::Continuation);
            tracing::debug!(run_id, ?outcome, "continuation wake requested");
        }
    }

    fn clear_launcher(&self, run_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.launchers.remove(run_id);
        state.pending_wakeups.remove(run_id);
    }

    fn clear_all_launchers(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.launchers.clear();
        state.pending_wakeups.clear();
    }
}

fn settle_dispatch_state(
    state: &mut ContinuationState,
    run_id: &str,
    dispatch_generation: u64,
    remove_launcher: bool,
) -> bool {
    if state.active.get(run_id).copied() == Some(dispatch_generation) {
        state.active.remove(run_id);
    }
    let pending_wakeup = state.pending_wakeups.remove(run_id);
    let has_newer_launcher = state
        .launchers
        .get(run_id)
        .is_some_and(|entry| entry.generation > dispatch_generation);
    if remove_launcher && !has_newer_launcher {
        state.launchers.remove(run_id);
    }
    has_newer_launcher || (pending_wakeup && !remove_launcher)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationEligibility {
    Ready,
    Deferred,
    Stop,
}

fn continuation_eligibility(store: &TaskRuntimeStore, run_id: &str) -> ContinuationEligibility {
    let Ok(Some(snapshot)) = store.get_run_state(run_id) else {
        return ContinuationEligibility::Stop;
    };
    if snapshot.run.status != TaskRunStatus::Running {
        return ContinuationEligibility::Stop;
    }
    let Some(continuation) = snapshot.continuation else {
        return ContinuationEligibility::Stop;
    };
    if !continuation.enabled || continuation.active_turn.is_some() {
        return ContinuationEligibility::Stop;
    }
    if continuation.deferred {
        return ContinuationEligibility::Deferred;
    }
    if continuation
        .token_budget
        .is_some_and(|budget| continuation.tokens_used >= budget)
        || continuation
            .time_budget_seconds
            .is_some_and(|budget| continuation.time_used_seconds >= budget)
    {
        return ContinuationEligibility::Stop;
    }
    ContinuationEligibility::Ready
}

fn runtime_for(store: &Arc<TaskRuntimeStore>) -> Arc<TaskContinuationRuntime> {
    store
        .continuation_runtime
        .get_or_init(|| Arc::new(TaskContinuationRuntime::new(Arc::downgrade(store))))
        .clone()
}

pub(crate) fn register_launcher(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    fallback_agent: AgentHandle,
    resources: Arc<ChatResources>,
    root_message_id: String,
) {
    runtime_for(store).register_launcher(run_id, fallback_agent, resources, root_message_id);
}

pub(crate) fn request_continue(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    origin: RunTurnOrigin,
) -> ContinueRequestOutcome {
    runtime_for(store).request(run_id, origin)
}

pub(crate) fn clear_launcher(store: &TaskRuntimeStore, run_id: &str) {
    if let Some(runtime) = store.continuation_runtime.get() {
        runtime.clear_launcher(run_id);
    }
}

pub(crate) fn shutdown(store: &TaskRuntimeStore) {
    if let Some(runtime) = store.continuation_runtime.get() {
        runtime.clear_all_launchers();
    }
}

/// A completed cell is a concrete wake-up, not a reason to spin model turns.
/// Only a Running run that explicitly deferred for cells is resumed here;
/// user-paused and recovery-paused runs remain under user control.
pub(crate) fn wake_after_cell_terminal(store: &Arc<TaskRuntimeStore>, run_id: &str) {
    let Ok(cells) = store.list_background_cells(run_id) else {
        return;
    };
    if cells
        .iter()
        .any(super::types::BackgroundCellState::is_active)
    {
        return;
    }
    let is_deferred = store
        .get_run_state(run_id)
        .ok()
        .flatten()
        .is_some_and(|snapshot| {
            snapshot.run.status == TaskRunStatus::Running
                && snapshot
                    .continuation
                    .is_some_and(|continuation| continuation.enabled && continuation.deferred)
        });
    if !is_deferred {
        return;
    }
    if let Err(error) = store.set_continuation_deferred(run_id, false) {
        tracing::warn!(run_id, %error, "failed to clear cell continuation deferral");
        return;
    }
    runtime_for(store).wake(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_cell_wakeup_requeues_after_deferred_dispatch_exit() {
        let mut state = ContinuationState::default();
        state.active.insert("run".to_string(), 7);
        state.pending_wakeups.insert("run".to_string());

        assert!(settle_dispatch_state(&mut state, "run", 7, false));
        assert!(!state.active.contains_key("run"));
        assert!(!state.pending_wakeups.contains("run"));
    }

    #[test]
    fn terminal_dispatch_ignores_stale_pending_wakeup() {
        let mut state = ContinuationState::default();
        state.active.insert("run".to_string(), 9);
        state.pending_wakeups.insert("run".to_string());

        assert!(!settle_dispatch_state(&mut state, "run", 9, true));
        assert!(!state.active.contains_key("run"));
        assert!(!state.pending_wakeups.contains("run"));
    }
}
