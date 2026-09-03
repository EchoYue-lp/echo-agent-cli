//! EKO long-horizon continuation control plane.
//!
//! A finite RunTurn owns one TaskRuntime driver. This coordinator stays outside
//! that driver, waits for exact driver release, and requests the next turn only
//! when the event-folded TaskRun projection remains eligible. It does not own a
//! second task graph, executor, or completion state machine.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use echo_agent::agent::{AgentHandle, CancellationToken};
use echo_agent::runtime::TurnReceipt;
use futures::FutureExt;

use crate::chat_resources::ChatResources;
use crate::prepared_turn::PreparedUserTurn;

use super::store::TaskRuntimeStore;
use super::types::{RunTurnBinding, RunTurnOrigin, TaskRunStatus, TurnVisibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueRequestDisposition {
    Started,
    Joined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationCompletionReason {
    Deferred,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinuationCompletion {
    pub(crate) terminal: crate::chat_driver::TurnOutcome,
    pub(crate) reason: ContinuationCompletionReason,
}

#[derive(Debug)]
pub(crate) struct ContinuationCompletionWaiter {
    completion_rx: tokio::sync::watch::Receiver<Option<ContinuationCompletion>>,
}

impl ContinuationCompletionWaiter {
    pub(crate) async fn wait(mut self) -> Result<ContinuationCompletion, String> {
        loop {
            if let Some(completion) = self.completion_rx.borrow().clone() {
                return Ok(completion);
            }
            self.completion_rx.changed().await.map_err(|_| {
                "continuation owner ended without publishing completion".to_string()
            })?;
        }
    }
}

#[derive(Debug)]
pub(crate) struct ContinueRequest {
    pub(crate) disposition: ContinueRequestDisposition,
    pub(crate) completion: ContinuationCompletionWaiter,
}

#[derive(Debug)]
pub(crate) enum ContinueRequestOutcome {
    Running(ContinueRequest),
    MissingLauncher,
}

#[derive(Clone)]
struct ContinuationLauncher {
    fallback_agent: AgentHandle,
    resources: Arc<ChatResources>,
    root_message_id: String,
    foreground: Option<crate::foreground_turn::ForegroundTurnProgress>,
}

impl ContinuationLauncher {
    fn detach_foreground_renderer(&self) -> Self {
        let sink = self
            .resources
            .sink
            .deferred_continuation_sink()
            .unwrap_or_else(|| Arc::new(DetachedContinuationSink));
        Self {
            fallback_agent: self.fallback_agent.clone(),
            resources: Arc::new(ChatResources {
                execution_scope: self.resources.execution_scope.clone(),
                workspace_io_receipt: self.resources.workspace_io_receipt.clone(),
                pool: self.resources.pool.clone(),
                store: None,
                sink,
                webhook_emitter: self.resources.webhook_emitter.clone(),
                conv_id: self.resources.conv_id.clone(),
                root_message_id: String::new(),
                attachments: self.resources.attachments.clone(),
                cancel: CancellationToken::new(),
                review_integration: self.resources.review_integration.clone(),
                memory_generation: None,
                human_loop_provider: self.resources.human_loop_provider.clone(),
            }),
            root_message_id: self.root_message_id.clone(),
            foreground: None,
        }
    }
}

struct DetachedContinuationSink;

impl crate::chat_driver::ChatSink for DetachedContinuationSink {
    fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
        true
    }
}

#[derive(Clone)]
struct RegisteredLauncher {
    generation: u64,
    launcher: ContinuationLauncher,
}

struct ActiveDispatch {
    generation: u64,
    completion_tx: tokio::sync::watch::Sender<Option<ContinuationCompletion>>,
    cancel: CancellationToken,
}

struct OwnedWake {
    generation: u64,
    wake: std::sync::Weak<tokio::sync::Notify>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContinuationGenerationCut {
    launcher: Option<u64>,
    active: Option<u64>,
    owned: Option<u64>,
}

impl ContinuationGenerationCut {
    fn contains(self, generation: u64) -> bool {
        self.launcher == Some(generation) || self.active == Some(generation)
    }
}

#[cfg(test)]
struct RetryWaitTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Default)]
struct ContinuationState {
    launchers: HashMap<String, RegisteredLauncher>,
    active: HashMap<String, ActiveDispatch>,
    pending_wakeups: HashSet<String>,
    owned_wakes: HashMap<String, OwnedWake>,
    next_generation: u64,
}

pub(crate) struct TaskContinuationRuntime {
    store: Weak<TaskRuntimeStore>,
    state: Mutex<ContinuationState>,
    shutdown: CancellationToken,
    #[cfg(test)]
    retry_wait_test_barrier: Mutex<Option<RetryWaitTestBarrier>>,
}

impl TaskContinuationRuntime {
    fn new(store: Weak<TaskRuntimeStore>) -> Self {
        Self {
            store,
            state: Mutex::new(ContinuationState::default()),
            shutdown: CancellationToken::new(),
            #[cfg(test)]
            retry_wait_test_barrier: Mutex::new(None),
        }
    }

    fn register_launcher(
        &self,
        run_id: &str,
        fallback_agent: AgentHandle,
        resources: Arc<ChatResources>,
        root_message_id: String,
        foreground: Option<crate::foreground_turn::ForegroundTurnProgress>,
    ) {
        // A launcher never retains a workspace generation. While a foreground
        // chain is active it carries only the non-owning progress capability;
        // deferred launchers replace that capability and renderer atomically.
        let retained_sink = resources
            .sink
            .continuation_sink()
            .unwrap_or_else(|| resources.sink.clone());
        let retained = Arc::new(ChatResources {
            execution_scope: resources.execution_scope.clone(),
            workspace_io_receipt: resources.workspace_io_receipt.clone(),
            pool: resources.pool.clone(),
            store: None,
            sink: retained_sink,
            webhook_emitter: resources.webhook_emitter.clone(),
            conv_id: resources.conv_id.clone(),
            root_message_id: String::new(),
            attachments: resources.attachments.clone(),
            cancel: CancellationToken::new(),
            review_integration: resources.review_integration.clone(),
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
                    foreground,
                },
            },
        );
    }

    fn request(self: &Arc<Self>, run_id: &str, origin: RunTurnOrigin) -> ContinueRequestOutcome {
        let (generation, completion_tx, completion_rx, dispatch_cancel) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(generation) = state.launchers.get(run_id).map(|entry| entry.generation) else {
                return ContinueRequestOutcome::MissingLauncher;
            };
            if let Some(active) = state.active.get(run_id) {
                return ContinueRequestOutcome::Running(ContinueRequest {
                    disposition: ContinueRequestDisposition::Joined,
                    completion: ContinuationCompletionWaiter {
                        completion_rx: active.completion_tx.subscribe(),
                    },
                });
            }
            let (completion_tx, completion_rx) = tokio::sync::watch::channel(None);
            let dispatch_cancel = CancellationToken::new();
            state.active.insert(
                run_id.to_string(),
                ActiveDispatch {
                    generation,
                    completion_tx: completion_tx.clone(),
                    cancel: dispatch_cancel.clone(),
                },
            );
            (generation, completion_tx, completion_rx, dispatch_cancel)
        };
        self.spawn_dispatch(
            run_id.to_string(),
            origin,
            generation,
            completion_tx,
            dispatch_cancel,
        );
        ContinueRequestOutcome::Running(ContinueRequest {
            disposition: ContinueRequestDisposition::Started,
            completion: ContinuationCompletionWaiter { completion_rx },
        })
    }

    fn spawn_dispatch(
        self: &Arc<Self>,
        run_id: String,
        origin: RunTurnOrigin,
        generation: u64,
        completion_tx: tokio::sync::watch::Sender<Option<ContinuationCompletion>>,
        dispatch_cancel: CancellationToken,
    ) {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            Err(error) => {
                self.finish_dispatch(
                    &run_id,
                    generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    continuation_failure("continuation_runtime", error.to_string()),
                    completion_tx,
                );
                return;
            }
        };
        let runtime = Arc::clone(self);
        handle.spawn(async move {
            let panic_runtime = Arc::clone(&runtime);
            let panic_run_id = run_id.clone();
            let panic_completion = completion_tx.clone();
            let driven = std::panic::AssertUnwindSafe(runtime.drive_until_deferred(
                run_id,
                origin,
                generation,
                completion_tx,
                dispatch_cancel,
            ))
            .catch_unwind()
            .await;
            if driven.is_err() {
                let terminal = if let Some(store) = panic_runtime.store.upgrade() {
                    store.wait_for_run_driver_idle(&panic_run_id).await;
                    stopped_terminal_for_run(
                        &store,
                        &panic_run_id,
                        continuation_failure(
                            "continuation_panic",
                            "continuation dispatch terminated unexpectedly",
                        ),
                    )
                    .await
                } else {
                    continuation_failure(
                        "continuation_panic",
                        "continuation dispatch terminated unexpectedly",
                    )
                };
                panic_runtime.finish_dispatch(
                    &panic_run_id,
                    generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    terminal,
                    panic_completion,
                );
            }
        });
    }

    async fn drive_until_deferred(
        self: Arc<Self>,
        run_id: String,
        mut origin: RunTurnOrigin,
        dispatch_generation: u64,
        completion_tx: tokio::sync::watch::Sender<Option<ContinuationCompletion>>,
        dispatch_cancel: CancellationToken,
    ) {
        let Some(store) = self.store.upgrade() else {
            self.finish_dispatch(
                &run_id,
                dispatch_generation,
                true,
                ContinuationCompletionReason::Stopped,
                continuation_failure("continuation_store", "TaskRuntime store is unavailable"),
                completion_tx,
            );
            return;
        };
        let mut terminal = crate::chat_driver::TurnOutcome::Completed;
        loop {
            let Some(launcher) = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .launchers
                .get(&run_id)
                .map(|entry| entry.launcher.clone())
            else {
                store.wait_for_run_driver_idle(&run_id).await;
                let terminal = stopped_terminal_for_run(&store, &run_id, terminal).await;
                self.finish_dispatch(
                    &run_id,
                    dispatch_generation,
                    false,
                    ContinuationCompletionReason::Stopped,
                    terminal,
                    completion_tx,
                );
                return;
            };
            let turn_cancel = launcher
                .foreground
                .as_ref()
                .map(crate::foreground_turn::ForegroundTurnProgress::cancellation_token)
                .unwrap_or_else(CancellationToken::new);
            if turn_cancel.is_cancelled() {
                request_continuation_cancel(&store, &run_id).await;
                store.wait_for_run_driver_idle(&run_id).await;
                drop(launcher);
                self.finish_dispatch(
                    &run_id,
                    dispatch_generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    crate::chat_driver::TurnOutcome::Cancelled,
                    completion_tx,
                );
                return;
            }
            if dispatch_cancel.is_cancelled() {
                store.wait_for_run_driver_idle(&run_id).await;
                let terminal = stopped_terminal_for_run(&store, &run_id, terminal).await;
                drop(launcher);
                self.finish_dispatch(
                    &run_id,
                    dispatch_generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    terminal,
                    completion_tx,
                );
                return;
            }
            if !store.is_run_driver_admission_open() {
                drop(launcher);
                self.finish_dispatch(
                    &run_id,
                    dispatch_generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    terminal,
                    completion_tx,
                );
                return;
            }
            tokio::select! {
                _ = turn_cancel.cancelled() => {
                    request_continuation_cancel(&store, &run_id).await;
                    store.wait_for_run_driver_idle(&run_id).await;
                    drop(launcher);
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        true,
                        ContinuationCompletionReason::Stopped,
                        crate::chat_driver::TurnOutcome::Cancelled,
                        completion_tx,
                    );
                    return;
                }
                _ = dispatch_cancel.cancelled() => {
                    store.wait_for_run_driver_idle(&run_id).await;
                    let terminal = stopped_terminal_for_run(&store, &run_id, terminal).await;
                    drop(launcher);
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        true,
                        ContinuationCompletionReason::Stopped,
                        terminal,
                        completion_tx,
                    );
                    return;
                }
                _ = self.shutdown.cancelled() => {
                    store.wait_for_run_driver_idle(&run_id).await;
                    drop(launcher);
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        true,
                        ContinuationCompletionReason::Stopped,
                        terminal,
                        completion_tx,
                    );
                    return;
                }
                _ = store.wait_for_run_driver_idle(&run_id) => {}
            }
            if !store.is_run_driver_admission_open() {
                drop(launcher);
                self.finish_dispatch(
                    &run_id,
                    dispatch_generation,
                    true,
                    ContinuationCompletionReason::Stopped,
                    terminal,
                    completion_tx,
                );
                return;
            }
            match continuation_eligibility(&store, &run_id).await {
                ContinuationEligibility::Ready => {}
                ContinuationEligibility::RetryAt(deadline) => {
                    let delay = (deadline - chrono::Utc::now()).to_std().unwrap_or_default();
                    match self
                        .wait_for_retry_deadline(delay, &turn_cancel, &dispatch_cancel)
                        .await
                    {
                        RetryWaitOutcome::RootCancelled => {
                            request_continuation_cancel(&store, &run_id).await;
                            drop(launcher);
                            self.finish_dispatch(
                                &run_id,
                                dispatch_generation,
                                true,
                                ContinuationCompletionReason::Stopped,
                                crate::chat_driver::TurnOutcome::Cancelled,
                                completion_tx,
                            );
                            return;
                        }
                        RetryWaitOutcome::DispatchCancelled => {
                            store.wait_for_run_driver_idle(&run_id).await;
                            let terminal =
                                stopped_terminal_for_run(&store, &run_id, terminal).await;
                            drop(launcher);
                            self.finish_dispatch(
                                &run_id,
                                dispatch_generation,
                                true,
                                ContinuationCompletionReason::Stopped,
                                terminal,
                                completion_tx,
                            );
                            return;
                        }
                        RetryWaitOutcome::Shutdown => {
                            drop(launcher);
                            self.finish_dispatch(
                                &run_id,
                                dispatch_generation,
                                true,
                                ContinuationCompletionReason::Stopped,
                                terminal,
                                completion_tx,
                            );
                            return;
                        }
                        RetryWaitOutcome::Deadline => {}
                    }
                    continue;
                }
                ContinuationEligibility::Deferred => {
                    drop(launcher);
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        false,
                        ContinuationCompletionReason::Deferred,
                        terminal,
                        completion_tx,
                    );
                    return;
                }
                ContinuationEligibility::Stop => {
                    let terminal = stopped_terminal_for_run(&store, &run_id, terminal).await;
                    drop(launcher);
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        true,
                        ContinuationCompletionReason::Stopped,
                        terminal,
                        completion_tx,
                    );
                    return;
                }
            }

            let turn_id = uuid::Uuid::new_v4().to_string();
            let binding = RunTurnBinding {
                run_id: Some(run_id.clone()),
                turn_id: turn_id.clone(),
                root_message_id: launcher.root_message_id.clone(),
                origin,
                transcript_visibility: TurnVisibility::Internal,
                expected_resume: None,
            };
            let result = drive_continuation_turn(
                launcher,
                Arc::clone(&store),
                run_id.clone(),
                binding,
                turn_cancel,
            )
            .await;
            match result {
                Ok(outcome) => terminal = outcome.outcome,
                Err(error) => {
                    tracing::warn!(run_id, %error, "long-horizon continuation turn failed");
                    let terminal = if dispatch_cancel.is_cancelled() {
                        store.wait_for_run_driver_idle(&run_id).await;
                        stopped_terminal_for_run(&store, &run_id, terminal).await
                    } else {
                        continuation_failure("continuation_driver", error)
                    };
                    self.finish_dispatch(
                        &run_id,
                        dispatch_generation,
                        true,
                        ContinuationCompletionReason::Stopped,
                        terminal,
                        completion_tx,
                    );
                    return;
                }
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
        reason: ContinuationCompletionReason,
        terminal: crate::chat_driver::TurnOutcome,
        completion_tx: tokio::sync::watch::Sender<Option<ContinuationCompletion>>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let finish =
            settle_dispatch_state(&mut state, run_id, dispatch_generation, remove_launcher);
        let retired_launcher = if matches!(finish, DispatchSettlement::Complete) {
            if remove_launcher {
                state.launchers.remove(run_id)
            } else {
                state.launchers.get(run_id).cloned().and_then(|registered| {
                    state.launchers.insert(
                        run_id.to_string(),
                        RegisteredLauncher {
                            generation: registered.generation,
                            launcher: registered.launcher.detach_foreground_renderer(),
                        },
                    )
                })
            }
        } else {
            None
        };
        let restart_cancel = matches!(finish, DispatchSettlement::Restart(_))
            .then(|| state.active.get(run_id).map(|active| active.cancel.clone()))
            .flatten();
        drop(state);
        // The receipt is the renderer lifetime boundary. Drop the registry's
        // retired launcher outside the state lock and before waking waiters.
        drop(retired_launcher);
        match finish {
            DispatchSettlement::Stale => {}
            DispatchSettlement::Complete => {
                completion_tx.send_replace(Some(ContinuationCompletion { terminal, reason }));
            }
            DispatchSettlement::Restart(next_generation) => {
                if let Some(dispatch_cancel) = restart_cancel {
                    self.spawn_dispatch(
                        run_id.to_string(),
                        RunTurnOrigin::Continuation,
                        next_generation,
                        completion_tx,
                        dispatch_cancel,
                    );
                } else {
                    completion_tx.send_replace(Some(ContinuationCompletion {
                        terminal: continuation_failure(
                            "continuation_dispatch",
                            "restarted continuation lost its dispatch cancellation capability",
                        ),
                        reason: ContinuationCompletionReason::Stopped,
                    }));
                }
            }
        }
    }

    async fn wait_for_retry_deadline(
        &self,
        delay: std::time::Duration,
        root_cancel: &CancellationToken,
        dispatch_cancel: &CancellationToken,
    ) -> RetryWaitOutcome {
        let test_release = {
            #[cfg(test)]
            {
                self.retry_wait_test_barrier
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .map(|barrier| {
                        let _entered = barrier.entered.send(());
                        barrier.release
                    })
            }
            #[cfg(not(test))]
            {
                None::<tokio::sync::oneshot::Receiver<()>>
            }
        };
        let test_barrier_active = test_release.is_some();
        let test_release = async move {
            match test_release {
                Some(release) => {
                    let _released = release.await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let retry_deadline = async move {
            if test_barrier_active {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(delay).await;
            }
        };
        tokio::select! {
            _ = root_cancel.cancelled() => RetryWaitOutcome::RootCancelled,
            _ = dispatch_cancel.cancelled() => RetryWaitOutcome::DispatchCancelled,
            _ = self.shutdown.cancelled() => RetryWaitOutcome::Shutdown,
            _ = retry_deadline => RetryWaitOutcome::Deadline,
            _ = test_release => RetryWaitOutcome::Deadline,
        }
    }

    fn wake(self: &Arc<Self>, run_id: &str) {
        let (owned_wake, request_now) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let owned_wake = state
                .owned_wakes
                .get(run_id)
                .and_then(|owned| owned.wake.upgrade());
            let request_now = if owned_wake.is_some() {
                false
            } else if state.active.contains_key(run_id) {
                state.pending_wakeups.insert(run_id.to_string());
                false
            } else {
                true
            };
            (owned_wake, request_now)
        };
        if let Some(wake) = owned_wake {
            wake.notify_one();
            return;
        }
        if request_now {
            let outcome = self.request(run_id, RunTurnOrigin::Continuation);
            tracing::debug!(run_id, ?outcome, "continuation wake requested");
        }
    }

    fn capture_generation_cut(&self, run_id: &str) -> ContinuationGenerationCut {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ContinuationGenerationCut {
            launcher: state.launchers.get(run_id).map(|entry| entry.generation),
            active: state.active.get(run_id).map(|entry| entry.generation),
            owned: state.owned_wakes.get(run_id).map(|entry| entry.generation),
        }
    }

    fn clear_launcher_at_cut(&self, run_id: &str, cut: ContinuationGenerationCut) {
        let (launcher, dispatch_cancel, owned_wake) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let launcher = state
                .launchers
                .get(run_id)
                .is_some_and(|launcher| cut.contains(launcher.generation))
                .then(|| state.launchers.remove(run_id))
                .flatten();
            let dispatch_cancel = state.active.get(run_id).and_then(|active| {
                cut.contains(active.generation)
                    .then(|| active.cancel.clone())
            });
            if launcher.is_some() || dispatch_cancel.is_some() {
                state.pending_wakeups.remove(run_id);
            }
            let owned_wake = state.owned_wakes.get(run_id).and_then(|owned| {
                (cut.owned == Some(owned.generation))
                    .then(|| owned.wake.upgrade())
                    .flatten()
            });
            (launcher, dispatch_cancel, owned_wake)
        };
        drop(launcher);
        if let Some(dispatch_cancel) = dispatch_cancel {
            dispatch_cancel.cancel();
        }
        if let Some(owned_wake) = owned_wake {
            owned_wake.notify_one();
        }
    }

    fn clear_launcher_unconditional(&self, run_id: &str) {
        let (launcher, dispatch_cancel, owned_wake) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let launcher = state.launchers.remove(run_id);
            state.pending_wakeups.remove(run_id);
            let dispatch_cancel = state.active.get(run_id).map(|active| active.cancel.clone());
            let owned_wake = state
                .owned_wakes
                .get(run_id)
                .and_then(|owned| owned.wake.upgrade());
            (launcher, dispatch_cancel, owned_wake)
        };
        drop(launcher);
        if let Some(dispatch_cancel) = dispatch_cancel {
            dispatch_cancel.cancel();
        }
        if let Some(owned_wake) = owned_wake {
            owned_wake.notify_one();
        }
    }

    fn clear_all_launchers(&self) {
        self.shutdown.cancel();
        let (launchers, dispatch_cancels, owned_wakes) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let launchers = std::mem::take(&mut state.launchers);
            state.pending_wakeups.clear();
            let dispatch_cancels = state
                .active
                .values()
                .map(|active| active.cancel.clone())
                .collect::<Vec<_>>();
            let owned_wakes = state
                .owned_wakes
                .values()
                .filter_map(|owned| owned.wake.upgrade())
                .collect::<Vec<_>>();
            (launchers, dispatch_cancels, owned_wakes)
        };
        drop(launchers);
        for dispatch_cancel in dispatch_cancels {
            dispatch_cancel.cancel();
        }
        for owned_wake in owned_wakes {
            owned_wake.notify_one();
        }
    }
}

async fn drive_continuation_turn(
    launcher: ContinuationLauncher,
    store: Arc<TaskRuntimeStore>,
    run_id: String,
    binding: RunTurnBinding,
    cancel: CancellationToken,
) -> Result<TurnReceipt, String> {
    let turn_id = binding.turn_id.clone();
    let resources = Arc::new(ChatResources {
        execution_scope: launcher.resources.execution_scope.clone(),
        workspace_io_receipt: launcher.resources.workspace_io_receipt.clone(),
        pool: launcher.resources.pool.clone(),
        store: Some(store),
        sink: launcher.resources.sink.clone(),
        webhook_emitter: launcher.resources.webhook_emitter.clone(),
        conv_id: launcher.resources.conv_id.clone(),
        root_message_id: turn_id,
        attachments: launcher.resources.attachments.clone(),
        cancel,
        review_integration: launcher.resources.review_integration.clone(),
        memory_generation: None,
        human_loop_provider: launcher.resources.human_loop_provider.clone(),
    });
    let progress = launcher.foreground.clone();
    let execute = move |resources: Arc<ChatResources>| async move {
        let turn = PreparedUserTurn::runtime_instruction(format!(
            "Continue the existing TaskRun {run_id} toward its unchanged Goal. Reload the authoritative TaskRuntime projection, execute the next useful work, and use task_execute for the current revision when ready. This is internal continuation context, not a new user request."
        ));
        let human_loop_provider = resources.human_loop_provider.clone();
        if let Some(pool) = resources.pool.clone() {
            let pool_key = resources
                .conv_id
                .clone()
                .unwrap_or_else(|| format!("__continuation__:{run_id}"));
            crate::chat_driver::drive_pooled_chat_turn(
                pool,
                &pool_key,
                move |agent| async move {
                    if let Some(provider) = human_loop_provider {
                        agent
                            .write_async(|agent| {
                                Box::pin(async move {
                                    agent.set_human_loop_provider_preserving_approvals(provider);
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
        }
    };
    match progress {
        Some(progress) => progress.scope_chat(resources, execute).await,
        None => execute(resources).await,
    }
}

fn continuation_failure(code: &str, message: impl Into<String>) -> crate::chat_driver::TurnOutcome {
    crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(code, message))
}

async fn stopped_terminal_for_run(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    previous: crate::chat_driver::TurnOutcome,
) -> crate::chat_driver::TurnOutcome {
    let lookup_run_id = run_id.to_string();
    let run = super::executor::TaskRuntimeOperation::new(store.clone())
        .run_store("load stopped continuation TaskRun", move |store| {
            store.get_run(&lookup_run_id)
        })
        .await;
    match run {
        Ok(Some(run)) => match run.status {
            TaskRunStatus::Cancelled => crate::chat_driver::TurnOutcome::Cancelled,
            TaskRunStatus::Failed => continuation_failure(
                "continuation_stopped",
                format!("TaskRun {run_id} stopped after entering Failed"),
            ),
            TaskRunStatus::Pending
            | TaskRunStatus::Running
            | TaskRunStatus::Paused
            | TaskRunStatus::Completed => match previous {
                crate::chat_driver::TurnOutcome::Cancelled
                    if run.status == TaskRunStatus::Paused =>
                {
                    crate::chat_driver::TurnOutcome::Completed
                }
                other => other,
            },
        },
        Ok(None) => continuation_failure(
            "continuation_stopped",
            format!("TaskRun {run_id} disappeared before continuation completion"),
        ),
        Err(error) => continuation_failure(
            "continuation_stopped",
            format!("TaskRun {run_id} completion could not be read: {error}"),
        ),
    }
}

fn settle_dispatch_state(
    state: &mut ContinuationState,
    run_id: &str,
    dispatch_generation: u64,
    remove_launcher: bool,
) -> DispatchSettlement {
    if state.active.get(run_id).map(|active| active.generation) != Some(dispatch_generation) {
        return DispatchSettlement::Stale;
    }
    let pending_wakeup = state.pending_wakeups.remove(run_id);
    let has_newer_launcher = state
        .launchers
        .get(run_id)
        .is_some_and(|entry| entry.generation > dispatch_generation);
    let should_restart = has_newer_launcher || (pending_wakeup && !remove_launcher);
    if should_restart {
        let next_generation = state
            .launchers
            .get(run_id)
            .map(|entry| entry.generation)
            .unwrap_or(dispatch_generation);
        if let Some(active) = state.active.get_mut(run_id) {
            active.generation = next_generation;
            active.cancel = CancellationToken::new();
        }
        return DispatchSettlement::Restart(next_generation);
    }
    state.active.remove(run_id);
    DispatchSettlement::Complete
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchSettlement {
    Stale,
    Complete,
    Restart(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryWaitOutcome {
    Deadline,
    RootCancelled,
    DispatchCancelled,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationEligibility {
    Ready,
    RetryAt(chrono::DateTime<chrono::Utc>),
    Deferred,
    Stop,
}

async fn continuation_eligibility(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
) -> ContinuationEligibility {
    let activity_run_id = run_id.to_string();
    let runtime_active = super::executor::TaskRuntimeOperation::new(store.clone())
        .run_store("defer active continuation runtime", move |store| {
            store.defer_continuation_if_runtime_active(&activity_run_id)
        })
        .await;
    let runtime_active = match runtime_active {
        Ok(runtime_active) => runtime_active,
        Err(error) => {
            tracing::warn!(run_id, %error, "continuation activity could not be inspected");
            return ContinuationEligibility::Stop;
        }
    };
    if runtime_active {
        return ContinuationEligibility::Deferred;
    }
    let lookup_run_id = run_id.to_string();
    let Ok(Some(snapshot)) = super::executor::TaskRuntimeOperation::new(store.clone())
        .run_store("load continuation eligibility", move |store| {
            store.get_run_state(&lookup_run_id)
        })
        .await
    else {
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
    if let Some(retry) = continuation.provider_retry {
        if retry.exhausted {
            return ContinuationEligibility::Stop;
        }
        if retry.next_retry_at > chrono::Utc::now() {
            return ContinuationEligibility::RetryAt(retry.next_retry_at);
        }
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

async fn request_continuation_cancel(store: &Arc<TaskRuntimeStore>, run_id: &str) {
    let run_id = run_id.to_string();
    if let Err(error) = super::executor::TaskRuntimeOperation::new(store.clone())
        .run_store("cancel continuation TaskRun", move |store| {
            store.request_cancel(&run_id)
        })
        .await
    {
        tracing::warn!(%error, "failed to persist continuation cancellation");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedContinueOutcome {
    Ready,
    Stop,
    Cancelled,
    Shutdown,
}

/// Wait for the next finite turn while the current canonical driver still owns
/// its registration. This shares the detached runtime's eligibility, durable
/// provider deadline, shutdown token, and cell wake source, but owns no second
/// active/generation/pending state.
pub(crate) async fn await_owned_continue(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    cancel: &CancellationToken,
) -> OwnedContinueOutcome {
    let runtime = runtime_for(store);
    let wake = Arc::new(tokio::sync::Notify::new());
    let owned_generation = {
        let mut state = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_generation = state.next_generation.saturating_add(1).max(1);
        let generation = state.next_generation;
        state.owned_wakes.insert(
            run_id.to_string(),
            OwnedWake {
                generation,
                wake: Arc::downgrade(&wake),
            },
        );
        generation
    };

    let outcome = loop {
        let notified = wake.notified();
        match continuation_eligibility(store, run_id).await {
            ContinuationEligibility::Ready => break OwnedContinueOutcome::Ready,
            ContinuationEligibility::Stop => break OwnedContinueOutcome::Stop,
            ContinuationEligibility::Deferred => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break OwnedContinueOutcome::Cancelled,
                    _ = runtime.shutdown.cancelled() => break OwnedContinueOutcome::Shutdown,
                    _ = notified => {}
                }
            }
            ContinuationEligibility::RetryAt(deadline) => {
                let delay = (deadline - chrono::Utc::now()).to_std().unwrap_or_default();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break OwnedContinueOutcome::Cancelled,
                    _ = runtime.shutdown.cancelled() => break OwnedContinueOutcome::Shutdown,
                    _ = notified => {}
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    };
    let mut state = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let matches_current = state.owned_wakes.get(run_id).is_some_and(|registered| {
        registered.generation == owned_generation
            && registered
                .wake
                .upgrade()
                .is_some_and(|registered| Arc::ptr_eq(&registered, &wake))
    });
    if matches_current {
        state.owned_wakes.remove(run_id);
    }
    outcome
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
    foreground: Option<crate::foreground_turn::ForegroundTurnProgress>,
) {
    runtime_for(store).register_launcher(
        run_id,
        fallback_agent,
        resources,
        root_message_id,
        foreground,
    );
}

pub(crate) fn request_continue(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    origin: RunTurnOrigin,
) -> ContinueRequestOutcome {
    runtime_for(store).request(run_id, origin)
}

#[cfg(test)]
pub(crate) fn runtime_state_for_test(
    store: &TaskRuntimeStore,
    run_id: &str,
) -> (Option<u64>, Option<u64>, bool) {
    let Some(runtime) = store.continuation_runtime.get() else {
        return (None, None, false);
    };
    let state = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        state.launchers.get(run_id).map(|entry| entry.generation),
        state.active.get(run_id).map(|entry| entry.generation),
        state.pending_wakeups.contains(run_id),
    )
}

pub(crate) fn clear_launcher(store: &TaskRuntimeStore, run_id: &str) {
    if let Some(runtime) = store.continuation_runtime.get() {
        runtime.clear_launcher_unconditional(run_id);
    }
}

pub(crate) fn capture_generation_cut(
    store: &TaskRuntimeStore,
    run_id: &str,
) -> ContinuationGenerationCut {
    store
        .continuation_runtime
        .get()
        .map(|runtime| runtime.capture_generation_cut(run_id))
        .unwrap_or_default()
}

pub(crate) fn clear_launcher_at_cut(
    store: &TaskRuntimeStore,
    run_id: &str,
    cut: ContinuationGenerationCut,
) {
    if let Some(runtime) = store.continuation_runtime.get() {
        runtime.clear_launcher_at_cut(run_id, cut);
    }
}

pub(crate) fn shutdown(store: &TaskRuntimeStore) {
    if let Some(runtime) = store.continuation_runtime.get() {
        runtime.clear_all_launchers();
    }
}

/// A completed background cell or a settled plan task is a concrete wake-up,
/// not a reason to spin model turns. Only a Running run that explicitly
/// deferred (for active cells or in-flight subagent work) is resumed here,
/// and only once the runtime is quiet — no active background cells and no
/// Running plan tasks. User-paused and recovery-paused runs remain under
/// user control.
pub(crate) fn wake_deferred_when_runtime_quiet(store: &Arc<TaskRuntimeStore>, run_id: &str) {
    match store.resume_deferred_continuation_if_quiet(run_id) {
        Ok(true) => runtime_for(store).wake(run_id),
        Ok(false) => {}
        Err(error) => tracing::warn!(run_id, %error, "failed to clear continuation deferral"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropTrackingSink(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropTrackingSink {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl crate::chat_driver::ChatSink for DropTrackingSink {
        fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
            true
        }
    }

    struct DropOrderSink {
        entered: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl Drop for DropOrderSink {
        fn drop(&mut self) {
            let _entered = self.entered.send(());
            let _released = self
                .release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv_timeout(std::time::Duration::from_secs(5));
        }
    }

    impl crate::chat_driver::ChatSink for DropOrderSink {
        fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
            true
        }
    }

    fn test_execution_scope() -> crate::workspace::WorkspaceExecutionScope {
        crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_name("continuation-test"),
            ".",
        )
    }

    fn retry_wait_fixture(
        run_id: &str,
    ) -> Result<(Arc<TaskRuntimeStore>, AgentHandle, Arc<ChatResources>), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                run_id,
                "test",
                &format!("{run_id}-conversation"),
                &format!("{run_id}-root"),
                super::super::types::DomainProfile::General,
                "wait for provider retry",
                "agent_task_plan",
                super::super::types::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .schedule_provider_retry_at_for_test(
                run_id,
                "retry-wait-test",
                chrono::Utc::now()
                    .checked_add_signed(chrono::Duration::minutes(1))
                    .ok_or_else(|| "retry test clock overflowed".to_string())?,
            )
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("continuation-retry-wait")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("continuation-retry-wait"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resources = Arc::new(ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(DetachedContinuationSink),
            webhook_emitter: None,
            conv_id: Some(format!("{run_id}-conversation")),
            root_message_id: format!("{run_id}-root"),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        Ok((store, agent, resources))
    }

    fn install_retry_wait_barrier(
        store: &Arc<TaskRuntimeStore>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *runtime_for(store)
            .retry_wait_test_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(RetryWaitTestBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }

    async fn hold_run_driver(
        store: &Arc<TaskRuntimeStore>,
        run_id: &str,
    ) -> Result<
        (
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<Result<(), String>>,
        ),
        String,
    > {
        let admission = store
            .reserve_run_driver_admission(run_id.to_string(), CancellationToken::new())
            .map_err(|error| error.to_string())?;
        let generation = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let waiter = store
            .spawn_run_driver(admission, generation, move |_receipt_owner| async move {
                let _started = started_tx.send(());
                release_rx.await.map_err(|error| error.to_string())?;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        started_rx
            .await
            .map_err(|_| "held RunDriver did not start".to_string())?;
        Ok((release_tx, waiter))
    }

    async fn assert_dispatch_completion_waits_for_held_driver(
        run_id: &str,
        cancel: bool,
    ) -> Result<(), String> {
        let (store, agent, resources) = retry_wait_fixture(run_id)?;
        register_launcher(
            &store,
            run_id,
            agent,
            resources,
            format!("{run_id}-root"),
            None,
        );
        let (release_driver, driver_waiter) = hold_run_driver(&store, run_id).await?;
        let request = match request_continue(&store, run_id, RunTurnOrigin::Recovery) {
            ContinueRequestOutcome::Running(request) => request,
            other => return Err(format!("held-driver request was not accepted: {other:?}")),
        };
        let accepted = if cancel {
            store.request_cancel(run_id)
        } else {
            store.request_pause(run_id)
        }
        .map_err(|error| error.to_string())?;
        if !accepted {
            return Err(format!(
                "held-driver {} intent was not accepted",
                if cancel { "cancel" } else { "pause" }
            ));
        }
        let mut completion_waiter = tokio::spawn(request.completion.wait());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut completion_waiter,)
                .await
                .is_err(),
            "dispatch completion was published before the exact RunDriver released"
        );
        release_driver
            .send(())
            .map_err(|_| "held RunDriver release receiver closed".to_string())?;
        driver_waiter.await.map_err(|error| error.to_string())??;
        let completion = tokio::time::timeout(std::time::Duration::from_secs(2), completion_waiter)
            .await
            .map_err(|_| "dispatch did not complete after exact RunDriver release".to_string())?
            .map_err(|error| error.to_string())??;
        assert_eq!(completion.reason, ContinuationCompletionReason::Stopped);
        assert_eq!(
            completion.terminal,
            if cancel {
                crate::chat_driver::TurnOutcome::Cancelled
            } else {
                crate::chat_driver::TurnOutcome::Completed
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn owned_driver_waits_for_deferred_wake_without_detached_handoff() -> Result<(), String> {
        use super::super::types::{AttendedMode, DomainProfile};

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "owned-deferred",
                "default",
                "conversation",
                "message",
                DomainProfile::General,
                "wait for cells",
                "test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("owned-deferred", true, true, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("owned-deferred", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_continuation_deferred("owned-deferred", true)
            .map_err(|error| error.to_string())?;

        let waiter_store = store.clone();
        let wait = tokio::spawn(async move {
            await_owned_continue(&waiter_store, "owned-deferred", &CancellationToken::new()).await
        });
        let runtime = runtime_for(&store);
        for _ in 0..64 {
            if runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .owned_wakes
                .contains_key("owned-deferred")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .owned_wakes
                .contains_key("owned-deferred"),
            "owned waiter did not register its wake source"
        );
        store
            .set_continuation_deferred("owned-deferred", false)
            .map_err(|error| error.to_string())?;
        runtime.wake("owned-deferred");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), wait)
            .await
            .map_err(|_| "owned deferred waiter missed its wake".to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome, OwnedContinueOutcome::Ready);
        assert!(
            runtime_for(&store)
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .active
                .is_empty(),
            "owned waiter created detached active state"
        );
        Ok(())
    }

    #[tokio::test]
    async fn generation_cut_preserves_replacement_owned_waiter() -> Result<(), String> {
        let runtime = TaskContinuationRuntime::new(Weak::new());
        let old_wake = Arc::new(tokio::sync::Notify::new());
        {
            let mut state = runtime
                .state
                .lock()
                .map_err(|_| "continuation state is unavailable".to_string())?;
            state.owned_wakes.insert(
                "run".to_string(),
                OwnedWake {
                    generation: 1,
                    wake: Arc::downgrade(&old_wake),
                },
            );
        }
        let old_cut = runtime.capture_generation_cut("run");
        let replacement_wake = Arc::new(tokio::sync::Notify::new());
        {
            let mut state = runtime
                .state
                .lock()
                .map_err(|_| "continuation state is unavailable".to_string())?;
            state.owned_wakes.insert(
                "run".to_string(),
                OwnedWake {
                    generation: 2,
                    wake: Arc::downgrade(&replacement_wake),
                },
            );
        }

        runtime.clear_launcher_at_cut("run", old_cut);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                replacement_wake.notified(),
            )
            .await
            .is_err(),
            "an old control cut woke the replacement owned driver"
        );

        runtime.clear_launcher_unconditional("run");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            replacement_wake.notified(),
        )
        .await
        .map_err(|_| "unconditional clear did not wake the current owned driver".to_string())?;
        Ok(())
    }

    #[test]
    fn pending_cell_wakeup_requeues_after_deferred_dispatch_exit() {
        let mut state = ContinuationState::default();
        let (completion_tx, _completion_rx) = tokio::sync::watch::channel(None);
        state.active.insert(
            "run".to_string(),
            ActiveDispatch {
                generation: 7,
                completion_tx,
                cancel: CancellationToken::new(),
            },
        );
        state.pending_wakeups.insert("run".to_string());

        assert_eq!(
            settle_dispatch_state(&mut state, "run", 7, false),
            DispatchSettlement::Restart(7)
        );
        assert!(state.active.contains_key("run"));
        assert!(!state.pending_wakeups.contains("run"));
    }

    #[test]
    fn replacement_generation_does_not_inherit_cancelled_dispatch_token() {
        let mut state = ContinuationState::default();
        let (completion_tx, _completion_rx) = tokio::sync::watch::channel(None);
        let old_cancel = CancellationToken::new();
        old_cancel.cancel();
        state.active.insert(
            "run".to_string(),
            ActiveDispatch {
                generation: 7,
                completion_tx,
                cancel: old_cancel,
            },
        );
        state.pending_wakeups.insert("run".to_string());

        assert_eq!(
            settle_dispatch_state(&mut state, "run", 7, false),
            DispatchSettlement::Restart(7)
        );
        let replacement_cancelled = state
            .active
            .get("run")
            .map(|active| active.cancel.is_cancelled())
            .unwrap_or(true);
        assert!(!replacement_cancelled);
    }

    #[test]
    fn terminal_dispatch_ignores_stale_pending_wakeup() {
        let mut state = ContinuationState::default();
        let (completion_tx, _completion_rx) = tokio::sync::watch::channel(None);
        state.active.insert(
            "run".to_string(),
            ActiveDispatch {
                generation: 9,
                completion_tx,
                cancel: CancellationToken::new(),
            },
        );
        state.pending_wakeups.insert("run".to_string());

        assert_eq!(
            settle_dispatch_state(&mut state, "run", 9, true),
            DispatchSettlement::Complete
        );
        assert!(!state.active.contains_key("run"));
        assert!(!state.pending_wakeups.contains("run"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_requests_join_the_same_completion_receipt() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("continuation-join")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new().with_model_name("continuation-join"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resources = Arc::new(ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(DetachedContinuationSink),
            webhook_emitter: None,
            conv_id: Some("join-conversation".to_string()),
            root_message_id: "join-root".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        register_launcher(
            &store,
            "missing-run",
            agent,
            resources,
            "join-root".to_string(),
            None,
        );

        let first = match request_continue(&store, "missing-run", RunTurnOrigin::Continuation) {
            ContinueRequestOutcome::Running(request) => request,
            other => {
                return Err(format!(
                    "first continuation request was not accepted: {other:?}"
                ));
            }
        };
        let second = match request_continue(&store, "missing-run", RunTurnOrigin::Continuation) {
            ContinueRequestOutcome::Running(request) => request,
            other => {
                return Err(format!(
                    "joined continuation request was not accepted: {other:?}"
                ));
            }
        };
        assert_eq!(first.disposition, ContinueRequestDisposition::Started);
        assert_eq!(second.disposition, ContinueRequestDisposition::Joined);

        let (first_completion, second_completion) =
            tokio::join!(first.completion.wait(), second.completion.wait());
        assert_eq!(first_completion?, second_completion?);
        Ok(())
    }

    #[tokio::test]
    async fn deferred_completion_releases_foreground_renderer_before_receipt() -> Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "deferred-run",
                "test",
                "deferred-conversation",
                "deferred-root",
                super::super::types::DomainProfile::General,
                "wait for a background cell",
                "agent_task_plan",
                super::super::types::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("deferred-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("deferred-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .set_continuation_deferred("deferred-run", true)
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("continuation-deferred")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("continuation-deferred"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let renderer_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resources = Arc::new(ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(DropTrackingSink(Arc::clone(&renderer_dropped))),
            webhook_emitter: None,
            conv_id: Some("deferred-conversation".to_string()),
            root_message_id: "deferred-root".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        register_launcher(
            &store,
            "deferred-run",
            agent,
            resources.clone(),
            "deferred-root".to_string(),
            None,
        );
        drop(resources);
        let request = match request_continue(&store, "deferred-run", RunTurnOrigin::Continuation) {
            ContinueRequestOutcome::Running(request) => request,
            other => return Err(format!("deferred request was not accepted: {other:?}")),
        };
        let completion = request.completion.wait().await?;
        assert_eq!(completion.reason, ContinuationCompletionReason::Deferred);
        assert!(
            renderer_dropped.load(std::sync::atomic::Ordering::SeqCst),
            "deferred completion was published before releasing its renderer"
        );
        let runtime = runtime_for(&store);
        let state = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            state
                .launchers
                .get("deferred-run")
                .is_some_and(|registered| registered.launcher.foreground.is_none())
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_wait_cancel_wakes_detached_dispatch_with_cancelled_completion()
    -> Result<(), String> {
        let run_id = "retry-cancel";
        let (store, agent, resources) = retry_wait_fixture(run_id)?;
        let (entered, _release) = install_retry_wait_barrier(&store);
        register_launcher(
            &store,
            run_id,
            agent,
            resources,
            format!("{run_id}-root"),
            None,
        );
        let request = match request_continue(&store, run_id, RunTurnOrigin::Recovery) {
            ContinueRequestOutcome::Running(request) => request,
            other => return Err(format!("retry cancel request was not accepted: {other:?}")),
        };
        entered
            .await
            .map_err(|_| "retry cancel dispatch never entered RetryAt".to_string())?;
        if !store
            .request_cancel(run_id)
            .map_err(|error| error.to_string())?
        {
            return Err("retry cancel intent was not accepted".to_string());
        }
        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(2), request.completion.wait())
                .await
                .map_err(|_| "RetryAt cancellation did not complete dispatch".to_string())??;
        assert_eq!(completion.reason, ContinuationCompletionReason::Stopped);
        assert_eq!(
            completion.terminal,
            crate::chat_driver::TurnOutcome::Cancelled
        );
        assert_eq!(
            store
                .get_run(run_id)
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(TaskRunStatus::Cancelled)
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_wait_pause_wakes_detached_dispatch_with_clean_completion() -> Result<(), String>
    {
        let run_id = "retry-pause";
        let (store, agent, resources) = retry_wait_fixture(run_id)?;
        let (entered, _release) = install_retry_wait_barrier(&store);
        register_launcher(
            &store,
            run_id,
            agent,
            resources,
            format!("{run_id}-root"),
            None,
        );
        let request = match request_continue(&store, run_id, RunTurnOrigin::Recovery) {
            ContinueRequestOutcome::Running(request) => request,
            other => return Err(format!("retry pause request was not accepted: {other:?}")),
        };
        entered
            .await
            .map_err(|_| "retry pause dispatch never entered RetryAt".to_string())?;
        if !store
            .request_pause(run_id)
            .map_err(|error| error.to_string())?
        {
            return Err("retry pause intent was not accepted".to_string());
        }
        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(2), request.completion.wait())
                .await
                .map_err(|_| "RetryAt pause did not complete dispatch".to_string())??;
        assert_eq!(completion.reason, ContinuationCompletionReason::Stopped);
        assert_eq!(
            completion.terminal,
            crate::chat_driver::TurnOutcome::Completed
        );
        assert_eq!(
            store
                .get_run(run_id)
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(TaskRunStatus::Paused)
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_completion_waits_for_held_run_driver_release() -> Result<(), String> {
        assert_dispatch_completion_waits_for_held_driver("held-cancel", true).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_completion_waits_for_held_run_driver_release() -> Result<(), String> {
        assert_dispatch_completion_waits_for_held_driver("held-pause", false).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deferred_receipt_waits_for_stack_and_registry_renderer_drop() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "drop-order-run",
                "test",
                "drop-order-conversation",
                "drop-order-root",
                super::super::types::DomainProfile::General,
                "release the renderer first",
                "agent_task_plan",
                super::super::types::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("drop-order-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("drop-order-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .set_continuation_deferred("drop-order-run", true)
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("continuation-drop-order")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("continuation-drop-order"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let (drop_entered_tx, drop_entered_rx) = std::sync::mpsc::sync_channel(1);
        let (drop_release_tx, drop_release_rx) = std::sync::mpsc::sync_channel(1);
        let resources = Arc::new(ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(DropOrderSink {
                entered: drop_entered_tx,
                release: Mutex::new(drop_release_rx),
            }),
            webhook_emitter: None,
            conv_id: Some("drop-order-conversation".to_string()),
            root_message_id: "drop-order-root".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        register_launcher(
            &store,
            "drop-order-run",
            agent,
            resources.clone(),
            "drop-order-root".to_string(),
            None,
        );
        drop(resources);
        let request = match request_continue(&store, "drop-order-run", RunTurnOrigin::Continuation)
        {
            ContinueRequestOutcome::Running(request) => request,
            other => return Err(format!("drop-order request was not accepted: {other:?}")),
        };
        let mut waiter = tokio::spawn(request.completion.wait());
        tokio::task::spawn_blocking(move || {
            drop_entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())??;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiter)
                .await
                .is_err(),
            "completion receipt was published before renderer Drop returned"
        );
        drop_release_tx
            .send(())
            .map_err(|_| "renderer Drop release receiver closed".to_string())?;
        let completion = waiter.await.map_err(|error| error.to_string())??;
        assert_eq!(completion.reason, ContinuationCompletionReason::Deferred);
        Ok(())
    }
}
