//! 工作区注册表
//!
//! 管理工作区的创建、发现、切换和删除。
//! 支持两种工作区位置：
//! - 默认：`~/.eko/workspaces/{id}/`
//! - 自定义：用户指定的任意目录
//!
//! 所有工作区通过 `registry.json` 索引文件统一追踪。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::layout::WorkspaceLayout;
use super::{
    ProjectLocation, ProjectSource, Workspace, WorkspaceId, WorkspaceKind, WorkspaceMetadata,
};

// ── Registry Index ──────────────────────────────────────────────────

/// 工作区索引 — 存储在 `base_dir/registry.json`。
///
/// 记录所有工作区 ID 到其根目录的映射，使得工作区可以存放在任意位置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryIndex {
    /// workspace id → root path
    entries: HashMap<String, String>,
}

impl RegistryIndex {
    fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(data) => Ok(serde_json::from_str(&data)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        echo_agent::utils::fs::atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    fn insert(&mut self, id: &str, root: &Path) {
        self.entries
            .insert(id.to_string(), root.to_string_lossy().to_string());
    }

    fn remove(&mut self, id: &str) {
        self.entries.remove(id);
    }

    fn get_root(&self, id: &str) -> Option<PathBuf> {
        self.entries.get(id).map(PathBuf::from)
    }
}

// ── WorkspaceRegistry ───────────────────────────────────────────────

/// 工作区注册表。
///
/// 管理所有工作区的生命周期，支持默认路径和自定义路径。
pub struct WorkspaceRegistry {
    /// 工作区基础目录，通常为 `~/.eko/workspaces/`。
    base_dir: PathBuf,
}

impl WorkspaceRegistry {
    /// 创建注册表，使用默认基础目录。
    pub fn new() -> anyhow::Result<Self> {
        let base_dir = WorkspaceLayout::base_dir();
        fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    /// 创建注册表，使用指定基础目录（测试用）。
    pub fn with_base_dir(base_dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    /// Construct a registry after directory creation has already failed.
    ///
    /// Operations retain their normal `Result`-based I/O errors instead of
    /// making application startup panic.
    pub(crate) fn without_initialization(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// 基础目录路径。
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// 索引文件路径。
    fn index_path(&self) -> PathBuf {
        self.base_dir.join("registry.json")
    }

    /// 加载索引。
    fn load_index(&self) -> anyhow::Result<RegistryIndex> {
        RegistryIndex::load(&self.index_path())
    }

    /// 保存索引。
    fn save_index(&self, index: &RegistryIndex) -> anyhow::Result<()> {
        index.save(&self.index_path())
    }

    /// 工作区的默认根目录（当用户不指定路径时）。
    pub fn default_root(&self, name: &str) -> PathBuf {
        let id = WorkspaceId::from_name(name);
        self.base_dir.join(id.as_path_segment())
    }

    // ── CRUD ──

    /// 创建新工作区（使用默认路径）。
    pub fn create(&self, name: &str, kind: WorkspaceKind) -> anyhow::Result<Workspace> {
        let root = self.default_root(name);
        self.create_at(name, kind, root)
    }

    /// 创建新工作区，使用指定的根目录。
    ///
    /// 在指定目录下初始化标准工作区布局，写入清单文件，并注册到索引。
    pub fn create_at(
        &self,
        name: &str,
        kind: WorkspaceKind,
        root: PathBuf,
    ) -> anyhow::Result<Workspace> {
        let location = ProjectLocation::Local { path: root.clone() };
        self.create_at_with_location(name, kind, root, location, None)
    }

    /// Publish a project using the same manifest/index authority, keeping EKO
    /// data separate from user code. SSH locations are verified by the owned
    /// application transition before reaching this publication boundary.
    pub(crate) fn create_project(
        &self,
        name: &str,
        kind: WorkspaceKind,
        location: ProjectLocation,
        execution_location: Option<ProjectLocation>,
    ) -> anyhow::Result<Workspace> {
        self.create_at_with_location(
            name,
            kind,
            self.default_root(name),
            location,
            execution_location,
        )
    }

    pub(crate) fn create_local_project(
        &self,
        name: &str,
        kind: WorkspaceKind,
        source: ProjectSource,
    ) -> anyhow::Result<Workspace> {
        // Check collision before creating a user directory. A failed request
        // must not create a second directory for an already registered name.
        let id = WorkspaceId::from_name(name);
        if self.load_index()?.get_root(id.as_str()).is_some() {
            anyhow::bail!("Workspace '{}' already exists", id);
        }
        let (path, newly_created) = match source {
            ProjectSource::ExistingLocal { path } => (path.canonicalize()?, false),
            ProjectSource::NewLocal {
                parent,
                directory_name,
            } => {
                let parent = parent.canonicalize()?;
                if !parent.is_dir() {
                    anyhow::bail!("Project parent is not a directory");
                }
                // Exactly one directory, never a path traversal or recursive
                // creation that could make rollback ownership ambiguous.
                let mut components = Path::new(&directory_name).components();
                if directory_name.trim().is_empty()
                    || !matches!(components.next(), Some(std::path::Component::Normal(_)))
                    || components.next().is_some()
                {
                    anyhow::bail!("请输入一个文件夹名称，不要包含路径分隔符或 ..");
                }
                let path = parent.join(directory_name);
                fs::create_dir(&path)?;
                (path, true)
            }
            ProjectSource::Ssh { .. } => {
                anyhow::bail!("SSH projects require a verified connection")
            }
        };
        if !path.is_dir() {
            anyhow::bail!("Project root is not a directory: {}", path.display());
        }
        let result = self.create_project(
            name,
            kind,
            ProjectLocation::Local { path: path.clone() },
            None,
        );
        if result.is_err() && newly_created {
            // remove_dir only succeeds while empty. Never recursively remove
            // project content written by a user or another process meanwhile.
            if let Err(error) = fs::remove_dir(&path) {
                tracing::warn!(%error, path = %path.display(), "new project directory retained after failed creation");
            }
        }
        result
    }

    fn create_at_with_location(
        &self,
        name: &str,
        kind: WorkspaceKind,
        root: PathBuf,
        location: ProjectLocation,
        execution_location: Option<ProjectLocation>,
    ) -> anyhow::Result<Workspace> {
        let id = WorkspaceId::from_name(name);

        // 检查索引中是否已有同名工作区
        let index = self.load_index()?;
        if index.get_root(id.as_str()).is_some() {
            anyhow::bail!("Workspace '{}' already exists", id);
        }

        // 如果目录已存在且有清单文件，拒绝覆盖
        let manifest = WorkspaceLayout::manifest(&root);
        // The retired root marker is checked only to prevent overwriting an
        // existing user directory. It is never read as a workspace authority.
        let retired_manifest = root.join(".workspace.json");
        if manifest.exists() || retired_manifest.exists() {
            anyhow::bail!("Directory already contains a workspace: {}", root.display());
        }

        // 创建目录结构
        fs::create_dir_all(&root)?;
        let state_dir = WorkspaceLayout::state_dir(&root);
        let created_state_dir = !state_dir.exists();
        WorkspaceLayout::ensure_dirs(&root)?;

        let now = Utc::now();
        let mut workspace = Workspace {
            id: id.clone(),
            name: name.to_string(),
            root: root.clone(),
            project_root: match &location {
                ProjectLocation::Local { path } => Some(path.clone()),
                _ => None,
            },
            kind,
            metadata: WorkspaceMetadata::new(location),
            product_data_generation: String::new(),
            created_at: now,
            last_active: now,
        };
        workspace.metadata.project_execution_location = execution_location;
        workspace.refresh_product_data_generation();

        // 写入清单文件
        let publish = (|| {
            self.save_manifest(&workspace)?;
            let mut index = self.load_index()?;
            index.insert(id.as_str(), &root);
            self.save_index(&index)
        })();
        if let Err(error) = publish {
            // A pre-existing .eko may contain user-owned Skills/rules even
            // without a Workspace manifest. Never adopt its contents as ours.
            let cleanup = if created_state_dir {
                fs::remove_dir_all(&state_dir)
            } else {
                match fs::remove_file(&manifest) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    result => result,
                }
            };
            if let Err(cleanup_error) = cleanup {
                tracing::warn!(%cleanup_error, "workspace publication rollback retained application data");
            }
            return Err(error);
        }

        tracing::info!(
            workspace = %id,
            root = %root.display(),
            "Created workspace"
        );
        Ok(workspace)
    }

    /// 打开已有工作区。
    ///
    /// 先从索引查找路径，找不到时回退到默认路径。
    pub fn open(&self, id: &WorkspaceId) -> anyhow::Result<Workspace> {
        let mut workspace = self.inspect(id)?;

        // Opening is a user-visible lifecycle action, so it advances activity.
        workspace.touch();
        self.save_manifest(&workspace)?;

        Ok(workspace)
    }

    /// 身份是否已由索引或默认目录占用。损坏/不可读不是“未登记”，
    /// 用于保护现有连接owner，不发布Workspace或建立第二份注册表。
    pub fn is_identity_reserved(&self, id: &WorkspaceId) -> anyhow::Result<bool> {
        let index = self.load_index()?;
        if index.entries.contains_key(id.as_str()) {
            return Ok(true);
        }
        Ok(self.base_dir.join(id.as_path_segment()).try_exists()?)
    }

    /// Read one workspace without changing activity or writing its manifest.
    ///
    /// Product-data and runtime scope resolution use this path. A read may run
    /// in an owned blocking task after its caller is dropped, so it must remain
    /// free of side effects and safe to overlap a later lifecycle transition.
    pub fn inspect(&self, id: &WorkspaceId) -> anyhow::Result<Workspace> {
        let index = self.load_index()?;

        // 优先从索引获取路径
        let root = index
            .get_root(id.as_str())
            .unwrap_or_else(|| self.base_dir.join(id.as_path_segment()));

        if !root.exists() {
            anyhow::bail!("Workspace '{}' not found at {:?}", id, root);
        }

        let manifest_path = WorkspaceLayout::manifest(&root);
        let data = fs::read_to_string(&manifest_path)?;
        let mut workspace: Workspace = serde_json::from_str(&data)?;
        workspace.refresh_product_data_generation();

        Ok(workspace)
    }

    /// 按名称查找并打开工作区。
    pub fn open_by_name(&self, name: &str) -> anyhow::Result<Workspace> {
        let id = WorkspaceId::from_name(name);
        self.open(&id)
    }

    /// 列出所有工作区，按最后活跃时间降序排列。
    ///
    /// 同时扫描索引文件和基础目录，索引可由当前 manifest 重建。
    pub fn list(&self) -> anyhow::Result<Vec<Workspace>> {
        let mut workspaces = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        // 1. 从索引加载
        let index = self.load_index()?;
        for (id_str, root_str) in &index.entries {
            let root = PathBuf::from(root_str);
            let manifest = WorkspaceLayout::manifest(&root);
            if manifest.exists()
                && let Ok(data) = fs::read_to_string(&manifest)
                && let Ok(mut ws) = serde_json::from_str::<Workspace>(&data)
            {
                ws.refresh_product_data_generation();
                seen_ids.insert(id_str.clone());
                workspaces.push(ws);
                continue;
            }
            // 索引条目无效（目录被手动删除了），忽略
            tracing::warn!(
                id = %id_str,
                root = %root_str,
                "Indexed workspace root not found, skipping"
            );
        }

        // 2. 扫描基础目录以重建缺失的索引投影。
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let manifest = WorkspaceLayout::manifest(&path);
                if !manifest.exists() {
                    continue;
                }
                if let Ok(data) = fs::read_to_string(&manifest)
                    && let Ok(mut ws) = serde_json::from_str::<Workspace>(&data)
                    && !seen_ids.contains(ws.id.as_str())
                {
                    ws.refresh_product_data_generation();
                    workspaces.push(ws);
                }
            }
        }

        // 按最后活跃时间降序
        workspaces.sort_by_key(|w| std::cmp::Reverse(w.last_active));
        Ok(workspaces)
    }

    /// 删除工作区。
    ///
    /// 仅删除本产品状态与索引；产品数据与用户文件同目录时仍保护用户代码。
    pub fn delete(&self, id: &WorkspaceId) -> anyhow::Result<()> {
        let index = self.load_index()?;
        let root = index
            .get_root(id.as_str())
            .unwrap_or_else(|| self.base_dir.join(id.as_path_segment()));

        if !root.exists() {
            anyhow::bail!("Workspace '{}' not found", id);
        }

        // 安全检查：使用 canonicalize 解析符号链接，防止路径穿越攻击
        let canonical_root = root
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("无法解析工作区路径: {} ({})", root.display(), e))?;
        let manifest = WorkspaceLayout::manifest(&canonical_root);
        if !manifest.is_file() {
            anyhow::bail!("拒绝删除：工作区缺少 manifest 文件");
        }
        let state_dir = WorkspaceLayout::state_dir(&canonical_root);
        if state_dir.exists() {
            if fs::symlink_metadata(&state_dir)?.file_type().is_symlink() {
                // The Workspace owns its link, never the linked directory.
                fs::remove_file(&state_dir)?;
            } else {
                // 本地目录可能混有用户规则和扩展；只删除已知产品数据，
                // 保留 project.md/local.md、Skills/Subagents 与未知内容。
                for directory in [
                    "sessions",
                    "conversations",
                    "memory",
                    "evolution",
                    "data",
                    "papers",
                    "artifacts",
                    "tasks",
                    "traces",
                    "logs",
                    "uploads",
                    "conversation-deletions",
                ] {
                    let path = state_dir.join(directory);
                    match fs::symlink_metadata(&path) {
                        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
                        Ok(_) => fs::remove_file(path)?,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                for name in ["MEMORY.md", "learned-rules.md", "workspace.json"] {
                    match fs::remove_file(state_dir.join(name)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                // remove_dir never erases a concurrent user's new entry.
                match fs::remove_dir(&state_dir) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }

        // 更新索引
        let mut index = self.load_index()?;
        index.remove(id.as_str());
        self.save_index(&index)?;

        tracing::info!(workspace = %id, "Deleted workspace");
        Ok(())
    }

    // ── 项目关联 ──

    /// 将代码项目目录关联到工作区。
    pub fn link_project(
        &self,
        id: &WorkspaceId,
        project_root: PathBuf,
    ) -> anyhow::Result<Workspace> {
        let mut workspace = self.open(id)?;

        let canonical = project_root.canonicalize().unwrap_or(project_root.clone());

        if !canonical.is_dir() {
            anyhow::bail!("Project root is not a directory: {}", canonical.display());
        }

        let project_display = canonical.display().to_string();
        let location = ProjectLocation::Local {
            path: canonical.clone(),
        };
        if workspace.project_location() != location {
            workspace.metadata.project_root_revision = workspace
                .metadata
                .project_root_revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Project-root revision overflow"))?;
        }
        workspace.project_root = Some(canonical);
        workspace.metadata.project_location = location;
        workspace.metadata.project_execution_location = None;
        workspace.refresh_product_data_generation();
        self.save_manifest(&workspace)?;

        tracing::info!(
            workspace = %id,
            project = %project_display,
            "Linked project to workspace"
        );
        Ok(workspace)
    }

    /// Restore the project-link fields from a previously captured workspace
    /// snapshot after a runtime relink could not settle. This is only used by
    /// the application transition owner while its workspace write admission
    /// is held; it does not create a second workspace authority.
    pub(crate) fn restore_project_link(&self, snapshot: &Workspace) -> anyhow::Result<Workspace> {
        let mut workspace = self.open(&snapshot.id)?;
        workspace.project_root = snapshot.project_root.clone();
        workspace.metadata.project_location = snapshot.metadata.project_location.clone();
        workspace.metadata.project_execution_location =
            snapshot.metadata.project_execution_location.clone();
        workspace.metadata.project_root_revision = snapshot.metadata.project_root_revision;
        workspace.refresh_product_data_generation();
        self.save_manifest(&workspace)?;
        Ok(workspace)
    }

    /// 从 CWD 自动检测工作区。
    ///
    /// 检查当前目录是否在某个工作区的 `project_root` 下。
    pub fn detect_from_cwd(&self, cwd: &Path) -> Option<Workspace> {
        let workspaces = self.list().ok()?;
        let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

        for ws in &workspaces {
            if let Some(ref project_root) = ws.project_root {
                let canonical_project = project_root
                    .canonicalize()
                    .unwrap_or_else(|_| project_root.clone());
                if canonical_cwd.starts_with(&canonical_project) {
                    return Some(ws.clone());
                }
            }
        }
        None
    }

    /// 通过向上遍历目录查找 `.eko/workspace.json` 来检测工作区。
    pub fn detect_from_manifest(cwd: &Path) -> Option<Workspace> {
        let mut current = cwd.to_path_buf();
        loop {
            let manifest = WorkspaceLayout::manifest(&current);
            if manifest.exists()
                && let Ok(data) = fs::read_to_string(&manifest)
                && let Ok(ws) = serde_json::from_str::<Workspace>(&data)
            {
                return Some(ws);
            }
            if !current.pop() {
                break;
            }
        }
        None
    }

    // ── 内部方法 ──

    /// 保存工作区清单文件。
    fn save_manifest(&self, workspace: &Workspace) -> anyhow::Result<()> {
        let manifest_path = WorkspaceLayout::manifest(&workspace.root);
        if let Some(parent) = manifest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(workspace)?;
        echo_agent::utils::fs::atomic_write(&manifest_path, &json)?;
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_fork_owner_check_does_not_treat_broken_registry_as_an_unowned_draft()
    -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temporary.path().join("registry"))?;
        let created = registry.create_at(
            "reserved",
            WorkspaceKind::General,
            temporary.path().join("workspace"),
        )?;
        std::fs::write(WorkspaceLayout::manifest(&created.root), "broken manifest")?;
        assert!(registry.inspect(&created.id).is_err());
        assert!(registry.is_identity_reserved(&created.id)?);
        let missing = WorkspaceId::from_raw("reply-fork-auth-fixture".to_string());
        assert!(!registry.is_identity_reserved(&missing)?);
        std::fs::write(registry.base_dir.join("registry.json"), "broken index")?;
        assert!(registry.is_identity_reserved(&missing).is_err());
        Ok(())
    }

    #[test]
    fn inspect_is_side_effect_free_and_keeps_manifest_bytes_stable() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().to_path_buf())?;
        let workspace = registry.create("inspect-only", WorkspaceKind::default())?;
        let manifest = WorkspaceLayout::manifest(&workspace.root);
        let before = std::fs::read(&manifest)?;

        let first = registry.inspect(&workspace.id)?;
        let second = registry.inspect(&workspace.id)?;
        let after = std::fs::read(&manifest)?;

        assert_eq!(first.last_active, workspace.last_active);
        assert_eq!(second.last_active, workspace.last_active);
        assert_eq!(
            first.product_data_generation,
            workspace.product_data_generation
        );
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn inspect_cannot_roll_back_a_newer_project_link_revision() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("registry"))?;
        let workspace = registry.create("inspect-relink", WorkspaceKind::default())?;
        let stale_snapshot = registry.inspect(&workspace.id)?;
        let project = temp.path().join("linked-project");
        std::fs::create_dir_all(&project)?;

        let linked = registry.link_project(&workspace.id, project.canonicalize()?)?;
        let inspected = registry.inspect(&workspace.id)?;

        assert_eq!(stale_snapshot.metadata.project_root_revision, 0);
        assert_eq!(linked.metadata.project_root_revision, 1);
        assert_eq!(inspected.metadata.project_root_revision, 1);
        assert_eq!(inspected.project_root, linked.project_root);
        Ok(())
    }

    #[test]
    fn link_project_rejects_a_regular_file() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("registry"))?;
        let workspace = registry.create("reject-file", WorkspaceKind::default())?;
        let file = temp.path().join("project-file");
        std::fs::write(&file, "not a project directory")?;

        assert!(registry.link_project(&workspace.id, file).is_err());
        Ok(())
    }

    #[test]
    fn every_current_constructor_persists_explicit_location() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().join("registry"))?;
        let default = registry.create("current-default", WorkspaceKind::General)?;
        assert_eq!(
            default.project_location(),
            ProjectLocation::Local {
                path: default.root.clone()
            }
        );
        let custom_root = tmp.path().join("custom-data");
        let custom = registry.create_at(
            "current-custom",
            WorkspaceKind::General,
            custom_root.clone(),
        )?;
        assert_eq!(
            custom.project_location(),
            ProjectLocation::Local { path: custom_root }
        );
        for workspace in [default, custom] {
            let raw = std::fs::read_to_string(WorkspaceLayout::manifest(&workspace.root))?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            assert!(
                value
                    .get("metadata")
                    .and_then(|value| value.get("project_location"))
                    .is_some()
            );
            assert_eq!(
                registry.inspect(&workspace.id)?.project_location(),
                workspace.project_location()
            );
        }
        Ok(())
    }

    #[test]
    fn test_create_and_open_workspace() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf())?;

        let ws = registry.create("test-project", WorkspaceKind::Code { repo_url: None })?;

        assert_eq!(ws.name, "test-project");
        assert!(ws.root.exists());
        assert!(WorkspaceLayout::sessions(&ws.root).exists());

        let opened = registry.open(&ws.id)?;
        assert_eq!(opened.name, "test-project");
        Ok(())
    }

    #[test]
    fn test_create_at_custom_path() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = tmp.path().join("base");
        let custom = tmp.path().join("my-project");
        let registry = WorkspaceRegistry::with_base_dir(base)?;

        let ws = registry.create_at("my-project", WorkspaceKind::General, custom.clone())?;

        assert_eq!(ws.root, custom);
        assert!(WorkspaceLayout::sessions(&custom).exists());

        // Should be findable via open
        let opened = registry.open(&ws.id)?;
        assert_eq!(opened.root, custom);

        // Should appear in list
        let list = registry.list()?;
        assert_eq!(list.len(), 1);
        let listed = list
            .first()
            .ok_or_else(|| anyhow::anyhow!("custom workspace was not listed"))?;
        assert_eq!(listed.name, "my-project");
        Ok(())
    }

    #[test]
    fn retired_manifest_only_blocks_overwrite() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = tmp.path().join("base");
        let custom = tmp.path().join("existing-project");
        fs::create_dir_all(&custom)?;
        let retired = custom.join(".workspace.json");
        fs::write(&retired, "{}")?;
        let registry = WorkspaceRegistry::with_base_dir(base)?;

        assert!(
            registry
                .create_at("existing-project", WorkspaceKind::General, custom.clone())
                .is_err()
        );
        assert!(retired.exists());
        assert!(WorkspaceRegistry::detect_from_manifest(&custom).is_none());
        Ok(())
    }

    #[test]
    fn test_list_workspaces() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf())?;

        registry.create("project-a", WorkspaceKind::General)?;
        registry.create("project-b", WorkspaceKind::Research { topics: vec![] })?;

        let list = registry.list()?;
        assert_eq!(list.len(), 2);
        Ok(())
    }

    #[test]
    fn test_delete_workspace() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf())?;

        let ws = registry.create("to-delete", WorkspaceKind::General)?;
        assert!(ws.root.exists());

        registry.delete(&ws.id)?;
        assert!(!WorkspaceLayout::state_dir(&ws.root).exists());
        Ok(())
    }

    #[test]
    fn test_delete_custom_path_workspace() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = tmp.path().join("base");
        let custom = tmp.path().join("my-project");
        // Pre-create a user file in the custom dir
        fs::create_dir_all(&custom)?;
        fs::write(custom.join("README.md"), "# My Project")?;

        let registry = WorkspaceRegistry::with_base_dir(base)?;
        let ws = registry.create_at("my-project", WorkspaceKind::General, custom.clone())?;

        registry.delete(&ws.id)?;

        // Custom path: user's original file should be preserved
        assert!(custom.join("README.md").exists());
        // But EKO system data should be cleaned up
        assert!(!WorkspaceLayout::state_dir(&custom).exists());
        assert!(!WorkspaceLayout::manifest(&custom).exists());
        Ok(())
    }

    #[test]
    fn test_duplicate_creation_fails() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf())?;

        registry.create("dup", WorkspaceKind::General)?;
        let result = registry.create("dup", WorkspaceKind::General);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn existing_project_keeps_code_and_separates_product_data() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("既有项目 🦀");
        fs::create_dir(&project)?;
        fs::write(project.join("AGENTS.md"), "original user instructions")?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("data"))?;
        let workspace = registry.create_local_project(
            "existing",
            WorkspaceKind::General,
            ProjectSource::ExistingLocal {
                path: project.clone(),
            },
        )?;
        assert_ne!(workspace.root, project);
        assert_eq!(
            workspace.local_project_root(),
            Some(project.canonicalize()?)
        );
        assert!(!project.join(".eko").exists());
        let reopened = registry.inspect(&workspace.id)?;
        assert_eq!(reopened.project_location(), workspace.project_location());
        registry.delete(&workspace.id)?;
        assert_eq!(
            fs::read_to_string(project.join("AGENTS.md"))?,
            "original user instructions"
        );
        Ok(())
    }

    #[test]
    fn ssh_project_round_trip_preserves_raw_and_effective_locations() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("data"))?;
        let raw = ProjectLocation::Ssh {
            host: "eko-alias".to_string(),
            user: None,
            port: None,
            path: "~/项目".to_string(),
        };
        let effective = ProjectLocation::Ssh {
            host: "198.51.100.7".to_string(),
            user: Some("echo".to_string()),
            port: Some(2222),
            path: "/srv/项目".to_string(),
        };

        let workspace = registry.create_project(
            "remote-round-trip",
            WorkspaceKind::Code { repo_url: None },
            raw.clone(),
            Some(effective.clone()),
        )?;
        let reopened = registry.inspect(&workspace.id)?;

        assert_eq!(reopened.project_location(), raw);
        assert_eq!(reopened.execution_location(), effective);
        assert!(reopened.local_project_root().is_none());
        assert_eq!(reopened.project_root, None);
        Ok(())
    }

    #[test]
    fn new_project_creates_one_directory_and_never_adopts_existing_content() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("data"))?;
        let source = || ProjectSource::NewLocal {
            parent: temp.path().to_path_buf(),
            directory_name: "新项目".to_string(),
        };
        let workspace = registry.create_local_project("new", WorkspaceKind::General, source())?;
        let project = temp.path().join("新项目");
        fs::write(project.join("user.txt"), "keep")?;
        assert!(
            registry
                .create_local_project("another", WorkspaceKind::General, source())
                .is_err()
        );
        registry.delete(&workspace.id)?;
        assert_eq!(fs::read_to_string(project.join("user.txt"))?, "keep");
        Ok(())
    }

    #[test]
    fn failed_new_project_publication_rolls_back_only_its_empty_directory() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("data"))?;
        let occupied = registry.default_root("collision");
        fs::create_dir_all(&occupied)?;
        fs::write(occupied.join(".workspace.json"), "preserve old marker")?;
        let result = registry.create_local_project(
            "collision",
            WorkspaceKind::General,
            ProjectSource::NewLocal {
                parent: temp.path().to_path_buf(),
                directory_name: "rollback".to_string(),
            },
        );
        assert!(result.is_err());
        assert!(!temp.path().join("rollback").exists());
        assert_eq!(
            fs::read_to_string(occupied.join(".workspace.json"))?,
            "preserve old marker"
        );
        for name in ["..", "../escape", "nested/child", ""] {
            assert!(
                registry
                    .create_local_project(
                        "invalid",
                        WorkspaceKind::General,
                        ProjectSource::NewLocal {
                            parent: temp.path().to_path_buf(),
                            directory_name: name.to_string()
                        }
                    )
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn deleting_current_default_workspace_preserves_user_code() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().to_path_buf())?;
        let workspace = registry.create("legacy", WorkspaceKind::General)?;
        fs::write(workspace.root.join("main.rs"), "fn main() {}")?;
        let skills = WorkspaceLayout::state_dir(&workspace.root).join("skills/user-skill");
        fs::create_dir_all(&skills)?;
        fs::write(skills.join("SKILL.md"), "user skill source")?;
        fs::write(
            WorkspaceLayout::state_dir(&workspace.root).join("project.md"),
            "user project rule",
        )?;
        registry.delete(&workspace.id)?;
        assert_eq!(
            fs::read_to_string(workspace.root.join("main.rs"))?,
            "fn main() {}"
        );
        assert!(registry.inspect(&workspace.id).is_err());
        assert_eq!(
            fs::read_to_string(skills.join("SKILL.md"))?,
            "user skill source"
        );
        assert_eq!(
            fs::read_to_string(WorkspaceLayout::state_dir(&workspace.root).join("project.md"))?,
            "user project rule"
        );
        Ok(())
    }

    #[test]
    fn corrupt_index_is_not_replaced_by_an_empty_registry() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let registry = WorkspaceRegistry::with_base_dir(temp.path().join("data"))?;
        let old = registry.create_at(
            "existing",
            WorkspaceKind::General,
            temp.path().join("external"),
        )?;
        let previous = fs::read(registry.index_path())?;
        let corrupt = b"{broken index";
        fs::write(registry.index_path(), corrupt)?;
        assert!(registry.create("new", WorkspaceKind::General).is_err());
        assert_eq!(fs::read(registry.index_path())?, corrupt);
        assert!(WorkspaceLayout::manifest(&old.root).is_file());
        fs::write(registry.index_path(), previous)?;
        assert_eq!(registry.inspect(&old.id)?.root, old.root);
        Ok(())
    }

    #[test]
    fn test_mixed_default_and_custom_workspaces() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = tmp.path().join("base");
        let custom = tmp.path().join("external-project");
        let registry = WorkspaceRegistry::with_base_dir(base)?;

        registry.create("default-ws", WorkspaceKind::General)?;
        registry.create_at("custom-ws", WorkspaceKind::Code { repo_url: None }, custom)?;

        let list = registry.list()?;
        assert_eq!(list.len(), 2);
        Ok(())
    }
}
