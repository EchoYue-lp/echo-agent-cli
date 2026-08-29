//! Tauri shared state wrapper.

use echo_agent_app_core::api::{AppState, browser::BrowserRuntime};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Shared state accessible from all Tauri IPC commands.
pub struct TauriState {
    pub app_state: Arc<AppState>,
    pub browser_runtime: Arc<BrowserRuntime>,
    pub bridge_supervisor: Arc<TauriBridgeSupervisor>,
}

impl TauriState {
    pub fn new(
        app_state: Arc<AppState>,
        browser_runtime: Arc<BrowserRuntime>,
        bridge_supervisor: Arc<TauriBridgeSupervisor>,
    ) -> Self {
        Self {
            app_state,
            browser_runtime,
            bridge_supervisor,
        }
    }
}

/// Owns Tauri event-forwarding tasks so desktop shutdown can cancel and join
/// them instead of leaving detached receivers behind.
pub struct TauriBridgeSupervisor {
    cancel: CancellationToken,
    state: Arc<std::sync::Mutex<TauriBridgeState>>,
    reservation_released: Arc<tokio::sync::Notify>,
}

struct TauriBridgeState {
    accepting: bool,
    pending_reservations: usize,
    handles: Vec<JoinHandle<()>>,
}

pub struct TauriBridgeReservation {
    state: Arc<std::sync::Mutex<TauriBridgeState>>,
    reservation_released: Arc<tokio::sync::Notify>,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TauriBridgeAdmissionError;

impl std::fmt::Display for TauriBridgeAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Tauri event bridge admission is closed")
    }
}

impl std::error::Error for TauriBridgeAdmissionError {}

impl TauriBridgeReservation {
    pub fn track(mut self, handle: JoinHandle<()>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.handles.push(handle);
        state.pending_reservations = state.pending_reservations.saturating_sub(1);
        self.active = false;
        drop(state);
        self.reservation_released.notify_waiters();
    }
}

impl Drop for TauriBridgeReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending_reservations = state.pending_reservations.saturating_sub(1);
        drop(state);
        self.reservation_released.notify_waiters();
    }
}

impl TauriBridgeSupervisor {
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            state: Arc::new(std::sync::Mutex::new(TauriBridgeState {
                accepting: true,
                pending_reservations: 0,
                handles: Vec::new(),
            })),
            reservation_released: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Reserve bridge admission before spawning. Shutdown waits for every
    /// accepted reservation to either publish its handle or be dropped.
    pub fn reserve(&self) -> Result<TauriBridgeReservation, TauriBridgeAdmissionError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.accepting {
            return Err(TauriBridgeAdmissionError);
        }
        state.pending_reservations = state
            .pending_reservations
            .checked_add(1)
            .ok_or(TauriBridgeAdmissionError)?;
        Ok(TauriBridgeReservation {
            state: Arc::clone(&self.state),
            reservation_released: Arc::clone(&self.reservation_released),
            active: true,
        })
    }

    pub fn begin_shutdown(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.accepting = false;
        self.cancel.cancel();
    }

    pub async fn join(&self) -> Result<(), String> {
        let handles = loop {
            let reservation_released = self.reservation_released.notified();
            let handles = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.accepting {
                    return Err(
                        "Tauri event bridge join started before admission closed".to_string()
                    );
                }
                if state.pending_reservations == 0 {
                    Some(std::mem::take(&mut state.handles))
                } else {
                    None
                }
            };
            if let Some(handles) = handles {
                break handles;
            }
            reservation_released.await;
        };
        let mut failures = Vec::new();
        for handle in handles {
            if let Err(error) = handle.await {
                tracing::warn!(%error, "Tauri event bridge failed during shutdown");
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        self.join().await
    }
}

impl Default for TauriBridgeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{TauriBridgeAdmissionError, TauriBridgeSupervisor};
    use std::sync::Arc;

    #[tokio::test]
    async fn shutdown_cancels_and_joins_tracked_bridges() -> Result<(), String> {
        let supervisor = Arc::new(TauriBridgeSupervisor::new());
        let cancel = supervisor.cancellation_token();
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_in_task = completed.clone();
        let reservation = supervisor.reserve().map_err(|error| error.to_string())?;
        reservation.track(tokio::spawn(async move {
            cancel.cancelled().await;
            completed_in_task.store(true, std::sync::atomic::Ordering::Release);
        }));

        supervisor
            .shutdown()
            .await
            .map_err(|error| format!("bridge shutdown failed: {error}"))?;

        assert!(completed.load(std::sync::atomic::Ordering::Acquire));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_reports_bridge_join_failure() -> Result<(), String> {
        let supervisor = TauriBridgeSupervisor::new();
        let reservation = supervisor.reserve().map_err(|error| error.to_string())?;
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        reservation.track(task);

        let error = supervisor
            .shutdown()
            .await
            .err()
            .ok_or_else(|| "aborted bridge was reported as successful".to_string())?;
        assert!(error.contains("cancelled"));
        Ok(())
    }

    #[tokio::test]
    async fn begin_shutdown_waits_for_preaccepted_reservation_and_rejects_late_reserve()
    -> Result<(), String> {
        let supervisor = Arc::new(TauriBridgeSupervisor::new());
        let accepted_cancel = supervisor.cancellation_token();
        let accepted_settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accepted_settled_task = Arc::clone(&accepted_settled);
        let reservation = supervisor.reserve().map_err(|error| error.to_string())?;

        supervisor.begin_shutdown();
        assert!(matches!(
            supervisor.reserve(),
            Err(TauriBridgeAdmissionError)
        ));
        let join_supervisor = Arc::clone(&supervisor);
        let join = tokio::spawn(async move { join_supervisor.join().await });
        tokio::task::yield_now().await;
        assert!(!join.is_finished());

        reservation.track(tokio::spawn(async move {
            accepted_cancel.cancelled().await;
            accepted_settled_task.store(true, std::sync::atomic::Ordering::Release);
        }));
        join.await.map_err(|error| error.to_string())??;
        assert!(accepted_settled.load(std::sync::atomic::Ordering::Acquire));
        Ok(())
    }

    #[tokio::test]
    async fn rejected_bridge_reservation_prevents_late_spawn() -> Result<(), String> {
        let supervisor = TauriBridgeSupervisor::new();
        supervisor.begin_shutdown();
        let spawned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(matches!(
            supervisor.reserve(),
            Err(TauriBridgeAdmissionError)
        ));
        assert!(!spawned.load(std::sync::atomic::Ordering::Acquire));
        supervisor.join().await?;
        Ok(())
    }

    #[tokio::test]
    async fn bridge_join_failure_enters_application_lifecycle_receipt() -> Result<(), String> {
        let supervisor = Arc::new(TauriBridgeSupervisor::new());
        let reservation = supervisor.reserve().map_err(|error| error.to_string())?;
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        reservation.track(task);

        let begin = Arc::clone(&supervisor);
        let join = Arc::clone(&supervisor);
        let mut lifecycle = echo_agent_app_core::api::runtime::ApplicationLifecycleOwner::new(
            tokio_util::sync::CancellationToken::new(),
        );
        lifecycle.track_external_owner(
            "Tauri event bridges",
            move || {
                begin.begin_shutdown();
                Ok(())
            },
            async move { join.join().await },
        );
        let receipt = lifecycle
            .settle(
                echo_agent_app_core::api::runtime::ApplicationLifecycleReason::Shutdown,
                None,
            )
            .await;
        assert!(receipt.failures.iter().any(|failure| {
            failure.owner == "Tauri event bridges" && failure.error.contains("cancelled")
        }));
        Ok(())
    }
}
