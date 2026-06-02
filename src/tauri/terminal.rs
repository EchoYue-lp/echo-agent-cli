//! PTY-based terminal for Tauri desktop mode.
//!
//! Architecture:
//! - `PtySession`: wraps a single PTY (shell process + I/O handles)
//! - `TerminalManager`: concurrent map of active sessions
//! - Tauri Events stream PTY output to the frontend:
//!   - `terminal-output` → `{ id, data }` (base64-encoded)
//!   - `terminal-exit` → `{ id }` (process exited)
//! - Frontend uses xterm.js + FitAddon for rendering.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use base64::Engine;
use dashmap::DashMap;
use portable_pty::{CommandBuilder, PtyPair, PtySize};
use serde::Serialize;
use std::io::Write;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── Event payloads ──────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct OutputEvent {
    id: String,
    data: String, // base64-encoded raw bytes
}

#[derive(Clone, Serialize)]
struct ExitEvent {
    id: String,
}

// ── PtySession ──────────────────────────────────────────────────────────────

/// A single terminal session backed by a pseudo-terminal.
pub struct PtySession {
    pub id: String,
    pub pid: u32,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child_killer: Mutex<Box<dyn portable_pty::ChildKiller + Send>>,
}

impl PtySession {
    /// Spawn a new shell in a PTY.
    pub fn spawn(
        id: String,
        cwd: Option<String>,
        rows: u16,
        cols: u16,
        app_handle: tauri::AppHandle,
    ) -> Result<Arc<Self>, String> {
        let pty_system = portable_pty::native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair: PtyPair = pty_system
            .openpty(size)
            .map_err(|e| format!("Failed to open PTY: {e}"))?;

        // Clone reader BEFORE spawning the child (portable-pty requirement)
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {e}"))?;

        // Build shell command — use $SHELL on Unix, fallback to /bin/sh
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        if let Some(ref dir) = cwd {
            cmd.cwd(dir);
        }

        // Spawn child process on the slave side
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {e}"))?;

        // Drop slave — only master side is needed now
        drop(pair.slave);

        let pid = child.process_id().unwrap_or(0);

        // Take writer for stdin
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {e}"))?;

        // Start background reader thread → emit Tauri events
        let session_id = id.clone();
        std::thread::Builder::new()
            .name(format!("pty-reader-{id}"))
            .spawn(move || {
                Self::reader_loop(session_id, reader, app_handle);
            })
            .map_err(|e| format!("Failed to spawn reader thread: {e}"))?;

        info!("Terminal session '{id}' created (pid={pid})");

        Ok(Arc::new(Self {
            id,
            pid,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child_killer: Mutex::new(child.clone_killer()),
        }))
    }

    /// Background loop: read PTY output and emit to frontend via Tauri events.
    fn reader_loop(
        id: String,
        mut reader: Box<dyn std::io::Read + Send>,
        app_handle: tauri::AppHandle,
    ) {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // EOF — shell process exited
                    debug!("Terminal '{id}' EOF, process exited");
                    let _ = app_handle.emit("terminal-exit", ExitEvent { id: id.clone() });
                    break;
                }
                Ok(n) => {
                    // Encode as base64 to safely transport binary data over JSON
                    let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = app_handle.emit(
                        "terminal-output",
                        OutputEvent {
                            id: id.clone(),
                            data,
                        },
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    warn!("Terminal '{id}' read error: {e}");
                    let _ = app_handle.emit("terminal-exit", ExitEvent { id: id.clone() });
                    break;
                }
            }
        }
    }

    /// Write data to the PTY stdin.
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(data)
            .map_err(|e| format!("Write failed: {e}"))?;
        writer.flush().map_err(|e| format!("Flush failed: {e}"))?;
        Ok(())
    }

    /// Resize the PTY.
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let master = self.master.lock().await;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Resize failed: {e}"))
    }

    /// Kill the shell process.
    pub async fn kill(&self) -> Result<(), String> {
        let mut killer = self.child_killer.lock().await;
        killer.kill().map_err(|e| format!("Kill failed: {e}"))
    }
}

// ── TerminalManager ─────────────────────────────────────────────────────────

/// Manages multiple concurrent terminal sessions.
pub struct TerminalManager {
    sessions: DashMap<String, Arc<PtySession>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    pub fn create(
        &self,
        id: String,
        cwd: Option<String>,
        rows: u16,
        cols: u16,
        app_handle: tauri::AppHandle,
    ) -> Result<u32, String> {
        if self.sessions.contains_key(&id) {
            return Err(format!("Terminal '{id}' already exists"));
        }
        let session = PtySession::spawn(id.clone(), cwd, rows, cols, app_handle)?;
        let pid = session.pid;
        self.sessions.insert(id, session);
        Ok(pid)
    }

    pub fn get(&self, id: &str) -> Result<Arc<PtySession>, String> {
        self.sessions
            .get(id)
            .map(|r| r.value().clone())
            .ok_or_else(|| format!("Terminal '{id}' not found"))
    }

    pub fn remove(&self, id: &str) -> Option<Arc<PtySession>> {
        self.sessions.remove(id).map(|(_, s)| s)
    }

    pub fn close_all(&self) {
        let ids: Vec<String> = self.sessions.iter().map(|r| r.key().clone()).collect();
        for id in ids {
            if let Some(session) = self.sessions.remove(&id).map(|(_, s)| s) {
                // Best-effort kill
                tokio::spawn(async move {
                    let _ = session.kill().await;
                });
            }
        }
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tauri IPC Commands ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn create_terminal(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    id: String,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<serde_json::Value, IpcError> {
    let rows = rows.unwrap_or(24);
    let cols = cols.unwrap_or(80);
    let pid = state
        .terminal_manager
        .create(id.clone(), cwd, rows, cols, app)
        .map_err(IpcError::Internal)?;
    Ok(serde_json::json!({ "id": id, "pid": pid }))
}

#[tauri::command]
pub async fn write_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
    data: String,
) -> Result<serde_json::Value, IpcError> {
    let session = state
        .terminal_manager
        .get(&id)
        .map_err(IpcError::NotFound)?;
    // Data is base64-encoded from the frontend
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|e| IpcError::Validation(format!("Invalid base64: {e}")))?;
    session.write(&bytes).await.map_err(IpcError::Internal)?;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn resize_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<serde_json::Value, IpcError> {
    let session = state
        .terminal_manager
        .get(&id)
        .map_err(IpcError::NotFound)?;
    session
        .resize(rows, cols)
        .await
        .map_err(IpcError::Internal)?;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn close_terminal(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let session = state
        .terminal_manager
        .remove(&id)
        .ok_or_else(|| IpcError::NotFound(format!("Terminal '{id}' not found")))?;
    session.kill().await.map_err(IpcError::Internal)?;
    info!("Terminal '{id}' closed");
    Ok(serde_json::json!({"success": true}))
}
