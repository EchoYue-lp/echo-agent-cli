//! Tauri IPC commands.
//!
//! Critical paths use IPC (low latency, native experience):
//! - File read / write
//! - System notifications
//! - System info

use serde::Serialize;
use std::path::PathBuf;

/// File read result returned to the frontend.
#[derive(Debug, Serialize)]
pub struct FileReadResult {
    pub content: String,
    pub size: u64,
    pub path: String,
}

/// Validate that an IPC path is within allowed directories (user home).
///
/// This prevents arbitrary file read/write outside the user's home directory
/// via compromised or malicious frontend code.
fn validate_ipc_path(path: &str, must_exist: bool) -> Result<PathBuf, String> {
    // Reject empty paths
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }

    let requested = PathBuf::from(path);

    // Determine the allowed base directory (user home)
    let base = dirs_home_path().ok_or_else(|| "Cannot determine home directory".to_string())?;

    if must_exist {
        // For read operations: file must exist, canonicalize resolves symlinks and `..`
        if !requested.exists() {
            return Err(format!("File not found: {}", path));
        }
        let canonical = requested
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?;
        let canonical_base = base
            .canonicalize()
            .map_err(|e| format!("Cannot resolve home directory: {}", e))?;

        if !canonical.starts_with(&canonical_base) {
            return Err("Access denied: path is outside the home directory".to_string());
        }
        Ok(canonical)
    } else {
        // For write operations: canonicalize the parent directory (file may not exist yet)
        let parent = requested
            .parent()
            .ok_or_else(|| "Path has no parent directory".to_string())?;

        // If parent doesn't exist, try to find the nearest existing ancestor
        let mut check = parent;
        while !check.exists() {
            match check.parent() {
                Some(p) if p != check => check = p,
                _ => return Err(format!("Cannot resolve parent path: {}", parent.display())),
            }
        }

        let canonical_parent = check
            .canonicalize()
            .map_err(|e| format!("Cannot resolve parent path: {}", e))?;
        let canonical_base = base
            .canonicalize()
            .map_err(|e| format!("Cannot resolve home directory: {}", e))?;

        if !canonical_parent.starts_with(&canonical_base) {
            return Err("Access denied: path is outside the home directory".to_string());
        }

        // Reconstruct the full path using the canonical parent
        let suffix = requested
            .strip_prefix(parent)
            .map_err(|_| "Cannot compute relative suffix".to_string())?;
        Ok(canonical_parent.join(suffix))
    }
}

fn dirs_home_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Read file contents via IPC (low latency).
#[tauri::command]
pub async fn native_read_file(path: String) -> Result<FileReadResult, String> {
    let validated = validate_ipc_path(&path, true)?;

    let metadata = std::fs::metadata(&validated).map_err(|e| e.to_string())?;
    if metadata.len() > 5 * 1024 * 1024 {
        return Err("File too large (>5MB)".to_string());
    }

    let content = std::fs::read_to_string(&validated).map_err(|e| e.to_string())?;

    Ok(FileReadResult {
        content,
        size: metadata.len(),
        path,
    })
}

/// Write file contents via IPC.
#[tauri::command]
pub async fn native_write_file(path: String, content: String) -> Result<(), String> {
    let validated = validate_ipc_path(&path, false)?;

    if let Some(parent) = validated.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&validated, content).map_err(|e| e.to_string())
}

/// Send a system notification via IPC.
#[tauri::command]
pub async fn native_notify(
    app: tauri::AppHandle,
    title: String,
    body: String,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
        .map_err(|e| e.to_string())
}

/// Return basic system information.
#[tauri::command]
pub fn get_system_info() -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        home_dir: dirs_home(),
    }
}

#[derive(Debug, Serialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub home_dir: String,
}

fn dirs_home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "~".to_string())
}
