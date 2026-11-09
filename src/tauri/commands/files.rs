//! Tauri IPC commands for file operations.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use base64::Engine;
use echo_agent::tools::filesystem::FileSystem;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

// A thin receipt: original global data stays local; registered projects retain
// the exact host I/O lease across the existing physical blocking operation.
enum GuiFileControl {
    Global(echo_agent_app_core::api::product_data_io::ScopedProductData),
    Project(echo_agent_app_core::api::state::ScopedProjectResources),
}

impl GuiFileControl {
    fn is_non_native(&self) -> bool {
        match self {
            Self::Global(_) => false,
            Self::Project(control) => !control.file_system().paths_are_native(),
        }
    }
    fn project_root(&self) -> std::path::PathBuf {
        match self {
            Self::Global(control) => control.project_root(),
            Self::Project(control) => control.working_dir().to_path_buf(),
        }
    }
    fn file_system(&self) -> Arc<dyn FileSystem> {
        match self {
            Self::Global(_) => Arc::new(echo_agent::tools::files::LocalFileSystem),
            Self::Project(control) => control.file_system(),
        }
    }
    fn workspace_id(&self) -> &str {
        match self {
            Self::Global(control) => control.workspace_id(),
            Self::Project(control) => control.workspace().id.as_str(),
        }
    }
    fn generation(&self) -> String {
        match self {
            Self::Global(control) => control.generation(),
            Self::Project(control) => control.workspace().product_data_generation.clone(),
        }
    }
    fn diff_service(
        &self,
        runtime: tokio::runtime::Handle,
    ) -> echo_agent_app_core::api::diff::WorkspaceDiffService {
        match self {
            Self::Global(_) => {
                echo_agent_app_core::api::diff::WorkspaceDiffService::new(self.project_root())
            }
            Self::Project(control) => {
                echo_agent_app_core::api::diff::WorkspaceDiffService::with_project_resources(
                    control.working_dir(),
                    control.file_system(),
                    control.executor(),
                    runtime,
                    control.resource_guards(),
                )
            }
        }
    }
}

// This is a feedback bound, not a cancellation or remote execution deadline.
// The original ProductDataIo operation keeps the captured host/control receipt
// through physical completion even after this IPC returns Unconfirmed.
const REMOTE_FILE_OBSERVATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

async fn run_project_file_io<T, F>(
    service: &echo_agent_app_core::api::product_data_io::ProductDataIoService,
    operation: &'static str,
    control: GuiFileControl,
    function: F,
) -> Result<T, IpcError>
where
    T: Send + 'static,
    F: FnOnce(GuiFileControl) -> Result<T, IpcError> + Send + 'static,
{
    if control.is_non_native() {
        match service
            .run_observed(operation, REMOTE_FILE_OBSERVATION_WAIT, move || {
                function(control)
            })
            .await
            .map_err(super::product_data::blocking_error)?
        {
            echo_agent_app_core::api::product_data_io::ProductDataIoObservation::Completed(
                result,
            ) => result,
            echo_agent_app_core::api::product_data_io::ProductDataIoObservation::Unconfirmed {
                operation,
                operation_id,
            } => {
                tracing::warn!(
                    operation,
                    operation_id,
                    "remote file observation elapsed; original owner retained"
                );
                Err(IpcError::Internal("远端文件操作等待已超过30秒，结果尚未确认。操作实际完成后才会释放工作区占用，请先核对结果，避免重复保存。".into()))
            }
        }
    } else {
        service
            .run(operation, move || function(control))
            .await
            .map_err(super::product_data::blocking_error)?
    }
}

async fn project_file_control(
    state: &TauriState,
    workspace_id: &str,
    generation: &str,
) -> Result<GuiFileControl, IpcError> {
    if workspace_id == "global" {
        return super::product_data::scoped_control(state, workspace_id, generation)
            .await
            .map(GuiFileControl::Global);
    }
    state
        .app_state
        .project_resources_for_workspace(workspace_id, generation)
        .await
        .map(GuiFileControl::Project)
        .map_err(|error| IpcError::Validation(error.to_string()))
}

fn project_path(
    files: &dyn FileSystem,
    base: &Path,
    path: &str,
) -> Result<std::path::PathBuf, IpcError> {
    echo_agent_app_core::api::diff::resolve_project_file_path(files, base, path)
        .map_err(workspace_diff_error)
}

fn file_io_error(error: echo_agent::error::ReactError) -> IpcError {
    match error {
        echo_agent::error::ReactError::Io(error)
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            IpcError::NotFound(error.to_string())
        }
        error => IpcError::Internal(error.to_string()),
    }
}

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

pub type DiffResult = echo_agent_app_core::api::diff::WorkspaceFileDiff;

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
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "list workspace files",
        control,
        move |control| {
            let base = control.project_root();
            list_workspace_files(control.file_system().as_ref(), &base, path.as_deref())
        },
    )
    .await
}

fn list_workspace_files(
    files: &dyn FileSystem,
    base: &Path,
    path: Option<&str>,
) -> Result<Vec<FileEntry>, IpcError> {
    let target = project_path(files, base, path.unwrap_or(""))?;
    if !files.is_dir(&target).map_err(file_io_error)? {
        return Err(IpcError::NotFound("Directory not found".into()));
    }
    let canonical_base = files.canonicalize(base).map_err(file_io_error)?;
    let mut entries = Vec::new();
    for entry in files.read_dir(&target).map_err(file_io_error)? {
        let name = entry.file_name.to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let relative = entry
            .path
            .strip_prefix(&canonical_base)
            .map_err(|_| IpcError::Validation("directory entry escapes project root".into()))?
            .to_string_lossy()
            .to_string();
        // Enumerate the entry itself. Following every link here would make
        // one external or dangling link hide all ordinary sibling entries.
        // Links stay visible as leaves; an explicit open and real-directory
        // recursion still use project_path before consuming target contents.
        let metadata = files.symlink_metadata(&entry.path).map_err(file_io_error)?;
        entries.push(FileEntry {
            name,
            path: relative,
            is_dir: entry.file_type.is_dir() && metadata.is_dir(),
            size: metadata.len,
            // The portable provider does not promise target modification times.
            modified: None,
            extension: entry
                .path
                .extension()
                .map(|value| value.to_string_lossy().to_string()),
        });
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
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "read workspace file",
        control,
        move |control| {
            let base = control.project_root();
            read_workspace_file(
                control.file_system().as_ref(),
                &base,
                control.workspace_id(),
                &control.generation(),
                path,
            )
        },
    )
    .await
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
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "write workspace file",
        control,
        move |control| {
            let base = control.project_root();
            write_workspace_file(
                control.file_system().as_ref(),
                &base,
                control.workspace_id(),
                &control.generation(),
                path,
                content,
                expected_revision,
            )
        },
    )
    .await
}

fn write_workspace_file(
    files: &dyn FileSystem,
    base: &std::path::Path,
    workspace_id: &str,
    workspace_generation: &str,
    path: String,
    content: String,
    expected_revision: String,
) -> Result<FileContent, IpcError> {
    let target = project_path(files, base, &path)?;
    if !files.is_file(&target).map_err(file_io_error)? {
        return Err(IpcError::NotFound("File not found".to_string()));
    }
    if content.len() as u64 > MAX_TEXT_BYTES {
        return Err(IpcError::Validation(
            "File too large to edit (>2MB)".to_string(),
        ));
    }

    let current = files.read(&target).map_err(file_io_error)?;
    if file_revision(&current) != expected_revision {
        return Err(IpcError::Validation(
            "File changed on disk; reload it before saving".into(),
        ));
    }
    // Compare the same bytes at the provider's atomic replace boundary, not
    // merely before an unrelated write. Unsupported targets fail explicitly.
    let replaced = files
        .atomic_compare_and_swap(&target, &current, content.as_bytes())
        .map_err(file_io_error)?;
    if !replaced {
        return Err(IpcError::Validation(
            "File changed on disk; reload it before saving".to_string(),
        ));
    }

    read_workspace_file(files, base, workspace_id, workspace_generation, path)
}

#[tauri::command]
pub async fn workspace_changes(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<Vec<WorkspaceChange>, IpcError> {
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    let runtime = tokio::runtime::Handle::current();
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "load workspace changes",
        control,
        move |control| {
            let text = control
                .diff_service(runtime)
                .changes_porcelain()
                .map_err(workspace_diff_error)?;
            Ok(parse_workspace_changes(&text))
        },
    )
    .await
}

fn parse_workspace_changes(text: &str) -> Vec<WorkspaceChange> {
    let mut changes = Vec::new();
    let mut entries = text.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let status_code: String = entry.chars().take(2).collect();
        let path: String = entry.chars().skip(3).collect();
        if status_code.contains('R') || status_code.contains('C') {
            let _old_path = entries.next();
        }
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
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    let runtime = tokio::runtime::Handle::current();
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "diff workspace file",
        control,
        move |control| {
            control
                .diff_service(runtime)
                .diff_file(&path, &ref_str)
                .map_err(workspace_diff_error)
        },
    )
    .await
}

fn workspace_diff_error(error: echo_agent_app_core::api::diff::WorkspaceDiffError) -> IpcError {
    match error {
        echo_agent_app_core::api::diff::WorkspaceDiffError::Validation(message) => {
            IpcError::Validation(message)
        }
        echo_agent_app_core::api::diff::WorkspaceDiffError::Operation(message) => {
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
    let control = project_file_control(&state, &workspace_id, &workspace_generation).await?;
    let max_depth = depth.unwrap_or(3);
    run_project_file_io(
        &state.app_state.session.product_data_io,
        "build workspace file tree",
        control,
        move |control| {
            let base = control.project_root();
            build_tree(control.file_system().as_ref(), &base, &base, 0, max_depth)
        },
    )
    .await
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
    files: &dyn FileSystem,
    base: &std::path::Path,
    workspace_id: &str,
    workspace_generation: &str,
    path: String,
) -> Result<FileContent, IpcError> {
    let target = project_path(files, base, &path)?;
    let metadata = files.metadata(&target).map_err(file_io_error)?;
    if metadata.is_dir() {
        return Err(IpcError::Validation("Path is a directory".into()));
    }
    if metadata.len() > MAX_PREVIEW_BYTES {
        return Err(IpcError::Validation(
            "File too large to preview (>10MB)".to_string(),
        ));
    }
    let bytes = files.read(&target).map_err(file_io_error)?;
    if bytes.len() as u64 > MAX_PREVIEW_BYTES {
        return Err(IpcError::Validation(
            "File too large to preview (>10MB)".into(),
        ));
    }
    let size = bytes.len() as u64;
    let revision = file_revision(&bytes);
    let preview_type = preview_type(&path);

    match preview_type {
        Some((kind, mime_type)) => Ok(FileContent {
            workspace_id: workspace_id.to_string(),
            workspace_generation: workspace_generation.to_string(),
            path,
            content: String::new(),
            size,
            language: None,
            kind: kind.to_string(),
            mime_type: Some(mime_type.to_string()),
            data_url: Some(format!(
                "data:{mime_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )),
            revision,
        }),
        None if size <= MAX_TEXT_BYTES => match String::from_utf8(bytes) {
            Ok(content) => Ok(FileContent {
                workspace_id: workspace_id.to_string(),
                workspace_generation: workspace_generation.to_string(),
                language: detect_language(&path),
                path,
                content,
                size,
                kind: "text".to_string(),
                mime_type: Some("text/plain".to_string()),
                data_url: None,
                revision,
            }),
            Err(_) => Ok(binary_file_content(
                workspace_id,
                workspace_generation,
                path,
                size,
                revision,
            )),
        },
        None => Ok(binary_file_content(
            workspace_id,
            workspace_generation,
            path,
            size,
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
    files: &dyn FileSystem,
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
) -> Result<Vec<TreeNode>, IpcError> {
    if depth >= max_depth {
        return Ok(Vec::new());
    }
    let relative = current
        .strip_prefix(root)
        .map_err(|_| IpcError::Validation("directory escapes project root".into()))?
        .to_string_lossy();
    let entries = list_workspace_files(files, root, Some(&relative))?;
    entries
        .into_iter()
        .map(|entry| {
            let children = if entry.is_dir {
                Some(build_tree(
                    files,
                    root,
                    &root.join(&entry.path),
                    depth.saturating_add(1),
                    max_depth,
                )?)
            } else {
                None
            };
            Ok(TreeNode {
                name: entry.name,
                path: entry.path,
                is_dir: entry.is_dir,
                children,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
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
            " M src/main.rs\0?? 文档/说明.md\0R  new-name.txt\0old-name.txt\0 D removed.txt\0",
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

        let initial = read_workspace_file(
            &echo_agent::tools::files::LocalFileSystem,
            &base,
            "workspace:a",
            "generation-a",
            path.clone(),
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        let saved = write_workspace_file(
            &echo_agent::tools::files::LocalFileSystem,
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
            &echo_agent::tools::files::LocalFileSystem,
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
            &echo_agent::tools::files::LocalFileSystem,
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

    #[test]
    fn git_changes_keep_raw_status_and_unquoted_target_names() {
        let changes =
            parse_workspace_changes(" M 文件\n空格 \".txt\0R  new -> name.txt\0old name.txt\0");
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes.first().map(|change| change.path.as_str()),
            Some("文件\n空格 \".txt")
        );
        assert_eq!(
            changes.first().map(|change| change.status.as_str()),
            Some("modified")
        );
        assert_eq!(
            changes.get(1).map(|change| change.path.as_str()),
            Some("new -> name.txt")
        );
    }

    /// Two actual filesystem roots exposed at the same project path. The GUI
    /// helpers must use the captured provider, even though the host path exists.
    pub(crate) struct NamespacedFiles {
        visible: std::path::PathBuf,
        actual: std::path::PathBuf,
        offline: std::sync::atomic::AtomicBool,
        race_on_save: std::sync::atomic::AtomicBool,
        metadata_disconnected: std::sync::atomic::AtomicBool,
    }

    impl NamespacedFiles {
        fn target(&self, path: &Path) -> echo_agent::error::Result<std::path::PathBuf> {
            if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "project file channel disconnected",
                )
                .into());
            }
            let relative = path.strip_prefix(&self.visible).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "not in this target namespace")
            })?;
            Ok(self.actual.join(relative))
        }
        fn visible(&self, path: &Path) -> echo_agent::error::Result<std::path::PathBuf> {
            let relative = path
                .strip_prefix(&self.actual)
                .map_err(|_| std::io::Error::other("provider returned an unrelated target path"))?;
            Ok(self.visible.join(relative))
        }
    }

    impl FileSystem for NamespacedFiles {
        fn current_dir(&self) -> echo_agent::error::Result<std::path::PathBuf> {
            Ok(self.visible.clone())
        }
        fn canonicalize(&self, path: &Path) -> echo_agent::error::Result<std::path::PathBuf> {
            self.visible(
                &echo_agent::tools::files::LocalFileSystem.canonicalize(&self.target(path)?)?,
            )
        }
        fn metadata(
            &self,
            path: &Path,
        ) -> echo_agent::error::Result<echo_agent::tools::filesystem::FileMetadata> {
            echo_agent::tools::files::LocalFileSystem.metadata(&self.target(path)?)
        }
        fn symlink_metadata(
            &self,
            path: &Path,
        ) -> echo_agent::error::Result<echo_agent::tools::filesystem::FileMetadata> {
            if self
                .metadata_disconnected
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "project metadata channel disconnected",
                )
                .into());
            }
            echo_agent::tools::files::LocalFileSystem.symlink_metadata(&self.target(path)?)
        }
        fn read(&self, path: &Path) -> echo_agent::error::Result<Vec<u8>> {
            echo_agent::tools::files::LocalFileSystem.read(&self.target(path)?)
        }
        fn read_dir(
            &self,
            path: &Path,
        ) -> echo_agent::error::Result<Vec<echo_agent::tools::filesystem::FileEntry>> {
            echo_agent::tools::files::LocalFileSystem
                .read_dir(&self.target(path)?)?
                .into_iter()
                .map(|mut entry| {
                    entry.path = self.visible(&entry.path)?;
                    Ok(entry)
                })
                .collect()
        }
        fn create_dir_all(&self, path: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.create_dir_all(&self.target(path)?)
        }
        fn create_file(&self, path: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.create_file(&self.target(path)?)
        }
        fn write(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.write(&self.target(path)?, bytes)
        }
        fn append(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.append(&self.target(path)?, bytes)
        }
        fn rename(&self, from: &Path, to: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.rename(&self.target(from)?, &self.target(to)?)
        }
        fn remove_file(&self, path: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.remove_file(&self.target(path)?)
        }
        fn remove_dir(&self, path: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.remove_dir(&self.target(path)?)
        }
        fn remove_dir_all(&self, path: &Path) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.remove_dir_all(&self.target(path)?)
        }
        fn set_mode(&self, path: &Path, mode: u32) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.set_mode(&self.target(path)?, mode)
        }
        fn atomic_write(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            echo_agent::tools::files::LocalFileSystem.atomic_write(&self.target(path)?, bytes)
        }
        fn atomic_compare_and_swap(
            &self,
            path: &Path,
            expected: &[u8],
            bytes: &[u8],
        ) -> echo_agent::error::Result<bool> {
            let target = self.target(path)?;
            if self
                .race_on_save
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                std::fs::write(&target, "external target edit")?;
            }
            echo_agent::tools::files::LocalFileSystem
                .atomic_compare_and_swap(&target, expected, bytes)
        }
        fn checkpoint(&self, _path: &Path) -> echo_agent::error::Result<Option<String>> {
            Ok(None)
        }
    }

    pub(crate) fn namespaced_files(visible: &Path, actual: &Path) -> NamespacedFiles {
        NamespacedFiles {
            visible: visible.to_path_buf(),
            actual: actual.to_path_buf(),
            offline: false.into(),
            race_on_save: false.into(),
            metadata_disconnected: false.into(),
        }
    }

    #[test]
    fn project_tree_read_and_save_use_same_name_target_and_keep_host_untouched()
    -> Result<(), Box<dyn std::error::Error>> {
        let host = tempfile::tempdir()?;
        let remote = tempfile::tempdir()?;
        let base = host.path().canonicalize()?;
        let actual = remote.path().canonicalize()?;
        std::fs::write(base.join("同名🙂.txt"), "host bytes")?;
        std::fs::write(actual.join("同名🙂.txt"), "target bytes")?;
        std::fs::write(actual.join("target-only.txt"), "only target")?;
        let files = namespaced_files(&base, &actual);
        let tree = build_tree(&files, &base, &base, 0, 3)?;
        assert!(tree.iter().any(|entry| entry.path == "target-only.txt"));
        let read = read_workspace_file(
            &files,
            &base,
            "workspace:a",
            "target-v1",
            "同名🙂.txt".into(),
        )?;
        assert_eq!(read.content, "target bytes");
        let saved = write_workspace_file(
            &files,
            &base,
            "workspace:a",
            "target-v1",
            "同名🙂.txt".into(),
            "saved target".into(),
            read.revision,
        )?;
        assert_eq!(saved.content, "saved target");
        assert_eq!(saved.workspace_generation, "target-v1");
        assert_eq!(
            std::fs::read_to_string(base.join("同名🙂.txt"))?,
            "host bytes"
        );
        assert_eq!(
            std::fs::read_to_string(actual.join("同名🙂.txt"))?,
            "saved target"
        );
        files
            .offline
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            read_workspace_file(
                &files,
                &base,
                "workspace:a",
                "target-v1",
                "同名🙂.txt".into()
            )
            .is_err()
        );
        assert!(build_tree(&files, &base, &base, 0, 3).is_err());
        assert_eq!(
            std::fs::read_to_string(base.join("同名🙂.txt"))?,
            "host bytes"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn project_enumeration_keeps_siblings_with_external_and_dangling_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let host = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let base = host.path().canonicalize()?;
        let actual = project.path().canonicalize()?;
        std::fs::write(base.join("普通文件🙂.txt"), "host untouched")?;
        std::fs::write(actual.join("普通文件🙂.txt"), "project bytes")?;
        std::fs::create_dir(actual.join("nested"))?;
        std::fs::write(actual.join("nested/child.txt"), "nested project bytes")?;
        std::fs::write(
            outside.path().join("outside-only.txt"),
            "must not enter tree",
        )?;
        std::os::unix::fs::symlink(outside.path(), actual.join("external"))?;
        std::os::unix::fs::symlink(actual.join("missing-target"), actual.join("dangling"))?;
        std::os::unix::fs::symlink(
            actual.join("普通文件🙂.txt"),
            actual.join("internal-file-link"),
        )?;
        let files = namespaced_files(&base, &actual);

        let entries = list_workspace_files(&files, &base, None)?;
        assert!(entries.iter().any(|entry| entry.path == "普通文件🙂.txt"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == "nested" && entry.is_dir)
        );
        for link in ["external", "dangling", "internal-file-link"] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.path == link && !entry.is_dir)
            );
        }
        let tree = build_tree(&files, &base, &base, 0, 3)?;
        assert!(tree.iter().any(|entry| entry.path == "nested"
            && entry.children.as_ref().is_some_and(|children| {
                children
                    .iter()
                    .any(|child| child.path == "nested/child.txt")
            })));
        assert!(
            tree.iter()
                .filter(|entry| entry.path == "external" || entry.path == "dangling")
                .all(|entry| entry.children.is_none())
        );
        assert!(
            read_workspace_file(
                &files,
                &base,
                "workspace:a",
                "v1",
                "external/outside-only.txt".into()
            )
            .is_err()
        );
        assert!(
            read_workspace_file(&files, &base, "workspace:a", "v1", "dangling".into()).is_err()
        );
        let linked_file = read_workspace_file(
            &files,
            &base,
            "workspace:a",
            "v1",
            "internal-file-link".into(),
        )?;
        assert_eq!(linked_file.content, "project bytes");
        assert_eq!(
            std::fs::read_to_string(base.join("普通文件🙂.txt"))?,
            "host untouched"
        );

        // A real provider failure after read_dir must still fail the operation;
        // only the decision not to follow directory entries changed.
        files
            .metadata_disconnected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let disconnected = list_workspace_files(&files, &base, None);
        assert!(
            matches!(disconnected, Err(IpcError::Internal(message)) if message.contains("metadata channel disconnected"))
        );
        assert!(build_tree(&files, &base, &base, 0, 3).is_err());
        Ok(())
    }

    #[test]
    fn project_save_rechecks_preimage_at_target_atomic_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let host = tempfile::tempdir()?;
        let remote = tempfile::tempdir()?;
        let base = host.path().canonicalize()?;
        let actual = remote.path().canonicalize()?;
        std::fs::write(base.join("same.txt"), "host unchanged")?;
        std::fs::write(actual.join("same.txt"), "target before")?;
        let files = namespaced_files(&base, &actual);
        let read = read_workspace_file(&files, &base, "workspace:a", "v1", "same.txt".into())?;
        files
            .race_on_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let saved = write_workspace_file(
            &files,
            &base,
            "workspace:a",
            "v1",
            "same.txt".into(),
            "do not overwrite".into(),
            read.revision,
        );
        assert!(matches!(saved, Err(IpcError::Validation(_))));
        assert_eq!(
            std::fs::read_to_string(actual.join("same.txt"))?,
            "external target edit"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("same.txt"))?,
            "host unchanged"
        );
        Ok(())
    }

    pub(crate) struct ProjectGitExecutor {
        pub(crate) files: Arc<NamespacedFiles>,
    }

    impl echo_agent::sandbox::SandboxExecutor for ProjectGitExecutor {
        fn name(&self) -> &str {
            "test-project-target"
        }
        fn isolation_level(&self) -> echo_agent::sandbox::IsolationLevel {
            echo_agent::sandbox::IsolationLevel::None
        }
        fn is_available(&self) -> futures::future::BoxFuture<'_, bool> {
            Box::pin(async { true })
        }
        fn execute(
            &self,
            command: echo_agent::sandbox::SandboxCommand,
        ) -> futures::future::BoxFuture<
            '_,
            echo_agent::error::Result<echo_agent::sandbox::ExecutionResult>,
        > {
            Box::pin(async move {
                let cwd = command
                    .working_dir
                    .as_deref()
                    .ok_or_else(|| std::io::Error::other("missing target cwd"))?;
                let cwd = self.files.target(cwd)?;
                let echo_agent::sandbox::CommandKind::Program { program, args } = command.kind
                else {
                    return Err(std::io::Error::other("expected program invocation").into());
                };
                if program != "git" {
                    return Err(std::io::Error::other("expected actual Git").into());
                }
                let guards = command.resource_guards;
                let project_paths = args.iter().any(|arg| arg == "--show-toplevel")
                    || args.first().is_some_and(|arg| arg == "worktree");
                let actual_root = self.files.actual.to_string_lossy().to_string();
                let visible_root = self.files.visible.to_string_lossy().to_string();
                tokio::task::spawn_blocking(move || {
                    let _guards = guards;
                    let output = std::process::Command::new(program)
                        .args(args)
                        .current_dir(cwd)
                        .output()?;
                    Ok(echo_agent::sandbox::ExecutionResult {
                        exit_code: output.status.code().unwrap_or(1),
                        stdout_bytes: output.stdout.len() as u64,
                        stderr_bytes: output.stderr.len() as u64,
                        stdout: {
                            let stdout =
                                String::from_utf8(output.stdout).map_err(std::io::Error::other)?;
                            if project_paths {
                                stdout.replace(&actual_root, &visible_root)
                            } else {
                                stdout
                            }
                        },
                        stderr: String::from_utf8(output.stderr).map_err(std::io::Error::other)?,
                        duration: std::time::Duration::ZERO,
                        sandbox_type: "test-project-target".into(),
                        timed_out: false,
                        cancelled: false,
                        output_truncated: false,
                    })
                })
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn project_diff_reads_target_working_bytes_and_real_target_git_objects()
    -> Result<(), Box<dyn std::error::Error>> {
        let host = tempfile::tempdir()?;
        let remote = tempfile::tempdir()?;
        let base = host.path().canonicalize()?;
        let actual = remote.path().canonicalize()?;
        std::fs::write(base.join("same.txt"), "host unchanged\n")?;
        let git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&actual)
                .output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(
                    String::from_utf8_lossy(&output.stderr).to_string(),
                )
                .into());
            }
            Ok(())
        };
        git(&["init", "-q"])?;
        std::fs::write(actual.join("same.txt"), "target committed\n")?;
        git(&["add", "--", "same.txt"])?;
        git(&[
            "-c",
            "user.name=EKO Test",
            "-c",
            "user.email=eko@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "target commit",
        ])?;
        std::fs::write(actual.join("same.txt"), "target working\n")?;
        let files = Arc::new(namespaced_files(&base, &actual));
        let service = echo_agent_app_core::api::diff::WorkspaceDiffService::with_project_resources(
            &base,
            files.clone(),
            Arc::new(ProjectGitExecutor {
                files: files.clone(),
            }),
            tokio::runtime::Handle::current(),
            Vec::new(),
        );
        let diff = tokio::task::spawn_blocking(move || {
            let changes = parse_workspace_changes(&service.changes_porcelain()?);
            assert!(
                changes
                    .iter()
                    .any(|change| change.path == "same.txt" && change.status == "modified")
            );
            service.diff_file("same.txt", "HEAD")
        })
        .await??;
        assert_eq!(diff.old_content, "target committed\n");
        assert_eq!(diff.new_content, "target working\n");
        assert_eq!(
            std::fs::read_to_string(base.join("same.txt"))?,
            "host unchanged\n"
        );
        files
            .offline
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let service = echo_agent_app_core::api::diff::WorkspaceDiffService::with_project_resources(
            &base,
            files.clone(),
            Arc::new(ProjectGitExecutor { files }),
            tokio::runtime::Handle::current(),
            Vec::new(),
        );
        assert!(
            tokio::task::spawn_blocking(move || service.diff_file("same.txt", "HEAD"))
                .await?
                .is_err()
        );
        Ok(())
    }
}
