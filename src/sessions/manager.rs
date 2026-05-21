//! 会话管理器
//!
//! 提供会话的完整生命周期管理：创建、保存、加载、分支、差异对比。

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use super::export::SessionExporter;
use super::types::{Session, SessionDiff, SessionSummary};

/// 会话管理器
pub struct SessionManager {
    base_dir: PathBuf,
    /// 是否启用自动保存
    auto_save: bool,
    /// 当前活跃会话 ID
    active_session_id: Option<String>,
}

impl SessionManager {
    pub fn new() -> Self {
        let base_dir = Self::base_dir();
        fs::create_dir_all(&base_dir).ok();
        Self {
            base_dir,
            auto_save: true,
            active_session_id: None,
        }
    }

    pub fn base_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let new_dir = PathBuf::from(&home).join(".echo-agent").join("sessions");
        // Migration: if legacy sessions_v2 directory exists with data, use it.
        // New sessions use the unified "sessions" directory (same as Persistence v1).
        let legacy_dir = PathBuf::from(&home).join(".echo-agent").join("sessions_v2");
        if legacy_dir.exists() && !new_dir.exists() {
            return legacy_dir;
        }
        new_dir
    }

    /// 设置是否自动保存
    pub fn set_auto_save(&mut self, enabled: bool) {
        self.auto_save = enabled;
    }

    /// 获取当前活跃会话 ID
    pub fn active_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }

    // ── CRUD ─────────────────────────────────────────────

    /// 创建新会话
    pub fn create(&mut self, name: &str, model: &str) -> anyhow::Result<Session> {
        let session = Session::new(name, model);
        self.save(&session)?;
        self.active_session_id = Some(session.id.clone());
        Ok(session)
    }

    /// 保存会话
    pub fn save(&self, session: &Session) -> anyhow::Result<()> {
        let mut s = session.clone();
        s.updated_at = Utc::now().to_rfc3339();
        s.message_count = s.messages.len();
        let path = self.session_path(&s.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&s)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// 加载会话
    pub fn load(&self, id: &str) -> anyhow::Result<Session> {
        let path = self.session_path(id);
        let data = fs::read_to_string(&path)?;
        let session: Session = serde_json::from_str(&data)?;
        Ok(session)
    }

    /// 删除会话
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        let path = self.session_path(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// 列出所有会话摘要
    pub fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let mut sessions = Vec::new();
        let entries = match fs::read_dir(&self.base_dir) {
            Ok(e) => e,
            Err(_) => return Ok(sessions),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Ok(data) = fs::read_to_string(&path)
                    && let Ok(s) = serde_json::from_str::<Session>(&data) {
                        sessions.push(SessionSummary {
                            id: s.id,
                            name: s.name,
                            model: s.model,
                            branch: s.branch,
                            message_count: s.message_count,
                            estimated_tokens: s.estimated_tokens,
                            tags: s.tags,
                            created_at: s.created_at,
                            updated_at: s.updated_at,
                        });
                    }
        }

        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    // ── 分支操作 ─────────────────────────────────────────

    /// 从现有会话创建分支
    pub fn branch(&mut self, parent_id: &str, branch_name: &str) -> anyhow::Result<Session> {
        let parent = self.load(parent_id)?;
        let branch = Session::branch_from(&parent, branch_name);
        self.save(&branch)?;
        self.active_session_id = Some(branch.id.clone());
        Ok(branch)
    }

    /// 获取会话的所有分支
    pub fn list_branches(&self, root_id: &str) -> anyhow::Result<Vec<SessionSummary>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|s| {
                s.id == root_id
                    || s.id == self
                        .load(root_id)
                        .ok()
                        .and_then(|r| r.parent_id)
                        .unwrap_or_default()
            })
            .collect())
    }

    // ── 差异对比 ─────────────────────────────────────────

    /// 对比两个会话之间的差异
    pub fn diff(&self, id_a: &str, id_b: &str) -> anyhow::Result<SessionDiff> {
        let session_a = self.load(id_a)?;
        let session_b = self.load(id_b)?;
        Ok(Self::compute_diff(&session_a, &session_b))
    }

    fn compute_diff(a: &Session, b: &Session) -> SessionDiff {
        let text_a: Vec<String> = a
            .messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content.as_deref().unwrap_or("")))
            .collect();
        let text_b: Vec<String> = b
            .messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content.as_deref().unwrap_or("")))
            .collect();

        // Join lines with \n to create strings then diff as char-level
        let joined_a = text_a.join("\n");
        let joined_b = text_b.join("\n");

        let diff = similar::TextDiff::from_lines(&joined_a, &joined_b);
        let mut added = 0;
        let mut removed = 0;
        let mut hunks = Vec::new();

        for hunk in diff.unified_diff().iter_hunks() {
            let mut lines = Vec::new();
            for change in hunk.iter_changes() {
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        lines.push(super::types::DiffLine::Context(change.value().to_string()));
                    }
                    similar::ChangeTag::Insert => {
                        added += 1;
                        lines.push(super::types::DiffLine::Added(change.value().to_string()));
                    }
                    similar::ChangeTag::Delete => {
                        removed += 1;
                        lines.push(super::types::DiffLine::Removed(change.value().to_string()));
                    }
                }
            }

            // Compute hunk-level ranges from the diff operations
            let ops = hunk.ops();
            let (old_start, old_end) = ops
                .iter()
                .map(|op| op.old_range())
                .fold((usize::MAX, 0usize), |(min_s, max_e), r| {
                    (min_s.min(r.start), max_e.max(r.end))
                });
            let (new_start, new_end) = ops
                .iter()
                .map(|op| op.new_range())
                .fold((usize::MAX, 0usize), |(min_s, max_e), r| {
                    (min_s.min(r.start), max_e.max(r.end))
                });

            hunks.push(super::types::DiffHunk {
                old_start,
                old_count: old_end.saturating_sub(old_start),
                new_start,
                new_count: new_end.saturating_sub(new_start),
                lines,
            });
        }

        SessionDiff {
            hunks,
            added,
            removed,
        }
    }

    // ── 导出 ─────────────────────────────────────────────

    /// 导出会话为 JSON
    pub fn export_json(&self, id: &str, output_path: &Path) -> anyhow::Result<()> {
        let session = self.load(id)?;
        SessionExporter::to_json(&session, output_path)
    }

    /// 导出会话为 Markdown
    pub fn export_markdown(&self, id: &str, output_path: &Path) -> anyhow::Result<()> {
        let session = self.load(id)?;
        SessionExporter::to_markdown(&session, output_path)
    }

    /// 导出会话为 HTML
    pub fn export_html(&self, id: &str, output_path: &Path) -> anyhow::Result<()> {
        let session = self.load(id)?;
        SessionExporter::to_html(&session, output_path)
    }

    // ── 内部辅助 ────────────────────────────────────────

    fn session_path(&self, id: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", id))
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionMessage;

    #[test]
    fn test_session_create_and_list() {
        let mut manager = SessionManager::new();
        let session = manager.create("test-session", "qwen-plus").unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.name, "test-session");

        let list = manager.list().unwrap();
        assert!(list.iter().any(|s| s.id == session.id));

        manager.delete(&session.id).unwrap();
    }

    #[test]
    fn test_session_branch() {
        let mut manager = SessionManager::new();
        let parent = manager.create("parent", "qwen-plus").unwrap();
        let branch = manager.branch(&parent.id, "experiment").unwrap();

        assert_eq!(branch.parent_id, Some(parent.id.clone()));
        assert_eq!(branch.branch, Some("experiment".to_string()));

        manager.delete(&parent.id).unwrap();
        manager.delete(&branch.id).unwrap();
    }

    #[test]
    fn test_session_diff() {
        let mut manager = SessionManager::new();
        let mut a = manager.create("diff-a", "qwen-plus").unwrap();
        a.messages.push(SessionMessage {
            role: "user".into(),
            content: Some("hello".into()),
            tool_calls: None,
        });
        manager.save(&a).unwrap();

        let mut b = manager.create("diff-b", "qwen-plus").unwrap();
        b.messages.push(SessionMessage {
            role: "user".into(),
            content: Some("world".into()),
            tool_calls: None,
        });
        manager.save(&b).unwrap();

        let diff = manager.diff(&a.id, &b.id).unwrap();
        assert!(diff.added > 0 || diff.removed > 0);

        manager.delete(&a.id).unwrap();
        manager.delete(&b.id).unwrap();
    }
}
