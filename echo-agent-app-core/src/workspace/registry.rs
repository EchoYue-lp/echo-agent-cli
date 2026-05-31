//! 工作区注册表
//!
//! 管理工作区的创建、发现、切换和删除。
//! 支持两种工作区位置：
//! - 默认：`~/.echo-agent/workspaces/{id}/`
//! - 自定义：用户指定的任意目录
//!
//! 所有工作区通过 `registry.json` 索引文件统一追踪。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::layout::WorkspaceLayout;
use super::{Workspace, WorkspaceId, WorkspaceKind, WorkspaceMetadata};

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
    fn load(path: &Path) -> Self {
        if path.exists() {
            fs::read_to_string(path)
                .ok()
                .and_then(|data| serde_json::from_str(&data).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
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
    /// 工作区基础目录，通常为 `~/.echo-agent/workspaces/`。
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

    /// 基础目录路径。
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// 索引文件路径。
    fn index_path(&self) -> PathBuf {
        self.base_dir.join("registry.json")
    }

    /// 加载索引。
    fn load_index(&self) -> RegistryIndex {
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
    pub fn create(
        &self,
        name: &str,
        kind: WorkspaceKind,
    ) -> anyhow::Result<Workspace> {
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
        let id = WorkspaceId::from_name(name);

        // 检查索引中是否已有同名工作区
        let index = self.load_index();
        if index.get_root(id.as_str()).is_some() {
            anyhow::bail!("Workspace '{}' already exists", id);
        }

        // 如果目录已存在且有清单文件，拒绝覆盖
        let manifest = WorkspaceLayout::manifest(&root);
        if manifest.exists() {
            anyhow::bail!(
                "Directory already contains a workspace: {}",
                root.display()
            );
        }

        // 创建目录结构
        fs::create_dir_all(&root)?;
        WorkspaceLayout::ensure_dirs(&root)?;

        let now = Utc::now();
        let workspace = Workspace {
            id: id.clone(),
            name: name.to_string(),
            root: root.clone(),
            project_root: None,
            kind,
            metadata: WorkspaceMetadata::default(),
            created_at: now,
            last_active: now,
        };

        // 写入清单文件
        self.save_manifest(&workspace)?;

        // 更新索引
        let mut index = self.load_index();
        index.insert(id.as_str(), &root);
        self.save_index(&index)?;

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
        let index = self.load_index();

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

        // 更新最后活跃时间
        workspace.touch();
        self.save_manifest(&workspace)?;

        Ok(workspace)
    }

    /// 按名称查找并打开工作区。
    pub fn open_by_name(&self, name: &str) -> anyhow::Result<Workspace> {
        let id = WorkspaceId::from_name(name);
        self.open(&id)
    }

    /// 列出所有工作区，按最后活跃时间降序排列。
    ///
    /// 同时扫描索引文件和基础目录（兼容旧的无索引工作区）。
    pub fn list(&self) -> anyhow::Result<Vec<Workspace>> {
        let mut workspaces = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        // 1. 从索引加载
        let index = self.load_index();
        for (id_str, root_str) in &index.entries {
            let root = PathBuf::from(root_str);
            let manifest = WorkspaceLayout::manifest(&root);
            if manifest.exists() {
                if let Ok(data) = fs::read_to_string(&manifest) {
                    if let Ok(ws) = serde_json::from_str::<Workspace>(&data) {
                        seen_ids.insert(id_str.clone());
                        workspaces.push(ws);
                        continue;
                    }
                }
            }
            // 索引条目无效（目录被手动删除了），忽略
            tracing::warn!(
                id = %id_str,
                root = %root_str,
                "Indexed workspace root not found, skipping"
            );
        }

        // 2. 扫描基础目录（兼容旧的无索引工作区）
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
                if let Ok(data) = fs::read_to_string(&manifest) {
                    if let Ok(ws) = serde_json::from_str::<Workspace>(&data) {
                        if !seen_ids.contains(ws.id.as_str()) {
                            workspaces.push(ws);
                        }
                    }
                }
            }
        }

        // 按最后活跃时间降序
        workspaces.sort_by(|a, b| b.last_active.cmp(&a.last_active));
        Ok(workspaces)
    }

    /// 删除工作区。
    ///
    /// 删除工作区目录（如果路径在基础目录下）或仅清除清单文件（自定义路径），
    /// 并从索引中移除。
    pub fn delete(&self, id: &WorkspaceId) -> anyhow::Result<()> {
        let index = self.load_index();
        let root = index
            .get_root(id.as_str())
            .unwrap_or_else(|| self.base_dir.join(id.as_path_segment()));

        if !root.exists() {
            anyhow::bail!("Workspace '{}' not found", id);
        }

        // 判断是否在基础目录下
        let is_under_base = root.starts_with(&self.base_dir);

        if is_under_base {
            // 在基础目录下 → 整个删除
            fs::remove_dir_all(&root)?;
        } else {
            // 自定义路径 → 只删除清单文件和 echo 子目录，保留用户原有文件
            let manifest = WorkspaceLayout::manifest(&root);
            if manifest.exists() {
                fs::remove_file(&manifest)?;
            }
            // 清理 echo 创建的子目录
            for subdir in [
                "sessions",
                "conversations",
                "memory",
                "data",
                "papers",
                "artifacts",
                "tasks",
                "traces",
                "uploads",
            ] {
                let dir = root.join(subdir);
                if dir.exists() {
                    fs::remove_dir_all(&dir).ok();
                }
            }
            // 清理 scratchpad 和 decisions
            for file in ["scratchpad.md", "decisions.jsonl"] {
                let f = root.join(file);
                if f.exists() {
                    fs::remove_file(&f).ok();
                }
            }
        }

        // 更新索引
        let mut index = self.load_index();
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

        let canonical = project_root
            .canonicalize()
            .unwrap_or(project_root.clone());

        if !canonical.exists() {
            anyhow::bail!("Project root does not exist: {}", canonical.display());
        }

        workspace.project_root = Some(canonical);
        self.save_manifest(&workspace)?;

        tracing::info!(
            workspace = %id,
            project = %workspace.project_root.as_ref().unwrap().display(),
            "Linked project to workspace"
        );
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

    /// 通过向上遍历目录查找 `.workspace.json` 来检测工作区。
    pub fn detect_from_manifest(cwd: &Path) -> Option<Workspace> {
        let mut current = cwd.to_path_buf();
        loop {
            let manifest = WorkspaceLayout::manifest(&current);
            if manifest.exists() {
                if let Ok(data) = fs::read_to_string(&manifest) {
                    if let Ok(ws) = serde_json::from_str::<Workspace>(&data) {
                        return Some(ws);
                    }
                }
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
        let json = serde_json::to_string_pretty(workspace)?;
        fs::write(&manifest_path, json)?;
        Ok(())
    }
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new().expect("Failed to create workspace registry")
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_open_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf()).unwrap();

        let ws = registry
            .create("test-project", WorkspaceKind::Code { repo_url: None })
            .unwrap();

        assert_eq!(ws.name, "test-project");
        assert!(ws.root.exists());
        assert!(WorkspaceLayout::sessions(&ws.root).exists());

        let opened = registry.open(&ws.id).unwrap();
        assert_eq!(opened.name, "test-project");
    }

    #[test]
    fn test_create_at_custom_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let custom = tmp.path().join("my-project");
        let registry = WorkspaceRegistry::with_base_dir(base).unwrap();

        let ws = registry
            .create_at("my-project", WorkspaceKind::General, custom.clone())
            .unwrap();

        assert_eq!(ws.root, custom);
        assert!(WorkspaceLayout::sessions(&custom).exists());

        // Should be findable via open
        let opened = registry.open(&ws.id).unwrap();
        assert_eq!(opened.root, custom);

        // Should appear in list
        let list = registry.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "my-project");
    }

    #[test]
    fn test_list_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf()).unwrap();

        registry.create("project-a", WorkspaceKind::General).unwrap();
        registry
            .create("project-b", WorkspaceKind::Research { topics: vec![] })
            .unwrap();

        let list = registry.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_delete_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf()).unwrap();

        let ws = registry.create("to-delete", WorkspaceKind::General).unwrap();
        assert!(ws.root.exists());

        registry.delete(&ws.id).unwrap();
        assert!(!ws.root.exists());
    }

    #[test]
    fn test_delete_custom_path_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let custom = tmp.path().join("my-project");
        // Pre-create a user file in the custom dir
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("README.md"), "# My Project").unwrap();

        let registry = WorkspaceRegistry::with_base_dir(base).unwrap();
        let ws = registry
            .create_at("my-project", WorkspaceKind::General, custom.clone())
            .unwrap();

        registry.delete(&ws.id).unwrap();

        // Custom path: user's original file should be preserved
        assert!(custom.join("README.md").exists());
        // But echo-created dirs should be cleaned up
        assert!(!custom.join("sessions").exists());
        assert!(!WorkspaceLayout::manifest(&custom).exists());
    }

    #[test]
    fn test_duplicate_creation_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::with_base_dir(tmp.path().to_path_buf()).unwrap();

        registry.create("dup", WorkspaceKind::General).unwrap();
        let result = registry.create("dup", WorkspaceKind::General);
        assert!(result.is_err());
    }

    #[test]
    fn test_mixed_default_and_custom_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let custom = tmp.path().join("external-project");
        let registry = WorkspaceRegistry::with_base_dir(base).unwrap();

        registry.create("default-ws", WorkspaceKind::General).unwrap();
        registry
            .create_at("custom-ws", WorkspaceKind::Code { repo_url: None }, custom)
            .unwrap();

        let list = registry.list().unwrap();
        assert_eq!(list.len(), 2);
    }
}
