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

    /// 工作区基础目录：`~/.eko/workspaces/`
    ///
    pub fn base_dir() -> PathBuf {
        crate::data_root::user_data_path("workspaces")
    }

    /// 工作区根目录：`~/.eko/workspaces/{id}/`
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
    /// accepted evidence / L3 promotion / TaskRuntime memory_bridge writes).
    /// Physically isolated per workspace/project so memories don't leak
    /// across projects (mirrors how hot-layer `MEMORY.md` already follows
    /// the project root).
    pub fn memory_store(root: &Path) -> PathBuf {
        Self::memory(root).join("store.json")
    }

    /// 自进化状态目录：`{root}/.eko/evolution/`
    pub fn evolution(root: &Path) -> PathBuf {
        Self::state_dir(root).join("evolution")
    }

    /// 统一证据候选日志：`{root}/.eko/evolution/evidence-candidates.jsonl`
    pub fn evidence_candidates(root: &Path) -> PathBuf {
        Self::evolution(root).join("evidence-candidates.jsonl")
    }

    /// 工作区 Curator 状态：`{root}/.eko/evolution/curator-state.json`
    pub fn curator_state(root: &Path) -> PathBuf {
        Self::evolution(root).join("curator-state.json")
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

    /// 用户输入长文本落盘目录：`{root}/.eko/artifacts/user-input/`
    ///
    /// 长粘贴和超过输入预算的文本写入此目录（按
    /// `{conversation}/{turn}/` 分层），模型只收到引用 + 预览，通过
    /// `grep` / `read_artifact` 按需读取。它与工具输出共用 artifact 根目录和
    /// 30 天清理策略。
    pub fn user_input_artifacts(root: &Path) -> PathBuf {
        Self::artifacts(root).join("user-input")
    }

    /// 任务状态目录：`{root}/.eko/tasks/`
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
            Self::evolution(root),
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

        Ok(())
    }

    /// 检查工作区目录是否有效（存在且包含清单文件）。
    pub fn is_valid_workspace(root: &Path) -> bool {
        root.exists() && Self::manifest(root).exists()
    }
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
    fn test_ensure_dirs() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();

        WorkspaceLayout::ensure_dirs(root)?;

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
        Ok(())
    }

    #[test]
    fn retired_root_marker_is_not_a_workspace_manifest() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join(".workspace.json"), "{}")?;
        assert!(!WorkspaceLayout::is_valid_workspace(tmp.path()));
        Ok(())
    }
}
