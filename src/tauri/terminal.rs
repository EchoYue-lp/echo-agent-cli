//! Thin Tauri adapter over the application-owned terminal service.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use base64::Engine;
use echo_agent_app_core::terminal::{TerminalEvent, TerminalExitReason, TerminalService};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Emitter;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
struct OutputEvent {
    id: String,
    data: String,
}

#[derive(Clone, Serialize)]
struct ExitEvent {
    id: String,
    reason: &'static str,
}

pub fn spawn_event_bridge(
    app_handle: tauri::AppHandle,
    terminal: Arc<TerminalService>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let mut events = terminal.subscribe();
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                event = events.recv() => event,
            };
            match event {
                Ok(TerminalEvent::Output { id, bytes }) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    if let Err(error) = app_handle.emit("terminal-output", OutputEvent { id, data })
                    {
                        tracing::warn!(%error, "failed to emit terminal output");
                    }
                }
                Ok(TerminalEvent::Exited { id, reason }) => {
                    let reason = match reason {
                        TerminalExitReason::ProcessExited => "process_exited",
                        TerminalExitReason::Closed => "closed",
                        TerminalExitReason::ReadFailed(_) => "read_failed",
                    };
                    if let Err(error) = app_handle.emit("terminal-exit", ExitEvent { id, reason }) {
                        tracing::warn!(%error, "failed to emit terminal exit");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "terminal event receiver lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[tauri::command]
pub async fn create_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<serde_json::Value, IpcError> {
    // This is a direct user-operated local developer tool, not an agent-auto
    // execution path, so agent permission modes do not gate it.
    let info = state
        .app_state
        .terminal
        .create(
            id,
            cwd.map(PathBuf::from),
            rows.unwrap_or(24),
            cols.unwrap_or(80),
        )
        .await
        .map_err(IpcError::Internal)?;
    serde_json::to_value(info).map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn write_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
    data: String,
) -> Result<serde_json::Value, IpcError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| IpcError::Validation(format!("invalid base64: {error}")))?;
    state
        .app_state
        .terminal
        .write(&id, &bytes)
        .await
        .map_err(IpcError::Internal)?;
    Ok(serde_json::json!({ "success": true }))
}

#[tauri::command]
pub async fn resize_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<serde_json::Value, IpcError> {
    state
        .app_state
        .terminal
        .resize(&id, rows, cols)
        .await
        .map_err(IpcError::Internal)?;
    Ok(serde_json::json!({ "success": true }))
}

#[tauri::command]
pub async fn close_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let closed = state
        .app_state
        .terminal
        .close(&id)
        .await
        .map_err(IpcError::Internal)?;
    if !closed {
        return Err(IpcError::NotFound(format!("terminal '{id}' not found")));
    }
    Ok(serde_json::json!({ "success": true }))
}

#[tauri::command]
pub async fn list_terminal_sessions(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    serde_json::to_value(state.app_state.terminal.list())
        .map_err(|error| IpcError::Internal(error.to_string()))
}
