//! Sensitive path checker — prevents the agent from reading secrets.
//!
//! Maintains a default-deny list of sensitive file patterns. When the
//! agent attempts to read a matching file, the operation requires explicit
//! user approval.

use std::path::Path;

/// Patterns that match sensitive files (default deny).
const SENSITIVE_PATTERNS: &[&str] = &[
    // SSH keys
    "**/.ssh/id_rsa",
    "**/.ssh/id_ed25519",
    "**/.ssh/id_ecdsa",
    "**/.ssh/*_key",
    "**/.ssh/authorized_keys",
    // AWS / cloud credentials
    "**/.aws/credentials",
    "**/.aws/config",
    // API tokens / env
    "**/.env",
    "**/.env.local",
    "**/.env.production",
    // Git credentials
    "**/.git-credentials",
    "**/.gitconfig",
    // Private keys / certs
    "**.pem",
    "**.key",
    "**.pfx",
    "**.p12",
    "**.jks",
    // Docker / container auth
    "**/.docker/config.json",
    // GitHub CLI
    "**/.config/gh/hosts.yml",
    // Netrc
    "**/.netrc",
    // Database configs
    "**/.my.cnf",
    "**/.pgpass",
    // GPG
    "**/.gnupg/secring.gpg",
    "**/.gnupg/private-keys-v1.d/**",
];

/// Check if a path matches any sensitive file pattern.
///
/// Uses simple glob-style matching with `**` for recursive directories
/// and `*` for single-segment wildcards.
pub fn is_sensitive_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let normalized = path_str.replace('\\', "/");

    for pattern in SENSITIVE_PATTERNS {
        if glob_match(pattern, &normalized) {
            return true;
        }
    }
    false
}

/// Check if a path is sensitive and return the pattern that matched.
pub fn sensitive_match(path: &Path) -> Option<&'static str> {
    let path_str = path.to_string_lossy();
    let normalized = path_str.replace('\\', "/");

    for pattern in SENSITIVE_PATTERNS {
        if glob_match(pattern, &normalized) {
            return Some(pattern);
        }
    }
    None
}

/// Simple glob match: `**` matches any depth, `*` matches within one segment.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    let mut pi = 0;
    let mut si = 0;
    let mut star_idx: Option<(usize, usize)> = None;

    while si < path_parts.len() {
        if pi < pattern_parts.len() && pattern_parts[pi] == "**" {
            star_idx = Some((pi + 1, si));
            pi += 1;
        } else if pi < pattern_parts.len()
            && (pattern_parts[pi] == "*" || pattern_parts[pi].eq_ignore_ascii_case(path_parts[si]))
        {
            pi += 1;
            si += 1;
        } else if let Some((p_back, s_back)) = star_idx {
            pi = p_back;
            si = s_back + 1;
            star_idx = Some((p_back, si));
        } else {
            return false;
        }
    }

    while pi < pattern_parts.len() && pattern_parts[pi] == "**" {
        pi += 1;
    }

    pi >= pattern_parts.len()
}

/// Check if a write path escapes the project root (work directory isolation).
/// Returns true if the path is outside the allowed project directory.
pub fn is_outside_project(path: &std::path::Path, project_root: &std::path::Path) -> bool {
    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let canonical_root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    !canonical_path.starts_with(&canonical_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_key_detected() {
        assert!(is_sensitive_path(Path::new("/home/user/.ssh/id_rsa")));
        assert!(is_sensitive_path(Path::new("~/.ssh/id_ed25519")));
        assert!(is_sensitive_path(Path::new(".ssh/id_ecdsa")));
    }

    #[test]
    fn test_env_file_detected() {
        assert!(is_sensitive_path(Path::new(".env")));
        assert!(is_sensitive_path(Path::new("project/.env.local")));
        assert!(is_sensitive_path(Path::new("/app/.env.production")));
    }

    #[test]
    fn test_aws_credentials_detected() {
        assert!(is_sensitive_path(Path::new("~/.aws/credentials")));
    }

    #[test]
    fn test_pem_key_detected() {
        assert!(is_sensitive_path(Path::new("certs/server.pem")));
        assert!(is_sensitive_path(Path::new("secret.key")));
    }

    #[test]
    fn test_normal_files_not_sensitive() {
        assert!(!is_sensitive_path(Path::new("src/main.rs")));
        assert!(!is_sensitive_path(Path::new("Cargo.toml")));
        assert!(!is_sensitive_path(Path::new("README.md")));
        assert!(!is_sensitive_path(Path::new("config/settings.json")));
    }

    #[test]
    fn test_docker_config_detected() {
        assert!(is_sensitive_path(Path::new("~/.docker/config.json")));
    }

    #[test]
    fn test_git_credentials_detected() {
        assert!(is_sensitive_path(Path::new(".git-credentials")));
    }
}
