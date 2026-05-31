//! 文件系统 API
//!
//! 提供文件浏览、读取、差异对比等端点。
//!
//! | Method | Path              | Description              |
//! |--------|-------------------|--------------------------|
//! | GET    | /api/files/list   | List directory entries   |
//! | GET    | /api/files/read   | Read file content        |
//! | GET    | /api/files/diff   | Git diff for a file      |
//! | GET    | /api/files/tree   | Recursive file tree      |

use axum::{
    Json, debug_handler,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    pub extension: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadParams {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DiffParams {
    pub path: String,
    #[serde(default = "default_ref")]
    pub git_ref: String,
}

fn default_ref() -> String {
    "HEAD".to_string()
}

#[derive(Debug, Serialize)]
pub struct DiffResult {
    pub path: String,
    pub old_content: String,
    pub new_content: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Serialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Serialize)]
pub struct DiffLine {
    pub tag: String, // "equal", "insert", "delete"
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct TreeParams {
    pub depth: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<TreeNode>>,
}

// ── Handlers ───────────────────────────────────────────────────────

/// GET /api/files/list — list directory entries
#[debug_handler]
pub async fn list_files(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Response {
    let base = get_workspace_root(&state).await;
    let target = if let Some(ref p) = params.path {
        base.join(p)
    } else {
        base.clone()
    };

    // Security: validate path stays within workspace boundary
    if let Err(e) = validate_path_within_base(&target, &base) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }

    if !target.exists() {
        return (StatusCode::NOT_FOUND, "Directory not found").into_response();
    }

    let mut entries = Vec::new();
    if let Ok(dir_entries) = std::fs::read_dir(&target) {
        for entry in dir_entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files
            if name.starts_with('.') {
                continue;
            }

            let metadata = entry.metadata().ok();
            let is_dir = path.is_dir();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = metadata.and_then(|m| m.modified().ok()).map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.to_rfc3339()
            });
            let extension = path.extension().map(|e| e.to_string_lossy().to_string());
            let relative = path
                .strip_prefix(&base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            entries.push(FileEntry {
                name,
                path: relative,
                is_dir,
                size,
                modified,
                extension,
            });
        }
    }

    // Sort: dirs first, then by name
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));

    Json(entries).into_response()
}

/// GET /api/files/read — read file content
#[debug_handler]
pub async fn read_file(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ReadParams>,
) -> Response {
    let base = get_workspace_root(&state).await;
    let target = base.join(&params.path);

    // Security: validate path stays within workspace boundary
    if let Err(e) = validate_path_within_base(&target, &base) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }

    if !target.exists() {
        return (StatusCode::NOT_FOUND, "File not found").into_response();
    }
    if target.is_dir() {
        return (StatusCode::BAD_REQUEST, "Path is a directory").into_response();
    }

    // Size limit: 1MB
    let metadata = match std::fs::metadata(&target) {
        Ok(m) => m,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if metadata.len() > 1024 * 1024 {
        return (StatusCode::BAD_REQUEST, "File too large (>1MB)").into_response();
    }

    let content = match std::fs::read_to_string(&target) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let language = detect_language(&params.path);

    Json(FileContent {
        path: params.path,
        content,
        size: metadata.len(),
        language,
    })
    .into_response()
}

/// GET /api/files/diff — compute git diff for a file
#[debug_handler]
pub async fn diff_file(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DiffParams>,
) -> Response {
    let base = get_workspace_root(&state).await;

    // Security: validate git_ref contains only safe characters
    if !is_safe_git_ref(&params.git_ref) {
        return (StatusCode::BAD_REQUEST, "Invalid git reference").into_response();
    }

    // Security: validate path stays within workspace boundary
    let target = base.join(&params.path);
    if let Err(e) = validate_path_within_base(&target, &base) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }

    // Get current content
    let new_content = if target.exists() {
        std::fs::read_to_string(&target).unwrap_or_default()
    } else {
        String::new()
    };

    // Get old content via git show
    let old_content = {
        let output = std::process::Command::new("git")
            .args([
                "show",
                &format!("{}:{}", params.git_ref, params.path),
            ])
            .current_dir(&base)
            .output();
        match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => String::new(),
        }
    };

    // Compute diff using the `similar` crate
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(&old_content, &new_content);

    let mut hunks = Vec::new();
    let mut current_hunk_lines = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut hunk_old_start = 0usize;
    let mut hunk_new_start = 0usize;
    let mut hunk_old_count = 0usize;
    let mut hunk_new_count = 0usize;

    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                "equal"
            }
            ChangeTag::Insert => {
                new_line += 1;
                hunk_new_count += 1;
                "insert"
            }
            ChangeTag::Delete => {
                old_line += 1;
                hunk_old_count += 1;
                "delete"
            }
        };

        if current_hunk_lines.is_empty() {
            hunk_old_start = old_line;
            hunk_new_start = new_line;
            hunk_old_count = 0;
            hunk_new_count = 0;
        }

        current_hunk_lines.push(DiffLine {
            tag: tag.to_string(),
            old_line: if matches!(
                change.tag(),
                ChangeTag::Equal | ChangeTag::Delete
            ) {
                Some(old_line)
            } else {
                None
            },
            new_line: if matches!(
                change.tag(),
                ChangeTag::Equal | ChangeTag::Insert
            ) {
                Some(new_line)
            } else {
                None
            },
            content: change.value().to_string(),
        });
    }

    if !current_hunk_lines.is_empty() {
        hunks.push(DiffHunk {
            old_start: hunk_old_start,
            old_count: hunk_old_count,
            new_start: hunk_new_start,
            new_count: hunk_new_count,
            lines: current_hunk_lines,
        });
    }

    Json(DiffResult {
        path: params.path,
        old_content,
        new_content,
        hunks,
    })
    .into_response()
}

/// GET /api/files/tree — recursive file tree
#[debug_handler]
pub async fn file_tree(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TreeParams>,
) -> Response {
    let base = get_workspace_root(&state).await;
    let max_depth = params.depth.unwrap_or(3);

    // Security: validate base path
    if let Err(e) = validate_path_within_base(&base, &base) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }

    let nodes = build_tree(&base, &base, 0, max_depth);
    Json(nodes).into_response()
}

// ── Helpers ────────────────────────────────────────────────────────

fn build_tree(
    root: &std::path::Path,
    current: &std::path::Path,
    depth: usize,
    max_depth: usize,
) -> Vec<TreeNode> {
    if depth >= max_depth {
        return vec![];
    }

    let mut nodes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(current) {
        let mut sorted: Vec<_> = entries.flatten().collect();
        sorted.sort_by_key(|e| {
            let is_dir = e.path().is_dir();
            let name = e.file_name().to_string_lossy().to_string();
            (!is_dir, name)
        });

        for entry in sorted {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }

            let is_dir = path.is_dir();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            let children = if is_dir {
                Some(build_tree(root, &path, depth + 1, max_depth))
            } else {
                None
            };

            nodes.push(TreeNode {
                name,
                path: relative,
                is_dir,
                children,
            });
        }
    }
    nodes
}

/// Get the workspace root or fall back to CWD.
async fn get_workspace_root(state: &AppState) -> std::path::PathBuf {
    if let Some(ws) = state.current_workspace().await {
        ws.project_root.unwrap_or(ws.root)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
}

/// Detect programming language from file extension.
fn detect_language(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?;
    let lang = match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" => "cpp",
        "rb" => "ruby",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "md" => "markdown",
        "sh" => "bash",
        "sql" => "sql",
        "html" => "html",
        "css" => "css",
        _ => return None,
    };
    Some(lang.to_string())
}

// ── Directory browser (for workspace selection) ─────────────────────

#[derive(Debug, Deserialize)]
pub struct BrowseParams {
    /// Absolute path to browse. Defaults to user home directory.
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub current: String,
    pub parent: Option<String>,
    pub entries: Vec<BrowseEntry>,
}

#[derive(Debug, Serialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// GET /api/files/browse — browse directories for workspace selection
#[debug_handler]
pub async fn browse_directories(
    Query(params): Query<BrowseParams>,
) -> Response {
    let target = if let Some(ref p) = params.path {
        std::path::PathBuf::from(p)
    } else {
        // Default to home directory
        dirs_home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    };

    let target = if target.exists() && target.is_dir() {
        target
    } else {
        dirs_home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    };

    let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());
    let parent = canonical.parent().map(|p| p.to_string_lossy().to_string());

    let mut entries = Vec::new();
    if let Ok(dir_entries) = std::fs::read_dir(&canonical) {
        for entry in dir_entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files
            if name.starts_with('.') {
                continue;
            }

            let is_dir = path.is_dir();
            // Only include directories and symlinks to directories
            if !is_dir {
                continue;
            }

            let abs_path = path.to_string_lossy().to_string();

            entries.push(BrowseEntry {
                name,
                path: abs_path,
                is_dir,
            });
        }
    }

    // Sort by name
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Json(BrowseResult {
        current: canonical.to_string_lossy().to_string(),
        parent,
        entries,
    })
    .into_response()
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
}

/// Validate that a path stays within the base directory boundary.
/// Returns Ok(()) if safe, Err(reason) if path traversal detected.
fn validate_path_within_base(
    target: &std::path::Path,
    base: &std::path::Path,
) -> Result<(), String> {
    // Canonicalize both paths to resolve symlinks and ..
    let canonical_base = base
        .canonicalize()
        .map_err(|e| format!("Cannot resolve base path: {}", e))?;

    // If target doesn't exist yet, canonicalize its parent + filename
    let canonical_target = if target.exists() {
        target
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?
    } else {
        // For non-existent paths, validate the parent directory
        let parent = target.parent().ok_or("Path has no parent")?;
        let filename = target
            .file_name()
            .ok_or("Path has no filename")?;

        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("Cannot resolve parent path: {}", e))?;

        canonical_parent.join(filename)
    };

    // Check that target starts with base
    if !canonical_target.starts_with(&canonical_base) {
        return Err("Path traversal detected: path escapes workspace boundary".to_string());
    }

    Ok(())
}

/// Validate that a git reference contains only safe characters.
/// Allows: alphanumeric, dash, underscore, dot, slash, caret, tilde.
/// Rejects: spaces, shell metacharacters, backticks, etc.
fn is_safe_git_ref(git_ref: &str) -> bool {
    if git_ref.is_empty() {
        return false;
    }

    // Check each character
    for c in git_ref.chars() {
        if !matches!(c,
            'a'..='z' | 'A'..='Z' | '0'..='9' |
            '-' | '_' | '.' | '/' | '^' | '~' | '@' | ':'
        ) {
            return false;
        }
    }

    // Reject refs that start with special characters
    if git_ref.starts_with('-') || git_ref.starts_with('.') {
        return false;
    }

    // Reject refs with consecutive dots (potential path traversal)
    if git_ref.contains("..") {
        return false;
    }

    true
}
