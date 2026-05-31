//! Server PID file management for cross-process discovery.
//!
//! Writes `{ pid, port, started_at }` to `~/.echo-agent/server.pid`.
//! On startup, checks if an existing server is running. If so, the caller
//! decides whether to connect to it or kill it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerPid {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
}

/// Get the path to the PID file.
fn pid_file_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".echo-agent")
        .join("server.pid")
}

/// Write PID info to disk.
pub fn write_pid(port: u16) -> anyhow::Result<()> {
    let path = pid_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let info = ServerPid {
        pid: std::process::id(),
        port,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&info)?;
    std::fs::write(&path, json)?;
    tracing::debug!(port = port, "Wrote server PID file");
    Ok(())
}

/// Read PID info from disk, if it exists.
pub fn read_pid() -> Option<ServerPid> {
    let path = pid_file_path();
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Remove the PID file.
pub fn cleanup() {
    let path = pid_file_path();
    let _ = std::fs::remove_file(&path);
    tracing::debug!("Cleaned up server PID file");
}
