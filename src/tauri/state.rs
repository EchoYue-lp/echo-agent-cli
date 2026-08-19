//! Tauri shared state wrapper.

use echo_agent_app_core::{AppState, browser::BrowserRuntime};
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
    handles: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl TauriBridgeSupervisor {
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            handles: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn track(&self, handle: JoinHandle<()>) {
        let mut handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        handles.push(handle);
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let handles = {
            let mut handles = self
                .handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *handles)
        };
        for handle in handles {
            if let Err(error) = handle.await {
                tracing::warn!(%error, "Tauri event bridge failed during shutdown");
            }
        }
    }
}

impl Default for TauriBridgeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::TauriBridgeSupervisor;
    use std::sync::Arc;

    #[tokio::test]
    async fn shutdown_cancels_and_joins_tracked_bridges() {
        let supervisor = Arc::new(TauriBridgeSupervisor::new());
        let cancel = supervisor.cancellation_token();
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_in_task = completed.clone();
        supervisor.track(tokio::spawn(async move {
            cancel.cancelled().await;
            completed_in_task.store(true, std::sync::atomic::Ordering::Release);
        }));

        supervisor.shutdown().await;

        assert!(completed.load(std::sync::atomic::Ordering::Acquire));
    }
}
