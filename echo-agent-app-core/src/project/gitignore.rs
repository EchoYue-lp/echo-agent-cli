//! .gitignore rule loading and path matching.
//!
//! Supports a subset of gitignore syntax: simple globs, directory-only rules
//! (trailing `/`), negation (`!`), and comment/blank line skipping.

use std::path::Path;

/// A parsed set of gitignore rules loaded from a project root.
#[derive(Debug, Clone)]
pub struct GitIgnore {
    /// Patterns to ignore (in the order they appear in .gitignore).
    patterns: Vec<IgnorePattern>,
}

#[derive(Debug, Clone)]
struct IgnorePattern {
    /// The glob text, e.g. `target/`, `*.log`, `!important.log`.
    raw: String,
    /// If `true`, this is a negation rule (`!pattern`).
    negated: bool,
    /// If `true`, this only matches directories (trailing `/`).
    directory_only: bool,
}

impl GitIgnore {
    /// Load gitignore rules from the given directory.
    ///
    /// Reads `.gitignore` at the project root. Returns an empty rule set if
    /// the file is missing or unreadable.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".gitignore");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                return Self {
                    patterns: Vec::new(),
                };
            }
        };
        Self::parse(&content)
    }

    fn parse(content: &str) -> Self {
        let mut patterns = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            // Skip comments and blank lines
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut negated = false;
            let mut directory_only = false;
            let mut raw = trimmed.to_string();

            if raw.starts_with('!') {
                negated = true;
                raw = raw[1..].to_string();
            }
            // Remove leading slash for root-relative patterns
            if raw.starts_with('/') {
                raw = raw[1..].to_string();
            }
            if raw.ends_with('/') {
                directory_only = true;
                raw.pop(); // remove trailing /
            }
            patterns.push(IgnorePattern {
                raw,
                negated,
                directory_only,
            });
        }
        Self { patterns }
    }

    /// Check whether the given relative path should be ignored.
    ///
    /// Returns `true` if the path matches an ignore rule and does NOT match
    /// a subsequent negation rule. The rules are evaluated in order; later
    /// rules override earlier ones (including negation).
    pub fn is_ignored(&self, relative_path: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for pat in &self.patterns {
            if !Self::glob_matches(&pat.raw, relative_path) {
                continue;
            }
            if pat.directory_only && !is_dir {
                continue;
            }
            ignored = !pat.negated;
        }
        ignored
    }

    /// Simple glob matching: supports `*` (any characters except `/`),
    /// `**` (any characters including `/`), and literal text.
    fn glob_matches(pattern: &str, path: &str) -> bool {
        // Exact match
        if pattern == path {
            return true;
        }
        // Pattern ending with `/**` matches everything inside a directory
        if let Some(prefix) = pattern.strip_suffix("/**") {
            if path.starts_with(prefix) {
                return true;
            }
            // Also match the directory itself
            if path == prefix {
                return true;
            }
        }
        // Simple `*` matching (single segment wildcard)
        if pattern.contains('*') && !pattern.contains("**") {
            return simple_glob(pattern, path);
        }
        // `**` in the middle
        if pattern.contains("**") {
            return globstar_match(pattern, path);
        }
        false
    }
}

/// Match a simple glob with single-segment `*` wildcards.
fn simple_glob(pattern: &str, path: &str) -> bool {
    let mut pi = 0;
    let mut pp = 0;
    let path_bytes = path.as_bytes();
    let pat_bytes = pattern.as_bytes();
    let mut star_idx = None;
    let mut match_idx = 0;

    while pi < path_bytes.len() {
        if pp < pat_bytes.len() && pat_bytes[pp] == b'*' {
            star_idx = Some(pp);
            match_idx = pi;
            pp += 1;
        } else if pp < pat_bytes.len() && (pat_bytes[pp] == path_bytes[pi] || pat_bytes[pp] == b'?')
        {
            pi += 1;
            pp += 1;
        } else if let Some(si) = star_idx {
            pp = si + 1;
            match_idx += 1;
            pi = match_idx;
        } else {
            return false;
        }
    }

    // Consume trailing stars
    while pp < pat_bytes.len() && pat_bytes[pp] == b'*' {
        pp += 1;
    }
    pp == pat_bytes.len()
}

/// Match a pattern containing `**` (cross-directory glob).
fn globstar_match(pattern: &str, path: &str) -> bool {
    // Split on `**`
    let parts: Vec<&str> = pattern.split("**").collect();
    if parts.len() == 1 {
        return simple_glob(pattern, path);
    }

    let mut remaining = path;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let is_last = i == parts.len() - 1;
        if is_last {
            return remaining
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(remaining.len()))
                .filter_map(|index| remaining.get(index..))
                .any(|candidate| simple_glob(part, candidate));
        } else {
            // Middle part: find a match, then continue
            let mut found = false;
            for j in remaining
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(remaining.len()))
            {
                let Some(candidate) = remaining.get(j..) else {
                    continue;
                };
                if simple_glob(part, candidate) {
                    let next = j.saturating_add(part.len()).min(remaining.len());
                    let Some(suffix) = remaining.get(next..) else {
                        continue;
                    };
                    remaining = suffix;
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let gi = GitIgnore::parse("target/\n*.log\n!important.log\n# comment\n");
        // target/, *.log, !important.log = 3 patterns (comment + trailing blank skipped)
        assert_eq!(gi.patterns.len(), 3);
        assert!(
            gi.patterns
                .iter()
                .any(|p| p.raw == "target" && p.directory_only)
        );
        assert!(gi.patterns.iter().any(|p| p.raw == "*.log" && !p.negated));
        assert!(
            gi.patterns
                .iter()
                .any(|p| p.raw == "important.log" && p.negated)
        );
    }

    #[test]
    fn globstar_handles_multibyte_paths_without_panicking() {
        assert!(globstar_match("**/*.rs", "源码/模块/main.rs"));
        assert!(!globstar_match("**/*.md", "源码/模块/main.rs"));
    }

    #[test]
    fn test_is_ignored_directory() {
        let gi = GitIgnore::parse("target/\nnode_modules/\n");
        assert!(gi.is_ignored("target", true));
        assert!(!gi.is_ignored("target", false)); // file named "target" — not ignored
        assert!(gi.is_ignored("node_modules", true));
    }

    #[test]
    fn test_is_ignored_glob() {
        let gi = GitIgnore::parse("*.log\n");
        assert!(gi.is_ignored("debug.log", false));
        assert!(gi.is_ignored("error.log", false));
        assert!(!gi.is_ignored("main.rs", false));
        assert!(gi.is_ignored("logs/error.log", false));
    }

    #[test]
    fn test_negation() {
        let gi = GitIgnore::parse("*.log\n!important.log\n");
        assert!(gi.is_ignored("debug.log", false));
        assert!(!gi.is_ignored("important.log", false));
    }

    #[test]
    fn test_globstar() {
        let gi = GitIgnore::parse("target/**\n");
        assert!(gi.is_ignored("target", true));
        assert!(gi.is_ignored("target/debug", true));
        assert!(gi.is_ignored("target/debug/build", true));
        assert!(!gi.is_ignored("src/main.rs", false));
    }
}
