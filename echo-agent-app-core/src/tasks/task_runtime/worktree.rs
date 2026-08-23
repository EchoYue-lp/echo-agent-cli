//! Worktree lifecycle and integration primitives for Fork-dispatched writer
//! tasks, plus cleanup/review support for legacy unattended-run worktrees.
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
//! * Legacy branch prefix: `eko-unattended-`. New unattended runs no longer
//!   create this duplicate run-level checkout; the prefix remains so existing
//!   reviewable work can be listed, integrated, or cleaned without data loss.
//! * Path: `<repo_parent>/<repo_name>-<branch>` (matches Claude Code's
//!   `.claude/worktrees/` sibling-directory convention, and the existing
//!   `default_worktree_path` helper in `panels.rs`).

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::Mutex as TokioMutex;

/// Branch-name prefix reserved for unattended-run worktrees.
pub const BRANCH_PREFIX: &str = "eko-unattended-";

/// Branch-name prefix reserved for Fork-dispatched subagent worktrees (Sprint 8).
/// Distinct from [`BRANCH_PREFIX`] so list/merge/discard can tell unattended-run
/// worktrees apart from interactive Fork-subagent worktrees.
pub const FORK_BRANCH_PREFIX: &str = "eko-fork-";

static REPO_MERGE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<TokioMutex<()>>>>> = OnceLock::new();

/// Return the process-wide integration mutex for one repository.
///
/// The lock protects the shared Git index/refs when different TaskRuntime runs
/// finish isolated writers at the same time. Cross-process races still fail
/// through Git's own lock files and are surfaced as integration failures.
pub fn repo_merge_lock(repo_root: &Path) -> Arc<TokioMutex<()>> {
    let locks = REPO_MERGE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(repo_root).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(TokioMutex::new(()));
    locks.insert(repo_root.to_path_buf(), Arc::downgrade(&lock));
    lock
}

/// Convert an arbitrary runtime identity into a legal, stable Git ref suffix.
/// A short hash prevents collisions after punctuation is normalized.
pub fn safe_branch_id(identity: &str) -> String {
    let mut normalized = String::with_capacity(identity.len().min(96));
    let mut previous_dash = false;
    for ch in identity.chars().take(96) {
        let safe = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            ch
        } else {
            '-'
        };
        if safe == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        normalized.push(safe);
    }
    let normalized = normalized.trim_matches('-');
    let stem = if normalized.is_empty() {
        "task"
    } else {
        normalized
    };
    let digest = hex::encode(Sha256::digest(identity.as_bytes()));
    let short_hash: String = digest.chars().take(12).collect();
    format!("{stem}-{short_hash}")
}

pub fn fork_branch_name(label: &str) -> String {
    format!("{FORK_BRANCH_PREFIX}{}", safe_branch_id(label))
}

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
    /// Whether Git currently protects this worktree from prune/remove.
    pub locked: bool,
    /// Optional reason recorded by `git worktree lock --reason`.
    pub lock_reason: Option<String>,
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
    let output = git_output(repo, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(output_error(args, &output))
    }
}

fn git_output(repo: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(WorktreeError::from)
}

fn output_error(args: &[&str], output: &Output) -> WorktreeError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git {args:?} failed with status {}", output.status)
    };
    WorktreeError::new(detail)
}

fn bounded_output(output: &Output, max_chars: usize) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let joined = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
        (false, true) => stdout.trim().to_string(),
        (true, false) => stderr.trim().to_string(),
        (true, true) => "no git diagnostic output".to_string(),
    };
    joined.chars().take(max_chars).collect()
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
    let mut current_locked = false;
    let mut current_lock_reason: Option<String> = None;

    let flush = |items: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut String,
                 locked: &mut bool,
                 lock_reason: &mut Option<String>| {
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
            locked: *locked,
            lock_reason: lock_reason.take(),
        });
        head.clear();
        branch.clear();
        *locked = false;
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut current_locked,
                &mut current_lock_reason,
            );
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut current_locked,
                &mut current_lock_reason,
            );
            current_path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = branch.to_string();
        } else if let Some(reason) = line.strip_prefix("locked") {
            current_locked = true;
            let reason = reason.trim();
            current_lock_reason = (!reason.is_empty()).then(|| reason.to_string());
        }
    }
    flush(
        &mut items,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
        &mut current_locked,
        &mut current_lock_reason,
    );
    items
}

// ── RunWorktree lifecycle (A3–A5) ──────────────────────────────────────

/// An in-progress worktree for a Fork-dispatched writer Subagent.
///
/// Holds the identity needed to generate a diff summary, unlock, and integrate
/// the worktree after execution.
#[derive(Debug)]
pub struct RunWorktree {
    /// Canonical repository checkout used for shared Git operations.
    pub repo_root: PathBuf,
    /// Absolute path to the worktree checkout.
    pub path: PathBuf,
    /// Branch name (`eko-fork-<safe-label>`).
    pub branch: String,
    /// Merge-base commit SHA resolved when the worktree was acquired.
    pub base: String,
}

impl RunWorktree {
    /// Acquire the stable worktree for one logical writer task.
    ///
    /// A retained, unlocked checkout is reused across retries. An existing
    /// branch whose checkout was pruned is materialized again. A locked
    /// checkout is treated as active and is never shared concurrently.
    pub fn acquire_fork(label: &str, repo_root: &Path) -> Result<Self, WorktreeError> {
        let branch = fork_branch_name(label);
        validate_branch_name(repo_root, &branch)?;

        if let Some(existing) = find_worktree_info(repo_root, &branch)? {
            if existing.locked {
                return Err(WorktreeError::new(format!(
                    "worktree for logical task {label} is already active at {}",
                    existing.path
                )));
            }
            let path = PathBuf::from(existing.path);
            if path.exists() {
                let base = run_git(repo_root, &["merge-base", "HEAD", &branch])?;
                lock_worktree(repo_root, &path, label)?;
                return Ok(Self {
                    repo_root: repo_root.to_path_buf(),
                    path,
                    branch,
                    base,
                });
            }
            run_git(repo_root, &["worktree", "prune"])?;
        }

        if branch_exists(repo_root, &branch)? {
            let path = default_worktree_path(repo_root, &branch)?;
            validate_worktree_target(repo_root, &path)?;
            let path_text = path
                .to_str()
                .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
            run_git(repo_root, &["worktree", "add", path_text, &branch])?;
            let path = path.canonicalize().unwrap_or(path);
            let base = run_git(repo_root, &["merge-base", "HEAD", &branch])?;
            lock_worktree(repo_root, &path, label)?;
            return Ok(Self {
                repo_root: repo_root.to_path_buf(),
                path,
                branch,
                base,
            });
        }

        Self::create_fork(label, repo_root)
    }

    /// Create a worktree for a Fork-dispatched subagent (Sprint 8).
    ///
    /// Uses the [`FORK_BRANCH_PREFIX`] namespace. `label` identifies the
    /// logical task (e.g. `"{agent_name}-{run_id}:{task_id}"`).
    pub fn create_fork(label: &str, repo_root: &Path) -> Result<Self, WorktreeError> {
        let branch = fork_branch_name(label);
        validate_branch_name(repo_root, &branch)?;
        let path = default_worktree_path(repo_root, &branch)?;
        validate_worktree_target(repo_root, &path)?;
        let base = run_git(repo_root, &["rev-parse", "HEAD"])?;
        let path_str = path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;

        // Create worktree + branch in one step (matches panels.rs convention).
        run_git(
            repo_root,
            &["worktree", "add", "-b", &branch, path_str, &base],
        )?;
        let path = path.canonicalize().unwrap_or(path);

        // Lock so concurrent cleanup won't touch it (Claude Code pattern).
        lock_worktree(repo_root, &path, label)?;

        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            path,
            branch,
            base,
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
        let untracked = run_git(&self.path, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_else(|_| "(untracked file listing unavailable)".to_string());
        Ok(format!(
            "=== Stat ===\n{stat}\n\n=== Full diff ===\n{full}\n\n=== Untracked files ===\n{untracked}"
        ))
    }

    /// Whether the checkout contains uncommitted files or commits not yet
    /// reachable from the authoritative checkout.
    pub fn has_changes(&self) -> Result<bool, WorktreeError> {
        Ok(worktree_has_uncommitted_changes(&self.path)?
            || branch_ahead_of_head(&self.repo_root, &self.branch)? > 0)
    }

    /// Release the lifecycle lock after the subagent has stopped.
    pub fn unlock(&self) -> Result<(), WorktreeError> {
        let wt_path = self
            .path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
        let output = git_output(&self.repo_root, &["worktree", "unlock", wt_path])?;
        if output.status.success() {
            return Ok(());
        }
        let diagnostic = bounded_output(&output, 2_000);
        if diagnostic.contains("not locked") {
            Ok(())
        } else {
            Err(WorktreeError::new(diagnostic))
        }
    }
}

// ── Fork worktree integration (M8) ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeIntegrationStatus {
    Merged,
    NoChanges,
    AlreadyIntegrated,
}

impl WorktreeIntegrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::NoChanges => "no_changes",
            Self::AlreadyIntegrated => "already_integrated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeIntegrationOutcome {
    pub status: WorktreeIntegrationStatus,
    pub branch: String,
    pub path: Option<PathBuf>,
    pub changed_files: Vec<String>,
    pub merge_commit: Option<String>,
    pub cleanup_warning: Option<String>,
}

impl WorktreeIntegrationOutcome {
    pub fn summary(&self) -> String {
        let commit = self
            .merge_commit
            .as_deref()
            .map(|value| format!(" commit={value}"))
            .unwrap_or_default();
        format!(
            "worktree integration={} branch={} files={}{}",
            self.status.as_str(),
            self.branch,
            self.changed_files.len(),
            commit
        )
    }
}

/// Integrate one completed/reviewed Fork writer into the authoritative local
/// checkout. This function is blocking; async callers must use
/// `tokio::task::spawn_blocking` and hold [`repo_merge_lock`].
pub fn integrate_fork_worktree(
    repo_root: &Path,
    label: &str,
    task_id: &str,
    execution_id: &str,
    ownership: &super::planner::FileOwnership,
) -> Result<WorktreeIntegrationOutcome, WorktreeError> {
    let branch = fork_branch_name(label);
    let trailer = format!("EKO-Execution-Id: {execution_id}");

    if let Some(commit) = find_integration_commit(repo_root, &trailer)? {
        let path = find_worktree_path(repo_root, &branch)?;
        let cleanup_warning = path
            .as_deref()
            .and_then(|path| cleanup_managed_worktree(repo_root, path, &branch).err());
        return Ok(WorktreeIntegrationOutcome {
            status: WorktreeIntegrationStatus::AlreadyIntegrated,
            branch,
            path,
            changed_files: Vec::new(),
            merge_commit: Some(commit),
            cleanup_warning,
        });
    }

    let path = find_worktree_path(repo_root, &branch)?;
    let branch_exists = branch_exists(repo_root, &branch)?;
    if !branch_exists && path.is_none() {
        return Ok(WorktreeIntegrationOutcome {
            status: WorktreeIntegrationStatus::NoChanges,
            branch,
            path: None,
            changed_files: Vec::new(),
            merge_commit: None,
            cleanup_warning: None,
        });
    }

    let path = match path {
        Some(path) => path,
        None => {
            let path = default_worktree_path(repo_root, &branch)?;
            validate_worktree_target(repo_root, &path)?;
            let path_text = path
                .to_str()
                .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
            run_git(repo_root, &["worktree", "add", path_text, &branch])?;
            path.canonicalize().unwrap_or(path)
        }
    };
    integrate_existing_worktree(repo_root, branch, path, task_id, &trailer, ownership)
}

fn integrate_existing_worktree(
    repo_root: &Path,
    branch: String,
    path: PathBuf,
    task_id: &str,
    trailer: &str,
    ownership: &super::planner::FileOwnership,
) -> Result<WorktreeIntegrationOutcome, WorktreeError> {
    let preserve_error = |message: String| {
        let _ = unlock_worktree(repo_root, &path);
        WorktreeError::new(format!(
            "{message}; isolated work preserved at {} on branch {branch}",
            path.display()
        ))
    };

    if matches!(ownership, super::planner::FileOwnership::ReadOnly) {
        return Err(preserve_error(
            "read-only task cannot integrate a writer worktree".to_string(),
        ));
    }

    run_git(&path, &["add", "-A"])
        .map_err(|error| preserve_error(format!("failed to stage writer changes: {error}")))?;
    let merge_base = run_git(repo_root, &["merge-base", "HEAD", &branch])
        .map_err(|error| preserve_error(format!("failed to resolve merge base: {error}")))?;
    let changed_files = git_nul_paths(
        &path,
        &["diff", "--cached", "--name-only", "-z", &merge_base],
    )
    .map_err(|error| preserve_error(format!("failed to inspect writer changes: {error}")))?;
    validate_changed_files(ownership, &changed_files).map_err(preserve_error)?;

    let staged_against_head = git_output(&path, &["diff", "--cached", "--quiet", "HEAD"])
        .map_err(|error| preserve_error(format!("failed to inspect staged changes: {error}")))?;
    match staged_against_head.status.code() {
        Some(0) => {}
        Some(1) => commit_writer_changes(&path, task_id, trailer)
            .map_err(|error| preserve_error(format!("failed to commit writer changes: {error}")))?,
        _ => {
            return Err(preserve_error(format!(
                "failed to compare staged writer changes: {}",
                bounded_output(&staged_against_head, 2_000)
            )));
        }
    }

    if changed_files.is_empty() {
        let cleanup_warning = cleanup_managed_worktree(repo_root, &path, &branch).err();
        return Ok(WorktreeIntegrationOutcome {
            status: WorktreeIntegrationStatus::NoChanges,
            branch,
            path: Some(path),
            changed_files,
            merge_commit: None,
            cleanup_warning,
        });
    }

    let ancestor = git_output(repo_root, &["merge-base", "--is-ancestor", &branch, "HEAD"])
        .map_err(|error| preserve_error(format!("failed to inspect ancestry: {error}")))?;
    match ancestor.status.code() {
        Some(0) => {
            let commit = run_git(repo_root, &["rev-parse", "HEAD"]).ok();
            let cleanup_warning = cleanup_managed_worktree(repo_root, &path, &branch).err();
            return Ok(WorktreeIntegrationOutcome {
                status: WorktreeIntegrationStatus::AlreadyIntegrated,
                branch,
                path: Some(path),
                changed_files,
                merge_commit: commit,
                cleanup_warning,
            });
        }
        Some(1) => {}
        _ => {
            return Err(preserve_error(format!(
                "failed to inspect integration ancestry: {}",
                bounded_output(&ancestor, 2_000)
            )));
        }
    }

    reject_active_git_operation(repo_root).map_err(preserve_error)?;
    let staged = git_nul_paths(repo_root, &["diff", "--cached", "--name-only", "-z"])
        .map_err(|error| preserve_error(format!("failed to inspect staged changes: {error}")))?;
    if !staged.is_empty() {
        return Err(preserve_error(format!(
            "local checkout index is not clean; refusing to include user-staged files in an EKO merge commit: {}",
            staged.join(", ")
        )));
    }
    let dirty = main_dirty_paths(repo_root)
        .map_err(|error| preserve_error(format!("failed to inspect local changes: {error}")))?;
    let dirty_overlap: Vec<String> = changed_files
        .iter()
        .filter(|path| dirty.contains(path.as_str()))
        .cloned()
        .collect();
    if !dirty_overlap.is_empty() {
        return Err(preserve_error(format!(
            "local checkout has uncommitted changes in writer-owned paths: {}",
            dirty_overlap.join(", ")
        )));
    }

    let preflight = git_output(
        repo_root,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--messages",
            "HEAD",
            &branch,
        ],
    )
    .map_err(|error| preserve_error(format!("failed to start merge preflight: {error}")))?;
    match preflight.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(preserve_error(format!(
                "worktree merge conflict: {}",
                bounded_output(&preflight, 4_000)
            )));
        }
        _ => {
            return Err(preserve_error(format!(
                "worktree merge preflight failed: {}",
                bounded_output(&preflight, 4_000)
            )));
        }
    }

    let message = format!("Merge EKO task {task_id}\n\n{trailer}");
    let merge = git_output(
        repo_root,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=EKO TaskRuntime",
            "-c",
            "user.email=eko@local",
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &message,
            &branch,
        ],
    )
    .map_err(|error| preserve_error(format!("failed to start worktree merge: {error}")))?;
    if !merge.status.success() {
        let diagnostic = bounded_output(&merge, 4_000);
        let abort_error = abort_own_merge(repo_root).err();
        let abort_suffix = abort_error
            .map(|error| format!("; merge abort also failed: {error}"))
            .unwrap_or_default();
        return Err(preserve_error(format!(
            "worktree merge failed: {diagnostic}{abort_suffix}"
        )));
    }

    let merge_commit = run_git(repo_root, &["rev-parse", "HEAD"]);
    let mut cleanup_warning = cleanup_managed_worktree(repo_root, &path, &branch).err();
    let merge_commit = match merge_commit {
        Ok(commit) => Some(commit),
        Err(error) => {
            let warning = format!("merge succeeded but commit id lookup failed: {error}");
            cleanup_warning = Some(match cleanup_warning {
                Some(cleanup) => format!("{warning}; {cleanup}"),
                None => warning,
            });
            None
        }
    };
    Ok(WorktreeIntegrationOutcome {
        status: WorktreeIntegrationStatus::Merged,
        branch,
        path: Some(path),
        changed_files,
        merge_commit,
        cleanup_warning,
    })
}

fn validate_changed_files(
    ownership: &super::planner::FileOwnership,
    changed_files: &[String],
) -> Result<(), String> {
    let super::planner::FileOwnership::Known(owned) = ownership else {
        return Ok(());
    };
    let outside: Vec<String> = changed_files
        .iter()
        .filter(|path| !owned.contains(path.as_str()))
        .cloned()
        .collect();
    if outside.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "writer changed files outside declared ownership: {}",
            outside.join(", ")
        ))
    }
}

fn commit_writer_changes(
    worktree: &Path,
    task_id: &str,
    trailer: &str,
) -> Result<(), WorktreeError> {
    let message = format!("EKO task {task_id}\n\n{trailer}");
    run_git(
        worktree,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=EKO TaskRuntime",
            "-c",
            "user.email=eko@local",
            "commit",
            "-m",
            &message,
        ],
    )
    .map(|_| ())
}

fn git_nul_paths(repo: &Path, args: &[&str]) -> Result<Vec<String>, WorktreeError> {
    let output = git_output(repo, args)?;
    if !output.status.success() {
        return Err(output_error(args, &output));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| WorktreeError::new("git returned a non-UTF-8 path"))?;
    let mut paths = Vec::new();
    for raw in text.split('\0').filter(|value| !value.is_empty()) {
        let normalized = super::planner::normalize_owned_file(raw)
            .ok_or_else(|| WorktreeError::new(format!("git returned unsafe path: {raw}")))?;
        if !paths.contains(&normalized) {
            paths.push(normalized);
        }
    }
    paths.sort();
    Ok(paths)
}

fn main_dirty_paths(repo_root: &Path) -> Result<std::collections::BTreeSet<String>, WorktreeError> {
    let mut dirty = std::collections::BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        dirty.extend(git_nul_paths(repo_root, &args)?);
    }
    Ok(dirty)
}

fn find_worktree_path(repo_root: &Path, branch: &str) -> Result<Option<PathBuf>, WorktreeError> {
    Ok(find_worktree_info(repo_root, branch)?.map(|worktree| PathBuf::from(worktree.path)))
}

fn find_worktree_info(
    repo_root: &Path,
    branch: &str,
) -> Result<Option<WorktreeInfo>, WorktreeError> {
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&output, repo_root)
        .into_iter()
        .find(|worktree| worktree.branch == branch))
}

fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let reference = format!("refs/heads/{branch}");
    let output = git_output(repo_root, &["show-ref", "--verify", "--quiet", &reference])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(output_error(
            &["show-ref", "--verify", "--quiet", &reference],
            &output,
        )),
    }
}

fn lock_worktree(repo_root: &Path, path: &Path, label: &str) -> Result<(), WorktreeError> {
    let path = path
        .to_str()
        .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
    run_git(
        repo_root,
        &[
            "worktree",
            "lock",
            path,
            "--reason",
            &format!("fork subagent logical task {label} in progress"),
        ],
    )
    .map(|_| ())
}

fn find_integration_commit(
    repo_root: &Path,
    trailer: &str,
) -> Result<Option<String>, WorktreeError> {
    let output = run_git(
        repo_root,
        &[
            "log",
            "HEAD",
            "--format=%H",
            "--fixed-strings",
            "--grep",
            trailer,
            "-n",
            "1",
        ],
    )?;
    let commit = output.trim();
    Ok((!commit.is_empty()).then(|| commit.to_string()))
}

fn reject_active_git_operation(repo_root: &Path) -> Result<(), String> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
    ] {
        let marker_path = run_git(repo_root, &["rev-parse", "--git-path", marker])
            .map_err(|error| format!("failed to inspect Git operation state: {error}"))?;
        let path = PathBuf::from(marker_path);
        let path = if path.is_absolute() {
            path
        } else {
            repo_root.join(path)
        };
        if path.exists() {
            return Err(format!(
                "repository already has an active Git operation ({marker}); integration refused"
            ));
        }
    }
    Ok(())
}

fn abort_own_merge(repo_root: &Path) -> Result<(), WorktreeError> {
    let marker = run_git(repo_root, &["rev-parse", "--git-path", "MERGE_HEAD"])?;
    let marker = PathBuf::from(marker);
    let marker = if marker.is_absolute() {
        marker
    } else {
        repo_root.join(marker)
    };
    if marker.exists() {
        run_git(repo_root, &["merge", "--abort"]).map(|_| ())
    } else {
        Ok(())
    }
}

fn unlock_worktree(repo_root: &Path, path: &Path) -> Result<(), WorktreeError> {
    let path = path
        .to_str()
        .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
    let output = git_output(repo_root, &["worktree", "unlock", path])?;
    if output.status.success() || bounded_output(&output, 2_000).contains("not locked") {
        Ok(())
    } else {
        Err(output_error(&["worktree", "unlock", path], &output))
    }
}

fn cleanup_managed_worktree(repo_root: &Path, path: &Path, branch: &str) -> Result<(), String> {
    let path_text = path
        .to_str()
        .ok_or_else(|| "worktree path is not valid UTF-8".to_string())?;
    let _ = unlock_worktree(repo_root, path);
    run_git(repo_root, &["worktree", "remove", path_text]).map_err(|error| error.to_string())?;
    let ahead = branch_ahead_of_head(repo_root, branch).map_err(|error| error.to_string())?;
    if ahead > 0 {
        return Err(format!(
            "worktree removed but branch {branch} retained because it has {ahead} unique commit(s)"
        ));
    }
    run_git(repo_root, &["branch", "-D", branch]).map_err(|error| error.to_string())?;
    run_git(repo_root, &["worktree", "prune"])
        .map(|_| ())
        .map_err(|error| error.to_string())
}

// ── Unattended worktree management (Phase D) ──────────────────────────

/// Information about an unattended worktree, including its associated run.
#[derive(Debug, Clone)]
pub struct UnattendedWorktreeInfo {
    pub run_id: String,
    pub branch: String,
    pub path: Option<PathBuf>,
    pub head: String,
    pub status: String,
    pub active: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub uncommitted_changes: bool,
    pub ahead_commits: u32,
    pub has_changes: bool,
    pub orphan_branch: bool,
}

#[derive(Debug, Clone, Default)]
pub struct UnattendedCleanupResult {
    pub removed: Vec<String>,
    pub unlocked: Vec<String>,
    pub kept: Vec<String>,
    pub errors: Vec<String>,
}

fn list_prefixed_branches(
    repo_root: &Path,
    prefix: &str,
) -> Result<Vec<(String, String)>, WorktreeError> {
    let pattern = format!("refs/heads/{prefix}*");
    let output = run_git(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            &pattern,
        ],
    )?;
    let mut branches = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let Some((branch, head)) = line.split_once('\0') else {
            return Err(WorktreeError::new(format!(
                "git returned malformed branch metadata: {line}"
            )));
        };
        branches.push((branch.to_string(), head.to_string()));
    }
    branches.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(branches)
}

fn worktree_has_uncommitted_changes(path: &Path) -> Result<bool, WorktreeError> {
    let status = run_git(path, &["status", "--porcelain=v1", "--untracked-files=all"])?;
    Ok(!status.trim().is_empty())
}

fn branch_ahead_of_head(repo_root: &Path, branch: &str) -> Result<u32, WorktreeError> {
    let count = run_git(
        repo_root,
        &["rev-list", "--count", &format!("HEAD..{branch}")],
    )?;
    count.trim().parse::<u32>().map_err(|error| {
        WorktreeError::new(format!(
            "git returned invalid commit count for {branch}: {error}"
        ))
    })
}

/// List every legacy `eko-unattended-*` branch, including orphan branches
/// whose worktree directory is already gone. A branch is considered to hold
/// work when its checkout is dirty or it contains commits not reachable from
/// the authoritative checkout's current `HEAD`.
pub fn list_unattended_worktrees(
    repo_root: &Path,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<Vec<UnattendedWorktreeInfo>, WorktreeError> {
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])?;
    let worktrees = parse_worktree_list(&output, repo_root)
        .into_iter()
        .map(|worktree| (worktree.branch.clone(), worktree))
        .collect::<HashMap<_, _>>();
    let mut unattended = Vec::new();
    for (branch, full_head) in list_prefixed_branches(repo_root, BRANCH_PREFIX)? {
        let run_id = branch
            .strip_prefix(BRANCH_PREFIX)
            .unwrap_or(branch.as_str())
            .to_string();
        let worktree = worktrees.get(&branch);
        let path = worktree.map(|item| PathBuf::from(&item.path));
        let uncommitted_changes = match path.as_deref() {
            Some(path) => worktree_has_uncommitted_changes(path)?,
            None => false,
        };
        let ahead_commits = branch_ahead_of_head(repo_root, &branch)?;
        let status = store
            .and_then(|store| store.get_run(&run_id).ok().flatten())
            .map(|run| run.status.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let active = store.is_some_and(|store| store.is_run_active(&run_id));
        unattended.push(UnattendedWorktreeInfo {
            run_id,
            branch,
            path: path.clone(),
            head: full_head.chars().take(12).collect(),
            status,
            active,
            locked: worktree.is_some_and(|item| item.locked),
            lock_reason: worktree.and_then(|item| item.lock_reason.clone()),
            uncommitted_changes,
            ahead_commits,
            has_changes: uncommitted_changes || ahead_commits > 0,
            orphan_branch: path.is_none(),
        });
    }
    Ok(unattended)
}

fn unattended_worktree(
    repo_root: &Path,
    run_id: &str,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<UnattendedWorktreeInfo, WorktreeError> {
    list_unattended_worktrees(repo_root, store)?
        .into_iter()
        .find(|worktree| worktree.run_id == run_id)
        .ok_or_else(|| {
            WorktreeError::new(format!("Unattended worktree for run {run_id} not found"))
        })
}

/// Merge a retained legacy unattended worktree through the same safe
/// integration boundary used by formal writer Subagents.
pub fn merge_unattended_worktree(
    repo_root: &Path,
    run_id: &str,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<WorktreeIntegrationOutcome, WorktreeError> {
    let worktree = unattended_worktree(repo_root, run_id, store)?;
    if worktree.active {
        return Err(WorktreeError::new(format!(
            "run {run_id} is still active; refusing to integrate its worktree"
        )));
    }
    let trailer = format!("EKO-Unattended-Run: {run_id}");
    if let Some(commit) = find_integration_commit(repo_root, &trailer)? {
        let cleanup_warning = worktree
            .path
            .as_deref()
            .and_then(|path| cleanup_managed_worktree(repo_root, path, &worktree.branch).err());
        return Ok(WorktreeIntegrationOutcome {
            status: WorktreeIntegrationStatus::AlreadyIntegrated,
            branch: worktree.branch,
            path: worktree.path,
            changed_files: Vec::new(),
            merge_commit: Some(commit),
            cleanup_warning,
        });
    }
    let path = match worktree.path {
        Some(path) => path,
        None => {
            let path = default_worktree_path(repo_root, &worktree.branch)?;
            validate_worktree_target(repo_root, &path)?;
            let path_text = path
                .to_str()
                .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
            run_git(repo_root, &["worktree", "add", path_text, &worktree.branch])?;
            path
        }
    };
    integrate_existing_worktree(
        repo_root,
        worktree.branch,
        path,
        &format!("unattended run {run_id}"),
        &trailer,
        &super::planner::FileOwnership::Unknown {
            reason: "legacy unattended worktree",
        },
    )
}

fn remove_unattended_worktree(
    repo_root: &Path,
    worktree: &UnattendedWorktreeInfo,
) -> Result<(), WorktreeError> {
    if let Some(path) = worktree.path.as_deref() {
        unlock_worktree(repo_root, path)?;
        let path = path
            .to_str()
            .ok_or_else(|| WorktreeError::new("worktree path is not valid UTF-8"))?;
        run_git(repo_root, &["worktree", "remove", "--force", path])?;
    }
    run_git(repo_root, &["branch", "-D", &worktree.branch])?;
    run_git(repo_root, &["worktree", "prune"])?;
    Ok(())
}

/// Discard an unattended worktree and its branch after an explicit user
/// decision. Active runs are never removed.
pub fn discard_unattended_worktree(
    repo_root: &Path,
    run_id: &str,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<(), WorktreeError> {
    let worktree = unattended_worktree(repo_root, run_id, store)?;
    if worktree.active {
        return Err(WorktreeError::new(format!(
            "run {run_id} is still active; refusing to discard its worktree"
        )));
    }
    remove_unattended_worktree(repo_root, &worktree)
}

/// Unlock all inactive legacy worktrees and remove only those that provably
/// contain no uncommitted files and no commits ahead of the authoritative
/// checkout. Changed worktrees remain available for review.
pub fn cleanup_unattended_worktrees(
    repo_root: &Path,
    store: Option<&crate::tasks::task_runtime::TaskRuntimeStore>,
) -> Result<UnattendedCleanupResult, WorktreeError> {
    let worktrees = list_unattended_worktrees(repo_root, store)?;
    let mut result = UnattendedCleanupResult::default();
    for worktree in worktrees {
        if worktree.active {
            result.kept.push(worktree.run_id);
            continue;
        }
        if worktree.locked
            && let Some(path) = worktree.path.as_deref()
        {
            match unlock_worktree(repo_root, path) {
                Ok(()) => result.unlocked.push(worktree.run_id.clone()),
                Err(error) => {
                    result.errors.push(format!(
                        "{}: failed to unlock worktree: {error}",
                        worktree.run_id
                    ));
                    result.kept.push(worktree.run_id);
                    continue;
                }
            }
        }
        if worktree.has_changes {
            result.kept.push(worktree.run_id);
            continue;
        }
        match remove_unattended_worktree(repo_root, &worktree) {
            Ok(()) => result.removed.push(worktree.run_id),
            Err(error) => result
                .errors
                .push(format!("{}: cleanup failed: {error}", worktree.run_id)),
        }
    }
    Ok(result)
}

// ── Fork-subagent worktree factory (Sprint 8) ────────────────────────────

/// EKO's application-layer git isolation policy. Acquires one git worktree per logical writer task
/// (via [`RunWorktree::acquire_fork`]) and reuses it across retries. Finalize
/// removes provably clean worktrees immediately and retains changed worktrees
/// for TaskRuntime's later review/integration stage.
///
/// Used by [`EkoIsolationProvider`]; the framework does not know about git.
#[derive(Debug, Clone)]
pub struct EkoWorktreeFactory {
    /// Repository root the worktrees branch from. Resolved by the application
    /// (e.g. `git_repo_root(cwd)`); `None` makes `create` fail fast.
    pub repo_root: PathBuf,
}

impl EkoWorktreeFactory {
    fn isolate(
        &self,
        label: &str,
    ) -> Result<
        echo_agent::agent::subagent::IsolationHandle,
        echo_agent::agent::subagent::IsolationError,
    > {
        // Build the worktree via the shared RunWorktree lifecycle. `acquire_fork`
        // runs blocking git subprocesses; Fork dispatch is already inside a
        // `tokio::spawn`, but the git ops themselves are short and synchronous
        // — acceptable. (If they ever block the runtime, wrap in
        // spawn_blocking inside the factory.)
        let wt = RunWorktree::acquire_fork(label, &self.repo_root)
            .map_err(|error| echo_agent::agent::subagent::IsolationError::new(error.message))?;
        let path = wt.path.clone();
        let evidence_subject = path.to_string_lossy().to_string();
        tracing::info!(
            subagent_label = label,
            worktree = %path.display(),
            branch = %wt.branch,
            "Acquired Fork-subagent worktree"
        );
        // `finalize` owns `wt`: clean checkouts are disposable and removed
        // immediately; changed checkouts are summarized, unlocked, and retained
        // for retry or review/integration.
        Ok(echo_agent::agent::subagent::IsolationHandle {
            path,
            observed: echo_agent::agent::subagent::ObservedIsolation::new("worktree"),
            finalize: Box::new(move || {
                let has_changes = match wt.has_changes() {
                    Ok(has_changes) => has_changes,
                    Err(error) => {
                        let _ = wt.unlock();
                        return Err(echo_agent::agent::subagent::IsolationError::new(
                            error.message,
                        ));
                    }
                };
                if !has_changes {
                    cleanup_managed_worktree(&wt.repo_root, &wt.path, &wt.branch)
                        .map_err(echo_agent::agent::subagent::IsolationError::new)?;
                    tracing::info!(
                        worktree = %wt.path.display(),
                        branch = %wt.branch,
                        "Removed clean Fork-subagent worktree"
                    );
                    return Ok(echo_agent::agent::subagent::IsolationOutcome {
                        summary: "(no worktree changes; clean checkout removed)".to_string(),
                        artifacts: Vec::new(),
                        evidence: vec![echo_agent::agent::subagent::SubagentEvidence {
                            kind: "worktree".to_string(),
                            subject: evidence_subject,
                            outcome: Some("clean".to_string()),
                            details: String::new(),
                            source: echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                            attributes: serde_json::Value::Null,
                        }],
                    });
                }
                let summary = wt.diff_summary();
                let unlock = wt.unlock();
                match (summary, unlock) {
                    (Ok(summary), Ok(())) => Ok(echo_agent::agent::subagent::IsolationOutcome {
                        summary: summary.clone(),
                        artifacts: Vec::new(),
                        evidence: vec![echo_agent::agent::subagent::SubagentEvidence {
                            kind: "worktree".to_string(),
                            subject: evidence_subject,
                            outcome: Some("changed".to_string()),
                            details: summary,
                            source: echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                            attributes: serde_json::Value::Null,
                        }],
                    }),
                    (Err(error), _) | (_, Err(error)) => Err(
                        echo_agent::agent::subagent::IsolationError::new(error.message),
                    ),
                }
            }),
        })
    }
}

impl EkoWorktreeFactory {
    /// Construct a factory bound to the given repo root. The caller resolves
    /// the root (typically `git_repo_root(cwd)`); pass `None` up-front by not
    /// constructing a factory at all if isolation is unavailable.
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

// ── Data-subagent workspace factory (Sprint 10) ──────────────────────────

/// EKO's application-layer data isolation policy. Creates a per-subagent `tempfile::TempDir`
/// (disjoint working directory, NO git coupling) for Fork-dispatched
/// data/research subagents emitting generated artifacts (CSVs/parquet/charts).
///
/// Unlike `EkoWorktreeFactory` (git-coupled, diff-finalize), this gives each
/// subagent an isolated tmpdir whose lifecycle is: create → subagent writes
/// disjoint output files into it → finalize lists those files (so the
/// orchestrator/analyst can concat+synthesize). The TempDir is KEPT across
/// the run (not auto-cleaned on finalize) so a downstream analyst subagent can
/// read the shards; it is dropped (cleaned) only when the handle itself is
/// dropped, which happens after finalize returns.
///
/// Used by [`EkoIsolationProvider`]; the framework does not know about tmpdirs.
#[derive(Debug, Clone)]
pub struct EkoDataWorkspaceFactory {
    /// Optional parent dir under which subagent tmpdirs are created. `None`
    /// uses the OS temp dir (`std::env::temp_dir()`). Keeping workspaces under
    /// a known parent aids debugging/cleanup.
    pub base_dir: Option<PathBuf>,
}

impl EkoDataWorkspaceFactory {
    fn isolate(
        &self,
        label: &str,
    ) -> Result<
        echo_agent::agent::subagent::IsolationHandle,
        echo_agent::agent::subagent::IsolationError,
    > {
        // Sanitize the label into a directory-name prefix (TempDir appends a
        // random suffix, so collisions are impossible and the prefix just aids
        // debuggability when listing /tmp).
        let safe_label: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let prefix = format!("eko-data-{safe_label}-");

        // Create the tmpdir (under base_dir if given, else OS temp).
        let dir = match &self.base_dir {
            Some(base) => tempfile::Builder::new().prefix(&prefix).tempdir_in(base),
            None => tempfile::Builder::new().prefix(&prefix).tempdir(),
        }
        .map_err(|e| {
            echo_agent::agent::subagent::IsolationError::new(format!(
                "failed to create data workspace tmpdir ({prefix}): {e}"
            ))
        })?;

        // KEEP the tmpdir from auto-cleanup: a downstream analyst subagent may
        // need to read this subagent's output shards after this dispatch returns.
        // `keep()` consumes the TempDir without removing it; cleanup is then
        // the application/user's responsibility (or a future sweep).
        let final_path = dir.keep();

        tracing::info!(
            subagent_label = label,
            workspace = %final_path.display(),
            "Created Fork-data-subagent workspace (tmpdir, kept for collect)"
        );

        let path_for_finalize = final_path.clone();
        Ok(echo_agent::agent::subagent::IsolationHandle {
            path: final_path,
            observed: echo_agent::agent::subagent::ObservedIsolation::new("workspace"),
            finalize: Box::new(move || {
                // List the files the subagent generated (non-recursive top-level
                // entries; data tools typically write flat outputs). The
                // orchestrator/analyst reads this to find each subagent's shards
                // for concat+synthesize.
                let mut entries: Vec<String> = std::fs::read_dir(&path_for_finalize)
                    .map_err(|e| {
                        echo_agent::agent::subagent::IsolationError::new(format!(
                            "workspace finalize read_dir failed: {e}"
                        ))
                    })?
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.is_empty() { None } else { Some(name) }
                    })
                    .collect();
                entries.sort();
                let summary = if entries.is_empty() {
                    "(no output files generated)".to_string()
                } else {
                    entries.join("\n")
                };
                let artifacts = entries
                    .into_iter()
                    .map(|entry| echo_agent::agent::subagent::SubagentArtifact {
                        path: path_for_finalize.join(&entry).to_string_lossy().to_string(),
                        kind: "workspace_output".to_string(),
                        bytes: None,
                        sha256: None,
                        producer_execution_id: None,
                        available: path_for_finalize.join(entry).is_file(),
                    })
                    .collect();
                Ok(echo_agent::agent::subagent::IsolationOutcome {
                    summary,
                    artifacts,
                    evidence: Vec::new(),
                })
            }),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct EkoIsolationProvider {
    worktree: Option<EkoWorktreeFactory>,
    workspace: EkoDataWorkspaceFactory,
}

impl EkoIsolationProvider {
    pub fn new(repo_root: Option<PathBuf>) -> Self {
        Self {
            worktree: repo_root.map(EkoWorktreeFactory::new),
            workspace: EkoDataWorkspaceFactory::new(),
        }
    }
}

impl echo_agent::agent::subagent::IsolationProvider for EkoIsolationProvider {
    fn isolate(
        &self,
        request: &echo_agent::agent::subagent::IsolationRequest,
    ) -> Result<
        echo_agent::agent::subagent::IsolationHandle,
        echo_agent::agent::subagent::IsolationError,
    > {
        match request.kind.as_str() {
            "worktree" => self
                .worktree
                .as_ref()
                .ok_or_else(|| {
                    echo_agent::agent::subagent::IsolationError::new(
                        "worktree isolation requires a git repository",
                    )
                })?
                .isolate(&request.label),
            "workspace" => self.workspace.isolate(&request.label),
            kind => Err(echo_agent::agent::subagent::IsolationError::new(format!(
                "unsupported EKO isolation kind '{kind}'"
            ))),
        }
    }
}

impl EkoDataWorkspaceFactory {
    /// Construct a factory that creates subagent tmpdirs under the OS temp dir.
    pub fn new() -> Self {
        Self { base_dir: None }
    }

    /// Create subagent tmpdirs under the given base directory (for debuggable
    /// placement / consolidated cleanup).
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir: Some(base_dir),
        }
    }
}

impl Default for EkoDataWorkspaceFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    // ── Pure-logic tests (no git subprocess needed) ──

    use super::*;
    use std::path::PathBuf;

    fn init_repo() -> Result<(tempfile::TempDir, PathBuf), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).map_err(|error| error.to_string())?;
        run_git(&repo, &["init", "-b", "main"]).map_err(|error| error.to_string())?;
        run_git(&repo, &["config", "user.name", "EKO Test"]).map_err(|error| error.to_string())?;
        run_git(&repo, &["config", "user.email", "eko-test@local"])
            .map_err(|error| error.to_string())?;
        std::fs::write(repo.join("shared.txt"), "base\n").map_err(|error| error.to_string())?;
        run_git(&repo, &["add", "shared.txt"]).map_err(|error| error.to_string())?;
        run_git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        )
        .map_err(|error| error.to_string())?;
        Ok((temp, repo))
    }

    fn known_ownership(files: &[&str]) -> super::super::planner::FileOwnership {
        super::super::planner::FileOwnership::Known(
            files.iter().map(|file| file.to_string()).collect(),
        )
    }

    fn create_legacy_unattended_worktree(repo: &Path, run_id: &str) -> Result<PathBuf, String> {
        let branch = format!("{BRANCH_PREFIX}{run_id}");
        let path = default_worktree_path(repo, &branch).map_err(|error| error.to_string())?;
        let path_text = path
            .to_str()
            .ok_or_else(|| "legacy worktree path is not valid UTF-8".to_string())?;
        run_git(repo, &["worktree", "add", "-b", &branch, path_text, "HEAD"])
            .map_err(|error| error.to_string())?;
        run_git(
            repo,
            &[
                "worktree",
                "lock",
                path_text,
                "--reason",
                &format!("unattended run {run_id} in progress"),
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(path)
    }

    #[test]
    fn fork_branch_identity_is_valid_and_stable() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-task:1 with spaces";
        let first = fork_branch_name(label);
        let second = fork_branch_name(label);
        if first != second {
            return Err("fork branch identity must be stable".to_string());
        }
        validate_branch_name(&repo, &first).map_err(|error| error.to_string())
    }

    #[test]
    fn fork_acquire_reuses_unlocked_dirty_worktree() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-run-1:task-1";
        let first = RunWorktree::acquire_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(first.path.join("retry.txt"), "first attempt\n")
            .map_err(|error| error.to_string())?;
        first.unlock().map_err(|error| error.to_string())?;

        let second = RunWorktree::acquire_fork(label, &repo).map_err(|error| error.to_string())?;
        if second.path != first.path || second.branch != first.branch {
            return Err("retry acquired a different worktree".to_string());
        }
        let contents = std::fs::read_to_string(second.path.join("retry.txt"))
            .map_err(|error| error.to_string())?;
        if contents != "first attempt\n" {
            return Err("retry did not preserve the previous attempt's changes".to_string());
        }
        second.unlock().map_err(|error| error.to_string())?;
        std::fs::remove_file(second.path.join("retry.txt")).map_err(|error| error.to_string())?;
        cleanup_managed_worktree(&repo, &second.path, &second.branch)
    }

    #[test]
    fn clean_factory_finalize_removes_checkout_and_branch() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-run-clean:task-clean";
        let branch = fork_branch_name(label);
        let factory = EkoWorktreeFactory::new(repo.clone());
        let handle = factory.isolate(label).map_err(|error| error.to_string())?;
        let path = handle.path.clone();
        let outcome = (handle.finalize)().map_err(|error| error.to_string())?;

        if !outcome.summary.contains("clean checkout removed") {
            return Err(format!(
                "unexpected clean finalize summary: {}",
                outcome.summary
            ));
        }
        if path.exists() {
            return Err("clean worktree directory was retained".to_string());
        }
        if branch_exists(&repo, &branch).map_err(|error| error.to_string())? {
            return Err("clean worktree branch was retained".to_string());
        }
        Ok(())
    }

    #[test]
    fn dirty_factory_finalize_retains_and_unlocks_checkout() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-run-dirty:task-dirty";
        let factory = EkoWorktreeFactory::new(repo.clone());
        let handle = factory.isolate(label).map_err(|error| error.to_string())?;
        let path = handle.path.clone();
        let dirty_path = path.join("dirty.txt");
        std::fs::write(&dirty_path, "retain me\n").map_err(|error| error.to_string())?;
        (handle.finalize)().map_err(|error| error.to_string())?;

        let branch = fork_branch_name(label);
        let info = find_worktree_info(&repo, &branch)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "dirty worktree was removed".to_string())?;
        if info.locked {
            return Err("dirty worktree remained locked after finalize".to_string());
        }
        if !path.exists() {
            return Err("dirty worktree directory was removed".to_string());
        }
        std::fs::remove_file(dirty_path).map_err(|error| error.to_string())?;
        cleanup_managed_worktree(&repo, &path, &branch)
    }

    #[test]
    fn automatic_cleanup_refuses_new_dirty_content() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-cleanup-race";
        let worktree = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        worktree.unlock().map_err(|error| error.to_string())?;
        let dirty_path = worktree.path.join("late-change.txt");
        std::fs::write(&dirty_path, "arrived after clean check\n")
            .map_err(|error| error.to_string())?;

        if cleanup_managed_worktree(&repo, &worktree.path, &worktree.branch).is_ok() {
            return Err("automatic cleanup removed a dirty worktree".to_string());
        }
        if !dirty_path.exists() {
            return Err("automatic cleanup lost newly written content".to_string());
        }
        std::fs::remove_file(dirty_path).map_err(|error| error.to_string())?;
        cleanup_managed_worktree(&repo, &worktree.path, &worktree.branch)
    }

    #[test]
    fn automatic_cleanup_retains_new_unique_commit() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-cleanup-commit-race";
        let worktree = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        worktree.unlock().map_err(|error| error.to_string())?;
        std::fs::write(worktree.path.join("committed.txt"), "preserve commit\n")
            .map_err(|error| error.to_string())?;
        run_git(&worktree.path, &["add", "committed.txt"]).map_err(|error| error.to_string())?;
        run_git(
            &worktree.path,
            &["-c", "commit.gpgsign=false", "commit", "-m", "late commit"],
        )
        .map_err(|error| error.to_string())?;

        if cleanup_managed_worktree(&repo, &worktree.path, &worktree.branch).is_ok() {
            return Err("automatic cleanup deleted a branch with unique commits".to_string());
        }
        if !branch_exists(&repo, &worktree.branch).map_err(|error| error.to_string())? {
            return Err("automatic cleanup lost the unique commit branch".to_string());
        }
        if find_worktree_path(&repo, &worktree.branch)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("clean committed checkout should have been released".to_string());
        }
        let outcome = integrate_fork_worktree(
            &repo,
            label,
            "cleanup-commit-race",
            "cleanup-commit-race:1",
            &known_ownership(&["committed.txt"]),
        )
        .map_err(|error| error.to_string())?;
        if outcome.status != WorktreeIntegrationStatus::Merged {
            return Err(format!(
                "orphan branch should integrate after materialization, got {:?}",
                outcome.status
            ));
        }
        if std::fs::read_to_string(repo.join("committed.txt")).map_err(|error| error.to_string())?
            != "preserve commit\n"
        {
            return Err("orphan branch commit was not integrated".to_string());
        }
        Ok(())
    }

    #[test]
    fn cleaned_worktree_integrates_as_no_changes() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-run-empty:task-empty";
        let factory = EkoWorktreeFactory::new(repo.clone());
        let handle = factory.isolate(label).map_err(|error| error.to_string())?;
        (handle.finalize)().map_err(|error| error.to_string())?;

        let outcome = integrate_fork_worktree(
            &repo,
            label,
            "task-empty",
            "task-empty:2",
            &known_ownership(&["shared.txt"]),
        )
        .map_err(|error| error.to_string())?;
        if outcome.status != WorktreeIntegrationStatus::NoChanges {
            return Err(format!(
                "cleaned worktree should integrate as no changes, got {:?}",
                outcome.status
            ));
        }
        Ok(())
    }

    #[test]
    fn disjoint_fork_worktrees_merge_into_main() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label_a = "implementer-task-a:1";
        let label_b = "implementer-task-b:1";
        let wt_a = RunWorktree::create_fork(label_a, &repo).map_err(|error| error.to_string())?;
        let wt_b = RunWorktree::create_fork(label_b, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt_a.path.join("a.txt"), "a\n").map_err(|error| error.to_string())?;
        std::fs::write(wt_b.path.join("b.txt"), "b\n").map_err(|error| error.to_string())?;

        let outcome_a = integrate_fork_worktree(
            &repo,
            label_a,
            "task-a",
            "task-a:1",
            &known_ownership(&["a.txt"]),
        )
        .map_err(|error| error.to_string())?;
        let outcome_b = integrate_fork_worktree(
            &repo,
            label_b,
            "task-b",
            "task-b:1",
            &known_ownership(&["b.txt"]),
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(outcome_a.status, WorktreeIntegrationStatus::Merged);
        assert_eq!(outcome_b.status, WorktreeIntegrationStatus::Merged);
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).map_err(|error| error.to_string())?,
            "a\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("b.txt")).map_err(|error| error.to_string())?,
            "b\n"
        );
        Ok(())
    }

    #[test]
    fn ownership_violation_does_not_touch_main_checkout() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-ownership:1";
        let wt = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt.path.join("outside.txt"), "outside\n")
            .map_err(|error| error.to_string())?;
        let error = integrate_fork_worktree(
            &repo,
            label,
            "ownership",
            "ownership:1",
            &known_ownership(&["declared.txt"]),
        )
        .err()
        .ok_or_else(|| "ownership violation should fail integration".to_string())?;
        if !error.message.contains("outside declared ownership") {
            return Err(format!("unexpected ownership error: {error}"));
        }
        if repo.join("outside.txt").exists() {
            return Err("ownership violation leaked into main checkout".to_string());
        }
        if !wt.path.exists() {
            return Err("failed integration must preserve the worktree".to_string());
        }
        Ok(())
    }

    #[test]
    fn conflicting_worktree_fails_without_dirtying_main_index() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label_a = "implementer-conflict-a:1";
        let label_b = "implementer-conflict-b:1";
        let wt_a = RunWorktree::create_fork(label_a, &repo).map_err(|error| error.to_string())?;
        let wt_b = RunWorktree::create_fork(label_b, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt_a.path.join("shared.txt"), "first\n")
            .map_err(|error| error.to_string())?;
        std::fs::write(wt_b.path.join("shared.txt"), "second\n")
            .map_err(|error| error.to_string())?;
        integrate_fork_worktree(
            &repo,
            label_a,
            "conflict-a",
            "conflict-a:1",
            &known_ownership(&["shared.txt"]),
        )
        .map_err(|error| error.to_string())?;
        let error = integrate_fork_worktree(
            &repo,
            label_b,
            "conflict-b",
            "conflict-b:1",
            &known_ownership(&["shared.txt"]),
        )
        .err()
        .ok_or_else(|| "second conflicting integration should fail".to_string())?;
        if !error.message.contains("merge conflict") {
            return Err(format!("unexpected conflict error: {error}"));
        }
        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt"))
                .map_err(|read_error| read_error.to_string())?,
            "first\n"
        );
        let status = run_git(&repo, &["status", "--porcelain"])
            .map_err(|status_error| status_error.to_string())?;
        if !status.is_empty() {
            return Err(format!(
                "main checkout left dirty after preflight: {status}"
            ));
        }
        if !wt_b.path.exists() {
            return Err("conflicting worktree must be preserved".to_string());
        }
        Ok(())
    }

    #[test]
    fn local_dirty_owned_path_blocks_integration() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-local-dirty:1";
        let wt = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt.path.join("shared.txt"), "subagent\n")
            .map_err(|error| error.to_string())?;
        std::fs::write(repo.join("shared.txt"), "local\n").map_err(|error| error.to_string())?;

        let error = integrate_fork_worktree(
            &repo,
            label,
            "local-dirty",
            "local-dirty:1",
            &known_ownership(&["shared.txt"]),
        )
        .err()
        .ok_or_else(|| "local dirty ownership overlap should fail".to_string())?;
        if !error.message.contains("uncommitted changes") {
            return Err(format!("unexpected dirty-overlap error: {error}"));
        }
        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt"))
                .map_err(|read_error| read_error.to_string())?,
            "local\n"
        );
        Ok(())
    }

    #[test]
    fn staged_user_change_is_never_captured_by_merge_commit() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-staged-user-change:1";
        let wt = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt.path.join("subagent.txt"), "subagent\n")
            .map_err(|error| error.to_string())?;
        std::fs::write(repo.join("user.txt"), "user\n").map_err(|error| error.to_string())?;
        run_git(&repo, &["add", "user.txt"]).map_err(|error| error.to_string())?;

        let error = integrate_fork_worktree(
            &repo,
            label,
            "staged-user-change",
            "staged-user-change:1",
            &known_ownership(&["subagent.txt"]),
        )
        .err()
        .ok_or_else(|| "staged user change should block integration".to_string())?;
        if !error.message.contains("index is not clean") {
            return Err(format!("unexpected staged-index error: {error}"));
        }
        let staged = run_git(&repo, &["diff", "--cached", "--name-only"])
            .map_err(|status_error| status_error.to_string())?;
        assert_eq!(staged, "user.txt");
        if repo.join("subagent.txt").exists() {
            return Err("blocked integration leaked subagent file into main".to_string());
        }
        Ok(())
    }

    #[test]
    fn committed_subagent_diff_uses_fixed_creation_base() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-committed:1";
        let wt = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt.path.join("committed.txt"), "committed\n")
            .map_err(|error| error.to_string())?;
        run_git(&wt.path, &["add", "committed.txt"]).map_err(|error| error.to_string())?;
        run_git(
            &wt.path,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "subagent commit",
            ],
        )
        .map_err(|error| error.to_string())?;
        let diff = wt.diff_summary().map_err(|error| error.to_string())?;
        if !diff.contains("committed.txt") {
            return Err(format!("committed diff lost its creation base: {diff}"));
        }
        let outcome = integrate_fork_worktree(
            &repo,
            label,
            "committed",
            "committed:1",
            &known_ownership(&["committed.txt"]),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(outcome.status, WorktreeIntegrationStatus::Merged);
        Ok(())
    }

    #[test]
    fn repeated_integration_detects_completed_execution() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let label = "implementer-resume:1";
        let wt = RunWorktree::create_fork(label, &repo).map_err(|error| error.to_string())?;
        std::fs::write(wt.path.join("resume.txt"), "done\n").map_err(|error| error.to_string())?;
        integrate_fork_worktree(
            &repo,
            label,
            "resume",
            "resume:1",
            &known_ownership(&["resume.txt"]),
        )
        .map_err(|error| error.to_string())?;
        let second = integrate_fork_worktree(
            &repo,
            label,
            "resume",
            "resume:1",
            &known_ownership(&["resume.txt"]),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(second.status, WorktreeIntegrationStatus::AlreadyIntegrated);
        Ok(())
    }

    #[test]
    fn unattended_cleanup_removes_unchanged_worktree_and_orphan_branch() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let clean_path = create_legacy_unattended_worktree(&repo, "clean-run")?;
        let orphan_branch = format!("{BRANCH_PREFIX}orphan-run");
        run_git(&repo, &["branch", &orphan_branch, "HEAD"]).map_err(|error| error.to_string())?;

        let result =
            cleanup_unattended_worktrees(&repo, None).map_err(|error| error.to_string())?;
        if !result.removed.iter().any(|run_id| run_id == "clean-run")
            || !result.removed.iter().any(|run_id| run_id == "orphan-run")
        {
            return Err(format!("unexpected cleanup result: {result:?}"));
        }
        if clean_path.exists() {
            return Err("unchanged worktree directory should be removed".to_string());
        }
        let remaining =
            list_unattended_worktrees(&repo, None).map_err(|error| error.to_string())?;
        if !remaining.is_empty() {
            return Err(format!("unexpected remaining worktrees: {remaining:?}"));
        }
        Ok(())
    }

    #[test]
    fn unattended_cleanup_preserves_changes_and_releases_stale_lock() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let path = create_legacy_unattended_worktree(&repo, "changed-run")?;
        std::fs::write(path.join("review.txt"), "keep me\n").map_err(|error| error.to_string())?;

        let result =
            cleanup_unattended_worktrees(&repo, None).map_err(|error| error.to_string())?;
        if !result.kept.iter().any(|run_id| run_id == "changed-run")
            || !result.unlocked.iter().any(|run_id| run_id == "changed-run")
        {
            return Err(format!("unexpected cleanup result: {result:?}"));
        }
        let listed = list_unattended_worktrees(&repo, None).map_err(|error| error.to_string())?;
        let item = listed
            .into_iter()
            .find(|item| item.run_id == "changed-run")
            .ok_or_else(|| "changed worktree should remain listed".to_string())?;
        if !item.has_changes || item.locked || item.orphan_branch {
            return Err(format!("unexpected retained worktree state: {item:?}"));
        }
        Ok(())
    }

    #[test]
    fn unattended_cleanup_skips_active_run() -> Result<(), String> {
        let (temp, repo) = init_repo()?;
        let path = create_legacy_unattended_worktree(&repo, "active-run")?;
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                temp.path().join("runtime"),
            )
            .map_err(|error| error.to_string())?,
        );
        let _registration = store
            .register_run_cancellation("active-run", echo_agent::agent::CancellationToken::new())
            .map_err(|error| error.to_string())?;

        let result = cleanup_unattended_worktrees(&repo, Some(store.as_ref()))
            .map_err(|error| error.to_string())?;
        if !result.kept.iter().any(|run_id| run_id == "active-run") || !path.exists() {
            return Err(format!("active worktree was not preserved: {result:?}"));
        }
        let listed = list_unattended_worktrees(&repo, Some(store.as_ref()))
            .map_err(|error| error.to_string())?;
        let item = listed
            .into_iter()
            .find(|item| item.run_id == "active-run")
            .ok_or_else(|| "active worktree should remain listed".to_string())?;
        if !item.active || !item.locked {
            return Err(format!("unexpected active worktree state: {item:?}"));
        }
        Ok(())
    }

    #[test]
    fn unattended_merge_commits_untracked_work_before_integration() -> Result<(), String> {
        let (_temp, repo) = init_repo()?;
        let path = create_legacy_unattended_worktree(&repo, "merge-run")?;
        std::fs::write(path.join("merged.txt"), "merged\n").map_err(|error| error.to_string())?;

        let outcome = merge_unattended_worktree(&repo, "merge-run", None)
            .map_err(|error| error.to_string())?;
        if outcome.status != WorktreeIntegrationStatus::Merged {
            return Err(format!("unexpected merge outcome: {outcome:?}"));
        }
        let merged =
            std::fs::read_to_string(repo.join("merged.txt")).map_err(|error| error.to_string())?;
        if merged != "merged\n" {
            return Err(format!("unexpected merged content: {merged}"));
        }
        if path.exists() {
            return Err("integrated unattended worktree should be removed".to_string());
        }
        Ok(())
    }

    #[test]
    fn unattended_merge_materializes_and_integrates_orphan_branch() -> Result<(), String> {
        let (_tmp, repo) = init_repo()?;
        let path = create_legacy_unattended_worktree(&repo, "orphan-merge")?;
        std::fs::write(path.join("orphan.txt"), "orphan branch change\n")
            .map_err(|error| error.to_string())?;
        run_git(&path, &["add", "orphan.txt"]).map_err(|error| error.to_string())?;
        run_git(
            &path,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=EKO Test",
                "-c",
                "user.email=eko-test@local",
                "commit",
                "-m",
                "orphan change",
            ],
        )
        .map_err(|error| error.to_string())?;
        unlock_worktree(&repo, &path).map_err(|error| error.to_string())?;
        let path_text = path.to_string_lossy().to_string();
        run_git(&repo, &["worktree", "remove", &path_text]).map_err(|error| error.to_string())?;

        let listed = list_unattended_worktrees(&repo, None).map_err(|error| error.to_string())?;
        let orphan = listed
            .iter()
            .find(|item| item.run_id == "orphan-merge")
            .ok_or_else(|| "orphan branch was not listed".to_string())?;
        assert!(orphan.orphan_branch);
        assert!(orphan.has_changes);

        let outcome = merge_unattended_worktree(&repo, "orphan-merge", None)
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome.status, WorktreeIntegrationStatus::Merged);
        assert_eq!(
            std::fs::read_to_string(repo.join("orphan.txt")).map_err(|error| error.to_string())?,
            "orphan branch change\n"
        );
        assert!(
            list_unattended_worktrees(&repo, None)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn eko_worktree_factory_create_fails_outside_git_repo() {
        // Sprint 8: EkoWorktreeFactory implements the framework trait; on a
        // non-git directory, create() must surface an error (RunWorktree::create_fork
        // → git rev-parse fails). This guards the trait wiring without needing
        // a real git repo (which CI/sandbox may not have).
        let tmp = std::env::temp_dir();
        let factory = EkoWorktreeFactory::new(tmp.clone());
        let res = factory.isolate("writer-run1");
        assert!(res.is_err(), "expected error outside a git repo");
    }

    #[test]
    fn eko_data_workspace_factory_create_and_finalize() -> Result<(), String> {
        // Sprint 10: EkoDataWorkspaceFactory creates a real tmpdir, the subagent
        // writes a file into it, and finalize lists the generated files.
        use std::io::Write;
        let factory = EkoDataWorkspaceFactory::new();
        let handle = factory
            .isolate("analyst-run1")
            .map_err(|error| error.to_string())?;
        assert!(handle.path.exists(), "workspace dir should exist");
        assert!(
            handle
                .path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with("eko-data-analyst-run1-"))
                .unwrap_or(false),
            "workspace dir should carry the label-derived prefix"
        );
        // Simulate the subagent writing a disjoint output file.
        let out = handle.path.join("run_001_clean.parquet");
        std::fs::File::create(&out)
            .map_err(|error| error.to_string())?
            .write_all(b"data")
            .map_err(|error| error.to_string())?;
        // Finalize lists the generated files.
        let outcome = (handle.finalize)().map_err(|error| error.to_string())?;
        assert!(
            outcome.summary.contains("run_001_clean.parquet"),
            "got: {}",
            outcome.summary
        );
        Ok(())
    }

    #[test]
    fn eko_data_workspace_factory_empty_finalize_reports_nothing() -> Result<(), String> {
        // No files written → finalize reports "(no output files generated)".
        let factory = EkoDataWorkspaceFactory::new();
        let handle = factory
            .isolate("empty-run")
            .map_err(|error| error.to_string())?;
        let outcome = (handle.finalize)().map_err(|error| error.to_string())?;
        assert_eq!(outcome.summary, "(no output files generated)");
        Ok(())
    }

    #[test]
    fn parse_worktree_list_round_trips_sample_output() {
        let sample = "\
worktree /abs/repo
HEAD abcdef0123456789abcdef0123456789abcdef01
branch refs/heads/main

worktree /abs/repo-wt
HEAD 1111111111111111111111111111111111111111
branch refs/heads/feature
locked unattended run stale in progress

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
        assert!(items[1].locked);
        assert_eq!(
            items[1].lock_reason.as_deref(),
            Some("unattended run stale in progress")
        );
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
