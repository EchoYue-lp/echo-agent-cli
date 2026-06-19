//! Tauri IPC commands.
//!
//! Critical paths use IPC (low latency, native experience):
//! - File read / write
//! - System notifications
//! - System info
//!
//! Path validation is centralized in [`crate::tauri::path_validator`].

use serde::Serialize;

use super::path_validator;

/// File read result returned to the frontend.
#[derive(Debug, Serialize)]
pub struct FileReadResult {
    pub content: String,
    pub size: u64,
    pub path: String,
}

/// Maximum size for a single IPC file write (10 MiB). Prevents disk-exhaustion
/// via a compromised frontend repeatedly writing huge blobs.
const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

/// Read file contents via IPC (low latency).
#[tauri::command]
pub async fn native_read_file(path: String) -> Result<FileReadResult, String> {
    let validated = path_validator::validate_ipc_path(&path, true)?;

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
    // Size cap before path validation so a huge payload is rejected cheaply.
    let bytes = content.as_bytes();
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content too large ({} bytes > {} max)",
            bytes.len(),
            MAX_WRITE_BYTES
        ));
    }

    let validated = path_validator::validate_ipc_path(&path, false)?;

    if let Some(parent) = validated.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // Atomic write: temp + rename, so a crash mid-write cannot leave a torn or
    // empty target file (matches the discipline used in echo-agent state files).
    let tmp = validated.with_extension("tmp_write");
    std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &validated).map_err(|e| {
        // Best-effort cleanup of the temp file on rename failure.
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
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

/// Open a path in the system file explorer / default application.
#[tauri::command]
pub fn native_open_path(path: String) -> Result<(), String> {
    // Validate via the unified path validator (home confinement + secret
    // denylist). `must_exist=true` because we're opening an existing path.
    let validated = path_validator::validate_ipc_path(&path, true)?;

    // N-P0-B argument-injection hardening: `open`/`xdg-open`/`explorer` treat
    // arguments beginning with `-` as flags (e.g. `open -a Terminal <path>`).
    for comp in validated.components() {
        if let Some(os) = comp.as_os_str().to_str()
            && os.starts_with('-')
        {
            return Err("Path component begins with '-' (possible argument injection)".to_string());
        }
    }

    // Use platform-specific command to open the path
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&validated).spawn();

    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(&validated)
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(&validated)
        .spawn();

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("Failed to open path '{}': {}", path, e)),
    }
}
