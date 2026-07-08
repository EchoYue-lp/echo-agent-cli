//! 工作区目录布局
//!
//! 定义工作区内部的规范目录结构。
//! 所有路径解析都通过此模块进行，确保一致性。

use std::fs;
use std::path::{Path, PathBuf};

use super::WorkspaceId;

/// 工作区目录布局管理器。
///
/// 提供工作区内部各子目录的路径计算方法。
pub struct WorkspaceLayout;

impl WorkspaceLayout {
    // ── 基础目录 ──

    /// 工作区基础目录：`~/.echo-agent/workspaces/`
    ///
    /// # Panics
    /// 如果无法确定用户主目录，将 panic。
    pub fn base_dir() -> PathBuf {
        let home = home_dir().expect("无法确定用户主目录 (HOME 环境变量未设置)");
        home.join(".echo-agent").join("workspaces")
    }

    /// 工作区根目录：`~/.echo-agent/workspaces/{id}/`
    pub fn root(id: &WorkspaceId) -> PathBuf {
        Self::base_dir().join(id.as_path_segment())
    }

    // ── 子目录 ──

    /// 工作区系统数据目录：`{root}/.eko/`
    pub fn state_dir(root: &Path) -> PathBuf {
        root.join(".eko")
    }

    /// 会话历史目录：`{root}/.eko/sessions/`
    pub fn sessions(root: &Path) -> PathBuf {
        Self::state_dir(root).join("sessions")
    }

    /// 对话记录目录（前端持久化）：`{root}/.eko/conversations/`
    pub fn conversations(root: &Path) -> PathBuf {
        Self::state_dir(root).join("conversations")
    }

    /// 记忆存储目录：`{root}/.eko/memory/`
    pub fn memory(root: &Path) -> PathBuf {
        Self::state_dir(root).join("memory")
    }

    /// 动态记忆 store 文件：`{root}/.eko/memory/store.json`
    ///
    /// Warm-layer KV store for agent-learned dynamic memories (remember /
    /// AutoMemory / L3 promotion / TaskRuntime memory_bridge writes).
    /// Physically isolated per workspace/project so memories don't leak
    /// across projects (mirrors how hot-layer `MEMORY.md` already follows
    /// the project root).
    pub fn memory_store(root: &Path) -> PathBuf {
        Self::memory(root).join("store.json")
    }

    /// 数据集目录（数据分析工作区）：`{root}/.eko/data/`
    pub fn data(root: &Path) -> PathBuf {
        Self::state_dir(root).join("data")
    }

    /// 论文目录（学术研究工作区）：`{root}/.eko/papers/`
    pub fn papers(root: &Path) -> PathBuf {
        Self::state_dir(root).join("papers")
    }

    /// 生成物目录（报告、论文、图表）：`{root}/.eko/artifacts/`
    pub fn artifacts(root: &Path) -> PathBuf {
        Self::state_dir(root).join("artifacts")
    }

    /// 任务状态目录（SQLite DB）：`{root}/.eko/tasks/`
    pub fn tasks(root: &Path) -> PathBuf {
        Self::state_dir(root).join("tasks")
    }

    /// 执行轨迹目录（JSONL）：`{root}/.eko/traces/`
    pub fn traces(root: &Path) -> PathBuf {
        Self::state_dir(root).join("traces")
    }

    /// 日志目录：`{root}/.eko/logs/`
    pub fn logs(root: &Path) -> PathBuf {
        Self::state_dir(root).join("logs")
    }

    // ── 特殊文件 ──

    /// 工作区清单文件：`{root}/.eko/workspace.json`
    pub fn manifest(root: &Path) -> PathBuf {
        Self::state_dir(root).join("workspace.json")
    }

    /// 旧版工作区清单文件：`{root}/.workspace.json`
    pub fn legacy_manifest(root: &Path) -> PathBuf {
        root.join(".workspace.json")
    }

    /// 返回当前存在的清单路径，优先使用新版路径。
    pub fn existing_manifest(root: &Path) -> PathBuf {
        let manifest = Self::manifest(root);
        if manifest.exists() {
            manifest
        } else {
            Self::legacy_manifest(root)
        }
    }

    /// 共享草稿文件：`{root}/.eko/scratchpad.md`
    pub fn scratchpad(root: &Path) -> PathBuf {
        Self::state_dir(root).join("scratchpad.md")
    }

    /// 决策日志文件：`{root}/.eko/decisions.jsonl`
    pub fn decisions(root: &Path) -> PathBuf {
        Self::state_dir(root).join("decisions.jsonl")
    }

    /// 上传文件临时目录：`{root}/.eko/uploads/`
    pub fn uploads(root: &Path) -> PathBuf {
        Self::state_dir(root).join("uploads")
    }

    // ── 目录操作 ──

    /// 确保工作区的所有标准子目录都存在。
    pub fn ensure_dirs(root: &Path) -> anyhow::Result<()> {
        let dirs = [
            Self::sessions(root),
            Self::conversations(root),
            Self::memory(root),
            Self::data(root),
            Self::papers(root),
            Self::artifacts(root),
            Self::tasks(root),
            Self::traces(root),
            Self::uploads(root),
            Self::logs(root),
        ];

        for dir in &dirs {
            fs::create_dir_all(dir)?;
        }

        // 创建空的 scratchpad.md（如果不存在）
        let scratchpad = Self::scratchpad(root);
        if !scratchpad.exists() {
            fs::write(&scratchpad, "# Scratchpad\n\nShared workspace notes.\n")?;
        }

        Ok(())
    }

    /// 检查工作区目录是否有效（存在且包含清单文件）。
    pub fn is_valid_workspace(root: &Path) -> bool {
        root.exists() && (Self::manifest(root).exists() || Self::legacy_manifest(root).exists())
    }
}

/// 获取用户 home 目录。
fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_paths() {
        let root = Path::new("/tmp/test-workspace");

        assert_eq!(
            WorkspaceLayout::sessions(root),
            PathBuf::from("/tmp/test-workspace/.eko/sessions")
        );
        assert_eq!(
            WorkspaceLayout::papers(root),
            PathBuf::from("/tmp/test-workspace/.eko/papers")
        );
        assert_eq!(
            WorkspaceLayout::manifest(root),
            PathBuf::from("/tmp/test-workspace/.eko/workspace.json")
        );
    }

    #[test]
    fn test_ensure_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        WorkspaceLayout::ensure_dirs(root).unwrap();

        assert!(WorkspaceLayout::state_dir(root).exists());
        assert!(WorkspaceLayout::sessions(root).exists());
        assert!(WorkspaceLayout::conversations(root).exists());
        assert!(WorkspaceLayout::memory(root).exists());
        assert!(WorkspaceLayout::data(root).exists());
        assert!(WorkspaceLayout::papers(root).exists());
        assert!(WorkspaceLayout::artifacts(root).exists());
        assert!(WorkspaceLayout::tasks(root).exists());
        assert!(WorkspaceLayout::traces(root).exists());
        assert!(WorkspaceLayout::uploads(root).exists());
        assert!(WorkspaceLayout::logs(root).exists());
        assert!(WorkspaceLayout::scratchpad(root).exists());
    }
}
