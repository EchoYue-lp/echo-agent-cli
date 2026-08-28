//! 工作区模块
//!
//! 工作区（Workspace）是任务的主分割点，相当于命名空间。
//! 每个工作区拥有独立的目录，所有中间数据都存储在该目录下，实现工作区隔离。
//!
//! # 目录结构
//!
//! ```text
//! ~/.eko/workspaces/{id}/
//! └── .eko/           # 系统数据（默认隐藏）
//!     ├── workspace.json     # 清单文件
//!     ├── sessions/          # 会话历史
//!     ├── conversations/     # 对话记录（前端持久化）
//!     ├── memory/            # 向量存储、压缩历史
//!     ├── tasks/             # 后台任务文件状态
//!     ├── traces/            # 执行轨迹 JSONL
//!     ├── uploads/           # 上传临时文件
//!     ├── data/              # 数据集（数据分析工作区）
//!     ├── papers/            # PDF、参考文献、阅读笔记
//!     ├── artifacts/         # 生成的报告、论文、图表
//! ```

pub mod layout;
pub mod registry;
pub(crate) mod runtime;
pub mod templates;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ts_rs::TS;

// ── WorkspaceId ─────────────────────────────────────────────────────

/// 工作区标识符 — 目录安全名称。
///
/// 该标识符同时作为目录名，因此必须满足文件系统安全要求。
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, rename = "WorkspaceId")]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// 从任意名称创建安全的 WorkspaceId。
    ///
    /// 替换不安全字符为 `_`，截断至 64 字符。
    pub fn from_name(name: &str) -> Self {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .chars()
            .take(64)
            .collect();
        // 确保不为空
        let safe = if safe.is_empty() {
            "default".to_string()
        } else {
            safe
        };
        Self(safe)
    }

    /// 从已验证的字符串创建（不做清洗）。
    ///
    /// # Security
    /// Validates that the raw string doesn't contain path traversal sequences
    /// to prevent directory escape attacks.
    pub fn from_raw(raw: String) -> Self {
        // Reject path traversal attempts
        if raw.contains("..") || raw.contains('/') || raw.contains('\\') {
            // Sanitize by removing dangerous characters
            let sanitized: String = raw
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .take(64)
                .collect();
            tracing::warn!(
                original = %raw,
                sanitized = %sanitized,
                "WorkspaceId from_raw: sanitized potentially unsafe input"
            );
            Self(if sanitized.is_empty() {
                "default".to_string()
            } else {
                sanitized
            })
        } else {
            Self(raw.chars().take(64).collect())
        }
    }

    /// 作为路径段使用。
    pub fn as_path_segment(&self) -> &str {
        &self.0
    }

    /// 原始字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── WorkspaceExecutionScope ──────────────────────────────────

/// Immutable workspace identity and root carried by one execution path.
///
/// This is an application-layer scope: it selects EKO's per-workspace
/// runtime projection while the framework receives only the generic
/// per-invocation working directory.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceExecutionScope {
    workspace_id: String,
    root: PathBuf,
}

impl WorkspaceExecutionScope {
    /// Scope execution to the application-wide, non-workspace runtime.
    pub fn global(root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_id: "global".to_string(),
            root: root.into(),
        }
    }

    /// Scope execution to a registered workspace.
    pub fn workspace(workspace_id: &WorkspaceId, root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            root: root.into(),
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

// ── WorkspaceKind ───────────────────────────────────────────────────

/// 工作区类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum WorkspaceKind {
    /// 代码项目工作区。
    Code {
        #[serde(skip_serializing_if = "Option::is_none")]
        repo_url: Option<String>,
    },
    /// 数据分析工作区。
    DataAnalysis {
        #[serde(default)]
        datasets: Vec<String>,
    },
    /// 学术研究/论文工作区。
    Research {
        #[serde(default)]
        topics: Vec<String>,
    },
    /// 医学研究工作区。
    Medical {
        #[serde(default)]
        topics: Vec<String>,
    },
    /// 通用/其他工作区。
    #[default]
    General,
}

impl WorkspaceKind {
    /// 从字符串解析工作区类型。
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "code" | "coding" | "编程" | "代码" => Self::Code { repo_url: None },
            "data" | "data_analysis" | "数据" | "数据分析" => {
                Self::DataAnalysis { datasets: vec![] }
            }
            "research" | "研究" | "论文" | "academic" => Self::Research { topics: vec![] },
            "medical" | "medical_research" | "医学" | "医学研究" | "临床" => {
                Self::Medical { topics: vec![] }
            }
            _ => Self::General,
        }
    }

    /// 显示名称。
    pub fn display_name(&self) -> &str {
        match self {
            Self::Code { .. } => "Code",
            Self::DataAnalysis { .. } => "Data Analysis",
            Self::Research { .. } => "Research",
            Self::Medical { .. } => "Medical Research",
            Self::General => "Other",
        }
    }

    /// 图标。
    pub fn icon(&self) -> &str {
        match self {
            Self::Code { .. } => "💻",
            Self::DataAnalysis { .. } => "📊",
            Self::Research { .. } => "🔬",
            Self::Medical { .. } => "🩺",
            Self::General => "⋯",
        }
    }
}

// ── WorkspaceMetadata ───────────────────────────────────────────────

/// 工作区元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    /// 描述信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 标签列表。
    #[serde(default)]
    pub tags: Vec<String>,
    /// Monotonic incarnation of the linked project root.
    ///
    /// This stays in the existing workspace registry authority so product
    /// surfaces can reject drafts admitted before a project relink without a
    /// second generation store.
    #[serde(default)]
    pub project_root_revision: u64,
}

// ── Workspace ───────────────────────────────────────────────────────

/// 工作区 — 任务的主隔离单元。
///
/// 每个工作区对应一个独立的目录，包含该工作区的所有数据：
/// 会话、记忆、任务、论文、数据集等。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// 工作区标识符。
    pub id: WorkspaceId,
    /// 显示名称（用户友好的名称，可以与 id 不同）。
    pub name: String,
    /// 工作区根目录的绝对路径。
    pub root: PathBuf,
    /// 关联的代码项目目录（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
    /// 工作区类型。
    pub kind: WorkspaceKind,
    /// 元数据。
    #[serde(default)]
    pub metadata: WorkspaceMetadata,
    /// Backend-issued opaque incarnation token for product-data IPC.
    ///
    /// This is a derived projection. Consumers must return it verbatim and
    /// must not reconstruct it from localized timestamp strings.
    #[serde(default, skip_deserializing)]
    pub product_data_generation: String,
    /// 创建时间。
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub created_at: DateTime<Utc>,
    /// 最后活跃时间。
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub last_active: DateTime<Utc>,
}

impl Workspace {
    /// 更新最后活跃时间为当前时间。
    pub fn touch(&mut self) {
        self.last_active = Utc::now();
        self.refresh_product_data_generation();
    }

    pub fn refresh_product_data_generation(&mut self) {
        self.product_data_generation = self.opaque_product_data_generation();
    }

    pub fn opaque_product_data_generation(&self) -> String {
        let created_at = self
            .created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        serde_json::to_string(&(
            self.id.as_str(),
            created_at,
            self.metadata.project_root_revision,
        ))
        .unwrap_or_else(|error| {
            tracing::error!(%error, "failed to serialize workspace product-data generation");
            String::new()
        })
    }
}

/// Immutable EKO identity for one workspace host's product-data root.
///
/// Project relinking does not change this identity because research and
/// analysis data stay under the EKO workspace root. Deletion/recreation does:
/// `created_at` distinguishes a new host even when ID and path are reused.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct WorkspaceIoIdentity {
    workspace_id: String,
    host_generation: String,
    data_root: PathBuf,
}

impl WorkspaceIoIdentity {
    pub(crate) fn global(data_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_id: "global".to_string(),
            host_generation: "global".to_string(),
            data_root: data_root.into(),
        }
    }

    pub(crate) fn workspace(workspace: &Workspace) -> Self {
        Self {
            workspace_id: workspace.id.to_string(),
            host_generation: workspace
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            data_root: workspace.root.clone(),
        }
    }

    pub(crate) fn data_root(&self) -> &std::path::Path {
        &self.data_root
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub(crate) fn host_generation(&self) -> &str {
        &self.host_generation
    }
}

// ── ts-rs bindings ──────────────────────────────────────────────────

#[cfg(feature = "__ts_rs")]
mod ts_bindings {
    // ts-rs will auto-generate TypeScript bindings for the types above
    // when `cargo test` is run with the `__ts_rs` feature enabled.
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_id_from_name() {
        let id = WorkspaceId::from_name("my-project");
        assert_eq!(id.as_str(), "my-project");

        let id = WorkspaceId::from_name("hello world!");
        assert_eq!(id.as_str(), "hello_world_");

        let id = WorkspaceId::from_name("");
        assert_eq!(id.as_str(), "default");
    }

    #[test]
    fn test_workspace_id_truncation() {
        let long_name = "a".repeat(100);
        let id = WorkspaceId::from_name(&long_name);
        assert_eq!(id.as_str().len(), 64);
    }

    #[test]
    fn test_workspace_kind_from_str() {
        assert!(matches!(
            WorkspaceKind::from_str_loose("code"),
            WorkspaceKind::Code { .. }
        ));
        assert!(matches!(
            WorkspaceKind::from_str_loose("研究"),
            WorkspaceKind::Research { .. }
        ));
        assert!(matches!(
            WorkspaceKind::from_str_loose("unknown"),
            WorkspaceKind::General
        ));
    }

    #[test]
    fn test_workspace_kind_serialization() -> anyhow::Result<()> {
        let kind = WorkspaceKind::Code {
            repo_url: Some("https://github.com/test".into()),
        };
        let json = serde_json::to_string(&kind)?;
        assert!(json.contains("\"type\":\"code\""));
        Ok(())
    }

    #[test]
    fn test_workspace_serialization() -> anyhow::Result<()> {
        let ws = Workspace {
            id: WorkspaceId::from_name("test"),
            name: "Test Workspace".into(),
            root: PathBuf::from("/tmp/test"),
            project_root: None,
            kind: WorkspaceKind::General,
            metadata: WorkspaceMetadata::default(),
            product_data_generation: String::new(),
            created_at: Utc::now(),
            last_active: Utc::now(),
        };
        let json = serde_json::to_string_pretty(&ws)?;
        let parsed: Workspace = serde_json::from_str(&json)?;
        assert_eq!(parsed.id, ws.id);
        assert_eq!(parsed.name, ws.name);
        Ok(())
    }

    #[test]
    fn product_data_generation_is_timezone_independent_and_round_trips()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = |created_at: &str| {
            serde_json::json!({
                "id": "timezone",
                "name": "Timezone",
                "root": "/tmp/timezone",
                "kind": { "type": "general" },
                "metadata": { "tags": [], "project_root_revision": 7 },
                "product_data_generation": "forged-or-stale-token",
                "created_at": created_at,
                "last_active": created_at
            })
        };
        let mut utc: Workspace = serde_json::from_value(fixture("2026-08-24T00:00:00Z"))?;
        let mut local: Workspace = serde_json::from_value(fixture("2026-08-24T08:00:00+08:00"))?;
        assert!(utc.product_data_generation.is_empty());
        assert!(local.product_data_generation.is_empty());
        utc.refresh_product_data_generation();
        local.refresh_product_data_generation();

        assert_eq!(utc.product_data_generation, local.product_data_generation);
        let encoded = serde_json::to_value(&utc)?;
        assert_eq!(
            encoded
                .get("product_data_generation")
                .and_then(serde_json::Value::as_str),
            Some(utc.product_data_generation.as_str())
        );
        Ok(())
    }
}
