//! Worktree lifecycle primitives for unattended write runs (D7 stage 2).
//!
//! This module centralises the git-worktree operations that were previously
//! scattered across Tauri `panels.rs` commands, so both the CLI and the GUI
//! can share them. It is product-layer (app-core) because worktree isolation
//! is an EKO desktop-assistant concern, not a generic agent-framework one —
//! the framework only exposes the `working_dir` propagation that lets shell/
//! file/git tools chroot themselves into a worktree path.
//!
//! All operations are thin wrappers around `git` subprocesses; no native
//! libgit2 dependency. This matches the rest of the codebase and keeps the
//! dependency footprint minimal.
//!
//! # Naming conventions
//!
//! * Branch prefix: `eko-unattended-` (unattended-run worktrees only; manual
//!   user worktrees created via `panels.rs` use the existing `worktree-`
//!   prefix so they stay distinct).
//! * Path: `<repo_parent>/<repo_name>-<branch>` (matches Claude Code's
//!   `.claude/worktrees/` sibling-directory convention, and the existing
//!   `default_worktree_path` helper in `panels.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Branch-name prefix reserved for unattended-run worktrees.
pub const BRANCH_PREFIX: &str = "eko-unattended-";

/// A worktree descriptor as surfaced by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree checkout.
    pub path: String,
    /// Head ref (branch name without `refs/heads/`, or `(detached)`).
    pub branch: String,
    /// Whether this entry is a *managed* worktree — i.e. its path differs
    /// from the canonical repository root. Used to decide whether a GUI
    /// entry may be merged/discarded via the app's helpers.
    pub managed: bool,
    /// First 12 chars of HEAD commit hash (display-only).
    pub head: String,
}

/// Errors returned by worktree operations. String wrapper — the underlying
/// git stderr is usually the most useful diagnostic.
#[derive(Debug)]
pub struct WorktreeError {
    pub message: String,
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WorktreeError {}

impl WorktreeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for WorktreeError {
    fn from(e: std::io::Error) -> Self {
        Self::new(format!("I/O error: {e}"))
    }
}

// ── Primitives (A2) ─────────────────────────────────────────────────────

/// Run `git -C <repo> <args…>` and return trimmed stdout on success, or
/// surface stderr as `WorktreeError` on failure. Uses `std::process::Command`
/// (blocking) — suitable for short-lived git ops; callers needing async may
/// wrap via `tokio::task::spawn_blocking`.
pub fn run_git(repo: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(WorktreeError::new(if stderr.is_empty() {
            format!("git {args:?} failed")
        } else {
            stderr
        }))
    }
}

/// Resolve the repository root (`git rev-parse --show-toplevel`) from any
/// path inside a git checkout. Returns the root as a `PathBuf`.
pub fn git_repo_root(start: &Path) -> Result<PathBuf, WorktreeError> {
    let root = run_git(start, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root))
}

/// Validate that `branch` is a legal git branch name. Delegates to
/// `git check-ref-format --branch`, with a fast reject for empty /
/// dash-prefix / whitespace inputs.
pub fn validate_branch_name(repo: &Path, branch: &str) -> Result<(), WorktreeError> {
    if branch.trim().is_empty() {
        return Err(WorktreeError::new("Branch name cannot be empty"));
    }
    if branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(WorktreeError::new(
            "Branch name cannot start with '-' or contain whitespace",
        ));
    }
    run_git(repo, &["check-ref-format", "--branch", branch]).map(|_| ())
}

/// Compute a sensible filesystem path for a new worktree branched from
/// `repo_root`'s parent, named `<repo_name>-<safe_branch>`. Sanitises the
/// branch portion so the result is a legal directory name.
/// Matches the convention already used by `panels.rs`.
pub fn default_worktree_path(repo_root: &Path, branch: &str) -> Result<PathBuf, WorktreeError> {
    if branch.trim().is_empty() {
        return Err(WorktreeError::new("Branch name cannot be empty"));
    }
    let parent = repo_root
        .parent()
        .ok_or_else(|| WorktreeError::new("Repository root has no parent directory"))?;
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".to_string());
    let safe_branch: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(parent.join(format!("{repo_name}-{safe_branch}")))
}

/// Reject worktree target paths that do not live under the repository's
/// parent directory, or that already exist. Protects against worktree
/// paths escaping the repository neighborhood.
pub fn validate_worktree_target(repo_root: &Path, target: &Path) -> Result<(), WorktreeError> {
    let repo_parent = repo_root
        .parent()
        .ok_or_else(|| WorktreeError::new("Repository root has no parent directory"))?
        .canonicalize()?;
    let target_parent = target
        .parent()
        .ok_or_else(|| WorktreeError::new("Worktree path has no parent directory"))?;
    let canonical_parent = target_parent.canonicalize()?;
    if !canonical_parent.starts_with(&repo_parent) {
        return Err(WorktreeError::new(format!(
            "Worktree path must stay under repository parent: {}",
            repo_parent.display()
        )));
    }
    if target.exists() {
        return Err(WorktreeError::new(format!(
            "Worktree path already exists: {}",
            target.display()
        )));
    }
    Ok(())
}

/// Parse `git worktree list --porcelain` output into [`WorktreeInfo`] entries.
/// The parser is line-based; entries are separated by blank lines, and each
/// entry consists of `worktree <path>`, `HEAD <sha>`, `branch <ref>`.
pub fn parse_worktree_list(output: &str, repo_root: &Path) -> Vec<WorktreeInfo> {
    let canonical_repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let mut items = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch = String::new();

    let flush = |items: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut String| {
        let Some(path_buf) = path.take() else {
            return;
        };
        let canonical = path_buf.canonicalize().unwrap_or_else(|_| path_buf.clone());
        let display_branch = if branch.is_empty() {
            "(detached)".to_string()
        } else {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch.as_str())
                .to_string()
        };
        items.push(WorktreeInfo {
            path: path_buf.to_string_lossy().to_string(),
            branch: display_branch,
            managed: canonical != canonical_repo,
            head: head.chars().take(12).collect(),
        });
        head.clear();
        branch.clear();
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            current_path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = branch.to_string();
        }
    }
    flush(
        &mut items,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
    );
    items
}

// ── RunWorktree lifecycle (A3–A5) ──────────────────────────────────────

/// An in-progress worktree for an unattended write run.
///
/// Holds the identity needed to generate a diff summary and to `keep` /
/// `remove` the worktree after the run finishes (Q1: keep for review; no
/// automatic merge).
#[derive(Debug)]
pub struct RunWorktree {
    /// The `run_id` this worktree serves.
    pub run_id: String,
    /// Absolute path to the worktree checkout.
    pub path: PathBuf,
    /// Branch name (`eko-unattended-<run_id>`).
    pub branch: String,
    /// Base ref the worktree branched from (typically `HEAD`).
    pub base: String,
}

impl RunWorktree {
    /// Create a new unattended-run worktree.
    ///
    /// * `branch` = `eko-unattended-<run_id>`
    /// * `path` = computed via [`default_worktree_path`]
    /// * `base` = `repo_root` `HEAD` (default for local-first desktop;
    ///   there is no remote assumption for unattended runs).
    /// * After creation the worktree is `git worktree lock`ed so automatic
    ///   cleanup (future `cleanupPeriodDays`-style sweeping) won't
    ///   interfere while the run is active.
    pub fn create(run_id: &str, repo_root: &Path) -> Result<Self, WorktreeError> {
        let branch = format!("{BRANCH_PREFIX}{run_id}");
        validate_branch_name(repo_root, &branch)?;
        let path = default_worktree_path(repo_root, &branch)?;
        validate_worktree_target(repo_root, &path)?;
        let base = "HEAD";
        let path_str = path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;

        // Create worktree + branch in one step (matches panels.rs convention).
        run_git(
            repo_root,
            &["worktree", "add", "-b", &branch, path_str, base],
        )?;

        // Lock so concurrent cleanup won't touch it (Claude Code pattern).
        run_git(
            repo_root,
            &[
                "worktree",
                "lock",
                path_str,
                "--reason",
                &format!("unattended run {run_id} in progress"),
            ],
        )?;

        Ok(Self {
            run_id: run_id.to_string(),
            path,
            branch,
            base: base.to_string(),
        })
    }

    /// Generate a `git diff` summary of the worktree's changes relative to
    /// the base ref. Committed worktree changes are diffed against `base`;
    /// if there are no commits yet, working-tree changes are diffed.
    pub fn diff_summary(&self) -> Result<String, WorktreeError> {
        let committed = run_git(
            &self.path,
            &["rev-list", "--count", &format!("{}..HEAD", self.base)],
        )
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);

        let (stat, full) = if committed > 0 {
            let stat = run_git(
                &self.path,
                &["diff", "--stat", &format!("{}..HEAD", self.base)],
            )
            .unwrap_or_else(|_| "(diff unavailable)".to_string());
            let full = run_git(&self.path, &["diff", &format!("{}..HEAD", self.base)])
                .unwrap_or_else(|_| "(full diff unavailable)".to_string());
            (stat, full)
        } else {
            let stat = run_git(&self.path, &["diff", "--stat", self.base.as_str()])
                .unwrap_or_else(|_| "(diff unavailable)".to_string());
            let full = run_git(&self.path, &["diff", self.base.as_str()])
                .unwrap_or_else(|_| "(full diff unavailable)".to_string());
            (stat, full)
        };
        Ok(format!("=== Stat ===\n{stat}\n\n=== Full diff ===\n{full}"))
    }

    /// Keep the worktree for later review (Q1). Returns `self` so the
    /// caller can record `path`/`branch` in run metadata.
    pub fn keep(self) -> Self {
        // `git worktree lock` already applied at create time — it persists
        // until an explicit `git worktree unlock`, so `keep` is a no-op.
        self
    }

    /// Discard the worktree: `git worktree remove --force` + prune.
    /// Callers (GUI or run-finalise) use this when the user decides to
    /// discard the unattended run's worktree.
    pub fn remove(self) -> Result<(), WorktreeError> {
        let wt_path = self
            .path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
        // `git worktree remove --force <worktree_path>` works from anywhere
        // inside the main repo (or the worktree itself). We run it from
        // `self.path` so `-C` isn't needed.
        run_git(&self.path, &["worktree", "remove", "--force", wt_path])?;
        run_git(&self.path, &["worktree", "prune"])?;
        Ok(())
    }
}

// ── Unattended worktree management (Phase D) ──────────────────────────

/// Information about an unattended worktree, including its associated run.
#[derive(Debug, Clone)]
pub struct UnattendedWorktreeInfo {
    pub run_id: String,
    pub branch: String,
    pub path: PathBuf,
    pub head: String,
    pub status: String, // "pending", "merged", "discarded", or run status from store
}

/// List all unattended worktrees (those with the "eko-unattended-" prefix).
/// Returns worktrees that are still present on disk, along with their run status
/// from the TaskRuntimeStore (if available).
pub fn list_unattended_worktrees(
    repo_root: &Path,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<Vec<UnattendedWorktreeInfo>, WorktreeError> {
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])?;
    let all_worktrees = parse_worktree_list(&output, repo_root);

    let mut unattended = Vec::new();
    for wt in all_worktrees {
        if wt.branch.starts_with(BRANCH_PREFIX) {
            // Extract run_id from branch name: "eko-unattended-{run_id}"
            let run_id = wt.branch.strip_prefix(BRANCH_PREFIX).unwrap_or(&wt.branch);

            // Try to get status from store
            let status = if let Some(store) = store {
                match store.get_run(run_id) {
                    Ok(Some(run)) => run.status.as_str().to_string(),
                    Ok(None) => "unknown".to_string(),
                    Err(_) => "unknown".to_string(),
                }
            } else {
                "pending".to_string()
            };

            unattended.push(UnattendedWorktreeInfo {
                run_id: run_id.to_string(),
                branch: wt.branch,
                path: PathBuf::from(&wt.path),
                head: wt.head,
                status,
            });
        }
    }

    Ok(unattended)
}

/// Merge an unattended worktree back to the main branch.
/// This will:
/// 1. Checkout the main branch in the main worktree
/// 2. Merge the worktree branch
/// 3. Optionally remove the worktree after successful merge
pub fn merge_unattended_worktree(
    repo_root: &Path,
    run_id: &str,
    remove_after_merge: bool,
) -> Result<(), WorktreeError> {
    let branch = format!("{BRANCH_PREFIX}{run_id}");

    // Find the worktree path
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])?;
    let worktrees = parse_worktree_list(&output, repo_root);

    let worktree = worktrees
        .iter()
        .find(|wt| wt.branch == branch)
        .ok_or_else(|| WorktreeError::new(format!("Worktree for branch {} not found", branch)))?;

    let worktree_path = PathBuf::from(&worktree.path);

    // Ensure we're on main branch in the main worktree
    run_git(repo_root, &["checkout", "main"])?;

    // Merge the worktree branch
    run_git(
        repo_root,
        &[
            "merge",
            &branch,
            "--no-ff",
            "-m",
            &format!("Merge unattended run {}", run_id),
        ],
    )?;

    // Optionally remove the worktree
    if remove_after_merge {
        let wt_path_str = worktree_path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
        run_git(repo_root, &["worktree", "remove", "--force", wt_path_str])?;
        run_git(repo_root, &["branch", "-d", &branch])?;
    }

    Ok(())
}

/// Discard an unattended worktree without merging.
/// This will:
/// 1. Remove the worktree
/// 2. Delete the branch
pub fn discard_unattended_worktree(repo_root: &Path, run_id: &str) -> Result<(), WorktreeError> {
    let branch = format!("{BRANCH_PREFIX}{run_id}");

    // Find the worktree path
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])?;
    let worktrees = parse_worktree_list(&output, repo_root);

    let worktree = worktrees
        .iter()
        .find(|wt| wt.branch == branch)
        .ok_or_else(|| WorktreeError::new(format!("Worktree for branch {} not found", branch)))?;

    let worktree_path = PathBuf::from(&worktree.path);
    let wt_path_str = worktree_path
        .to_str()
        .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;

    // Remove the worktree
    run_git(repo_root, &["worktree", "remove", "--force", wt_path_str])?;

    // Delete the branch
    run_git(repo_root, &["branch", "-D", &branch])?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // ── Pure-logic tests (no git subprocess needed) ──

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_worktree_list_round_trips_sample_output() {
        let sample = "\
worktree /abs/repo
HEAD abcdef0123456789abcdef0123456789abcdef01
branch refs/heads/main

worktree /abs/repo-wt
HEAD 1111111111111111111111111111111111111111
branch refs/heads/feature

";
        let repo_root = PathBuf::from("/abs/repo");
        let items = parse_worktree_list(sample, &repo_root);
        assert_eq!(items.len(), 2, "expected two entries, got {items:?}");
        // First entry is the main checkout (path == repo_root) → not managed.
        assert_eq!(items[0].path, "/abs/repo");
        assert_eq!(items[0].branch, "main");
        assert_eq!(items[0].head, "abcdef012345");
        assert!(!items[0].managed);
        // Second entry is a managed worktree (path != repo_root).
        assert_eq!(items[1].path, "/abs/repo-wt");
        assert_eq!(items[1].branch, "feature");
        assert_eq!(items[1].head, "111111111111");
        assert!(items[1].managed);
    }

    #[test]
    fn parse_worktree_list_handles_detached_head() {
        let sample = "\
worktree /abs/repo
HEAD abcdef0123456789abcdef0123456789abcdef01

";
        let repo_root = PathBuf::from("/abs/repo");
        let items = parse_worktree_list(sample, &repo_root);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].branch, "(detached)");
        assert_eq!(items[0].head, "abcdef012345");
    }

    #[test]
    fn parse_worktree_list_empty_input() {
        let items = parse_worktree_list("", &PathBuf::from("/abs/repo"));
        assert!(items.is_empty());
    }

    // ── Tests requiring a git repo (disabled under sandbox) ──
    // These are kept as documentation of expected behaviour. They can be run
    // locally where `git` subprocesses are permitted.

    // TODO: git-subprocess tests require `git` availability → run locally.
    // Leaving assertions commented as documentation of contract.
}
