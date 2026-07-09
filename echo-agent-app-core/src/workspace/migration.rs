//! 旧数据迁移工具
//!
//! 将 `~/.echo-agent/` 下的扁平数据迁移到工作区目录结构中。
//! 非破坏性迁移：先复制到工作区结构，验证后才清理原文件。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::WorkspaceKind;
use super::layout::WorkspaceLayout;
use super::registry::WorkspaceRegistry;

// ── 迁移计划 ────────────────────────────────────────────────────────

/// 迁移计划 — 描述需要迁移的数据和目标工作区。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// 要创建的工作区：(名称, 关联的会话文件名列表)。
    pub workspaces_to_create: Vec<WorkspacePlan>,
    /// 无法自动分组的会话文件。
    pub ungrouped_sessions: Vec<String>,
    /// 需要迁移的对话记录数量。
    pub conversation_count: usize,
    /// 预估迁移大小（字节）。
    pub estimated_size_bytes: u64,
}

/// 单个工作区的迁移计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePlan {
    pub name: String,
    pub kind: WorkspaceKind,
    pub session_files: Vec<String>,
    pub conversation_files: Vec<String>,
}

/// 迁移报告 — 迁移执行后的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub workspaces_created: Vec<String>,
    pub sessions_migrated: usize,
    pub conversations_migrated: usize,
    pub errors: Vec<String>,
    pub completed_at: String,
}

// ── LegacyMigrator ──────────────────────────────────────────────────

/// 旧数据迁移器。
pub struct LegacyMigrator {
    /// 旧数据基础目录：`~/.echo-agent/`
    legacy_base: PathBuf,
}

impl LegacyMigrator {
    /// 创建迁移器。
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
        Self {
            legacy_base: PathBuf::from(home).join(".echo-agent"),
        }
    }

    /// 使用指定基础目录（测试用）。
    pub fn with_base_dir(base: PathBuf) -> Self {
        Self { legacy_base: base }
    }

    /// 审计旧数据，生成迁移计划。
    ///
    /// 扫描 `~/.echo-agent/sessions/` 和 `conversations/` 目录，
    /// 尝试按项目上下文自动分组。
    pub fn audit(&self) -> anyhow::Result<MigrationPlan> {
        let sessions_dir = self.legacy_base.join("sessions");
        let conversations_dir = sessions_dir.join("conversations");

        let mut session_files = Vec::new();
        let mut conversation_files = Vec::new();
        let mut total_size: u64 = 0;

        // 扫描会话文件
        if sessions_dir.exists() {
            for entry in fs::read_dir(&sessions_dir)?.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(meta) = path.metadata() {
                        total_size += meta.len();
                    }
                    session_files.push(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    );
                }
            }
        }

        // 扫描对话文件
        if conversations_dir.exists() {
            for entry in fs::read_dir(&conversations_dir)?.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(meta) = path.metadata() {
                        total_size += meta.len();
                    }
                    conversation_files.push(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    );
                }
            }
        }

        // 简单分组策略：所有会话归入一个 "default" 工作区
        // 更复杂的策略可以解析会话内容，按 model 或 system_prompt 分组
        let mut workspaces_to_create = Vec::new();

        if !session_files.is_empty() || !conversation_files.is_empty() {
            workspaces_to_create.push(WorkspacePlan {
                name: "default".to_string(),
                kind: WorkspaceKind::General,
                session_files: session_files.clone(),
                conversation_files: conversation_files.clone(),
            });
        }

        Ok(MigrationPlan {
            workspaces_to_create,
            ungrouped_sessions: vec![],
            conversation_count: conversation_files.len(),
            estimated_size_bytes: total_size,
        })
    }

    /// 执行迁移。
    ///
    /// 非破坏性：复制文件到新工作区结构，不删除原文件。
    pub fn execute(
        &self,
        plan: &MigrationPlan,
        registry: &WorkspaceRegistry,
    ) -> anyhow::Result<MigrationReport> {
        let mut report = MigrationReport {
            workspaces_created: vec![],
            sessions_migrated: 0,
            conversations_migrated: 0,
            errors: vec![],
            completed_at: echo_agent::utils::time::now_local().to_rfc3339(),
        };

        let sessions_dir = self.legacy_base.join("sessions");
        let conversations_dir = sessions_dir.join("conversations");

        for ws_plan in &plan.workspaces_to_create {
            // 创建工作区
            let workspace = match registry.create(&ws_plan.name, ws_plan.kind.clone()) {
                Ok(ws) => ws,
                Err(e) => {
                    report.errors.push(format!(
                        "Failed to create workspace '{}': {}",
                        ws_plan.name, e
                    ));
                    continue;
                }
            };

            // 复制会话文件
            let target_sessions = WorkspaceLayout::sessions(&workspace.root);
            for file in &ws_plan.session_files {
                let src = sessions_dir.join(file);
                let dst = target_sessions.join(file);
                if let Err(e) = fs::copy(&src, &dst) {
                    report
                        .errors
                        .push(format!("Failed to copy session {}: {}", file, e));
                } else {
                    report.sessions_migrated += 1;
                }
            }

            // 复制对话文件
            let target_conversations = WorkspaceLayout::conversations(&workspace.root);
            for file in &ws_plan.conversation_files {
                let src = conversations_dir.join(file);
                let dst = target_conversations.join(file);
                if let Err(e) = fs::copy(&src, &dst) {
                    report
                        .errors
                        .push(format!("Failed to copy conversation {}: {}", file, e));
                } else {
                    report.conversations_migrated += 1;
                }
            }

            report.workspaces_created.push(ws_plan.name.clone());
        }

        tracing::info!(
            workspaces = report.workspaces_created.len(),
            sessions = report.sessions_migrated,
            conversations = report.conversations_migrated,
            errors = report.errors.len(),
            "Migration completed"
        );

        Ok(report)
    }

    /// 检查是否有旧数据需要迁移。
    pub fn has_legacy_data(&self) -> bool {
        let sessions_dir = self.legacy_base.join("sessions");
        if !sessions_dir.exists() {
            return false;
        }
        // 检查是否有 JSON 文件
        fs::read_dir(&sessions_dir)
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "json")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }
}

impl Default for LegacyMigrator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let migrator = LegacyMigrator::with_base_dir(tmp.path().to_path_buf());

        let plan = migrator.audit().unwrap();
        assert!(plan.workspaces_to_create.is_empty());
        assert_eq!(plan.estimated_size_bytes, 0);
    }

    #[test]
    fn test_audit_with_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(
            sessions_dir.join("test.json"),
            r#"{"name":"test","messages":[]}"#,
        )
        .unwrap();

        let migrator = LegacyMigrator::with_base_dir(tmp.path().to_path_buf());
        let plan = migrator.audit().unwrap();
        assert_eq!(plan.workspaces_to_create.len(), 1);
        assert_eq!(plan.workspaces_to_create[0].session_files.len(), 1);
    }

    #[test]
    fn test_has_legacy_data() {
        let tmp = tempfile::tempdir().unwrap();
        let migrator = LegacyMigrator::with_base_dir(tmp.path().to_path_buf());
        assert!(!migrator.has_legacy_data());

        let sessions_dir = tmp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("test.json"), "{}").unwrap();
        assert!(migrator.has_legacy_data());
    }

    #[test]
    fn test_execute_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy_dir = tmp.path().join("legacy");
        let workspace_dir = tmp.path().join("workspaces");

        // 设置旧数据
        let sessions_dir = legacy_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(
            sessions_dir.join("session1.json"),
            r#"{"name":"s1","messages":[]}"#,
        )
        .unwrap();

        // 执行迁移
        let migrator = LegacyMigrator::with_base_dir(legacy_dir);
        let registry = WorkspaceRegistry::with_base_dir(workspace_dir).unwrap();
        let plan = migrator.audit().unwrap();
        let report = migrator.execute(&plan, &registry).unwrap();

        assert_eq!(report.workspaces_created.len(), 1);
        assert_eq!(report.sessions_migrated, 1);
        assert!(report.errors.is_empty());
    }
}
