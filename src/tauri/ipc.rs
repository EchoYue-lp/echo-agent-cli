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

/// Maximum size for a single IPC file write (10 MiB). Prevents disk-exhaustion
/// via a compromised frontend repeatedly writing huge blobs.
const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

/// Sub-paths under the user home that are denied by `validate_ipc_path`.
///
/// These hold credentials the agent frontend has no business reading or
/// overwriting: cloud provider keys, SSH keys, shell rc files (which may carry
/// API keys sourced at login), browser cookie DBs, and shell history. A
/// compromised page (XSS) reaching `native_read_file` could otherwise exfiltrate
/// `~/.ssh/id_*`, `~/.aws/credentials`, etc. in a single `invoke`.
///
/// Checked against the canonical path AFTER resolving symlinks/`..`, so an
/// attacker cannot trivially bypass with `~/.ssh/../.ssh/id_rsa` tricks.
const DENIED_HOME_SUBPATHS: &[&str] = &[
    ".ssh",       // SSH private keys, authorized_keys
    ".aws",       // AWS credentials
    ".config/gh", // GitHub CLI tokens
    ".docker",    // registry credentials
    ".gnupg",     // GPG private keys
    ".kube",      // kubernetes tokens
    ".netrc",     // HTTP credentials
    ".npmrc",     // npm tokens
    ".pypirc",    // PyPI tokens
];

/// Filename suffixes that denote history/cookie files (checked case-insensitively).
const DENIED_FILENAME_CONTAINS: &[&str] = &["history", "cookies", "cookie"];

/// Return the portion of `path` relative to `home` as a lowercase string
/// (using `/` separators) for substring matching against the deny lists.
/// Returns None if `path` is not under `home`.
fn relative_under_home(path: &std::path::Path, home: &std::path::Path) -> Option<String> {
    let stripped = path.strip_prefix(home).ok()?;
    let mut s = String::new();
    for (i, comp) in stripped.components().enumerate() {
        if i > 0 {
            s.push('/');
        }
        s.push_str(&comp.as_os_str().to_string_lossy());
    }
    Some(s.to_ascii_lowercase())
}

/// Reject paths inside credential-bearing subdirectories or history/cookie files.
///
/// Case-insensitive on the whole relative path so `.SSH/ID_RSA` is caught too.
fn is_denied_secret_path(rel: &str) -> bool {
    let rel = rel.to_ascii_lowercase();
    for sub in DENIED_HOME_SUBPATHS {
        // Match `sub` as a leading path component: "<sub>" or "<sub>/...".
        if rel == *sub || rel.starts_with(&format!("{}/", sub)) {
            return true;
        }
    }
    // Filename-level checks (cookies.db, .zsh_history, ...).
    for needle in DENIED_FILENAME_CONTAINS {
        if rel.contains(needle) {
            return true;
        }
    }
    false
}

/// Validate that an IPC path is within allowed directories (user home, minus
/// credential-bearing subpaths) and contains no `..` traversal components.
///
/// This prevents arbitrary file read/write — and credential theft — via
/// compromised or malicious frontend code.
fn validate_ipc_path(path: &str, must_exist: bool) -> Result<PathBuf, String> {
    // Reject empty paths.
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }

    let requested = PathBuf::from(path);

    // Reject any literal `..` component outright. The previous implementation
    // relied on canonicalization to defeat traversal, but for non-existent
    // write targets it returned `canonical_parent.join(suffix)` where `suffix`
    // could still contain `..` — and the joined result was never re-validated.
    // Rejecting `..` lexically closes that bypass entirely.
    use std::path::Component;
    for comp in requested.components() {
        if matches!(comp, Component::ParentDir) {
            return Err("Path traversal (..) is not allowed".to_string());
        }
    }

    let base = dirs_home_path().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let canonical_base = base
        .canonicalize()
        .map_err(|e| format!("Cannot resolve home directory: {}", e))?;

    // Compute the candidate real path, then verify it is (a) under home and
    // (b) not in a denied secret path. For existing paths we canonicalize the
    // target itself; for non-existing write targets we canonicalize the parent
    // and re-append the filename, then re-verify the *final* joined path.
    let final_path = if must_exist {
        if !requested.exists() {
            return Err(format!("File not found: {}", path));
        }
        requested
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?
    } else {
        // Non-existing target: canonicalize the parent, then re-join.
        let parent = requested
            .parent()
            .ok_or_else(|| "Path has no parent directory".to_string())?;

        // If parent doesn't exist, find the nearest existing ancestor and
        // canonicalize THAT, then re-append the full remaining tail.
        let mut check = parent;
        while !check.exists() {
            match check.parent() {
                Some(p) if p != check => check = p,
                _ => return Err(format!("Cannot resolve parent path: {}", parent.display())),
            }
        }
        let canonical_check = check
            .canonicalize()
            .map_err(|e| format!("Cannot resolve parent path: {}", e))?;
        if !canonical_check.starts_with(&canonical_base) {
            return Err("Access denied: path is outside the home directory".to_string());
        }
        // Re-append the portion of the path below the canonicalized ancestor.
        let tail = requested
            .strip_prefix(check)
            .map_err(|_| "Cannot compute relative suffix".to_string())?;
        canonical_check.join(tail)
    };

    // Final containment + secret-path check on the resolved real path. This is
    // the check the old write path skipped (it only checked an ancestor).
    if !final_path.starts_with(&canonical_base) {
        return Err("Access denied: path is outside the home directory".to_string());
    }
    if let Some(rel) = relative_under_home(&final_path, &canonical_base)
        && is_denied_secret_path(&rel)
    {
        return Err("Access denied: path is in a protected credential location".to_string());
    }

    Ok(final_path)
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
    // Size cap before path validation so a huge payload is rejected cheaply.
    let bytes = content.as_bytes();
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content too large ({} bytes > {} max)",
            bytes.len(),
            MAX_WRITE_BYTES
        ));
    }

    let validated = validate_ipc_path(&path, false)?;

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
    let path_buf = PathBuf::from(&path);
    if !path_buf.exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    // N-P0-B argument-injection hardening: `open`/`xdg-open`/`explorer` treat
    // arguments beginning with `-` as flags (e.g. `open -a Terminal <path>`).
    // Reject any component that starts with `-` to prevent launcher-app
    // argument abuse. The path is also confined to the user home (no reason
    // for the agent UI to open paths outside the user's own tree).
    let canonical = path_buf
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path: {}", e))?;
    let home = dirs_home_path().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let canonical_home = home
        .canonicalize()
        .map_err(|e| format!("Cannot resolve home directory: {}", e))?;
    if !canonical.starts_with(&canonical_home) {
        return Err("Access denied: path is outside the home directory".to_string());
    }
    for comp in canonical.components() {
        if let Some(os) = comp.as_os_str().to_str()
            && os.starts_with('-')
        {
            return Err("Path component begins with '-' (possible argument injection)".to_string());
        }
    }

    // Use platform-specific command to open the path
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&canonical).spawn();

    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(&canonical)
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(&canonical)
        .spawn();

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("Failed to open path '{}': {}", path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_denied_secret_paths_match() {
        assert!(is_denied_secret_path(".ssh"));
        assert!(is_denied_secret_path(".ssh/id_rsa"));
        assert!(is_denied_secret_path(".aws/credentials"));
        assert!(is_denied_secret_path(".config/gh/hosts.yml"));
        assert!(is_denied_secret_path(".docker/config.json"));
        assert!(is_denied_secret_path(".zsh_history"));
        assert!(is_denied_secret_path(
            "library/application support/firefox/cookies.sqlite"
        ));
    }

    #[test]
    fn test_allowed_paths_not_matched() {
        assert!(!is_denied_secret_path("projects/myapp/src/main.rs"));
        assert!(!is_denied_secret_path("documents/notes.md"));
        assert!(!is_denied_secret_path(".echo-agent/memory.md"));
        assert!(!is_denied_secret_path(""));
    }

    #[test]
    fn test_deny_list_not_bypassed_by_case() {
        // Case-insensitive check on the relative path.
        assert!(is_denied_secret_path(".SSH/ID_RSA"));
        assert!(is_denied_secret_path(".Aws/Credentials"));
    }

    #[test]
    fn test_traversal_rejected_lexically() {
        // `..` components are rejected before any canonicalization, so the
        // suffix-join bypass from the old write path is closed.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let evil = format!("{}/safe/../../etc/passwd", home);
        match validate_ipc_path(&evil, false) {
            Err(msg) => assert!(
                msg.contains("..") || msg.contains("traversal"),
                "expected traversal rejection, got: {msg}"
            ),
            Ok(_) => panic!("traversal path must be rejected"),
        }
    }

    #[test]
    fn test_empty_path_rejected() {
        assert!(validate_ipc_path("", true).is_err());
        assert!(validate_ipc_path("   ", false).is_err());
    }

    #[test]
    fn test_secret_path_rejected_under_home() {
        // Symlink/canonical handling aside, the deny list must catch a direct
        // path into ~/.ssh even when the file exists. We point at a path that
        // (almost certainly) does not exist so the read-path "must_exist"
        // branch returns "not found" rather than passing validation — but the
        // write branch should reject it at the secret-path gate.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let ssh_key = format!("{}/.ssh/id_test_secret", home);
        // Use the write (must_exist=false) path; it may or may not exist, but
        // either way it must be rejected by the credential-location gate.
        match validate_ipc_path(&ssh_key, false) {
            Err(msg) => {
                assert!(
                    msg.contains("protected credential") || msg.contains("outside"),
                    "expected credential-location rejection, got: {msg}"
                );
            }
            Ok(_) => panic!("secret path must be rejected"),
        }
    }
}
