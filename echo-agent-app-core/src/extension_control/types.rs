// EKO extension control authority.
//
// Framework registries remain the execution authorities. This service owns
// EKO-specific workspace selection, durable enablement, mutation sequencing
// and surface-neutral receipts so GUI, TUI, CLI and channels cannot each
// invent a second lifecycle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::hook_config_loader::{HookConfigLoader, HooksLoadResult};
use crate::skills_hub::enabled_skills::{EnabledSkillsConfig, SkillEnableEntry};
use crate::skills_hub::{SkillHubEntry, SkillsHub};
use crate::state::{
    AppState, ExtensionRuntimeTargets, McpHealthStatus, ScopedChatRuntime, ScopedExtensionControl,
};

const USER_SKILL_SOURCE_PREFIX: &str = "eko:user-skill:";

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ExtensionSkillEntry")]
pub struct ExtensionSkillEntry {
    #[serde(flatten)]
    pub catalog: SkillHubEntry,
    pub loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionMcpTool {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionMcpServer {
    pub name: String,
    pub status: String,
    pub transport: String,
    pub tool_count: usize,
    pub tools: Vec<ExtensionMcpTool>,
    pub connected_at: Option<String>,
    pub error: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookSourceSnapshot {
    pub source: String,
    pub rules: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookReloadReceipt {
    pub loaded_from: Vec<PathBuf>,
    pub rule_count: usize,
}

#[derive(Debug, Clone)]
pub struct PluginCatalogSnapshot {
    pub authority_scope: String,
    pub plugins: Vec<echo_agent::plugin::PluginEntry>,
}

#[derive(Debug, Clone)]
pub struct PluginThemeSnapshot {
    pub authority_scope: String,
    pub active: Option<String>,
    pub themes: Vec<crate::plugin_runtime::PluginThemeDefinition>,
}

#[derive(Debug, Clone)]
pub struct PluginOutputStyleSnapshot {
    pub authority_scope: String,
    pub active: Option<String>,
    pub styles: Vec<crate::plugin_runtime::PluginOutputStyle>,
}

#[derive(Debug)]
pub struct PluginMutationReceipt {
    pub authority_scope: String,
    pub status: PluginSettlementStatus,
    pub plugin_id: Option<String>,
    pub entry: Option<echo_agent::plugin::PluginEntry>,
    pub summary: crate::plugin_runtime::ReloadSummary,
    pub target_receipts: Vec<PluginTargetGenerationReceipt>,
    pub theme: PluginThemeSnapshot,
    pub output_style: PluginOutputStyleSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginSettlementStatus {
    Settled,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginTargetSettlementStatus {
    Settled,
    Degraded,
}

/// Result of publishing one prepared plugin generation to one host captured at
/// mutation admission. Both generations are opaque framework identities; the
/// workspace generation independently fences delete/recreate ABA.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginTargetGenerationReceipt {
    pub target: String,
    pub workspace_generation: String,
    pub previous_prepared_generation: String,
    pub candidate_prepared_generation: Option<String>,
    pub status: PluginTargetSettlementStatus,
    pub diagnostics: Vec<String>,
}

#[derive(Debug)]
pub struct PluginPreferenceReceipt<T> {
    pub authority_scope: String,
    pub active: Option<String>,
    pub value: T,
}

/// Result of promoting one workspace-curated Skill through its durable and
/// runtime authorities. An `Active` curator record is the restart authority;
/// runtime publication failure is therefore degraded, never rolled back or
/// reported as a pre-commit failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CuratedSkillPublicationReceipt {
    pub name: String,
    pub active_path: PathBuf,
    pub durable_committed: bool,
    pub idempotent: bool,
    pub status: SkillSettlementStatus,
    pub loaded_entries: Vec<String>,
    pub runtime_error: Option<String>,
}

struct CuratedSkillArtifactCommit {
    active_path: PathBuf,
    load_root: PathBuf,
    idempotent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SkillSettlementStatus")]
pub enum SkillSettlementStatus {
    Settled,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SkillTargetSettlementStatus")]
pub enum SkillTargetSettlementStatus {
    Settled,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillTargetSettlementReceipt")]
pub struct SkillTargetSettlementReceipt {
    pub target: String,
    pub workspace_generation: String,
    pub status: SkillTargetSettlementStatus,
    pub changed_entries: Vec<String>,
    pub error: Option<String>,
}

/// Surface-neutral result of one durable skill policy mutation or repair pass.
///
/// 2026-09 简化(取代 ADR 0032 的 durable 状态机):不再携带
/// generation/content_identity/repair_debt 等崩溃恢复对账字段,只报告
/// 本次操作对各运行时目标的即时结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillSyncReceipt")]
pub struct SkillSyncReceipt {
    pub operation_id: String,
    /// 本次操作是否未改变任何状态(重复开关同一个 skill 等)。
    pub idempotent: bool,
    pub status: SkillSettlementStatus,
    pub target_receipts: Vec<SkillTargetSettlementReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillInstallSettlementReceipt")]
pub struct SkillInstallSettlementReceipt {
    pub name: String,
    pub installed_names: Vec<String>,
    pub path: PathBuf,
    pub source: String,
    pub revision: Option<String>,
    pub settlement: SkillSyncReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillArtifactSyncResult")]
pub struct SkillArtifactSyncResult {
    pub name: String,
    pub success: bool,
    pub updated: bool,
    pub revision: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillArtifactSyncReceipt")]
pub struct SkillArtifactSyncReceipt {
    pub results: Vec<SkillArtifactSyncResult>,
    pub settlement: SkillSyncReceipt,
}

impl SkillArtifactSyncReceipt {
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, SkillArtifactSyncResult> {
        self.results.iter()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillUninstallSettlementReceipt")]
pub struct SkillUninstallSettlementReceipt {
    pub name: String,
    pub artifact_removed: bool,
    pub artifact_error: Option<String>,
    pub settlement: SkillSyncReceipt,
}

impl SkillSyncReceipt {
    /// Compatibility projection for text surfaces that reported runtime entries.
    pub fn len(&self) -> usize {
        self.target_receipts
            .iter()
            .map(|receipt| receipt.changed_entries.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SkillMutationError {
    #[error("Skill mutation admission failed: {0}")]
    Admission(String),
    #[error("Skill mutation failed before durable commit: {0}")]
    BeforeCommit(String),
    #[error("Skill settlement task failed: {0}")]
    SettlementTask(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SkillInstallError {
    #[error("Skill install failed before publication: {0}")]
    Install(String),
    #[error("Skill was installed, but enablement failed before its durable policy commit: {0}")]
    Enable(SkillMutationError),
}

/// One application-owned mutation sequence for every extension surface.
pub struct ExtensionControlService {
    mutation: Mutex<()>,
    enabled_config_path: PathBuf,
}

async fn await_owned_extension_settlement<T, E, F, J>(
    flow: crate::product_data_io::ProductDataIoFlow,
    settlement: F,
    join_error: J,
) -> Result<T, E>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    J: FnOnce(String) -> E,
{
    let task = tokio::spawn(async move {
        let outcome = settlement.await;
        flow.settle(outcome.as_ref().err().map(ToString::to_string));
        outcome
    });
    match task.await {
        Ok(outcome) => outcome,
        Err(error) => Err(join_error(error.to_string())),
    }
}

impl Default for ExtensionControlService {
    fn default() -> Self {
        Self {
            mutation: Mutex::new(()),
            enabled_config_path: crate::data_root::user_data_path("enabled-skills.json"),
        }
    }
}
