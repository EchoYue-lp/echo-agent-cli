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

/// Read file contents via IPC (low latency).
#[tauri::command]
pub async fn native_read_file(path: String) -> Result<FileReadResult, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("File not found: {}", path));
    }

    let metadata = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if metadata.len() > 5 * 1024 * 1024 {
        return Err("File too large (>5MB)".to_string());
    }

    let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;

    Ok(FileReadResult {
        content,
        size: metadata.len(),
        path,
    })
}

/// Write file contents via IPC.
#[tauri::command]
pub async fn native_write_file(path: String, content: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, content).map_err(|e| e.to_string())
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
