//! Embedded axum server — always starts, even in CLI/TUI mode.
//!
//! Binds to `127.0.0.1:0` (OS-assigned port). Writes the actual port
//! to `~/.echo-agent/server.pid` for cross-process discovery.
//!
//! This ensures all modes (web, cli, tui, tauri) have access to the
//! REST API and background task management.

use crate::server_pid;
use crate::state::AppState;
use echo_agent::agent::CancellationToken;
use std::sync::Arc;

/// Embedded server that always runs in the background.
pub struct EmbeddedServer {
    port: u16,
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl EmbeddedServer {
    /// Start the embedded server on a random local port.
    ///
    /// The `build_router` function is provided by the caller (from
    /// `echo-agent-server`) to avoid circular dependencies.
    pub async fn start<F>(
        state: Arc<AppState>,
        build_router: F,
        cancel: CancellationToken,
    ) -> anyhow::Result<Self>
    where
        F: FnOnce(Arc<AppState>) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::Router> + Send>>,
    {
        let app = build_router(state).await;

        // Bind to random port on localhost
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();

        // Write PID file for cross-process discovery
        if let Err(e) = server_pid::write_pid(port) {
            tracing::warn!("Failed to write server PID file: {e}");
        }

        tracing::info!(port = port, "Embedded server started on 127.0.0.1:{port}");

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { cancel_clone.cancelled().await })
                .await
                .unwrap_or_else(|e| tracing::error!("Embedded server error: {e}"));
        });

        Ok(Self {
            port,
            cancel,
            handle: Some(handle),
        })
    }

    /// Get the port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the base URL for the embedded server.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Shutdown the embedded server gracefully.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;
        }
        server_pid::cleanup();
        tracing::info!("Embedded server shut down");
    }
}

impl Drop for EmbeddedServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        server_pid::cleanup();
    }
}
