//! Diff rendering for file edit preview.
//!
//! Generates unified diffs with ANSI color support for terminal output
//! and HTML diff for web/Tauri frontend. Uses the `similar` crate for
//! the core diff algorithm and wraps it with rich rendering types.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const WORKSPACE_DIFF_CONTEXT_LINES: usize = 3;

// ── Types ────────────────────────────────────────────────────────────────

/// A single diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    /// Starting line in the old file (1-based)
    pub old_start: usize,
    /// Number of lines in old file
    pub old_count: usize,
    /// Starting line in the new file (1-based)
    pub new_start: usize,
    /// Number of lines in new file
    pub new_count: usize,
    /// Lines of the hunk (prefixed with ' ', '+', or '-')
    pub lines: Vec<DiffLine>,
}

/// A single line in a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    pub old_line_num: Option<usize>,
    pub new_line_num: Option<usize>,
}

/// The kind of a diff line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Unchanged line (prefix: ' ')
    Context,
    /// New line (prefix: '+')
    Added,
    /// Deleted line (prefix: '-')
    Removed,
}

/// Full diff result for a file edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub file_path: String,
    pub hunks: Vec<DiffHunk>,
    pub stats: DiffStats,
}

/// Summary statistics for a diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffStats {
    pub lines_added: usize,
    pub lines_removed: usize,
    pub lines_changed: usize,
}

/// Workspace-scoped comparison against one verified Git commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileDiff {
    pub path: String,
    pub old_content: String,
    pub new_content: String,
    pub hunks: Vec<WorkspaceDiffHunk>,
}

/// Stable GUI wire projection of an application-owned diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<WorkspaceDiffLine>,
}

/// Stable GUI wire projection of one structured diff line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDiffLine {
    pub tag: WorkspaceDiffTag,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceDiffTag {
    Equal,
    Insert,
    Delete,
}

impl From<DiffHunk> for WorkspaceDiffHunk {
    fn from(hunk: DiffHunk) -> Self {
        Self {
            old_start: hunk.old_start,
            old_count: hunk.old_count,
            new_start: hunk.new_start,
            new_count: hunk.new_count,
            lines: hunk
                .lines
                .into_iter()
                .map(WorkspaceDiffLine::from)
                .collect(),
        }
    }
}

impl From<DiffLine> for WorkspaceDiffLine {
    fn from(line: DiffLine) -> Self {
        let tag = match line.kind {
            DiffLineKind::Context => WorkspaceDiffTag::Equal,
            DiffLineKind::Added => WorkspaceDiffTag::Insert,
            DiffLineKind::Removed => WorkspaceDiffTag::Delete,
        };
        Self {
            tag,
            old_line: line.old_line_num,
            new_line: line.new_line_num,
            content: line.content,
        }
    }
}

/// Failure classification retained across application and surface adapters.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceDiffError {
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Operation(String),
}

/// EKO workspace/ref-aware diff service.
///
/// The caller resolves and pins the workspace generation. This service owns
/// path/ref validation, Git reads, and structured diff generation so surfaces
/// do not implement their own TextDiff algorithms.
#[derive(Debug, Clone)]
pub struct WorkspaceDiffService {
    project_root: PathBuf,
}

impl WorkspaceDiffService {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    pub fn diff_file(
        &self,
        path: &str,
        git_ref: &str,
    ) -> Result<WorkspaceFileDiff, WorkspaceDiffError> {
        validate_git_ref(git_ref)?;
        let (target, repository_path) = resolve_workspace_path(&self.project_root, path)?;
        let new_content = if target.exists() {
            std::fs::read_to_string(&target).map_err(|error| {
                WorkspaceDiffError::Operation(format!(
                    "failed to read workspace text file '{}': {error}",
                    target.display()
                ))
            })?
        } else {
            String::new()
        };

        let commit = run_git(
            &self.project_root,
            ["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")],
            "git rev-parse",
        )?;
        if !commit.status.success() {
            return Err(WorkspaceDiffError::Validation(format!(
                "invalid git reference: {}",
                String::from_utf8_lossy(&commit.stderr).trim()
            )));
        }

        let object = format!("{git_ref}:{repository_path}");
        let exists = run_git(
            &self.project_root,
            ["cat-file", "-e", object.as_str()],
            "git cat-file",
        )?;
        let old_content = if exists.status.success() {
            let output = run_git(&self.project_root, ["show", object.as_str()], "git show")?;
            if !output.status.success() {
                return Err(WorkspaceDiffError::Operation(format!(
                    "git show failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            String::from_utf8(output.stdout).map_err(|error| {
                WorkspaceDiffError::Validation(format!("git object is not UTF-8 text: {error}"))
            })?
        } else {
            String::new()
        };

        let diff = generate_unified_diff(
            path,
            &old_content,
            &new_content,
            WORKSPACE_DIFF_CONTEXT_LINES,
        );
        Ok(WorkspaceFileDiff {
            path: path.to_string(),
            old_content,
            new_content,
            hunks: diff
                .hunks
                .into_iter()
                .map(WorkspaceDiffHunk::from)
                .collect(),
        })
    }
}

fn validate_git_ref(git_ref: &str) -> Result<(), WorkspaceDiffError> {
    if git_ref.is_empty()
        || git_ref.starts_with('-')
        || git_ref.starts_with('.')
        || git_ref.contains("..")
        || !git_ref.chars().all(|character| {
            matches!(
                character,
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '/'
            )
        })
    {
        return Err(WorkspaceDiffError::Validation(
            "invalid git reference".to_string(),
        ));
    }
    Ok(())
}

fn resolve_workspace_path(
    project_root: &Path,
    path: &str,
) -> Result<(PathBuf, String), WorkspaceDiffError> {
    if path.trim().is_empty() {
        return Err(WorkspaceDiffError::Validation(
            "workspace diff path cannot be empty".to_string(),
        ));
    }
    let canonical_root = project_root.canonicalize().map_err(|error| {
        WorkspaceDiffError::Operation(format!(
            "failed to resolve workspace root '{}': {error}",
            project_root.display()
        ))
    })?;
    let requested = canonical_root.join(path);
    echo_agent::tools::security::PathValidator::new()
        .validate_within_base(&requested.to_string_lossy(), &canonical_root)
        .map_err(|error| WorkspaceDiffError::Validation(error.to_string()))?;
    let relative = requested.strip_prefix(&canonical_root).map_err(|_| {
        WorkspaceDiffError::Validation("workspace diff path escapes project root".to_string())
    })?;
    let repository_path = relative
        .components()
        .map(|component| {
            component.as_os_str().to_str().ok_or_else(|| {
                WorkspaceDiffError::Validation("workspace diff path is not valid UTF-8".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("/");
    if repository_path.is_empty() {
        return Err(WorkspaceDiffError::Validation(
            "workspace diff path must identify a file".to_string(),
        ));
    }
    Ok((requested, repository_path))
}

fn run_git<const N: usize>(
    project_root: &Path,
    args: [&str; N],
    operation: &str,
) -> Result<Output, WorkspaceDiffError> {
    Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .map_err(|error| WorkspaceDiffError::Operation(format!("{operation} failed: {error}")))
}

// ── Diff Generation ──────────────────────────────────────────────────────

/// Generate a unified diff between old and new content.
///
/// Uses the `similar` crate's patience diff algorithm for high-quality
/// results. The `context_lines` parameter controls how many unchanged
/// lines are shown around each change (default: 3).
pub fn generate_unified_diff(
    file_path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> FileDiff {
    let diff = TextDiff::from_lines(old_content, new_content);

    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut lines_added: usize = 0;
    let mut lines_removed: usize = 0;

    // Group changes into hunks with context
    let groups = diff.grouped_ops(context_lines);

    for group in &groups {
        let mut hunk_lines: Vec<DiffLine> = Vec::new();
        let mut old_count = 0usize;
        let mut new_count = 0usize;

        // Compute hunk start from first op in group
        let old_start = group.first().map(|op| op.old_range().start).unwrap_or(0) + 1;
        let new_start = group.first().map(|op| op.new_range().start).unwrap_or(0) + 1;

        for op in group {
            for change in diff.iter_changes(op) {
                let (kind, content, old_num, new_num) = match change.tag() {
                    ChangeTag::Equal => {
                        let on = change.old_index().map(|i| i + 1);
                        let nn = change.new_index().map(|i| i + 1);
                        (
                            DiffLineKind::Context,
                            change.value().trim_end_matches('\n').to_string(),
                            on,
                            nn,
                        )
                    }
                    ChangeTag::Delete => {
                        lines_removed += 1;
                        let on = change.old_index().map(|i| i + 1);
                        (
                            DiffLineKind::Removed,
                            change.value().trim_end_matches('\n').to_string(),
                            on,
                            None,
                        )
                    }
                    ChangeTag::Insert => {
                        lines_added += 1;
                        let nn = change.new_index().map(|i| i + 1);
                        (
                            DiffLineKind::Added,
                            change.value().trim_end_matches('\n').to_string(),
                            None,
                            nn,
                        )
                    }
                };

                if kind != DiffLineKind::Added {
                    old_count += 1;
                }
                if kind != DiffLineKind::Removed {
                    new_count += 1;
                }

                hunk_lines.push(DiffLine {
                    kind,
                    content,
                    old_line_num: old_num,
                    new_line_num: new_num,
                });
            }
        }

        hunks.push(DiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: hunk_lines,
        });
    }

    FileDiff {
        file_path: file_path.to_string(),
        hunks,
        stats: DiffStats {
            lines_added,
            lines_removed,
            lines_changed: 0,
        },
    }
}

/// Generate a FileDiff from a raw unified diff string (e.g. from `similar::udiff`).
///
/// Parses the unified diff format back into structured hunks and lines.
pub fn parse_unified_diff(file_path: &str, diff_text: &str) -> FileDiff {
    let mut hunks = Vec::new();
    let mut lines_added = 0usize;
    let mut lines_removed = 0usize;

    let mut current_hunk_lines: Vec<DiffLine> = Vec::new();
    let mut hunk_old_start = 0usize;
    let mut hunk_new_start = 0usize;
    let mut hunk_old_count = 0usize;
    let mut hunk_new_count = 0usize;
    let mut in_hunk = false;
    let mut old_line = 0usize;
    let mut new_line = 0usize;

    for raw_line in diff_text.lines() {
        // Skip file headers
        if raw_line.starts_with("---") || raw_line.starts_with("+++") {
            continue;
        }

        // Hunk header: @@ -old_start,old_count +new_start,new_count @@
        if raw_line.starts_with("@@") {
            // Flush previous hunk
            if in_hunk && !current_hunk_lines.is_empty() {
                hunks.push(DiffHunk {
                    old_start: hunk_old_start,
                    old_count: hunk_old_count,
                    new_start: hunk_new_start,
                    new_count: hunk_new_count,
                    lines: current_hunk_lines.clone(),
                });
                current_hunk_lines.clear();
            }

            if let Some(parsed) = parse_hunk_header(raw_line) {
                hunk_old_start = parsed.0;
                hunk_old_count = parsed.1;
                hunk_new_start = parsed.2;
                hunk_new_count = parsed.3;
                old_line = parsed.0;
                new_line = parsed.2;
                in_hunk = true;
            }
            continue;
        }

        if !in_hunk {
            continue;
        }

        if let Some(content) = raw_line.strip_prefix('+') {
            current_hunk_lines.push(DiffLine {
                kind: DiffLineKind::Added,
                content: content.to_string(),
                old_line_num: None,
                new_line_num: Some(new_line),
            });
            new_line += 1;
            lines_added += 1;
        } else if let Some(content) = raw_line.strip_prefix('-') {
            current_hunk_lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                content: content.to_string(),
                old_line_num: Some(old_line),
                new_line_num: None,
            });
            old_line += 1;
            lines_removed += 1;
        } else {
            // Context line (may start with ' ' or be empty for blank lines)
            let content = raw_line.strip_prefix(' ').unwrap_or(raw_line);
            current_hunk_lines.push(DiffLine {
                kind: DiffLineKind::Context,
                content: content.to_string(),
                old_line_num: Some(old_line),
                new_line_num: Some(new_line),
            });
            old_line += 1;
            new_line += 1;
        }
    }

    // Flush last hunk
    if in_hunk && !current_hunk_lines.is_empty() {
        hunks.push(DiffHunk {
            old_start: hunk_old_start,
            old_count: hunk_old_count,
            new_start: hunk_new_start,
            new_count: hunk_new_count,
            lines: current_hunk_lines,
        });
    }

    FileDiff {
        file_path: file_path.to_string(),
        hunks,
        stats: DiffStats {
            lines_added,
            lines_removed,
            lines_changed: 0,
        },
    }
}

/// Parse `@@ -old_start,old_count +new_start,new_count @@` header.
fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    // Find the range between @@ markers
    let rest = line.strip_prefix("@@")?.trim();
    let end = rest.find("@@")?;
    let ranges = rest[..end].trim();

    let mut parts = ranges.split_whitespace();
    let old_part = parts.next()?; // -start,count
    let new_part = parts.next()?; // +start,count

    let (os, oc) = parse_range(old_part.strip_prefix('-').unwrap_or(old_part))?;
    let (ns, nc) = parse_range(new_part.strip_prefix('+').unwrap_or(new_part))?;

    Some((os, oc, ns, nc))
}

fn parse_range(s: &str) -> Option<(usize, usize)> {
    if let Some((start, count)) = s.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((s.parse().ok()?, 1))
    }
}

// ── ANSI Rendering ───────────────────────────────────────────────────────

/// Render a FileDiff as ANSI-colored unified diff text.
pub fn render_diff_ansi(diff: &FileDiff) -> String {
    let mut out = String::new();

    // Header
    out.push_str(&format!("\x1b[1m--- a/{}\x1b[0m\n", diff.file_path));
    out.push_str(&format!("\x1b[1m+++ b/{}\x1b[0m\n", diff.file_path));

    for hunk in &diff.hunks {
        // Hunk header (cyan)
        out.push_str(&format!(
            "\x1b[36m@@ -{},{} +{},{} @@\x1b[0m\n",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));

        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Added => {
                    out.push_str(&format!("\x1b[32m+{}\x1b[0m\n", line.content));
                }
                DiffLineKind::Removed => {
                    out.push_str(&format!("\x1b[31m-{}\x1b[0m\n", line.content));
                }
                DiffLineKind::Context => {
                    out.push_str(&format!(" {}\n", line.content));
                }
            }
        }
    }

    // Stats summary
    out.push_str(&format!(
        "\n\x1b[32m+{} added\x1b[0m, \x1b[31m-{} removed\x1b[0m\n",
        diff.stats.lines_added, diff.stats.lines_removed
    ));

    out
}

/// Add ANSI coloring to a raw unified diff string.
///
/// This is useful for coloring diffs generated by external tools
/// (e.g., `similar::udiff::unified_diff` or `git diff`).
pub fn colorize_unified_diff(diff_text: &str) -> String {
    let mut out = String::with_capacity(diff_text.len() + diff_text.len() / 4);

    for line in diff_text.lines() {
        if (line.starts_with("--- ") && line.len() > 4)
            || (line.starts_with("+++ ") && line.len() > 4)
        {
            // File header — bold (matches "--- a/file" or "+++ b/file", not bare "---")
            out.push_str(&format!("\x1b[1m{}\x1b[0m\n", line));
        } else if line.starts_with("@@") {
            // Hunk header — cyan
            out.push_str(&format!("\x1b[36m{}\x1b[0m\n", line));
        } else if line.starts_with('+') {
            // Added line — green
            out.push_str(&format!("\x1b[32m{}\x1b[0m\n", line));
        } else if line.starts_with('-') {
            // Removed line — red
            out.push_str(&format!("\x1b[31m{}\x1b[0m\n", line));
        } else {
            // Context line — unchanged
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

// ── HTML Rendering ───────────────────────────────────────────────────────

/// Render a FileDiff as HTML for web/Tauri frontend.
pub fn render_diff_html(diff: &FileDiff) -> String {
    let mut html = String::from("<div class=\"diff-container\">\n");
    html.push_str(&format!(
        "<div class=\"diff-header\">{}</div>\n",
        html_escape(&diff.file_path)
    ));

    for hunk in &diff.hunks {
        html.push_str("<table class=\"diff-hunk\">\n");
        html.push_str(&format!(
            "<tr class=\"diff-hunk-header\"><td colspan=\"2\">@@ -{},{} +{},{} @@</td></tr>\n",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));

        for line in &hunk.lines {
            let (class, prefix) = match line.kind {
                DiffLineKind::Added => ("diff-added", "+"),
                DiffLineKind::Removed => ("diff-removed", "-"),
                DiffLineKind::Context => ("diff-context", " "),
            };
            let old_num = line.old_line_num.map(|n| n.to_string()).unwrap_or_default();
            let new_num = line.new_line_num.map(|n| n.to_string()).unwrap_or_default();
            html.push_str(&format!(
                "<tr class=\"{}\"><td class=\"diff-line-num\">{}{}</td><td class=\"diff-line-content\">{}{}</td></tr>\n",
                class, old_num, new_num, prefix, html_escape(&line.content)
            ));
        }

        html.push_str("</table>\n");
    }

    html.push_str(&format!(
        "<div class=\"diff-stats\">+{} added, -{} removed</div>\n",
        diff.stats.lines_added, diff.stats.lines_removed
    ));
    html.push_str("</div>\n");

    html
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Convenience ──────────────────────────────────────────────────────────

/// Quick diff-and-render for an edit operation. Returns ANSI-colored output.
pub fn render_edit_diff(file_path: &str, old_content: &str, new_content: &str) -> String {
    let diff = generate_unified_diff(file_path, old_content, new_content, 3);
    if diff.hunks.is_empty() {
        String::new()
    } else {
        render_diff_ansi(&diff)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run_test_git<const N: usize>(
        root: &Path,
        args: [&str; N],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new("git").args(args).current_dir(root).output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "git command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
            .into())
        }
    }

    fn committed_test_workspace()
    -> Result<(tempfile::TempDir, WorkspaceDiffService), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        run_test_git(temp.path(), ["init", "-q"])?;
        std::fs::write(temp.path().join("notes.txt"), "first\nstable\n")?;
        run_test_git(temp.path(), ["add", "--", "notes.txt"])?;
        run_test_git(
            temp.path(),
            [
                "-c",
                "user.name=EKO Test",
                "-c",
                "user.email=eko@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "initial",
            ],
        )?;
        let service = WorkspaceDiffService::new(temp.path());
        Ok((temp, service))
    }

    #[test]
    fn test_no_changes() {
        let diff = generate_unified_diff("test.rs", "hello\nworld\n", "hello\nworld\n", 3);
        assert!(diff.hunks.is_empty());
        assert_eq!(diff.stats.lines_added, 0);
        assert_eq!(diff.stats.lines_removed, 0);
    }

    #[test]
    fn test_addition() {
        let diff = generate_unified_diff("test.rs", "line1\nline3\n", "line1\nline2\nline3\n", 3);
        assert!(!diff.hunks.is_empty());
        assert_eq!(diff.stats.lines_added, 1);
    }

    #[test]
    fn test_removal() {
        let diff = generate_unified_diff("test.rs", "line1\nline2\nline3\n", "line1\nline3\n", 3);
        assert!(!diff.hunks.is_empty());
        assert_eq!(diff.stats.lines_removed, 1);
    }

    #[test]
    fn test_replacement() {
        let diff = generate_unified_diff("test.rs", "hello\nworld\n", "hello\nrust\n", 3);
        assert!(!diff.hunks.is_empty());
        assert_eq!(diff.stats.lines_added, 1);
        assert_eq!(diff.stats.lines_removed, 1);
    }

    #[test]
    fn test_ansi_rendering() {
        let diff = generate_unified_diff("test.rs", "old\n", "new\n", 3);
        let ansi = render_diff_ansi(&diff);
        assert!(ansi.contains("\x1b[32m")); // green for added
        assert!(ansi.contains("\x1b[31m")); // red for removed
        assert!(ansi.contains("\x1b[36m")); // cyan for hunk header
    }

    #[test]
    fn test_html_rendering() {
        let diff = generate_unified_diff("test.rs", "old\n", "new\n", 3);
        let html = render_diff_html(&diff);
        assert!(html.contains("diff-container"));
        assert!(html.contains("diff-added"));
        assert!(html.contains("diff-removed"));
    }

    #[test]
    fn test_colorize_unified_diff() {
        let raw = "--- a/test.rs\n+++ b/test.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let colored = colorize_unified_diff(raw);
        assert!(colored.contains("\x1b[1m")); // bold header
        assert!(colored.contains("\x1b[36m")); // cyan hunk
        assert!(colored.contains("\x1b[31m")); // red removed
        assert!(colored.contains("\x1b[32m")); // green added
    }

    #[test]
    fn test_parse_unified_diff() {
        let raw = "--- a/test.rs\n+++ b/test.rs\n@@ -1,2 +1,2 @@\n hello\n-world\n+rust\n";
        let diff = parse_unified_diff("test.rs", raw);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.stats.lines_added, 1);
        assert_eq!(diff.stats.lines_removed, 1);
    }

    #[test]
    fn test_parse_hunk_header() -> anyhow::Result<()> {
        let h = parse_hunk_header("@@ -10,5 +20,7 @@")
            .ok_or_else(|| anyhow::anyhow!("valid hunk header did not parse"))?;
        assert_eq!(h, (10, 5, 20, 7));
        Ok(())
    }

    #[test]
    fn test_parse_hunk_header_single_line() -> anyhow::Result<()> {
        let h = parse_hunk_header("@@ -1 +1 @@")
            .ok_or_else(|| anyhow::anyhow!("valid single-line hunk header did not parse"))?;
        assert_eq!(h, (1, 1, 1, 1));
        Ok(())
    }

    #[test]
    fn test_render_edit_diff_no_change() {
        let result = render_edit_diff("test.rs", "same\n", "same\n");
        assert!(result.is_empty());
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<div>"), "&lt;div&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn workspace_diff_reads_verified_ref_and_current_text() -> Result<(), Box<dyn std::error::Error>>
    {
        let (temp, service) = committed_test_workspace()?;
        std::fs::write(temp.path().join("notes.txt"), "second\nstable\n")?;

        let result = service.diff_file("notes.txt", "HEAD")?;

        assert_eq!(result.path, "notes.txt");
        assert_eq!(result.old_content, "first\nstable\n");
        assert_eq!(result.new_content, "second\nstable\n");
        assert!(
            result
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.tag == WorkspaceDiffTag::Delete && line.content == "first")
        );
        assert!(
            result
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.tag == WorkspaceDiffTag::Insert && line.content == "second")
        );
        Ok(())
    }

    #[test]
    fn workspace_diff_rejects_ref_syntax_and_path_escape() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp, service) = committed_test_workspace()?;

        assert!(matches!(
            service.diff_file("notes.txt", "HEAD^"),
            Err(WorkspaceDiffError::Validation(_))
        ));
        assert!(matches!(
            service.diff_file("../outside.txt", "HEAD"),
            Err(WorkspaceDiffError::Validation(_))
        ));
        Ok(())
    }

    #[test]
    fn workspace_diff_rejects_non_utf8_current_file() -> Result<(), Box<dyn std::error::Error>> {
        let (temp, service) = committed_test_workspace()?;
        std::fs::write(temp.path().join("notes.txt"), [0xff, 0xfe])?;

        assert!(matches!(
            service.diff_file("notes.txt", "HEAD"),
            Err(WorkspaceDiffError::Operation(_))
        ));
        Ok(())
    }

    #[test]
    fn workspace_diff_wire_projection_preserves_frontend_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let wire = WorkspaceFileDiff {
            path: "src/main.rs".to_string(),
            old_content: "old".to_string(),
            new_content: "new".to_string(),
            hunks: vec![WorkspaceDiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                lines: vec![
                    WorkspaceDiffLine {
                        tag: WorkspaceDiffTag::Delete,
                        old_line: Some(1),
                        new_line: None,
                        content: "old".to_string(),
                    },
                    WorkspaceDiffLine {
                        tag: WorkspaceDiffTag::Insert,
                        old_line: None,
                        new_line: Some(1),
                        content: "new".to_string(),
                    },
                ],
            }],
        };
        let json = serde_json::to_value(&wire)?;

        assert_eq!(
            json.get("path").and_then(serde_json::Value::as_str),
            Some("src/main.rs")
        );
        assert_eq!(
            json.pointer("/hunks/0/lines/0/tag")
                .and_then(serde_json::Value::as_str),
            Some("delete")
        );
        assert_eq!(
            json.pointer("/hunks/0/lines/1/tag")
                .and_then(serde_json::Value::as_str),
            Some("insert")
        );
        assert_eq!(
            json.pointer("/hunks/0/lines/0/old_line")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert!(
            json.pointer("/hunks/0/lines/0/new_line")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
    }
}
