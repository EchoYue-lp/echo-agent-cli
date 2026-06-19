//! Tauri IPC commands for file operations.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    pub extension: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub language: Option<String>,
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
    pub tag: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<TreeNode>>,
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

#[tauri::command]
pub async fn list_files(
    state: tauri::State<'_, TauriState>,
    path: Option<String>,
) -> Result<Vec<FileEntry>, IpcError> {
    let base = get_workspace_root(&state).await;
    let target = if let Some(ref p) = path {
        base.join(p)
    } else {
        base.clone()
    };

    validate_path_within_base(&target, &base).map_err(IpcError::Validation)?;

    if !target.exists() {
        return Err(IpcError::NotFound("Directory not found".to_string()));
    }

    let mut entries = Vec::new();
    if let Ok(dir_entries) = std::fs::read_dir(&target) {
        for entry in dir_entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
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

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

#[tauri::command]
pub async fn read_file(
    state: tauri::State<'_, TauriState>,
    path: String,
) -> Result<FileContent, IpcError> {
    let base = get_workspace_root(&state).await;
    let target = base.join(&path);

    validate_path_within_base(&target, &base).map_err(IpcError::Validation)?;

    if !target.exists() {
        return Err(IpcError::NotFound("File not found".to_string()));
    }
    if target.is_dir() {
        return Err(IpcError::Validation("Path is a directory".to_string()));
    }

    let metadata = std::fs::metadata(&target).map_err(|e| IpcError::Internal(e.to_string()))?;
    if metadata.len() > 1024 * 1024 {
        return Err(IpcError::Validation("File too large (>1MB)".to_string()));
    }

    let content =
        std::fs::read_to_string(&target).map_err(|e| IpcError::Internal(e.to_string()))?;
    let language = detect_language(&path);

    Ok(FileContent {
        path,
        content,
        size: metadata.len(),
        language,
    })
}

#[tauri::command]
pub async fn diff_file(
    state: tauri::State<'_, TauriState>,
    path: String,
    git_ref: Option<String>,
) -> Result<DiffResult, IpcError> {
    let base = get_workspace_root(&state).await;
    let ref_str = git_ref.unwrap_or_else(|| "HEAD".to_string());

    if !is_safe_git_ref(&ref_str) {
        return Err(IpcError::Validation("Invalid git reference".to_string()));
    }

    let target = base.join(&path);
    validate_path_within_base(&target, &base).map_err(IpcError::Validation)?;

    let new_content = if target.exists() {
        std::fs::read_to_string(&target).unwrap_or_default()
    } else {
        String::new()
    };

    let old_content = {
        let base = base.clone();
        let ref_str = ref_str.clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new("git")
                .args(["show", &format!("{}:{}", ref_str, path)])
                .current_dir(&base)
                .output();
            match output {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
                _ => String::new(),
            }
        })
        .await
        .unwrap_or_default()
    };

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
            old_line: if matches!(change.tag(), ChangeTag::Equal | ChangeTag::Delete) {
                Some(old_line)
            } else {
                None
            },
            new_line: if matches!(change.tag(), ChangeTag::Equal | ChangeTag::Insert) {
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

    Ok(DiffResult {
        path,
        old_content,
        new_content,
        hunks,
    })
}

#[tauri::command]
pub async fn file_tree(
    state: tauri::State<'_, TauriState>,
    depth: Option<usize>,
) -> Result<Vec<TreeNode>, IpcError> {
    let base = get_workspace_root(&state).await;
    let max_depth = depth.unwrap_or(3);
    tokio::task::spawn_blocking(move || build_tree(&base, &base, 0, max_depth))
        .await
        .map_err(|e| IpcError::Internal(format!("spawn_blocking failed: {e}")))
}

#[tauri::command]
pub async fn browse_directories(path: Option<String>) -> Result<BrowseResult, IpcError> {
    let home = dirs_home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let home_canonical = home.canonicalize().unwrap_or_else(|_| home.clone());

    let target = if let Some(ref p) = path {
        std::path::PathBuf::from(p)
    } else {
        home.clone()
    };

    let target = if target.exists() && target.is_dir() {
        target
    } else {
        home.clone()
    };

    let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());

    if !canonical.starts_with(&home_canonical) && canonical != home_canonical {
        return Err(IpcError::Validation(
            "Access denied: cannot browse outside home directory".to_string(),
        ));
    }

    let parent = canonical.parent().map(|p| p.to_string_lossy().to_string());

    let mut entries = Vec::new();
    if let Ok(dir_entries) = std::fs::read_dir(&canonical) {
        for entry in dir_entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            entries.push(BrowseEntry {
                name,
                path: path.to_string_lossy().to_string(),
                is_dir: true,
            });
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(BrowseResult {
        current: canonical.to_string_lossy().to_string(),
        parent,
        entries,
    })
}

// ── Helpers ────────────────────────────────────────────────────────

async fn get_workspace_root(state: &TauriState) -> std::path::PathBuf {
    if let Some(ws) = state.app_state.current_workspace().await {
        ws.project_root.unwrap_or(ws.root)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
}

fn validate_path_within_base(
    target: &std::path::Path,
    base: &std::path::Path,
) -> Result<(), String> {
    let canonical_base = base
        .canonicalize()
        .map_err(|e| format!("Cannot resolve base path: {}", e))?;

    let canonical_target = if target.exists() {
        target
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?
    } else {
        let parent = target.parent().ok_or("Path has no parent")?;
        let filename = target.file_name().ok_or("Path has no filename")?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("Cannot resolve parent path: {}", e))?;
        canonical_parent.join(filename)
    };

    if !canonical_target.starts_with(&canonical_base) {
        return Err("Path traversal detected: path escapes workspace boundary".to_string());
    }

    Ok(())
}

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

fn is_safe_git_ref(git_ref: &str) -> bool {
    if git_ref.is_empty() {
        return false;
    }
    // P1-5: tightened charset. `^`/`~`/`@`/`:` are valid git rev syntax
    // (`HEAD^`, `HEAD~2`, `@`, `ref:path`) but they double as injection /
    // treeish-traversal surfaces when the ref is interpolated into `git show
    // <ref>:<path>`. Diff_file only needs branch / tag names, so we restrict
    // to alphanumerics, `-`, `_`, `.`, `/` (branch names like `feat/x`).
    for c in git_ref.chars() {
        if !matches!(c,
            'a'..='z' | 'A'..='Z' | '0'..='9' |
            '-' | '_' | '.' | '/'
        ) {
            return false;
        }
    }
    if git_ref.starts_with('-') || git_ref.starts_with('.') {
        return false;
    }
    if git_ref.contains("..") {
        return false;
    }
    true
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}

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
