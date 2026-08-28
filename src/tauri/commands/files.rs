//! Tauri IPC commands for file operations.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PREVIEW_BYTES: u64 = 10 * 1024 * 1024;

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
    pub workspace_id: String,
    pub workspace_generation: String,
    pub path: String,
    pub content: String,
    pub size: u64,
    pub language: Option<String>,
    pub kind: String,
    pub mime_type: Option<String>,
    pub data_url: Option<String>,
    pub revision: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceChange {
    pub path: String,
    pub status: String,
}

pub type DiffResult = echo_agent_app_core::diff::WorkspaceFileDiff;

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
    workspace_id: String,
    workspace_generation: String,
    path: Option<String>,
) -> Result<Vec<FileEntry>, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("list workspace files", move || {
            let base = control.project_root();
            list_workspace_files(&base, path.as_deref())
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

fn list_workspace_files(
    base: &std::path::Path,
    path: Option<&str>,
) -> Result<Vec<FileEntry>, IpcError> {
    let target = if let Some(p) = path {
        base.join(p)
    } else {
        base.to_path_buf()
    };

    crate::tauri::path_validator::validate_within_base(&target, base)
        .map_err(IpcError::Validation)?;

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
                echo_agent::utils::time::to_local(datetime).to_rfc3339()
            });
            let extension = path.extension().map(|e| e.to_string_lossy().to_string());
            let relative = path
                .strip_prefix(base)
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
    workspace_id: String,
    workspace_generation: String,
    path: String,
) -> Result<FileContent, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("read workspace file", move || {
            let base = control.project_root();
            read_workspace_file(&base, control.workspace_id(), &control.generation(), path)
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

#[tauri::command]
pub async fn write_file(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    path: String,
    content: String,
    expected_revision: String,
) -> Result<FileContent, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("write workspace file", move || {
            let base = control.project_root();
            write_workspace_file(
                &base,
                control.workspace_id(),
                &control.generation(),
                path,
                content,
                expected_revision,
            )
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

fn write_workspace_file(
    base: &std::path::Path,
    workspace_id: &str,
    workspace_generation: &str,
    path: String,
    content: String,
    expected_revision: String,
) -> Result<FileContent, IpcError> {
    let target = base.join(&path);
    crate::tauri::path_validator::validate_within_base(&target, base)
        .map_err(IpcError::Validation)?;
    if !target.is_file() {
        return Err(IpcError::NotFound("File not found".to_string()));
    }
    if content.len() as u64 > MAX_TEXT_BYTES {
        return Err(IpcError::Validation(
            "File too large to edit (>2MB)".to_string(),
        ));
    }

    let replaced =
        echo_agent::utils::fs::atomic_compare_and_swap(&target, content.as_bytes(), |current| {
            file_revision(current) == expected_revision
        })
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    if !replaced {
        return Err(IpcError::Validation(
            "File changed on disk; reload it before saving".to_string(),
        ));
    }

    read_workspace_file(base, workspace_id, workspace_generation, path)
}

#[tauri::command]
pub async fn workspace_changes(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<Vec<WorkspaceChange>, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("load workspace changes", move || {
            let base = control.project_root();
            let output = std::process::Command::new("git")
                .args(["status", "--porcelain=v1", "--untracked-files=all"])
                .current_dir(base)
                .output()
                .map_err(|error| IpcError::Internal(error.to_string()))?;
            if !output.status.success() {
                return Err(IpcError::Internal(format!(
                    "git status failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(parse_workspace_changes(&String::from_utf8_lossy(
                &output.stdout,
            )))
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

fn parse_workspace_changes(text: &str) -> Vec<WorkspaceChange> {
    let mut changes = Vec::new();
    for line in text.lines() {
        let Some(status_code) = line.get(..2) else {
            continue;
        };
        let Some(raw_path) = line.get(3..) else {
            continue;
        };
        let path = raw_path
            .rsplit_once(" -> ")
            .map(|(_, current)| current)
            .unwrap_or(raw_path)
            .trim_matches('"')
            .to_string();
        if path.is_empty() {
            continue;
        }
        let status = if status_code.contains('D') {
            "deleted"
        } else if status_code.contains('?') || status_code.contains('A') {
            "added"
        } else {
            "modified"
        };
        changes.push(WorkspaceChange {
            path,
            status: status.to_string(),
        });
    }
    changes
}

#[tauri::command]
pub async fn diff_file(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    path: String,
    git_ref: Option<String>,
) -> Result<DiffResult, IpcError> {
    let ref_str = git_ref.unwrap_or_else(|| "HEAD".to_string());
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("diff workspace file", move || {
            echo_agent_app_core::diff::WorkspaceDiffService::new(control.project_root())
                .diff_file(&path, &ref_str)
                .map_err(workspace_diff_error)
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

fn workspace_diff_error(error: echo_agent_app_core::diff::WorkspaceDiffError) -> IpcError {
    match error {
        echo_agent_app_core::diff::WorkspaceDiffError::Validation(message) => {
            IpcError::Validation(message)
        }
        echo_agent_app_core::diff::WorkspaceDiffError::Operation(message) => {
            IpcError::Internal(message)
        }
    }
}

#[tauri::command]
pub async fn file_tree(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    depth: Option<usize>,
) -> Result<Vec<TreeNode>, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let max_depth = depth.unwrap_or(3);
    state
        .app_state
        .session
        .product_data_io
        .run("build workspace file tree", move || {
            let base = control.project_root();
            build_tree(&base, &base, 0, max_depth)
        })
        .await
        .map_err(super::product_data::blocking_error)
}

#[tauri::command]
pub async fn browse_directories(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    path: Option<String>,
) -> Result<BrowseResult, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    state
        .app_state
        .session
        .product_data_io
        .run("browse host directories", move || {
            let _control = control;
            browse_host_directories(path)
        })
        .await
        .map_err(super::product_data::blocking_error)?
}

fn browse_host_directories(path: Option<String>) -> Result<BrowseResult, IpcError> {
    let home = dirs_home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));

    let target = if let Some(ref p) = path {
        std::path::PathBuf::from(p)
    } else {
        home.clone()
    };

    if !target.is_dir() {
        return Err(IpcError::Validation(format!(
            "Directory does not exist: {}",
            target.display()
        )));
    }
    let canonical = target
        .canonicalize()
        .map_err(|error| IpcError::Validation(format!("Cannot resolve directory: {error}")))?;

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

fn read_workspace_file(
    base: &std::path::Path,
    workspace_id: &str,
    workspace_generation: &str,
    path: String,
) -> Result<FileContent, IpcError> {
    let target = base.join(&path);
    crate::tauri::path_validator::validate_within_base(&target, base)
        .map_err(IpcError::Validation)?;
    if !target.exists() {
        return Err(IpcError::NotFound("File not found".to_string()));
    }
    if target.is_dir() {
        return Err(IpcError::Validation("Path is a directory".to_string()));
    }

    let metadata =
        std::fs::metadata(&target).map_err(|error| IpcError::Internal(error.to_string()))?;
    if metadata.len() > MAX_PREVIEW_BYTES {
        return Err(IpcError::Validation(
            "File too large to preview (>10MB)".to_string(),
        ));
    }
    let bytes = std::fs::read(&target).map_err(|error| IpcError::Internal(error.to_string()))?;
    let revision = file_revision(&bytes);
    let preview_type = preview_type(&path);

    match preview_type {
        Some((kind, mime_type)) => Ok(FileContent {
            workspace_id: workspace_id.to_string(),
            workspace_generation: workspace_generation.to_string(),
            path,
            content: String::new(),
            size: metadata.len(),
            language: None,
            kind: kind.to_string(),
            mime_type: Some(mime_type.to_string()),
            data_url: Some(format!(
                "data:{mime_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )),
            revision,
        }),
        None if metadata.len() <= MAX_TEXT_BYTES => match String::from_utf8(bytes) {
            Ok(content) => Ok(FileContent {
                workspace_id: workspace_id.to_string(),
                workspace_generation: workspace_generation.to_string(),
                language: detect_language(&path),
                path,
                content,
                size: metadata.len(),
                kind: "text".to_string(),
                mime_type: Some("text/plain".to_string()),
                data_url: None,
                revision,
            }),
            Err(_) => Ok(binary_file_content(
                workspace_id,
                workspace_generation,
                path,
                metadata.len(),
                revision,
            )),
        },
        None => Ok(binary_file_content(
            workspace_id,
            workspace_generation,
            path,
            metadata.len(),
            revision,
        )),
    }
}

fn binary_file_content(
    workspace_id: &str,
    workspace_generation: &str,
    path: String,
    size: u64,
    revision: String,
) -> FileContent {
    FileContent {
        workspace_id: workspace_id.to_string(),
        workspace_generation: workspace_generation.to_string(),
        path,
        content: String::new(),
        size,
        language: None,
        kind: "binary".to_string(),
        mime_type: None,
        data_url: None,
        revision,
    }
}

fn preview_type(path: &str) -> Option<(&'static str, &'static str)> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(("image", "image/png")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        "pdf" => Some(("pdf", "application/pdf")),
        _ => None,
    }
}

fn file_revision(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_preview_types_and_text_languages() {
        assert_eq!(
            preview_type("assets/photo.PNG"),
            Some(("image", "image/png"))
        );
        assert_eq!(
            preview_type("docs/report.pdf"),
            Some(("pdf", "application/pdf"))
        );
        assert_eq!(preview_type("src/main.rs"), None);
        assert_eq!(detect_language("src/main.rs").as_deref(), Some("rust"));
    }

    #[test]
    fn revisions_change_with_unicode_content() {
        let first = file_revision("你好".as_bytes());
        let second = file_revision("你好!".as_bytes());
        assert_eq!(first.chars().count(), 64);
        assert_ne!(first, second);
    }

    #[test]
    fn parses_git_status_with_unicode_and_renames() {
        let changes = parse_workspace_changes(
            " M src/main.rs\n?? 文档/说明.md\nR  old-name.txt -> new-name.txt\n D removed.txt\n",
        );
        assert_eq!(changes.len(), 4);
        assert_eq!(
            changes.first().map(|item| item.status.as_str()),
            Some("modified")
        );
        assert_eq!(
            changes.get(1).map(|item| item.path.as_str()),
            Some("文档/说明.md")
        );
        assert_eq!(
            changes.get(2).map(|item| item.path.as_str()),
            Some("new-name.txt")
        );
        assert_eq!(
            changes.get(3).map(|item| item.status.as_str()),
            Some("deleted")
        );
    }

    #[test]
    fn refuses_to_overwrite_an_external_file_change() -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir().join(format!("eko-file-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base)?;
        let path = "notes.txt".to_string();
        let target = base.join(&path);
        std::fs::write(&target, "first")?;

        let initial = read_workspace_file(&base, "workspace:a", "generation-a", path.clone())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let saved = write_workspace_file(
            &base,
            "workspace:a",
            "generation-a",
            path.clone(),
            "second".to_string(),
            initial.revision,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        assert_eq!(saved.content, "second");

        std::fs::write(&target, "external")?;
        let stale_save = write_workspace_file(
            &base,
            "workspace:a",
            "generation-a",
            path,
            "third".to_string(),
            saved.revision,
        );
        assert!(matches!(stale_save, Err(IpcError::Validation(_))));
        assert_eq!(std::fs::read_to_string(&target)?, "external");

        std::fs::remove_dir_all(base)?;
        Ok(())
    }

    #[test]
    fn file_content_keeps_workspace_identity_separate_from_generation()
    -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir().join(format!("eko-file-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base)?;
        let target = base.join("same.txt");
        std::fs::write(&target, "same bytes")?;

        let file = read_workspace_file(
            &base,
            "workspace:a",
            "2026-08-24T12:00:00+08:00",
            "same.txt".to_string(),
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        assert_eq!(file.workspace_id, "workspace:a");
        assert_eq!(file.workspace_generation, "2026-08-24T12:00:00+08:00");
        std::fs::remove_dir_all(base)?;
        Ok(())
    }
}
