//! Unified path validation for the Tauri IPC layer.
//!
//! Consolidates the three legacy validators (`validate_ipc_path`,
//! `validate_path_within_base`, `validate_workspace_root`) onto the canonical
//! [`echo_tools::security::PathValidator`] (6-7). The secret-denylist logic
//! (`.ssh`, `.aws`, cookie/history files) — which `PathValidator` does not
//! provide — stays here as a thin wrapper layer, since it is specific to the
//! IPC threat model (XSS exfiltrating credentials via `native_read_file`).

use std::path::{Component, Path, PathBuf};

use echo_tools::security::PathValidator;

// ── Secret denylist ────────────────────────────────────────────────

/// Sub-paths under the user home that are denied by the IPC path validator.
const DENIED_HOME_SUBPATHS: &[&str] = &[
    ".ssh",
    ".aws",
    ".config/gh",
    ".docker",
    ".gnupg",
    ".kube",
    ".netrc",
    ".npmrc",
    ".pypirc",
];

/// Filename substrings that denote history/cookie files.
const DENIED_FILENAME_CONTAINS: &[&str] = &["history", "cookies", "cookie"];

/// Return the portion of `path` relative to `home` as a lowercase string.
fn relative_under_home(path: &Path, home: &Path) -> Option<String> {
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
fn is_denied_secret_path(rel: &str) -> bool {
    let rel = rel.to_ascii_lowercase();
    for sub in DENIED_HOME_SUBPATHS {
        if rel == *sub || rel.starts_with(&format!("{}/", sub)) {
            return true;
        }
    }
    for needle in DENIED_FILENAME_CONTAINS {
        if rel.contains(needle) {
            return true;
        }
    }
    false
}

/// Return the user home directory.
fn dirs_home_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

// ── Unified validators ─────────────────────────────────────────────

/// Validate a path for IPC file read/write, confined to the user home with
/// secret-path denylist enforcement.
///
/// Replaces the legacy `validate_ipc_path`. Delegates base/`..`/canonical
/// logic to `PathValidator::validate_within_base`, then applies the secret
/// denylist on top.
///
/// - `must_exist`: if true, the path must already exist (for read ops).
/// - Rejects `..` components lexically (before canonicalization).
/// - Rejects paths in `~/.ssh`, `~/.aws`, etc., or files containing
///   "history"/"cookies".
pub fn validate_ipc_path(path: &str, must_exist: bool) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }

    let requested = PathBuf::from(path);
    // Reject any literal `..` component outright.
    for comp in requested.components() {
        if matches!(comp, Component::ParentDir) {
            return Err("Path traversal (..) is not allowed".to_string());
        }
    }

    let home = dirs_home_path().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let validator = PathValidator::new();

    let resolved = validator
        .validate_within_base(path, &home)
        .map_err(|e| format!("Path validation failed: {}", e))?;

    if must_exist && !resolved.exists() {
        return Err(format!("File not found: {}", path));
    }

    // Secret denylist check on the resolved real path.
    if let Some(rel) = relative_under_home(&resolved, &home) {
        if is_denied_secret_path(&rel) {
            return Err("Access denied: path is in a protected credential location".to_string());
        }
    }

    Ok(resolved)
}

/// Validate a path is within a given base directory (e.g. workspace root).
///
/// Replaces the legacy `validate_path_within_base`. Uses
/// `PathValidator::validate_within_base` directly — no secret denylist (the
/// caller controls the base; workspace files are not credentials).
pub fn validate_within_base(path: &Path, base: &Path) -> Result<PathBuf, String> {
    let validator = PathValidator::new();
    let path_str = path.to_string_lossy();
    validator
        .validate_within_base(&path_str, base)
        .map_err(|e| format!("Path validation failed: {}", e))
}

/// Validate a workspace root path is within the user home.
///
/// Replaces the legacy `validate_workspace_root`. Also applies the secret
/// denylist (a workspace root under `~/.ssh` is nonsensical and dangerous).
pub fn validate_workspace_root(root: &str) -> Result<(), String> {
    if root.trim().is_empty() {
        return Err("Workspace root cannot be empty".to_string());
    }
    let home = dirs_home_path().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let resolved = validate_ipc_path(root, false)?;
    // Extra safety: even though validate_ipc_path checks secret paths, double
    // check the resolved path is under home (belt and suspenders).
    if !resolved.starts_with(&home) {
        return Err("Workspace root must be within the home directory".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_denied_secret_paths_match() {
        assert!(is_denied_secret_path(".ssh"));
        assert!(is_denied_secret_path(".ssh/id_rsa"));
        assert!(is_denied_secret_path(".aws/credentials"));
        assert!(is_denied_secret_path(".zsh_history"));
        assert!(!is_denied_secret_path("projects/myapp/src/main.rs"));
        assert!(!is_denied_secret_path(".echo-agent/memory.md"));
    }

    #[test]
    fn test_deny_list_case_insensitive() {
        assert!(is_denied_secret_path(".SSH/ID_RSA"));
        assert!(is_denied_secret_path(".Aws/Credentials"));
    }

    #[test]
    fn test_traversal_rejected_lexically() {
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
}
