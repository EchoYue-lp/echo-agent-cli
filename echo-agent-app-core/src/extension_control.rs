//! EKO extension control authority.
//!
//! Framework registries remain the execution authorities. This service owns
//! EKO-specific workspace selection, durable enablement, mutation sequencing
//! and surface-neutral receipts so GUI, TUI, CLI and channels cannot each
//! invent a second lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::hook_config_loader::{HookConfigLoader, HooksLoadResult};
use crate::skills_hub::enabled_skills::{
    EnabledSkillsConfig, SkillArtifactSyncDebt, SkillEnableEntry, SkillOperationIdentity,
    SkillRepairDebt, SkillRepairTargetDebt,
};
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

struct AdmittedSkillMutation {
    operation_id: String,
    command_identity: String,
    name: String,
    enabled: bool,
    artifact_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SkillSettlementStatus")]
pub enum SkillSettlementStatus {
    Committed,
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
    #[serde(with = "crate::skills_hub::enabled_skills::u64_string")]
    #[ts(type = "string")]
    pub specialist_generation: u64,
    pub status: SkillTargetSettlementStatus,
    pub changed_entries: Vec<String>,
    pub error: Option<String>,
}

/// Surface-neutral result of one durable skill policy mutation or repair pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillSyncReceipt")]
pub struct SkillSyncReceipt {
    pub operation_id: String,
    #[ts(type = "string")]
    pub committed_file_path: PathBuf,
    pub content_identity: String,
    #[serde(with = "crate::skills_hub::enabled_skills::u64_string")]
    #[ts(type = "string")]
    pub desired_generation: u64,
    #[serde(with = "crate::skills_hub::enabled_skills::u64_string")]
    #[ts(type = "string")]
    pub settled_generation: u64,
    pub durable_committed: bool,
    pub idempotent: bool,
    pub status: SkillSettlementStatus,
    pub target_receipts: Vec<SkillTargetSettlementReceipt>,
    pub repair_debt: Option<SkillRepairDebt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillInstallSettlementReceipt")]
pub struct SkillInstallSettlementReceipt {
    pub name: String,
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
    #[error(
        "Skill operation '{operation_id}' conflicts with committed content '{committed_content_identity}'"
    )]
    OperationConflict {
        operation_id: String,
        committed_content_identity: String,
    },
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

impl ExtensionControlService {
    #[cfg(test)]
    pub(crate) fn with_enabled_config_path(path: PathBuf) -> Self {
        Self {
            mutation: Mutex::new(()),
            enabled_config_path: path,
        }
    }

    async fn context(&self, state: &AppState) -> anyhow::Result<ScopedExtensionControl> {
        state
            .current_extension_control()
            .await
            .map_err(anyhow::Error::new)
    }

    async fn scoped_context(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<ScopedExtensionControl> {
        match runtime {
            Some(runtime) => state
                .extension_control_for_runtime(runtime)
                .await
                .map_err(anyhow::Error::new),
            None => self.context(state).await,
        }
    }

    pub async fn plugin_catalog(&self, state: &AppState) -> anyhow::Result<PluginCatalogSnapshot> {
        self.plugin_catalog_scoped(state, None).await
    }

    pub async fn plugin_catalog_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginCatalogSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let plugins = control.plugin_runtime().list().await;
        Ok(PluginCatalogSnapshot {
            authority_scope,
            plugins,
        })
    }

    pub async fn plugin_entry(
        &self,
        state: &AppState,
        name: &str,
    ) -> anyhow::Result<(String, Option<echo_agent::plugin::PluginEntry>)> {
        self.plugin_entry_scoped(state, None, name).await
    }

    pub async fn plugin_entry_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
    ) -> anyhow::Result<(String, Option<echo_agent::plugin::PluginEntry>)> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let entry = control.plugin_runtime().get(name).await;
        Ok((authority_scope, entry))
    }

    pub async fn plugin_themes(&self, state: &AppState) -> anyhow::Result<PluginThemeSnapshot> {
        self.plugin_themes_scoped(state, None).await
    }

    pub async fn plugin_themes_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginThemeSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let runtime = control.plugin_runtime();
        let active = runtime.active_theme().await;
        let themes = runtime.themes().await;
        Ok(PluginThemeSnapshot {
            authority_scope,
            active,
            themes,
        })
    }

    pub async fn plugin_output_styles(
        &self,
        state: &AppState,
    ) -> anyhow::Result<PluginOutputStyleSnapshot> {
        self.plugin_output_styles_scoped(state, None).await
    }

    pub async fn plugin_output_styles_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginOutputStyleSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let runtime = control.plugin_runtime();
        let active = runtime.active_output_style().await;
        let styles = runtime.output_styles().await;
        Ok(PluginOutputStyleSnapshot {
            authority_scope,
            active,
            styles,
        })
    }

    pub async fn rebind_plugin_runtime(
        &self,
        runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
        root: PathBuf,
    ) -> anyhow::Result<crate::plugin_runtime::ReloadSummary> {
        let _mutation = self.mutation.lock().await;
        runtime.rebind_workspace(root).await
    }

    pub async fn reload_plugin_lsp(
        &self,
        runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
        root: PathBuf,
    ) -> anyhow::Result<usize> {
        let _mutation = self.mutation.lock().await;
        runtime.reload_lsp_generation(root).await
    }

    pub async fn reload_plugins(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.reload_plugins_scoped(state, None).await
    }

    pub async fn reload_plugins_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("reload and settle plugins")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.reload().await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    None,
                    None,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin reload settlement task failed: {error}"),
        )
        .await
    }

    pub async fn install_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        source: &echo_agent::plugin::InstallSource,
        scope: echo_agent::plugin::PluginScope,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.install_plugin_scoped(state, None, source, scope).await
    }

    pub async fn install_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        source: &echo_agent::plugin::InstallSource,
        scope: echo_agent::plugin::PluginScope,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let source = source.clone();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("install and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let (plugin_id, mut summary) = authority.install(&source, scope).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&plugin_id).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(plugin_id),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin install settlement task failed: {error}"),
        )
        .await
    }

    pub async fn uninstall_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.uninstall_plugin_scoped(state, None, name, keep_data)
            .await
    }

    pub async fn uninstall_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("uninstall and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.uninstall(&name, keep_data).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    None,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin uninstall settlement task failed: {error}"),
        )
        .await
    }

    pub async fn set_plugin_enabled(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        enabled: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.set_plugin_enabled_scoped(state, None, name, enabled)
            .await
    }

    pub async fn set_plugin_enabled_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        enabled: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("toggle and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = if enabled {
                    authority.enable(&name).await?
                } else {
                    authority.disable(&name).await?
                };
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&name).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin toggle settlement task failed: {error}"),
        )
        .await
    }

    pub async fn configure_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.configure_plugin_scoped(state, None, name, values)
            .await
    }

    pub async fn configure_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("configure and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.configure(&name, values).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&name).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin configuration settlement task failed: {error}"),
        )
        .await
    }

    /// Admit scaffold writes into the shared Extension and ProductData
    /// lifecycle. Dropping the caller cannot abort an accepted settlement.
    pub async fn scaffold_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        directory: String,
        name: String,
    ) -> anyhow::Result<crate::plugin_runtime::PluginScaffoldResult> {
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("scaffold plugin artifact")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow.clone(),
            async move {
                let _mutation = service.mutation.lock().await;
                flow.run("write plugin scaffold", move || {
                    crate::plugin_runtime::PluginRuntimeService::scaffold(directory, &name)
                })
                .await
                .map_err(anyhow::Error::new)
                .and_then(|result| result)
            },
            |error| anyhow::anyhow!("Plugin scaffold settlement task failed: {error}"),
        )
        .await
    }

    /// Validate plugin artifacts through EKO's bounded filesystem I/O owner.
    pub async fn validate_plugin(
        &self,
        state: &AppState,
        directory: String,
    ) -> anyhow::Result<crate::plugin_runtime::PluginValidationReport> {
        let _read = self.mutation.lock().await;
        state
            .session
            .product_data_io
            .run("validate plugin artifact", move || {
                crate::plugin_runtime::PluginRuntimeService::validate(directory)
            })
            .await
            .map_err(anyhow::Error::new)
    }

    pub async fn activate_output_style(
        self: &Arc<Self>,
        state: &AppState,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<()>> {
        self.activate_output_style_scoped(state, None, name).await
    }

    pub async fn activate_output_style_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<()>> {
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        let name = name.map(str::to_string);
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("activate plugin output style")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                authority.activate_output_style(name.as_deref()).await?;
                Ok(PluginPreferenceReceipt {
                    authority_scope,
                    active: name,
                    value: (),
                })
            },
            |error| anyhow::anyhow!("Plugin output-style settlement task failed: {error}"),
        )
        .await
    }

    pub async fn activate_theme(
        self: &Arc<Self>,
        state: &AppState,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<Option<crate::plugin_runtime::PluginThemeDefinition>>>
    {
        self.activate_theme_scoped(state, None, name).await
    }

    pub async fn activate_theme_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<Option<crate::plugin_runtime::PluginThemeDefinition>>>
    {
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        let name = name.map(str::to_string);
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("activate plugin theme")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let value = authority.activate_theme(name.as_deref()).await?;
                Ok(PluginPreferenceReceipt {
                    authority_scope,
                    active: name,
                    value,
                })
            },
            |error| anyhow::anyhow!("Plugin theme settlement task failed: {error}"),
        )
        .await
    }

    pub async fn publish_curated_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        generation: crate::evolution::ReviewGenerationLease,
        name: &str,
    ) -> anyhow::Result<CuratedSkillPublicationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let authority = control.plugin_runtime();
        let agent = control.runtime().primary_agent();
        let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("promote and publish curated skill")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let outcome: anyhow::Result<CuratedSkillPublicationReceipt> = async {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let artifact_name = name.clone();
                let artifact = flow
                    .run("promote curated skill artifact", move || {
                        promote_curated_skill_artifact(echo_agent_dir, &artifact_name)
                    })
                    .await
                    .map_err(anyhow::Error::new)?
                    .map_err(anyhow::Error::msg)?;
                let runtime_publication = authority
                    .enable_application_skill(
                        name.clone(),
                        artifact.load_root,
                        format!("eko:curated-skill:{name}"),
                    )
                    .await;
                let receipt = match runtime_publication {
                    Ok(mut loaded_entries) => {
                        loaded_entries.sort();
                        loaded_entries.dedup();
                        crate::evolution::fire_evolution_hook(
                            &agent,
                            echo_agent::hooks::HookEvent::SkillLifecycleTransition,
                            &name,
                        )
                        .await;
                        CuratedSkillPublicationReceipt {
                            name,
                            active_path: artifact.active_path,
                            durable_committed: true,
                            idempotent: artifact.idempotent,
                            status: SkillSettlementStatus::Settled,
                            loaded_entries,
                            runtime_error: None,
                        }
                    }
                    Err(error) => CuratedSkillPublicationReceipt {
                        name,
                        active_path: artifact.active_path,
                        durable_committed: true,
                        idempotent: artifact.idempotent,
                        status: SkillSettlementStatus::Degraded,
                        loaded_entries: Vec::new(),
                        runtime_error: Some(error.to_string()),
                    },
                };
                let _generation = generation;
                Ok(receipt)
            }
            .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => receipt
                    .runtime_error
                    .clone()
                    .or_else(|| Some("curated Skill runtime publication degraded".to_string())),
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| {
            anyhow::anyhow!("Curated skill publication settlement task failed: {error}")
        })?
    }

    pub async fn replace_mcp_config(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        config: echo_agent::mcp::McpConfigFile,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("replace and settle MCP config")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state.replace_mcp_config_owned(&targets, config).await?;
                state.plugins.mcp_health.write().await.clear();
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn upsert_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: String,
        entry: echo_agent::mcp::McpServerEntry,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("upsert and settle MCP server")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .upsert_mcp_server_owned(&targets, name.clone(), entry)
                    .await?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn remove_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("remove and settle MCP server")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state.remove_mcp_server_owned(&targets, &name).await?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn list_skills(&self, state: &AppState) -> anyhow::Result<Vec<ExtensionSkillEntry>> {
        self.list_skills_scoped(state, None).await
    }

    pub async fn list_skills_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<ExtensionSkillEntry>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        let loaded = context
            .runtime()
            .primary_agent()
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .iter()
                    .map(|descriptor| descriptor.name.clone())
                    .collect::<std::collections::HashSet<_>>()
            })
            .await;
        let mut hub = state.skills_hub.write().await;
        hub.refresh();
        Ok(hub
            .list()
            .into_iter()
            .map(|entry| ExtensionSkillEntry {
                loaded: loaded.contains(&entry.name),
                catalog: entry.clone(),
            })
            .collect())
    }

    pub async fn enable_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.set_skill_enabled_with_operation(state, &uuid::Uuid::new_v4().to_string(), name, true)
            .await
    }

    pub async fn disable_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.set_skill_enabled_with_operation(state, &uuid::Uuid::new_v4().to_string(), name, false)
            .await
    }

    /// Admit one durable desired-state mutation. Once spawned, settlement is
    /// independent of the caller's future and is joined by ProductData shutdown.
    pub async fn set_skill_enabled_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        name: &str,
        enabled: bool,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle enabled skills mutation")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let operation_id = operation_id.to_string();
        let name = name.to_string();
        let command_identity = skill_toggle_command_identity(&name, enabled);
        let settlement_flow = flow;
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome = service
                .settle_skill_mutation_owned(
                    &state,
                    &settlement_flow,
                    AdmittedSkillMutation {
                        operation_id,
                        command_identity,
                        name,
                        enabled,
                        artifact_name: None,
                    },
                )
                .await;
            settlement_flow.settle(skill_business_failure(&outcome));
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    async fn settle_skill_mutation_owned(
        &self,
        state: &Arc<AppState>,
        flow: &crate::product_data_io::ProductDataIoFlow,
        mutation: AdmittedSkillMutation,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        let AdmittedSkillMutation {
            operation_id,
            command_identity,
            name,
            enabled,
            artifact_name,
        } = mutation;
        let _repair = self
            .reconcile_committed_skill_policy(state, flow, format!("repair-before-{operation_id}"))
            .await?;
        let skill_root = state.skills_hub.read().await.root().to_path_buf();
        let mut config = read_enabled_skills_config(flow, self.enabled_config_path.clone()).await?;
        normalize_skill_content_identity(flow, &mut config, skill_root.clone()).await?;
        let durable_config = config.clone();
        if let Some(committed) = config.operation(&operation_id)
            && !committed.command_identity.is_empty()
        {
            if committed.command_identity != command_identity {
                return Err(SkillMutationError::OperationConflict {
                    operation_id,
                    committed_content_identity: committed.content_identity.clone(),
                });
            }
            return self
                .reconcile_skill_config(
                    state,
                    flow,
                    durable_config,
                    operation_id,
                    true,
                    true,
                    Vec::new(),
                )
                .await;
        }
        let category = if !enabled {
            config.skills.get(&name).map(|entry| entry.category.clone())
        } else {
            None
        };
        let category = match category {
            Some(category) => category,
            None => skill_entry(state, &name)
                .await
                .map(|(_, category)| category)
                .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?,
        };
        match config.skills.get_mut(&name) {
            Some(entry) => entry.enabled = enabled,
            None => {
                config.skills.insert(
                    name.clone(),
                    SkillEnableEntry {
                        category,
                        enabled,
                        baseline: false,
                    },
                );
            }
        }
        let proposed_identity =
            compute_skill_content_identity(flow, config.skills.clone(), skill_root).await?;
        if let Some(committed) = config.operation(&operation_id) {
            if committed.content_identity != proposed_identity {
                return Err(SkillMutationError::OperationConflict {
                    operation_id,
                    committed_content_identity: committed.content_identity.clone(),
                });
            }
            return self
                .reconcile_skill_config(
                    state,
                    flow,
                    durable_config,
                    operation_id,
                    true,
                    true,
                    Vec::new(),
                )
                .await;
        }

        let same_content = proposed_identity == config.content_identity;
        if !same_content {
            config.desired_generation =
                config.desired_generation.checked_add(1).ok_or_else(|| {
                    SkillMutationError::BeforeCommit(
                        "enabled skill desired generation is exhausted".to_string(),
                    )
                })?;
            config.content_identity = proposed_identity.clone();
            config.set_repair_debt(SkillRepairDebt {
                generation: config.desired_generation,
                content_identity: proposed_identity.clone(),
                attempts: 0,
                target_failures: Vec::new(),
                artifact_removals: Vec::new(),
                artifact_syncs: Vec::new(),
                artifact_enablements: Vec::new(),
            });
        }
        config.record_operation(SkillOperationIdentity {
            operation_id: operation_id.clone(),
            command_identity,
            artifact_name,
            content_identity: proposed_identity,
            generation: config.desired_generation,
        });
        write_enabled_skills_config(flow, self.enabled_config_path.clone(), config.clone()).await?;
        self.reconcile_skill_config(
            state,
            flow,
            config,
            operation_id,
            same_content,
            true,
            Vec::new(),
        )
        .await
    }

    pub async fn refresh_enabled_skills(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.refresh_enabled_skills_with_operation(
            state,
            &format!("refresh-{}", uuid::Uuid::new_v4()),
        )
        .await
    }

    pub async fn refresh_enabled_skills_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("refresh enabled skills")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let operation_id = operation_id.to_string();
        let command_identity = skill_artifact_command_identity("refresh", "enabled-skills", false);
        let settlement_flow = flow;
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome = async {
                let duplicate = admitted_skill_operation(
                    &settlement_flow,
                    service.enabled_config_path.clone(),
                    &operation_id,
                    &command_identity,
                )
                .await?
                .is_some();
                let receipt = service
                    .reconcile_committed_skill_policy(
                        &state,
                        &settlement_flow,
                        operation_id.clone(),
                    )
                    .await?;
                if !duplicate {
                    record_skill_operation_identity(
                        &settlement_flow,
                        service.enabled_config_path.clone(),
                        &receipt,
                        operation_id,
                        command_identity,
                        None,
                    )
                    .await?;
                }
                Ok(receipt)
            }
            .await;
            settlement_flow.settle(skill_business_failure(&outcome));
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    /// Restart and workspace-load owners call the same reconciliation path as
    /// explicit refresh; repair debt never has a surface-specific replayer.
    pub async fn reconcile_enabled_skills_on_load(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.refresh_enabled_skills(state).await
    }

    async fn reconcile_committed_skill_policy(
        &self,
        state: &Arc<AppState>,
        flow: &crate::product_data_io::ProductDataIoFlow,
        operation_id: String,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        let skill_root = state.skills_hub.read().await.root().to_path_buf();
        let mut config = read_enabled_skills_config(flow, self.enabled_config_path.clone()).await?;
        let (artifact_changed, target_receipts, terminal_receipts) =
            replay_skill_artifact_debt(state, &mut config).await;
        let metadata_changed =
            normalize_skill_content_identity(flow, &mut config, skill_root).await?;
        if artifact_changed || metadata_changed {
            write_enabled_skills_config(flow, self.enabled_config_path.clone(), config.clone())
                .await?;
        }
        let mut receipt = self
            .reconcile_skill_config(
                state,
                flow,
                config,
                operation_id,
                true,
                true,
                target_receipts,
            )
            .await?;
        if !terminal_receipts.is_empty() {
            receipt.status = SkillSettlementStatus::Degraded;
            receipt.target_receipts.extend(terminal_receipts);
        }
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    async fn reconcile_skill_config(
        &self,
        state: &Arc<AppState>,
        flow: &crate::product_data_io::ProductDataIoFlow,
        config: EnabledSkillsConfig,
        operation_id: String,
        idempotent: bool,
        durable_committed: bool,
        mut target_receipts: Vec<SkillTargetSettlementReceipt>,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        let skill_root = state.skills_hub.read().await.root().to_path_buf();
        if !skill_commit_is_current(flow, self.enabled_config_path.clone(), &config).await? {
            target_receipts.push(stale_skill_generation_receipt(&config));
            return settle_skill_generation(
                flow,
                self.enabled_config_path.clone(),
                config,
                operation_id,
                idempotent,
                durable_committed,
                target_receipts,
            )
            .await;
        }
        let artifact_removals = config
            .repair_debt
            .as_ref()
            .map(|debt| debt.artifact_removals.clone())
            .unwrap_or_default();
        for name in artifact_removals {
            if config.skills.get(&name).is_some_and(|entry| entry.enabled) {
                continue;
            }
            let removal_root = skill_root.clone();
            let removal_name = name.clone();
            let removal = flow
                .run("repair disabled skill artifact", move || {
                    remove_skill_artifact(removal_root, &removal_name)
                })
                .await;
            let receipt = match removal {
                Ok(Ok(removed)) => SkillTargetSettlementReceipt {
                    target: format!("skill-artifact:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Settled,
                    changed_entries: removed.then_some(name).into_iter().collect(),
                    error: None,
                },
                Ok(Err(error)) => SkillTargetSettlementReceipt {
                    target: format!("skill-artifact:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(error),
                },
                Err(error) => SkillTargetSettlementReceipt {
                    target: format!("skill-artifact:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(error.to_string()),
                },
            };
            target_receipts.push(receipt);
        }
        let desired_config = config.clone();
        let desired_root = skill_root.clone();
        let desired = match flow
            .run("resolve enabled skill catalog", move || {
                desired_skill_entries(&desired_config, desired_root)
            })
            .await
        {
            Ok(desired) => desired,
            Err(error) => {
                target_receipts.push(SkillTargetSettlementReceipt {
                    target: "skill-catalog".to_string(),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(error.to_string()),
                });
                return settle_skill_generation(
                    flow,
                    self.enabled_config_path.clone(),
                    config,
                    operation_id,
                    idempotent,
                    durable_committed,
                    target_receipts,
                )
                .await;
            }
        };
        match state.extension_runtime_targets().await {
            Ok(targets) => {
                for target in targets.iter() {
                    if !skill_commit_is_current(flow, self.enabled_config_path.clone(), &config)
                        .await?
                    {
                        target_receipts.push(stale_skill_generation_receipt(&config));
                        break;
                    }
                    let workspace_generation = target.workspace_generation().to_string();
                    let receipt = match reconcile_target_skills(target, &desired, &skill_root).await
                    {
                        Ok(mut changed_entries) => {
                            changed_entries.sort();
                            changed_entries.dedup();
                            SkillTargetSettlementReceipt {
                                target: target.scope().to_string(),
                                workspace_generation: workspace_generation.clone(),
                                specialist_generation: config.desired_generation,
                                status: SkillTargetSettlementStatus::Settled,
                                changed_entries,
                                error: None,
                            }
                        }
                        Err(error) => SkillTargetSettlementReceipt {
                            target: target.scope().to_string(),
                            workspace_generation,
                            specialist_generation: config.desired_generation,
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(error.to_string()),
                        },
                    };
                    target_receipts.push(receipt);
                }
            }
            Err(error) => target_receipts.push(SkillTargetSettlementReceipt {
                target: "runtime-targets".to_string(),
                workspace_generation: "unknown".to_string(),
                specialist_generation: config.desired_generation,
                status: SkillTargetSettlementStatus::Degraded,
                changed_entries: Vec::new(),
                error: Some(error.to_string()),
            }),
        }
        settle_skill_generation(
            flow,
            self.enabled_config_path.clone(),
            config,
            operation_id,
            idempotent,
            durable_committed,
            target_receipts,
        )
        .await
    }

    /// Extension settlement is part of the application ProductData lifecycle;
    /// these methods deliberately do not create a second shutdown supervisor.
    pub fn begin_shutdown(&self, state: &AppState) -> Result<(), String> {
        state.session.product_data_io.begin_shutdown()
    }

    pub async fn join_shutdown(&self, state: &AppState) -> Result<(), String> {
        state.session.product_data_io.join_shutdown().await
    }

    pub async fn install_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        source: &str,
    ) -> Result<SkillInstallSettlementReceipt, SkillInstallError> {
        self.install_skill_with_operation(state, &uuid::Uuid::new_v4().to_string(), source)
            .await
    }

    pub async fn install_skill_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        source: &str,
    ) -> Result<SkillInstallSettlementReceipt, SkillInstallError> {
        if operation_id.trim().is_empty() {
            return Err(SkillInstallError::Enable(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            )));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("install and settle enabled skill")
            .map_err(|error| {
                SkillInstallError::Enable(SkillMutationError::Admission(error.to_string()))
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let source = source.to_string();
        let command_identity = skill_artifact_command_identity("install", &source, false);
        let operation_id = operation_id.to_string();
        let settlement_flow = flow;
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: Result<
                (crate::skills_hub::install::InstallResult, SkillSyncReceipt),
                SkillInstallError,
            > = async {
                let root = state.skills_hub.read().await.root().to_path_buf();
                if let Some(committed) = admitted_skill_operation(
                    &settlement_flow,
                    service.enabled_config_path.clone(),
                    &operation_id,
                    &command_identity,
                )
                .await
                .map_err(SkillInstallError::Enable)?
                {
                    let name = committed.artifact_name.ok_or_else(|| {
                        SkillInstallError::Enable(SkillMutationError::BeforeCommit(
                            "duplicate install identity has no artifact name; refusing to mutate"
                                .to_string(),
                        ))
                    })?;
                    let path = root.join(&name);
                    let revision = crate::skills_hub::install::read_source_record(&path)
                        .ok()
                        .flatten()
                        .map(|record| record.revision);
                    let installed = crate::skills_hub::install::InstallResult {
                        name,
                        path,
                        source: if source.starts_with("http://")
                            || source.starts_with("https://")
                            || source.ends_with(".git")
                        {
                            format!("git:{source}")
                        } else {
                            format!("local:{}", PathBuf::from(&source).display())
                        },
                        revision,
                    };
                    let receipt = service
                        .reconcile_committed_skill_policy(&state, &settlement_flow, operation_id)
                        .await
                        .map_err(SkillInstallError::Enable)?;
                    return Ok((installed, receipt));
                }
                let mut hub = SkillsHub::with_root(root);
                let installed = if source.starts_with("http://")
                    || source.starts_with("https://")
                    || source.ends_with(".git")
                {
                    crate::skills_hub::install::install_from_git(&source, None, &mut hub)
                        .await
                        .map_err(SkillInstallError::Install)?
                } else {
                    crate::skills_hub::install::install_from_local(
                        PathBuf::from(&source).as_path(),
                        &mut hub,
                    )
                    .map_err(SkillInstallError::Install)?
                };
                let artifact_name = installed.name.clone();
                let receipt = match service
                    .settle_skill_mutation_owned(
                        &state,
                        &settlement_flow,
                        AdmittedSkillMutation {
                            operation_id,
                            command_identity,
                            name: installed.name.clone(),
                            enabled: true,
                            artifact_name: Some(artifact_name),
                        },
                    )
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        record_install_repair_debt(
                            &state,
                            &settlement_flow,
                            service.enabled_config_path.clone(),
                            &installed.name,
                            &error.to_string(),
                        )
                        .await
                        .map_err(SkillInstallError::Enable)?;
                        return Err(SkillInstallError::Enable(error));
                    }
                };
                Ok((installed, receipt))
            }
            .await;
            let failure = match &outcome {
                Ok((_, receipt)) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some(format!(
                        "installed skill generation {} remains degraded",
                        receipt.desired_generation
                    ))
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            settlement_flow.settle(failure);
            outcome.map(|(installed, settlement)| SkillInstallSettlementReceipt {
                name: installed.name,
                path: installed.path,
                source: installed.source,
                revision: installed.revision,
                settlement,
            })
        })
        .await
        .map_err(|error| {
            SkillInstallError::Enable(SkillMutationError::SettlementTask(error.to_string()))
        })?
    }

    pub async fn uninstall_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillUninstallSettlementReceipt, SkillMutationError> {
        self.uninstall_skill_with_operation(state, &uuid::Uuid::new_v4().to_string(), name)
            .await
    }

    pub async fn uninstall_skill_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        name: &str,
    ) -> Result<SkillUninstallSettlementReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("uninstall and settle enabled skill")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let name = name.to_string();
        let command_identity = skill_artifact_command_identity("uninstall", &name, false);
        let operation_id = operation_id.to_string();
        let settlement_flow = flow;
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: Result<SkillUninstallSettlementReceipt, SkillMutationError> = async {
                if admitted_skill_operation(
                    &settlement_flow,
                    service.enabled_config_path.clone(),
                    &operation_id,
                    &command_identity,
                )
                .await?
                .is_some()
                {
                    let settlement = service
                        .reconcile_committed_skill_policy(&state, &settlement_flow, operation_id)
                        .await?;
                    return Ok(SkillUninstallSettlementReceipt {
                        name,
                        artifact_removed: false,
                        artifact_error: None,
                        settlement,
                    });
                }
                let artifact_name = name.clone();
                let mut settlement = service
                    .settle_skill_mutation_owned(
                        &state,
                        &settlement_flow,
                        AdmittedSkillMutation {
                            operation_id,
                            command_identity,
                            name: name.clone(),
                            enabled: false,
                            artifact_name: Some(artifact_name),
                        },
                    )
                    .await?;
                let root = state.skills_hub.read().await.root().to_path_buf();
                let uninstall_name = name.clone();
                let artifact = settlement_flow
                    .run("remove disabled skill artifact", move || {
                        remove_skill_artifact(root, &uninstall_name)
                    })
                    .await;
                let (artifact_removed, artifact_error) = match artifact {
                    Ok(Ok(removed)) => (removed, None),
                    Ok(Err(error)) => (false, Some(error)),
                    Err(error) => (false, Some(error.to_string())),
                };
                if let Some(error) = artifact_error.as_ref() {
                    settlement.status = SkillSettlementStatus::Degraded;
                    settlement
                        .target_receipts
                        .push(SkillTargetSettlementReceipt {
                            target: format!("skill-artifact:{name}"),
                            workspace_generation: "global".to_string(),
                            specialist_generation: settlement.desired_generation,
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(error.clone()),
                        });
                    match record_artifact_repair_debt(
                        &settlement_flow,
                        service.enabled_config_path.clone(),
                        &settlement,
                        &name,
                        error,
                    )
                    .await
                    {
                        Ok(debt) => settlement.repair_debt = Some(debt),
                        Err(debt_error) => {
                            settlement
                                .target_receipts
                                .push(SkillTargetSettlementReceipt {
                                    target: "enabled-skills.json".to_string(),
                                    workspace_generation: "global".to_string(),
                                    specialist_generation: settlement.desired_generation,
                                    status: SkillTargetSettlementStatus::Degraded,
                                    changed_entries: Vec::new(),
                                    error: Some(format!(
                                        "artifact repair debt commit failed: {debt_error}"
                                    )),
                                });
                        }
                    }
                }
                Ok(SkillUninstallSettlementReceipt {
                    name,
                    artifact_removed,
                    artifact_error,
                    settlement,
                })
            }
            .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.settlement.status == SkillSettlementStatus::Degraded => {
                    Some(format!(
                        "uninstalled skill generation {} remains degraded",
                        receipt.settlement.desired_generation
                    ))
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            settlement_flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    pub async fn sync_skills(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        target: Option<&str>,
        force: bool,
    ) -> anyhow::Result<SkillArtifactSyncReceipt> {
        self.sync_skills_with_operation(
            state,
            &format!("sync-{}", uuid::Uuid::new_v4()),
            target,
            force,
        )
        .await
    }

    pub async fn sync_skills_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        target: Option<&str>,
        force: bool,
    ) -> anyhow::Result<SkillArtifactSyncReceipt> {
        if operation_id.trim().is_empty() {
            anyhow::bail!("operation_id must not be empty");
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("sync and settle enabled skills")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let target = target.map(str::to_string);
        let operation_id = operation_id.to_string();
        let command_identity =
            skill_artifact_command_identity("sync", target.as_deref().unwrap_or("*"), force);
        let settlement_flow = flow;
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: anyhow::Result<(
                Vec<crate::skills_hub::install::SkillSyncResult>,
                SkillSyncReceipt,
            )> = async {
                let duplicate = admitted_skill_operation(
                    &settlement_flow,
                    service.enabled_config_path.clone(),
                    &operation_id,
                    &command_identity,
                )
                .await
                .map_err(anyhow::Error::new)?
                .is_some();
                if duplicate {
                    let receipt = service
                        .reconcile_committed_skill_policy(&state, &settlement_flow, operation_id)
                        .await
                        .map_err(anyhow::Error::new)?;
                    return Ok((Vec::new(), receipt));
                }
                let root = state.skills_hub.read().await.root().to_path_buf();
                let mut hub = SkillsHub::with_root(root);
                let results = crate::skills_hub::sync_skills(&mut hub, target.as_deref(), force)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let mut receipt = service
                    .reconcile_committed_skill_policy(
                        &state,
                        &settlement_flow,
                        operation_id.clone(),
                    )
                    .await
                    .map_err(anyhow::Error::new)?;
                let failures = results
                    .iter()
                    .filter(|result| !result.success)
                    .collect::<Vec<_>>();
                if !failures.is_empty() {
                    receipt.status = SkillSettlementStatus::Degraded;
                    receipt
                        .target_receipts
                        .extend(failures.iter().map(|result| SkillTargetSettlementReceipt {
                            target: format!("skill-artifact-sync:{}", result.name),
                            workspace_generation: "global".to_string(),
                            specialist_generation: receipt.desired_generation,
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(result.message.clone()),
                        }));
                    let retryable_failures = failures
                        .iter()
                        .filter(|result| result.retryable)
                        .map(|result| (result.name.clone(), result.message.clone()))
                        .collect::<Vec<_>>();
                    if !retryable_failures.is_empty() {
                        match record_artifact_sync_repair_debt(
                            &settlement_flow,
                            service.enabled_config_path.clone(),
                            &receipt,
                            &retryable_failures,
                            force,
                        )
                        .await
                        {
                            Ok(debt) => receipt.repair_debt = Some(debt),
                            Err(error) => {
                                receipt.target_receipts.push(SkillTargetSettlementReceipt {
                                    target: "enabled-skills.json".to_string(),
                                    workspace_generation: "global".to_string(),
                                    specialist_generation: receipt.desired_generation,
                                    status: SkillTargetSettlementStatus::Degraded,
                                    changed_entries: Vec::new(),
                                    error: Some(format!(
                                        "artifact sync repair debt commit failed: {error}"
                                    )),
                                });
                            }
                        }
                    }
                }
                record_skill_operation_identity(
                    &settlement_flow,
                    service.enabled_config_path.clone(),
                    &receipt,
                    operation_id,
                    command_identity,
                    None,
                )
                .await
                .map_err(anyhow::Error::new)?;
                Ok((results, receipt))
            }
            .await;
            let failure = match &outcome {
                Ok((_, receipt)) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some(format!(
                        "synced skill generation {} remains degraded",
                        receipt.desired_generation
                    ))
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            settlement_flow.settle(failure);
            outcome.map(|(results, settlement)| SkillArtifactSyncReceipt {
                results: results
                    .into_iter()
                    .map(|result| SkillArtifactSyncResult {
                        name: result.name,
                        success: result.success,
                        updated: result.updated,
                        revision: result.revision,
                        message: result.message,
                    })
                    .collect(),
                settlement,
            })
        })
        .await
        .map_err(|error| anyhow::anyhow!("Skill sync settlement task failed: {error}"))?
    }

    pub async fn list_hooks(&self, state: &AppState) -> anyhow::Result<Vec<HookSourceSnapshot>> {
        self.list_hooks_scoped(state, None).await
    }

    pub async fn list_hooks_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<HookSourceSnapshot>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        Ok(context
            .runtime()
            .primary_agent()
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .hook_registry()
                        .read()
                        .await
                        .list_sources()
                        .into_iter()
                        .map(|(source, rules)| HookSourceSnapshot { source, rules })
                        .collect()
                })
            })
            .await)
    }

    pub async fn reload_hooks(
        self: &Arc<Self>,
        state: &AppState,
    ) -> anyhow::Result<HookReloadReceipt> {
        self.reload_hooks_scoped(state, None).await
    }

    pub async fn reload_hooks_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<HookReloadReceipt> {
        let context = self.scoped_context(state, runtime).await?;
        let config_path = state
            .config_watcher
            .as_ref()
            .and_then(|watcher| watcher.config_path())
            .unwrap_or_else(|| state.config.config_path.clone());
        let project_root = context.project_root().to_path_buf();
        let agent = context.runtime().primary_agent();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("reload and settle hooks")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = context;
                service
                    .reload_hooks_target_locked(Some(config_path), project_root, agent, true)
                    .await
            },
            |error| anyhow::anyhow!("Hook reload settlement task failed: {error}"),
        )
        .await
    }

    pub(crate) async fn reload_hooks_for_target(
        &self,
        config_path: Option<PathBuf>,
        project_root: PathBuf,
        agent: crate::agent_handle::AgentHandle,
        preserve_on_error: bool,
    ) -> anyhow::Result<HookReloadReceipt> {
        let _mutation = self.mutation.lock().await;
        self.reload_hooks_target_locked(config_path, project_root, agent, preserve_on_error)
            .await
    }

    async fn reload_hooks_target_locked(
        &self,
        config_path: Option<PathBuf>,
        project_root: PathBuf,
        agent: crate::agent_handle::AgentHandle,
        preserve_on_error: bool,
    ) -> anyhow::Result<HookReloadReceipt> {
        let load_config = config_path.clone();
        let load_root = project_root.clone();
        let mut loaded = tokio::task::spawn_blocking(move || {
            HookConfigLoader::load_merged_from_disk_for_workspace(
                load_config.as_deref(),
                Some(load_root.as_path()),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Hook loader task failed: {error}"))?;
        let mut degraded_errors = Vec::new();
        if !loaded.errors.is_empty() && !preserve_on_error {
            degraded_errors = std::mem::take(&mut loaded.errors);
            let fallback_config = config_path;
            let fallback = tokio::task::spawn_blocking(move || {
                HookConfigLoader::load_merged_from_disk_for_workspace(
                    fallback_config.as_deref(),
                    None,
                )
            })
            .await
            .map_err(|error| anyhow::anyhow!("Hook fallback loader task failed: {error}"))?;
            if fallback.errors.is_empty() {
                loaded.definition = fallback.definition;
                loaded.loaded_from = fallback.loaded_from;
            } else {
                loaded.definition = Default::default();
                degraded_errors.extend(fallback.errors);
            }
        }
        ensure_hook_load_succeeded(&loaded)?;
        let rule_count = loaded.definition.rules.values().map(Vec::len).sum();
        let definition = loaded.definition;
        agent
            .write_async(|agent| {
                Box::pin(async move {
                    let mut registry = agent.hook_registry().write().await;
                    registry.clear_user_hooks();
                    if !definition.is_empty() {
                        registry.register_user_hooks(definition);
                    }
                })
            })
            .await;
        let receipt = HookReloadReceipt {
            loaded_from: loaded.loaded_from,
            rule_count,
        };
        if degraded_errors.is_empty() {
            Ok(receipt)
        } else {
            Err(anyhow::anyhow!(degraded_errors.join("; ")))
        }
    }

    pub async fn list_mcp_servers(
        &self,
        state: &AppState,
    ) -> anyhow::Result<Vec<ExtensionMcpServer>> {
        self.list_mcp_servers_scoped(state, None).await
    }

    pub async fn list_mcp_servers_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<ExtensionMcpServer>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        let scope = mcp_health_scope_key(context.runtime())?;
        let config = state.plugins.mcp_config.snapshot().await;
        let health = state
            .plugins
            .mcp_health
            .read()
            .await
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        let agent = context.runtime().primary_agent();
        let mut connected = agent
            .read(|agent| agent.list_mcp_servers().into_iter().collect::<Vec<_>>())
            .await;
        connected.sort();
        let mut names = connected.clone();
        names.extend(config.mcp_servers.keys().cloned());
        names.sort();
        names.dedup();
        let mut servers = Vec::with_capacity(names.len());
        for name in names {
            let configured = config.mcp_servers.get(&name);
            let is_connected = connected.contains(&name);
            let health_entry = health.get(&name);
            let tools = agent
                .read(|agent| {
                    agent
                        .mcp_client(&name)
                        .map(|client| {
                            client
                                .tools()
                                .iter()
                                .map(|tool| ExtensionMcpTool {
                                    name: tool.name.clone(),
                                    description: tool.description.clone().unwrap_or_default(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .await;
            let tool_count = tools.len();
            let status = if configured.is_some_and(|entry| entry.disabled) {
                "disabled"
            } else if health_entry.is_some_and(|entry| !entry.healthy) {
                "error"
            } else if is_connected {
                "connected"
            } else {
                "disconnected"
            };
            servers.push(ExtensionMcpServer {
                name,
                status: status.to_string(),
                transport: configured
                    .map(mcp_transport)
                    .unwrap_or("plugin")
                    .to_string(),
                tool_count,
                tools,
                connected_at: None,
                error: health_entry.and_then(|entry| entry.error.clone()),
                enabled: configured.is_none_or(|entry| !entry.disabled),
            });
        }
        Ok(servers)
    }

    pub async fn connect_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> anyhow::Result<u64> {
        let targets = state.extension_runtime_targets().await?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("connect and settle MCP server")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .set_mcp_server_enabled_owned(&targets, &name, true)
                    .await
                    .map_err(anyhow::Error::new)?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            |error| anyhow::anyhow!("MCP connect settlement task failed: {error}"),
        )
        .await
    }

    pub async fn disconnect_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> anyhow::Result<u64> {
        let targets = state.extension_runtime_targets().await?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("disconnect and settle MCP server")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .set_mcp_server_enabled_owned(&targets, &name, false)
                    .await
                    .map_err(anyhow::Error::new)?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            |error| anyhow::anyhow!("MCP disconnect settlement task failed: {error}"),
        )
        .await
    }

    async fn clear_mcp_health_for_server(&self, state: &AppState, name: &str) {
        let mut health = state.plugins.mcp_health.write().await;
        for scoped in health.values_mut() {
            scoped.remove(name);
        }
    }

    pub async fn refresh_current_mcp_health(&self, state: &AppState) -> anyhow::Result<()> {
        let _mutation = self.mutation.lock().await;
        let context = self.context(state).await?;
        let scope = mcp_health_scope_key(context.runtime())?;
        let agent = context.runtime().primary_agent();
        let names = agent
            .read(|agent| agent.list_mcp_servers().into_iter().collect::<Vec<_>>())
            .await;
        let now = chrono::Utc::now();
        let mut scoped = HashMap::new();
        for name in names {
            let healthy = agent.read(|agent| agent.mcp_client(&name).is_some()).await;
            scoped.insert(
                name.clone(),
                McpHealthStatus {
                    name,
                    healthy,
                    last_check: Some(now),
                    error: (!healthy).then(|| "MCP client is unavailable".to_string()),
                },
            );
        }
        state.plugins.mcp_health.write().await.insert(scope, scoped);
        Ok(())
    }

    pub async fn lsp_command(
        self: &Arc<Self>,
        state: &AppState,
        action: &str,
        language: Option<&str>,
    ) -> anyhow::Result<String> {
        self.lsp_command_scoped(state, None, action, language).await
    }

    pub async fn lsp_command_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        action: &str,
        language: Option<&str>,
    ) -> anyhow::Result<String> {
        let context = self.scoped_context(state, runtime).await?;
        let specialist = context.plugin_runtime();
        match action {
            "list" | "ls" => {
                let _read = self.mutation.lock().await;
                let languages = specialist.lsp_configured_languages().await;
                Ok(if languages.is_empty() {
                    "No language servers are configured.".to_string()
                } else {
                    languages.join("\n")
                })
            }
            "status" | "" => {
                let _read = self.mutation.lock().await;
                let statuses = specialist.lsp_status().await;
                Ok(if statuses.is_empty() {
                    "No language servers are configured.".to_string()
                } else {
                    statuses
                        .into_iter()
                        .map(|status| {
                            let state = if status.running && status.initialized {
                                "ready"
                            } else if status.running {
                                "starting"
                            } else {
                                "stopped"
                            };
                            status.last_error.map_or_else(
                                || format!("{}: {state}", status.language),
                                |error| format!("{}: {state} ({error})", status.language),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            "start" | "stop" | "restart" => {
                let language = language
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("lsp {action} requires a language"))?
                    .to_string();
                let action = action.to_string();
                let flow = state
                    .session
                    .product_data_io
                    .begin_owned_flow("settle LSP control mutation")
                    .map_err(anyhow::Error::new)?;
                let service = Arc::clone(self);
                await_owned_extension_settlement(
                    flow,
                    async move {
                        let _mutation = service.mutation.lock().await;
                        let _control = context;
                        if action == "start" {
                            specialist.lsp_start(language.clone()).await?;
                        } else if action == "stop" {
                            specialist.lsp_stop(language.clone()).await?;
                        } else {
                            specialist.lsp_restart(language.clone()).await?;
                        }
                        Ok(format!("Language server '{language}' {action}ed."))
                    },
                    |error| anyhow::anyhow!("LSP settlement task failed: {error}"),
                )
                .await
            }
            _ => anyhow::bail!("usage: lsp <list|status|start|stop|restart> [language]"),
        }
    }

    pub async fn browser_command(
        self: &Arc<Self>,
        state: &AppState,
        conversation_id: &str,
        args: &[&str],
    ) -> anyhow::Result<String> {
        self.browser_command_scoped(state, None, conversation_id, args)
            .await
    }

    pub async fn browser_command_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        conversation_id: &str,
        args: &[&str],
    ) -> anyhow::Result<String> {
        let action = args.first().copied().unwrap_or("status");
        match action {
            "status" => {
                let status = self.browser_status_scoped(state, runtime).await?;
                Ok(format!(
                    "Browser extension: {}; token: {}",
                    if status.connected {
                        "connected"
                    } else {
                        "disconnected"
                    },
                    if status.token_configured {
                        "configured"
                    } else {
                        "missing"
                    }
                ))
            }
            "stop" => {
                self.browser_stop_scoped(state, runtime).await?;
                Ok("Browser stop completed.".to_string())
            }
            _ => {
                let (browser_action, parameters) = browser_specialist_action(action, args)?;
                self.execute_browser_action_scoped(
                    state,
                    runtime,
                    conversation_id,
                    browser_action,
                    parameters,
                )
                .await?;
                Ok(format!("Browser {action} completed."))
            }
        }
    }

    pub async fn browser_status_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<crate::browser::BrowserExtensionStatus> {
        let _context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        Ok(browser.extension_status().await)
    }

    pub async fn browser_stop_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<()> {
        let _context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle browser stop")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = _context;
                browser.interrupt().await;
                Ok(())
            },
            |error| anyhow::anyhow!("Browser stop settlement task failed: {error}"),
        )
        .await
    }

    pub async fn execute_browser_action_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        conversation_id: &str,
        browser_action: crate::browser::BrowserAction,
        parameters: echo_agent::prelude::ToolParameters,
    ) -> anyhow::Result<()> {
        let context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        let workspace_id = context
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let workspace_root = context.runtime().execution_scope().root().to_path_buf();
        let conversation_id = conversation_id.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle browser action")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = context;
                browser
                    .execute_main(
                        workspace_id,
                        workspace_root,
                        conversation_id,
                        browser_action,
                        parameters,
                        None,
                    )
                    .await?;
                Ok(())
            },
            |error| anyhow::anyhow!("Browser action settlement task failed: {error}"),
        )
        .await
    }
}

fn browser_specialist_action(
    action: &str,
    args: &[&str],
) -> anyhow::Result<(
    crate::browser::BrowserAction,
    echo_agent::prelude::ToolParameters,
)> {
    match action {
        "managed" | "chrome" => Ok((
            crate::browser::BrowserAction::Backend,
            HashMap::from([(
                "backend".to_string(),
                serde_json::Value::String(action.to_string()),
            )]),
        )),
        "navigate" => {
            let url = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("Usage: /browser navigate <url>"))?;
            Ok((
                crate::browser::BrowserAction::Navigate,
                HashMap::from([(
                    "url".to_string(),
                    serde_json::Value::String((*url).to_string()),
                )]),
            ))
        }
        "back" => Ok((crate::browser::BrowserAction::Back, HashMap::new())),
        "reload" => Ok((crate::browser::BrowserAction::Reload, HashMap::new())),
        "screenshot" => Ok((crate::browser::BrowserAction::Screenshot, HashMap::new())),
        "click" => Ok((
            crate::browser::BrowserAction::ClickAt,
            HashMap::from([
                (
                    "x".to_string(),
                    serde_json::json!(browser_number(args, 1, "x")?),
                ),
                (
                    "y".to_string(),
                    serde_json::json!(browser_number(args, 2, "y")?),
                ),
                (
                    "effect".to_string(),
                    serde_json::Value::String("none".to_string()),
                ),
            ]),
        )),
        "scroll" => Ok((
            crate::browser::BrowserAction::Scroll,
            HashMap::from([
                (
                    "deltaX".to_string(),
                    serde_json::json!(browser_number(args, 1, "delta-x")?),
                ),
                (
                    "deltaY".to_string(),
                    serde_json::json!(browser_number(args, 2, "delta-y")?),
                ),
            ]),
        )),
        "tabs" => {
            let tab_action = args.get(1).copied().unwrap_or("list");
            let mut parameters = HashMap::from([(
                "action".to_string(),
                serde_json::Value::String(tab_action.to_string()),
            )]);
            match tab_action {
                "list" => {}
                "select" | "close" => {
                    let index = args
                        .get(2)
                        .ok_or_else(|| anyhow::anyhow!("browser tabs {tab_action} requires index"))?
                        .parse::<u64>()
                        .map_err(|error| anyhow::anyhow!("invalid browser tab index: {error}"))?;
                    parameters.insert("index".to_string(), serde_json::Value::Number(index.into()));
                }
                "new" => {
                    if let Some(url) = args.get(2).filter(|value| !value.trim().is_empty()) {
                        parameters.insert(
                            "url".to_string(),
                            serde_json::Value::String((*url).to_string()),
                        );
                    }
                }
                _ => anyhow::bail!("browser tabs action must be list, select, new, or close"),
            }
            Ok((crate::browser::BrowserAction::Tabs, parameters))
        }
        _ => anyhow::bail!(
            "Usage: /browser [status|managed|chrome|navigate <url>|back|reload|screenshot|click <x> <y>|scroll <delta-x> <delta-y>|tabs <action>|stop]"
        ),
    }
}

fn browser_number(args: &[&str], index: usize, name: &str) -> anyhow::Result<f64> {
    args.get(index)
        .ok_or_else(|| anyhow::anyhow!("browser {name} is required"))?
        .parse::<f64>()
        .map_err(|error| anyhow::anyhow!("invalid browser {name}: {error}"))
}

async fn plugin_mutation_receipt(
    authority_scope: String,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
    plugin_id: Option<String>,
    entry: Option<echo_agent::plugin::PluginEntry>,
    summary: crate::plugin_runtime::ReloadSummary,
    target_receipts: Vec<PluginTargetGenerationReceipt>,
) -> PluginMutationReceipt {
    let active_theme = authority.active_theme().await;
    let themes = authority.themes().await;
    let active_output_style = authority.active_output_style().await;
    let styles = authority.output_styles().await;
    let status = if !summary.errors.is_empty()
        || target_receipts
            .iter()
            .any(|target| target.status == PluginTargetSettlementStatus::Degraded)
    {
        PluginSettlementStatus::Degraded
    } else {
        PluginSettlementStatus::Settled
    };
    PluginMutationReceipt {
        theme: PluginThemeSnapshot {
            authority_scope: authority_scope.clone(),
            active: active_theme,
            themes,
        },
        output_style: PluginOutputStyleSnapshot {
            authority_scope: authority_scope.clone(),
            active: active_output_style,
            styles,
        },
        authority_scope,
        status,
        plugin_id,
        entry,
        summary,
        target_receipts,
    }
}

fn captured_targets_include_authority(
    targets: &ExtensionRuntimeTargets,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
) -> bool {
    targets
        .iter()
        .any(|target| Arc::ptr_eq(&target.plugin_runtime(), authority))
}

async fn settle_captured_plugin_targets(
    targets: &ExtensionRuntimeTargets,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
    summary: &mut crate::plugin_runtime::ReloadSummary,
) -> Vec<PluginTargetGenerationReceipt> {
    let mut receipts = Vec::new();
    for target in targets.iter() {
        let runtime = target.plugin_runtime();
        let is_authority = Arc::ptr_eq(authority, &runtime);
        let settlement = if is_authority {
            Ok(summary.errors.clone())
        } else {
            runtime.reload().await.map(|follower| follower.errors)
        };
        let previous = target.prepared_generation_identity().to_string();
        let candidate = runtime.prepared_generation_identity().await;
        let (committed, diagnostics) = match settlement {
            Ok(diagnostics) => (true, diagnostics),
            Err(error) => (false, vec![error.to_string()]),
        };
        let status = if committed && diagnostics.is_empty() {
            PluginTargetSettlementStatus::Settled
        } else {
            if !is_authority {
                summary.errors.extend(
                    diagnostics
                        .iter()
                        .map(|error| format!("plugin host {}: {error}", target.scope())),
                );
            }
            PluginTargetSettlementStatus::Degraded
        };
        receipts.push(PluginTargetGenerationReceipt {
            target: target.scope().to_string(),
            workspace_generation: target.workspace_generation().to_string(),
            previous_prepared_generation: previous,
            candidate_prepared_generation: Some(candidate),
            status,
            diagnostics,
        });
    }
    receipts
}

fn promote_curated_skill_artifact(
    echo_agent_dir: PathBuf,
    name: &str,
) -> Result<CuratedSkillArtifactCommit, String> {
    let name_path = std::path::Path::new(name);
    if name.trim().is_empty()
        || name_path
            .file_name()
            .is_none_or(|component| component != std::ffi::OsStr::new(name))
    {
        return Err("curated Skill name must be one non-empty path component".to_string());
    }

    let curator = crate::evolution::workspace_curator(&echo_agent_dir);
    let state = curator.load_state().map_err(|error| error.to_string())?;
    let lifecycle = state
        .skills
        .get(name)
        .map(|metadata| metadata.lifecycle)
        .ok_or_else(|| format!("Skill '{name}' was not found in curator state"))?;
    let draft_path = echo_agent_dir
        .join("skills")
        .join("_drafts")
        .join(name)
        .join("SKILL.md");
    let active_path = echo_agent_dir.join("skills").join(name).join("SKILL.md");
    let load_root = echo_agent_dir.join("skills");

    if lifecycle == echo_agent::evolution::SkillLifecycle::Active {
        if !active_path.is_file() {
            return Err(format!(
                "Skill '{name}' is Active but its artifact is missing at {}",
                active_path.display()
            ));
        }
        return Ok(CuratedSkillArtifactCommit {
            active_path,
            load_root,
            idempotent: true,
        });
    }
    if lifecycle != echo_agent::evolution::SkillLifecycle::Draft {
        return Err(format!(
            "Skill '{name}' is in {lifecycle:?} state and cannot be promoted"
        ));
    }

    let draft = std::fs::read(&draft_path).map_err(|error| {
        format!(
            "failed to read curated Skill draft '{}': {error}",
            draft_path.display()
        )
    })?;
    let wrote_artifact = match std::fs::read(&active_path) {
        Ok(existing) if existing == draft => false,
        Ok(_) => {
            return Err(format!(
                "refusing to overwrite a different active Skill artifact at {}",
                active_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            echo_agent::utils::fs::atomic_write(&active_path, &draft).map_err(|error| {
                format!(
                    "failed to commit curated Skill artifact '{}': {error}",
                    active_path.display()
                )
            })?;
            true
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect active Skill artifact '{}': {error}",
                active_path.display()
            ));
        }
    };

    match curator.promote_to_active_at(name, Some(&active_path)) {
        Ok(true) => Ok(CuratedSkillArtifactCommit {
            active_path,
            load_root,
            idempotent: false,
        }),
        Ok(false) => {
            let concurrently_active = curator
                .load_state()
                .ok()
                .and_then(|state| state.skills.get(name).cloned())
                .is_some_and(|metadata| {
                    metadata.lifecycle == echo_agent::evolution::SkillLifecycle::Active
                });
            if concurrently_active && active_path.is_file() {
                return Ok(CuratedSkillArtifactCommit {
                    active_path,
                    load_root,
                    idempotent: true,
                });
            }
            if wrote_artifact {
                let _ = echo_agent::utils::fs::remove_file_durable(&active_path);
            }
            Err(format!("Skill '{name}' is no longer in Draft state"))
        }
        Err(error) => {
            let cleanup_error = if wrote_artifact {
                echo_agent::utils::fs::remove_file_durable(&active_path)
                    .err()
                    .map(|cleanup| cleanup.to_string())
            } else {
                None
            };
            Err(match cleanup_error {
                Some(cleanup) => format!(
                    "failed to promote Skill '{name}': {error}; artifact cleanup failed: {cleanup}"
                ),
                None => format!("failed to promote Skill '{name}': {error}"),
            })
        }
    }
}

fn user_skill_source(name: &str) -> String {
    format!("{USER_SKILL_SOURCE_PREFIX}{name}")
}

#[cfg(test)]
async fn skill_source_present(
    agent: &crate::agent_handle::AgentHandle,
    name: &str,
    source: &str,
) -> bool {
    agent
        .read(|agent| {
            agent.skill_descriptors().iter().any(|descriptor| {
                descriptor.name == name && descriptor.source.as_deref() == Some(source)
            })
        })
        .await
}

async fn skill_entry(state: &AppState, name: &str) -> anyhow::Result<(PathBuf, String)> {
    let mut hub = state.skills_hub.write().await;
    hub.refresh();
    let entry = hub
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Skill '{name}' not found"))?;
    let load_root = entry
        .path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| entry.path.clone());
    Ok((load_root, entry.category.clone()))
}

fn skill_business_failure(
    outcome: &Result<SkillSyncReceipt, SkillMutationError>,
) -> Option<String> {
    match outcome {
        Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => Some(format!(
            "skill generation {} committed but runtime settlement is degraded",
            receipt.desired_generation
        )),
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    }
}

fn skill_toggle_command_identity(name: &str, enabled: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(b"set-skill-enabled\0");
    digest.update(name.as_bytes());
    digest.update([u8::from(enabled)]);
    format!("sha256_{:x}", digest.finalize())
}

fn skill_artifact_command_identity(action: &str, value: &str, force: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(action.as_bytes());
    digest.update(b"\0");
    digest.update(value.as_bytes());
    digest.update([u8::from(force)]);
    format!("sha256_{:x}", digest.finalize())
}

async fn replay_skill_artifact_debt(
    state: &Arc<AppState>,
    config: &mut EnabledSkillsConfig,
) -> (
    bool,
    Vec<SkillTargetSettlementReceipt>,
    Vec<SkillTargetSettlementReceipt>,
) {
    let Some(mut debt) = config.repair_debt.clone() else {
        return (false, Vec::new(), Vec::new());
    };
    let mut changed = false;
    let mut receipts = Vec::new();
    let mut terminal_receipts = Vec::new();
    let mut pending_enablements = Vec::new();
    for name in std::mem::take(&mut debt.artifact_enablements) {
        match skill_entry(state, &name).await {
            Ok((_, category)) => {
                config.skills.insert(
                    name.clone(),
                    SkillEnableEntry {
                        category,
                        enabled: true,
                        baseline: false,
                    },
                );
                changed = true;
                receipts.push(SkillTargetSettlementReceipt {
                    target: format!("skill-artifact-enable:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Settled,
                    changed_entries: vec![name],
                    error: None,
                });
            }
            Err(error) => {
                pending_enablements.push(name.clone());
                receipts.push(SkillTargetSettlementReceipt {
                    target: format!("skill-artifact-enable:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(error.to_string()),
                });
            }
        }
    }
    debt.artifact_enablements = pending_enablements;

    let skill_root = state.skills_hub.read().await.root().to_path_buf();
    let mut pending_syncs = Vec::new();
    for pending in std::mem::take(&mut debt.artifact_syncs) {
        let target = format!("skill-artifact-sync:{}", pending.name);
        debt.target_failures
            .retain(|failure| failure.target != target || failure.component != "artifact_sync");
        let mut hub = SkillsHub::with_root(skill_root.clone());
        let result =
            crate::skills_hub::sync_skills(&mut hub, Some(pending.name.as_str()), pending.force)
                .await;
        let (receipt, terminal) = match result {
            Ok(results) if results.iter().all(|result| result.success) => {
                changed = true;
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Settled,
                        changed_entries: results.into_iter().map(|result| result.name).collect(),
                        error: None,
                    },
                    false,
                )
            }
            Ok(results) => {
                let message = results
                    .iter()
                    .filter(|result| !result.success)
                    .map(|result| format!("{}: {}", result.name, result.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                let retryable = results
                    .iter()
                    .filter(|result| !result.success)
                    .any(|result| result.retryable);
                if retryable {
                    pending_syncs.push(pending.clone());
                } else {
                    changed = true;
                }
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Degraded,
                        changed_entries: Vec::new(),
                        error: Some(message),
                    },
                    !retryable,
                )
            }
            Err(error) => {
                pending_syncs.push(pending.clone());
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Degraded,
                        changed_entries: Vec::new(),
                        error: Some(error),
                    },
                    false,
                )
            }
        };
        if terminal {
            terminal_receipts.push(receipt);
        } else {
            receipts.push(receipt);
        }
    }
    debt.artifact_syncs = pending_syncs;
    config.set_repair_debt(debt);
    (changed, receipts, terminal_receipts)
}

fn repair_component(target: &str) -> &'static str {
    if target.starts_with("skill-artifact-sync:") {
        "artifact_sync"
    } else if target.starts_with("skill-artifact-enable:") {
        "artifact_enablement"
    } else if target.starts_with("skill-artifact:") {
        "artifact"
    } else {
        match target {
            "enabled-skills.json" => "durable_file",
            "skill-catalog" => "catalog",
            "workspace-generations" => "workspace_identity",
            "runtime-targets" => "runtime_targets",
            _ => "runtime_fanout",
        }
    }
}

fn repair_target_debt(
    receipt: &SkillTargetSettlementReceipt,
    expected_generation: u64,
) -> Option<SkillRepairTargetDebt> {
    if receipt.status != SkillTargetSettlementStatus::Degraded {
        return None;
    }
    let reason = receipt
        .error
        .clone()
        .unwrap_or_else(|| "target reported degraded settlement without a reason".to_string());
    Some(SkillRepairTargetDebt {
        target: receipt.target.clone(),
        component: repair_component(&receipt.target).to_string(),
        expected_generation,
        observed_generation: (receipt.specialist_generation != expected_generation)
            .then_some(receipt.specialist_generation),
        retryable: !reason.contains("newer desired generation superseded"),
        reason,
    })
}

fn repair_debt_from_target_receipts(
    generation: u64,
    content_identity: String,
    attempts: u32,
    target_receipts: &[SkillTargetSettlementReceipt],
) -> SkillRepairDebt {
    SkillRepairDebt {
        generation,
        content_identity,
        attempts,
        target_failures: target_receipts
            .iter()
            .filter_map(|receipt| repair_target_debt(receipt, generation))
            .collect(),
        artifact_removals: target_receipts
            .iter()
            .filter(|receipt| receipt.status == SkillTargetSettlementStatus::Degraded)
            .filter_map(|receipt| receipt.target.strip_prefix("skill-artifact:"))
            .map(str::to_string)
            .collect(),
        artifact_syncs: Vec::new(),
        artifact_enablements: Vec::new(),
    }
}

fn preserve_artifact_repair_actions(
    debt: &mut SkillRepairDebt,
    existing: Option<&SkillRepairDebt>,
) {
    let Some(existing) = existing else {
        return;
    };
    debt.artifact_removals
        .extend(existing.artifact_removals.iter().cloned());
    debt.artifact_syncs
        .extend(existing.artifact_syncs.iter().cloned());
    debt.artifact_enablements
        .extend(existing.artifact_enablements.iter().cloned());
}

fn remove_skill_artifact(skill_root: PathBuf, name: &str) -> Result<bool, String> {
    let mut hub = SkillsHub::with_root(skill_root);
    crate::skills_hub::install::uninstall(name, &mut hub)
}

async fn record_artifact_repair_debt(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    name: &str,
    failure: &str,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded artifact repair debt".to_string(),
        ));
    }
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
    debt.attempts = debt.attempts.saturating_add(1);
    let target_receipt = SkillTargetSettlementReceipt {
        target: format!("skill-artifact:{name}"),
        workspace_generation: "global".to_string(),
        specialist_generation: receipt.desired_generation,
        status: SkillTargetSettlementStatus::Degraded,
        changed_entries: Vec::new(),
        error: Some(failure.to_string()),
    };
    if let Some(target_debt) = repair_target_debt(&target_receipt, receipt.desired_generation) {
        debt.target_failures.push(target_debt);
    }
    if !debt
        .artifact_removals
        .iter()
        .any(|candidate| candidate == name)
    {
        debt.artifact_removals.push(name.to_string());
    }
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

async fn record_install_repair_debt(
    state: &Arc<AppState>,
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    name: &str,
    failure: &str,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let skill_root = state.skills_hub.read().await.root().to_path_buf();
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    normalize_skill_content_identity(flow, &mut config, skill_root).await?;
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| empty_skill_repair_debt(&config));
    debt.attempts = debt.attempts.saturating_add(1);
    debt.target_failures.push(SkillRepairTargetDebt {
        target: format!("skill-artifact-enable:{name}"),
        component: "artifact_enablement".to_string(),
        expected_generation: config.desired_generation,
        observed_generation: None,
        reason: failure.to_string(),
        retryable: true,
    });
    debt.artifact_enablements.push(name.to_string());
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

async fn record_artifact_sync_repair_debt(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    failures: &[(String, String)],
    force: bool,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded artifact sync repair debt".to_string(),
        ));
    }
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| empty_skill_repair_debt(&config));
    debt.attempts = debt.attempts.saturating_add(1);
    for (name, failure) in failures {
        debt.target_failures.push(SkillRepairTargetDebt {
            target: format!("skill-artifact-sync:{name}"),
            component: "artifact_sync".to_string(),
            expected_generation: config.desired_generation,
            observed_generation: None,
            reason: failure.clone(),
            retryable: true,
        });
        debt.artifact_syncs.push(SkillArtifactSyncDebt {
            name: name.clone(),
            force,
        });
    }
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

fn empty_skill_repair_debt(config: &EnabledSkillsConfig) -> SkillRepairDebt {
    SkillRepairDebt {
        generation: config.desired_generation,
        content_identity: config.content_identity.clone(),
        attempts: 0,
        target_failures: Vec::new(),
        artifact_removals: Vec::new(),
        artifact_syncs: Vec::new(),
        artifact_enablements: Vec::new(),
    }
}

async fn read_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
) -> Result<EnabledSkillsConfig, SkillMutationError> {
    flow.run("read enabled skills desired state", move || {
        EnabledSkillsConfig::load(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

async fn skill_commit_is_current(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    committed: &EnabledSkillsConfig,
) -> Result<bool, SkillMutationError> {
    let latest = read_enabled_skills_config(flow, path).await?;
    Ok(latest.desired_generation == committed.desired_generation
        && latest.content_identity == committed.content_identity)
}

async fn admitted_skill_operation(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    operation_id: &str,
    command_identity: &str,
) -> Result<Option<SkillOperationIdentity>, SkillMutationError> {
    let config = read_enabled_skills_config(flow, path).await?;
    let Some(committed) = config.operation(operation_id) else {
        return Ok(None);
    };
    if committed.command_identity == command_identity {
        return Ok(Some(committed.clone()));
    }
    Err(SkillMutationError::OperationConflict {
        operation_id: operation_id.to_string(),
        committed_content_identity: committed.content_identity.clone(),
    })
}

async fn record_skill_operation_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    operation_id: String,
    command_identity: String,
    artifact_name: Option<String>,
) -> Result<(), SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded operation identity commit".to_string(),
        ));
    }
    config.record_operation(SkillOperationIdentity {
        operation_id,
        command_identity,
        artifact_name,
        content_identity: receipt.content_identity.clone(),
        generation: receipt.desired_generation,
    });
    write_enabled_skills_config(flow, path, config).await
}

fn stale_skill_generation_receipt(committed: &EnabledSkillsConfig) -> SkillTargetSettlementReceipt {
    SkillTargetSettlementReceipt {
        target: "enabled-skills.json".to_string(),
        workspace_generation: "global".to_string(),
        specialist_generation: committed.desired_generation,
        status: SkillTargetSettlementStatus::Degraded,
        changed_entries: Vec::new(),
        error: Some(
            "a newer desired generation superseded this settlement before runtime fanout"
                .to_string(),
        ),
    }
}

async fn write_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    config: EnabledSkillsConfig,
) -> Result<(), SkillMutationError> {
    flow.run("commit enabled skills desired state", move || {
        config.save(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

async fn compute_skill_content_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    skills: HashMap<String, SkillEnableEntry>,
    skill_root: PathBuf,
) -> Result<String, SkillMutationError> {
    flow.run("hash enabled skill desired content", move || {
        skill_content_identity(&skills, skill_root)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(SkillMutationError::BeforeCommit)
}

fn skill_content_identity(
    skills: &HashMap<String, SkillEnableEntry>,
    skill_root: PathBuf,
) -> Result<String, String> {
    let hub = SkillsHub::with_root(skill_root);
    let skill_paths = hub
        .list()
        .into_iter()
        .map(|entry| (entry.name.clone(), entry.path.join("SKILL.md")))
        .collect::<BTreeMap<_, _>>();
    let mut canonical = BTreeMap::new();
    for (name, entry) in skills {
        let body_identity = if entry.enabled {
            match skill_paths.get(name) {
                Some(path) => {
                    let bytes = std::fs::read(path).map_err(|error| {
                        format!("failed to hash enabled skill '{}': {error}", path.display())
                    })?;
                    format!("sha256_{:x}", Sha256::digest(bytes))
                }
                None => "missing".to_string(),
            }
        } else {
            "disabled".to_string()
        };
        canonical.insert(
            name.clone(),
            (
                entry.category.clone(),
                entry.enabled,
                entry.baseline,
                body_identity,
            ),
        );
    }
    let encoded = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(format!("sha256_{:x}", Sha256::digest(encoded)))
}

async fn normalize_skill_content_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    config: &mut EnabledSkillsConfig,
    skill_root: PathBuf,
) -> Result<bool, SkillMutationError> {
    if config.settled_generation > config.desired_generation {
        return Err(SkillMutationError::BeforeCommit(format!(
            "settled generation {} exceeds desired generation {}",
            config.settled_generation, config.desired_generation
        )));
    }
    let mut changed = false;
    let existing_debt = config.repair_debt.clone();
    if config.version < 2 {
        config.version = 2;
        changed = true;
    }
    let overflow = config
        .operation_identities
        .len()
        .saturating_sub(crate::skills_hub::enabled_skills::MAX_OPERATION_IDENTITIES);
    if overflow > 0 {
        config.operation_identities.drain(..overflow);
        changed = true;
    }
    let identity = compute_skill_content_identity(flow, config.skills.clone(), skill_root).await?;
    if config.content_identity.is_empty() {
        config.content_identity = identity.clone();
        changed = true;
    } else if config.content_identity != identity {
        config.desired_generation = config.desired_generation.checked_add(1).ok_or_else(|| {
            SkillMutationError::BeforeCommit(
                "enabled skill desired generation is exhausted".to_string(),
            )
        })?;
        config.content_identity = identity.clone();
        let mut debt = SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: identity.clone(),
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        };
        preserve_artifact_repair_actions(&mut debt, existing_debt.as_ref());
        config.set_repair_debt(debt);
        changed = true;
    }
    if config.settled_generation < config.desired_generation && config.repair_debt.is_none() {
        let mut debt = SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: identity,
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        };
        preserve_artifact_repair_actions(&mut debt, existing_debt.as_ref());
        config.set_repair_debt(debt);
        changed = true;
    }
    Ok(changed)
}

async fn settle_skill_generation(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    committed: EnabledSkillsConfig,
    operation_id: String,
    idempotent: bool,
    durable_committed: bool,
    mut target_receipts: Vec<SkillTargetSettlementReceipt>,
) -> Result<SkillSyncReceipt, SkillMutationError> {
    let mut latest = match read_enabled_skills_config(flow, path.clone()).await {
        Ok(config) => config,
        Err(error) => {
            target_receipts.push(SkillTargetSettlementReceipt {
                target: "enabled-skills.json".to_string(),
                workspace_generation: "global".to_string(),
                specialist_generation: committed.desired_generation,
                status: SkillTargetSettlementStatus::Degraded,
                changed_entries: Vec::new(),
                error: Some(error.to_string()),
            });
            return Ok(degraded_skill_receipt(
                path,
                committed,
                operation_id,
                idempotent,
                durable_committed,
                target_receipts,
            ));
        }
    };
    if latest.desired_generation != committed.desired_generation
        || latest.content_identity != committed.content_identity
    {
        target_receipts.push(SkillTargetSettlementReceipt {
            target: "enabled-skills.json".to_string(),
            workspace_generation: "global".to_string(),
            specialist_generation: committed.desired_generation,
            status: SkillTargetSettlementStatus::Degraded,
            changed_entries: Vec::new(),
            error: Some("a newer desired generation superseded this settlement".to_string()),
        });
        let repair_debt = repair_debt_from_target_receipts(
            committed.desired_generation,
            committed.content_identity.clone(),
            1,
            &target_receipts,
        );
        return Ok(SkillSyncReceipt {
            operation_id,
            committed_file_path: path,
            content_identity: committed.content_identity.clone(),
            desired_generation: committed.desired_generation,
            settled_generation: latest.settled_generation,
            durable_committed,
            idempotent,
            status: SkillSettlementStatus::Degraded,
            target_receipts,
            repair_debt: Some(repair_debt),
        });
    }

    let has_failures = target_receipts
        .iter()
        .any(|receipt| receipt.status == SkillTargetSettlementStatus::Degraded);
    if !has_failures {
        if latest.settled_generation != latest.desired_generation || latest.repair_debt.is_some() {
            latest.settled_generation = latest.desired_generation;
            latest.repair_debt = None;
            if let Err(error) =
                write_enabled_skills_config(flow, path.clone(), latest.clone()).await
            {
                target_receipts.push(SkillTargetSettlementReceipt {
                    target: "enabled-skills.json".to_string(),
                    workspace_generation: "global".to_string(),
                    specialist_generation: committed.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(format!(
                        "runtime settled but generation CAS failed: {error}"
                    )),
                });
                return Ok(degraded_skill_receipt(
                    path,
                    committed,
                    operation_id,
                    idempotent,
                    durable_committed,
                    target_receipts,
                ));
            }
        }
        return Ok(SkillSyncReceipt {
            operation_id,
            committed_file_path: path,
            content_identity: latest.content_identity,
            desired_generation: latest.desired_generation,
            settled_generation: latest.settled_generation,
            durable_committed,
            idempotent,
            status: SkillSettlementStatus::Settled,
            target_receipts,
            repair_debt: None,
        });
    }

    let attempts = latest
        .repair_debt
        .as_ref()
        .map_or(1, |debt| debt.attempts.saturating_add(1));
    let mut debt = repair_debt_from_target_receipts(
        latest.desired_generation,
        latest.content_identity.clone(),
        attempts,
        &target_receipts,
    );
    preserve_artifact_repair_actions(&mut debt, latest.repair_debt.as_ref());
    latest.set_repair_debt(debt.clone());
    if let Err(error) = write_enabled_skills_config(flow, path.clone(), latest.clone()).await {
        target_receipts.push(SkillTargetSettlementReceipt {
            target: "enabled-skills.json".to_string(),
            workspace_generation: "global".to_string(),
            specialist_generation: committed.desired_generation,
            status: SkillTargetSettlementStatus::Degraded,
            changed_entries: Vec::new(),
            error: Some(format!("repair debt update failed: {error}")),
        });
        debt = repair_debt_from_target_receipts(
            latest.desired_generation,
            latest.content_identity.clone(),
            attempts,
            &target_receipts,
        );
        preserve_artifact_repair_actions(&mut debt, latest.repair_debt.as_ref());
    }
    Ok(SkillSyncReceipt {
        operation_id,
        committed_file_path: path,
        content_identity: latest.content_identity,
        desired_generation: latest.desired_generation,
        settled_generation: latest.settled_generation,
        durable_committed,
        idempotent,
        status: SkillSettlementStatus::Degraded,
        target_receipts,
        repair_debt: Some(debt),
    })
}

fn degraded_skill_receipt(
    committed_file_path: PathBuf,
    committed: EnabledSkillsConfig,
    operation_id: String,
    idempotent: bool,
    durable_committed: bool,
    target_receipts: Vec<SkillTargetSettlementReceipt>,
) -> SkillSyncReceipt {
    let attempts = committed
        .repair_debt
        .as_ref()
        .map_or(1, |debt| debt.attempts.saturating_add(1));
    let repair_debt = repair_debt_from_target_receipts(
        committed.desired_generation,
        committed.content_identity.clone(),
        attempts,
        &target_receipts,
    );
    SkillSyncReceipt {
        operation_id,
        committed_file_path,
        content_identity: committed.content_identity,
        desired_generation: committed.desired_generation,
        settled_generation: committed.settled_generation,
        durable_committed,
        idempotent,
        status: SkillSettlementStatus::Degraded,
        target_receipts,
        repair_debt: Some(repair_debt),
    }
}

fn desired_skill_entries(
    config: &EnabledSkillsConfig,
    skill_root: PathBuf,
) -> Vec<(String, PathBuf)> {
    let hub = SkillsHub::with_root(skill_root);
    let mut selected = hub
        .list()
        .into_iter()
        .filter(|entry| {
            config
                .skills
                .get(&entry.name)
                .is_some_and(|state| state.enabled)
        })
        .map(|entry| {
            let load_root = entry
                .path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| entry.path.clone());
            (entry.name.clone(), load_root)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.0.cmp(&right.0));
    selected
}

async fn reconcile_target_skills(
    target: &crate::state::ExtensionRuntimeTarget,
    desired: &[(String, PathBuf)],
    skill_root: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let mut current = target
        .primary_agent()
        .read(|agent| {
            agent
                .skill_descriptors()
                .iter()
                .filter_map(|descriptor| {
                    let source = descriptor.source.as_deref()?;
                    source
                        .starts_with(USER_SKILL_SOURCE_PREFIX)
                        .then(|| (descriptor.name.clone(), source.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .await;
    current.sort();
    current.dedup();
    for (name, source) in current {
        let load_root = desired
            .iter()
            .find(|(candidate, _)| candidate == &name)
            .map(|(_, root)| root.clone())
            .unwrap_or_else(|| skill_root.to_path_buf());
        target
            .plugin_runtime()
            .disable_application_skill(name, load_root, source)
            .await?;
    }
    let mut loaded = Vec::new();
    for (name, load_root) in desired {
        loaded.extend(
            target
                .plugin_runtime()
                .enable_application_skill(name.clone(), load_root.clone(), user_skill_source(name))
                .await?,
        );
    }
    Ok(loaded)
}

#[cfg(test)]
async fn load_exact_user_skill(
    agent: &crate::agent_handle::AgentHandle,
    requested: &str,
    load_root: PathBuf,
    requested_source: String,
) -> anyhow::Result<Vec<String>> {
    let requested = requested.to_string();
    agent
        .write_async(|agent| {
            Box::pin(async move {
                let loaded = agent.load_skills_from_dir(load_root).await?;
                for name in &loaded {
                    let source = if name == &requested {
                        requested_source.clone()
                    } else {
                        format!("eko:discarded-sibling-skill:{name}")
                    };
                    agent
                        .tag_skills_source(std::slice::from_ref(name), &source)
                        .await;
                    if name != &requested {
                        agent.unregister_skills_by_source(&source).await;
                    }
                }
                Ok::<_, echo_agent::error::ReactError>(
                    loaded
                        .into_iter()
                        .filter(|name| name == &requested)
                        .collect(),
                )
            })
        })
        .await
        .map_err(anyhow::Error::new)
}

fn ensure_hook_load_succeeded(loaded: &HooksLoadResult) -> anyhow::Result<()> {
    if loaded.errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(loaded.errors.join("; ")))
    }
}

fn mcp_transport(entry: &echo_agent::mcp::McpServerEntry) -> &'static str {
    if entry.url.is_some() {
        if entry.transport.as_deref() == Some("sse") {
            "sse"
        } else {
            "http"
        }
    } else if entry.command.is_some() {
        "stdio"
    } else {
        "unknown"
    }
}

fn mcp_health_scope_key(runtime: &ScopedChatRuntime) -> anyhow::Result<String> {
    serde_json::to_string(&(
        runtime.execution_scope().workspace_id(),
        runtime.workspace_host_generation(),
    ))
    .map_err(|error| anyhow::anyhow!("failed to encode MCP health scope: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    struct FanoutFixture {
        temp: tempfile::TempDir,
        state: Arc<AppState>,
        seed_pool: Arc<crate::agent_pool::AgentPool>,
        workspaces: Vec<crate::workspace::Workspace>,
        enabled_config_path: PathBuf,
    }

    async fn fanout_fixture(workspace_count: usize) -> Result<FanoutFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let skill_root = temp.path().join("skills");
        let skill = skill_root.join("fanout-skill");
        std::fs::create_dir_all(&skill).map_err(|error| error.to_string())?;
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fanout-skill\ndescription: fanout fixture\n---\nfanout",
        )
        .map_err(|error| error.to_string())?;
        let primary = crate::agent_handle::AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("extension fanout test")
                .enable_tools()
                .working_dir(temp.path())
                .build()
                .map_err(|error| error.to_string())?,
        );
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            primary.clone(),
            temp.path().to_path_buf(),
            temp.path().join("plugins.json"),
            temp.path().join("plugin-data"),
        )
        .await
        .map_err(|error| error.to_string())?;
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 8, false).await,
        );
        seed_pool
            .update_mcp_config_snapshot(Default::default())
            .await;
        plugin_runtime
            .bind_agent_pool(Arc::downgrade(&seed_pool))
            .await
            .map_err(|error| error.to_string())?;
        let mut state = AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_plugin_runtime(Some(plugin_runtime));
        state.set_pool(seed_pool.clone());
        state.skills_hub = Arc::new(tokio::sync::RwLock::new(SkillsHub::with_root(
            skill_root.clone(),
        )));
        let enabled_config_path = temp.path().join("enabled-skills.json");
        state.extension_control = Arc::new(ExtensionControlService::with_enabled_config_path(
            enabled_config_path.clone(),
        ));
        let registry = Arc::new(
            crate::workspace::registry::WorkspaceRegistry::with_base_dir(
                temp.path().join("workspaces"),
            )
            .map_err(|error| error.to_string())?,
        );
        let mut workspaces = Vec::new();
        for index in 0..workspace_count {
            let name = format!("workspace-{index}");
            workspaces.push(
                registry
                    .create_at(
                        &name,
                        crate::workspace::WorkspaceKind::General,
                        temp.path().join(&name),
                    )
                    .map_err(|error| error.to_string())?,
            );
        }
        state.workspace.registry = registry;
        state.storage.chat_events = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(
                temp.path().join("tool-executions"),
            )
            .map_err(|error| error.to_string())?,
        );
        let state = Arc::new(state);
        for workspace in &workspaces {
            state
                .switch_workspace(workspace.clone())
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(FanoutFixture {
            temp,
            state,
            seed_pool,
            workspaces,
            enabled_config_path,
        })
    }

    async fn begin_shutdown_after_extension_admission(
        state: &Arc<AppState>,
    ) -> Result<tokio::task::JoinHandle<Result<(), String>>, String> {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        state.session.product_data_io.begin_shutdown()?;
        let product_data_io = state.session.product_data_io.clone();
        let shutdown = tokio::spawn(async move { product_data_io.join_shutdown().await });
        tokio::task::yield_now().await;
        if shutdown.is_finished() {
            shutdown.await.map_err(|error| error.to_string())??;
            return Err(
                "extension mutation was not admitted into ProductData lifecycle".to_string(),
            );
        }
        Ok(shutdown)
    }

    #[test]
    fn user_skill_source_is_exact_and_stable() {
        assert_eq!(
            user_skill_source("paper-reader"),
            "eko:user-skill:paper-reader"
        );
    }

    #[test]
    fn curated_skill_artifact_and_lifecycle_commit_are_idempotent() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_agent_dir = temp.path().join(".eko");
        let draft_path = echo_agent_dir.join("skills/_drafts/curated-fixture/SKILL.md");
        let draft_parent = draft_path
            .parent()
            .ok_or_else(|| "draft fixture has no parent".to_string())?;
        std::fs::create_dir_all(draft_parent).map_err(|error| error.to_string())?;
        std::fs::write(
            &draft_path,
            "---\nname: curated-fixture\ndescription: fixture\n---\nbody",
        )
        .map_err(|error| error.to_string())?;
        let curator = crate::evolution::workspace_curator(&echo_agent_dir);
        curator
            .register_candidate_at("curated-fixture", Some(&draft_path))
            .map_err(|error| error.to_string())?;
        if !curator
            .promote_to_draft_at("curated-fixture", Some(&draft_path))
            .map_err(|error| error.to_string())?
        {
            return Err("fixture candidate did not become Draft".to_string());
        }

        let committed = promote_curated_skill_artifact(echo_agent_dir.clone(), "curated-fixture")?;
        assert!(!committed.idempotent);
        assert!(committed.active_path.is_file());
        let active = curator
            .load_state()
            .map_err(|error| error.to_string())?
            .skills
            .get("curated-fixture")
            .map(|metadata| metadata.lifecycle);
        assert_eq!(active, Some(echo_agent::evolution::SkillLifecycle::Active));

        let repeated = promote_curated_skill_artifact(echo_agent_dir, "curated-fixture")?;
        assert!(repeated.idempotent);
        assert_eq!(repeated.active_path, committed.active_path);
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_curated_skill_promotion() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        let echo_agent_dir = fixture.temp.path().join("curated-workspace/.eko");
        let draft_path = echo_agent_dir.join("skills/_drafts/curated-drop/SKILL.md");
        let draft_parent = draft_path
            .parent()
            .ok_or_else(|| "draft fixture has no parent".to_string())?;
        std::fs::create_dir_all(draft_parent).map_err(|error| error.to_string())?;
        std::fs::write(
            &draft_path,
            "---\nname: curated-drop\ndescription: fixture\n---\nbody",
        )
        .map_err(|error| error.to_string())?;
        let curator = crate::evolution::workspace_curator(&echo_agent_dir);
        curator
            .register_candidate_at("curated-drop", Some(&draft_path))
            .map_err(|error| error.to_string())?;
        if !curator
            .promote_to_draft_at("curated-drop", Some(&draft_path))
            .map_err(|error| error.to_string())?
        {
            return Err("fixture candidate did not become Draft".to_string());
        }
        let store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration = crate::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            echo_agent_dir,
            store,
        );
        let generation = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        fixture.state.session.product_data_io.install_test_barrier(
            "promote curated skill artifact",
            entered_tx,
            release_rx,
        );
        let state = Arc::clone(&fixture.state);
        let service = Arc::clone(&state.extension_control);
        let caller = tokio::spawn(async move {
            service
                .publish_curated_skill(&state, None, generation, "curated-drop")
                .await
        });
        entered_rx
            .await
            .map_err(|_| "curated promotion never reached durable I/O".to_string())?;
        caller.abort();
        release_tx
            .send(())
            .map_err(|_| "curated promotion barrier owner was dropped".to_string())?;
        fixture
            .state
            .extension_control
            .begin_shutdown(&fixture.state)?;
        fixture
            .state
            .extension_control
            .join_shutdown(&fixture.state)
            .await?;

        let active = curator
            .load_state()
            .map_err(|error| error.to_string())?
            .skills
            .get("curated-drop")
            .map(|metadata| metadata.lifecycle);
        assert_eq!(active, Some(echo_agent::evolution::SkillLifecycle::Active));
        let primary = fixture.state.connection.primary_agent();
        assert!(
            skill_source_present(&primary, "curated-drop", "eko:curated-skill:curated-drop").await
        );
        Ok(())
    }

    #[test]
    fn mcp_transport_preserves_protocol_shape() {
        let stdio = echo_agent::mcp::McpServerEntry {
            command: Some("npx".to_string()),
            ..Default::default()
        };
        assert_eq!(mcp_transport(&stdio), "stdio");
    }

    #[test]
    fn mcp_snapshot_preserves_gui_contract_fields() -> Result<(), String> {
        let snapshot = ExtensionMcpServer {
            name: "local".to_string(),
            status: "connected".to_string(),
            transport: "stdio".to_string(),
            tool_count: 1,
            tools: vec![ExtensionMcpTool {
                name: "read".to_string(),
                description: "Read a value".to_string(),
            }],
            connected_at: None,
            error: None,
            enabled: true,
        };
        let value = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("enabled").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .get("tools")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert!(
            value
                .get("connected_at")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_health_projection_rejects_same_id_workspace_aba() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let workspace = fixture
            .workspaces
            .first()
            .cloned()
            .ok_or_else(|| "workspace fixture missing".to_string())?;
        let service = Arc::clone(&fixture.state.extension_control);
        service
            .upsert_mcp_server(
                &fixture.state,
                "aba-mcp".to_string(),
                echo_agent::mcp::McpServerEntry {
                    command: Some("unused-disabled-mcp".to_string()),
                    disabled: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;

        let old_runtime = fixture
            .state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let old_scope = mcp_health_scope_key(&old_runtime).map_err(|error| error.to_string())?;
        drop(old_runtime);

        fixture
            .state
            .delete_workspace_owned(&workspace.id)
            .await
            .map_err(|error| error.to_string())?;
        let recreated = fixture
            .state
            .workspace
            .registry
            .create_at(&workspace.name, workspace.kind, workspace.root)
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .switch_workspace(recreated)
            .await
            .map_err(|error| error.to_string())?;
        let new_runtime = fixture
            .state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let new_scope = mcp_health_scope_key(&new_runtime).map_err(|error| error.to_string())?;
        assert_ne!(old_scope, new_scope);

        fixture.state.plugins.mcp_health.write().await.insert(
            old_scope,
            HashMap::from([(
                "aba-mcp".to_string(),
                McpHealthStatus {
                    name: "aba-mcp".to_string(),
                    healthy: false,
                    last_check: Some(chrono::Utc::now()),
                    error: Some("stale-generation-health".to_string()),
                },
            )]),
        );
        let stale_projection = service
            .list_mcp_servers_scoped(&fixture.state, Some(&new_runtime))
            .await
            .map_err(|error| error.to_string())?;
        let stale_server = stale_projection
            .iter()
            .find(|server| server.name == "aba-mcp")
            .ok_or_else(|| "MCP projection missing after workspace recreation".to_string())?;
        assert_eq!(stale_server.error, None);

        fixture.state.plugins.mcp_health.write().await.insert(
            new_scope,
            HashMap::from([(
                "aba-mcp".to_string(),
                McpHealthStatus {
                    name: "aba-mcp".to_string(),
                    healthy: false,
                    last_check: Some(chrono::Utc::now()),
                    error: Some("new-generation-health".to_string()),
                },
            )]),
        );
        let current_projection = service
            .list_mcp_servers_scoped(&fixture.state, Some(&new_runtime))
            .await
            .map_err(|error| error.to_string())?;
        let current_server = current_projection
            .iter()
            .find(|server| server.name == "aba-mcp")
            .ok_or_else(|| "current MCP projection missing".to_string())?;
        assert_eq!(
            current_server.error.as_deref(),
            Some("new-generation-health")
        );
        Ok(())
    }

    #[tokio::test]
    async fn exact_skill_load_discards_disabled_siblings() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        for name in ["enabled", "disabled"] {
            let root = temp.path().join(name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            std::fs::write(
                root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} fixture\n---\n{name}"),
            )
            .map_err(|error| error.to_string())?;
        }
        let agent = crate::agent_handle::AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("extension control test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let loaded = load_exact_user_skill(
            &agent,
            "enabled",
            temp.path().to_path_buf(),
            user_skill_source("enabled"),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(loaded, vec!["enabled".to_string()]);
        let (enabled, disabled) = agent
            .read(|agent| (agent.has_skill("enabled"), agent.has_skill("disabled")))
            .await;
        assert!(enabled);
        assert!(!disabled);
        Ok(())
    }

    #[tokio::test]
    async fn global_policy_reaches_three_loaded_workspaces_and_future_forks() -> Result<(), String>
    {
        let fixture = fanout_fixture(3).await?;
        let before = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let mut existing_agents = Vec::new();
        for target in before.iter() {
            let lease = target
                .pool()
                .acquire(&format!("existing-{}", target.scope()))
                .await
                .map_err(|error| error.to_string())?;
            existing_agents.push((target.scope().to_string(), lease.agent()));
            drop(lease);
        }
        drop(before);
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(targets.iter().count(), 4);
        for target in targets.iter() {
            assert!(
                skill_source_present(
                    &target.primary_agent(),
                    "fanout-skill",
                    &user_skill_source("fanout-skill"),
                )
                .await,
                "target {} missed global skill policy",
                target.scope()
            );
        }
        for (scope, agent) in existing_agents {
            assert!(
                agent.read(|agent| agent.has_skill("fanout-skill")).await,
                "existing pooled Agent in {scope} missed the generation"
            );
        }
        drop(targets);
        let future = fixture
            .seed_pool
            .acquire("future-global-consumer")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future
                .agent()
                .read(|agent| agent.has_skill("fanout-skill"))
                .await
        );
        drop(future);

        let future_workspace = fixture
            .state
            .workspace
            .registry
            .create_at(
                "future-workspace",
                crate::workspace::WorkspaceKind::General,
                fixture.temp.path().join("future-workspace"),
            )
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .switch_workspace(future_workspace)
            .await
            .map_err(|error| error.to_string())?;
        let future_control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future_control
                .runtime()
                .primary_agent()
                .read(|agent| agent.has_skill("fanout-skill"))
                .await
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_projection_reads_loaded_state_from_agent_descriptors() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let entries = fixture
            .state
            .extension_control
            .list_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        let projected = entries
            .iter()
            .find(|entry| entry.catalog.name == "fanout-skill")
            .ok_or_else(|| "fanout-skill was absent from Extension projection".to_string())?;
        assert!(projected.loaded);
        Ok(())
    }

    #[tokio::test]
    async fn operation_and_content_identity_are_idempotent_and_conflicts_fail_closed()
    -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let first = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        std::fs::write(
            fixture.temp.path().join("skills/fanout-skill/SKILL.md"),
            "---\nname: fanout-skill\ndescription: changed after operation\n---\nchanged",
        )
        .map_err(|error| error.to_string())?;
        let changed_content = service
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert!(changed_content.desired_generation > first.desired_generation);
        let repeated = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(repeated.idempotent);
        assert_eq!(
            repeated.desired_generation,
            changed_content.desired_generation
        );

        let same_content = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-same-content",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(same_content.idempotent);
        assert_eq!(
            same_content.desired_generation,
            changed_content.desired_generation
        );

        let conflict = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                false,
            )
            .await
            .err()
            .ok_or_else(|| "same operation with different content was accepted".to_string())?;
        assert!(matches!(
            conflict,
            SkillMutationError::OperationConflict { .. }
        ));
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert_eq!(
            config.desired_generation,
            changed_content.desired_generation
        );
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_accepted_skill_settlement() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let state = Arc::clone(&fixture.state);
        let admission = Arc::clone(&state.extension_control);
        let service = Arc::clone(&admission);
        let mutation = admission.mutation.lock().await;
        let caller = tokio::spawn(async move {
            service
                .set_skill_enabled_with_operation(
                    &state,
                    "caller-drop-operation",
                    "fanout-skill",
                    true,
                )
                .await
        });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(mutation);
        shutdown.await.map_err(|error| error.to_string())??;
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(config.desired_generation, config.settled_generation);
        assert!(config.repair_debt.is_none());
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn reopened_service_replays_durable_repair_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let mut config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        config.settled_generation = config.desired_generation.saturating_sub(1);
        config.set_repair_debt(SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "workspace-0".to_string(),
                component: "runtime_fanout".to_string(),
                expected_generation: config.desired_generation,
                observed_generation: None,
                reason: "simulated restart debt".to_string(),
                retryable: true,
            }],
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
        config
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;

        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let receipt = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Settled);
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(repaired.desired_generation, repaired.settled_generation);
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn reopened_service_replays_artifact_removal_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        let mut config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        config.set_repair_debt(SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "skill-artifact:fanout-skill".to_string(),
                component: "artifact".to_string(),
                expected_generation: config.desired_generation,
                observed_generation: None,
                reason: "simulated".to_string(),
                retryable: true,
            }],
            artifact_removals: vec!["fanout-skill".to_string()],
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
        config
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let receipt = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Settled);
        assert!(!fixture.temp.path().join("skills/fanout-skill").exists());
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn terminal_artifact_sync_failure_does_not_create_replay_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let receipt = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.settlement.operation_id, "sync-operation");
        assert_eq!(receipt.settlement.status, SkillSettlementStatus::Degraded);
        assert!(receipt.results.iter().any(|result| !result.success));
        let committed = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(committed.repair_debt.is_none());
        let duplicate = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.results.is_empty());
        assert_eq!(duplicate.settlement.status, SkillSettlementStatus::Settled);
        let conflict = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                true,
            )
            .await
            .err()
            .ok_or_else(|| "conflicting sync operation was accepted".to_string())?;
        assert!(
            conflict
                .to_string()
                .contains("conflicts with committed content")
        );

        let mut legacy_debt = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        legacy_debt.set_repair_debt(SkillRepairDebt {
            generation: legacy_debt.desired_generation,
            content_identity: legacy_debt.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "skill-artifact-sync:fanout-skill".to_string(),
                component: "artifact_sync".to_string(),
                expected_generation: legacy_debt.desired_generation,
                observed_generation: None,
                reason: "legacy untracked sync debt".to_string(),
                retryable: true,
            }],
            artifact_removals: Vec::new(),
            artifact_syncs: vec![SkillArtifactSyncDebt {
                name: "fanout-skill".to_string(),
                force: false,
            }],
            artifact_enablements: Vec::new(),
        });
        legacy_debt
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let replayed = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(replayed.status, SkillSettlementStatus::Degraded);
        let after_replay = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(after_replay.repair_debt.is_none());
        let next_mutation = reopened
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(next_mutation.status, SkillSettlementStatus::Settled);
        Ok(())
    }

    #[tokio::test]
    async fn installed_artifact_enablement_debt_replays_after_restart() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test installed artifact repair debt")
            .map_err(|error| error.to_string())?;
        record_install_repair_debt(
            &fixture.state,
            &flow,
            fixture.enabled_config_path.clone(),
            "fanout-skill",
            "simulated policy commit failure",
        )
        .await
        .map_err(|error| error.to_string())?;
        flow.settle(Some("simulated policy commit failure".to_string()));
        let committed = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(committed.repair_debt.as_ref().is_some_and(|debt| {
            debt.artifact_enablements
                .iter()
                .any(|name| name == "fanout-skill")
        }));

        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let replayed = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(replayed.status, SkillSettlementStatus::Settled);
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            repaired
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn enabled_skill_content_change_advances_generation_on_refresh() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let first = fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        std::fs::write(
            fixture.temp.path().join("skills/fanout-skill/SKILL.md"),
            "---\nname: fanout-skill\ndescription: changed fixture\n---\nchanged",
        )
        .map_err(|error| error.to_string())?;
        let refreshed = fixture
            .state
            .extension_control
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            refreshed.desired_generation,
            first.desired_generation.saturating_add(1)
        );
        assert_eq!(refreshed.status, SkillSettlementStatus::Settled);
        assert_ne!(refreshed.content_identity, first.content_identity);
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_cas_cannot_overwrite_newer_durable_policy() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let stale = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let newer = fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test stale skill settlement")
            .map_err(|error| error.to_string())?;
        let stale_receipt = settle_skill_generation(
            &flow,
            fixture.enabled_config_path.clone(),
            stale,
            "stale-operation".to_string(),
            false,
            true,
            Vec::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        flow.settle(None);
        assert_eq!(stale_receipt.status, SkillSettlementStatus::Degraded);
        let durable = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(durable.desired_generation, newer.desired_generation);
        assert_eq!(durable.content_identity, newer.content_identity);
        assert!(
            durable
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| !entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_is_rejected_before_runtime_fanout() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let stale = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test pre-fanout generation CAS")
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .reconcile_skill_config(
                &fixture.state,
                &flow,
                stale,
                "stale-prefanout".to_string(),
                false,
                true,
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        flow.settle(None);
        assert_eq!(receipt.status, SkillSettlementStatus::Degraded);
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            assert!(
                !skill_source_present(
                    &target.primary_agent(),
                    "fanout-skill",
                    &user_skill_source("fanout-skill"),
                )
                .await
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn mid_fanout_failure_keeps_durable_policy_and_records_repair_debt() -> Result<(), String>
    {
        let fixture = fanout_fixture(3).await?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let middle_workspace = fixture
            .workspaces
            .get(1)
            .ok_or_else(|| "middle workspace fixture missing".to_string())?;
        let failing = targets
            .iter()
            .find(|target| target.scope() == middle_workspace.id.as_str())
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "middle workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        let receipt = fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Degraded);
        assert!(receipt.durable_committed);
        assert!(receipt.repair_debt.is_some());
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let first = targets
            .iter()
            .next()
            .ok_or_else(|| "global target missing".to_string())?;
        assert!(
            skill_source_present(
                &first.primary_agent(),
                "fanout-skill",
                &user_skill_source("fanout-skill"),
            )
            .await,
            "a settled target was incorrectly rolled back"
        );
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(config.desired_generation > config.settled_generation);
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn stale_artifact_operations_cannot_overwrite_or_remove_a_reinstall() -> Result<(), String>
    {
        let fixture = fanout_fixture(1).await?;
        let source_parent = fixture.temp.path().join("operation-sources");
        let source = source_parent.join("operation-skill");
        std::fs::create_dir_all(&source).map_err(|error| error.to_string())?;
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: operation-skill\ndescription: first install\n---\nfirst",
        )
        .map_err(|error| error.to_string())?;
        let service = Arc::clone(&fixture.state.extension_control);
        service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .uninstall_skill_with_operation(
                &fixture.state,
                "old-uninstall-operation",
                "operation-skill",
            )
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: operation-skill\ndescription: reinstall\n---\nsecond",
        )
        .map_err(|error| error.to_string())?;
        service
            .install_skill_with_operation(
                &fixture.state,
                "new-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let installed_path = fixture.temp.path().join("skills/operation-skill/SKILL.md");
        std::fs::write(
            &installed_path,
            "---\nname: operation-skill\ndescription: local edit\n---\nkeep-me",
        )
        .map_err(|error| error.to_string())?;

        let duplicate_install = service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate_install.settlement.idempotent);
        let after_install_retry =
            std::fs::read_to_string(&installed_path).map_err(|error| error.to_string())?;
        assert!(after_install_retry.contains("keep-me"));

        let conflicting_source = source_parent.join("different-operation-skill");
        std::fs::create_dir_all(&conflicting_source).map_err(|error| error.to_string())?;
        std::fs::write(
            conflicting_source.join("SKILL.md"),
            "---\nname: different-operation-skill\ndescription: conflict\n---\nconflict",
        )
        .map_err(|error| error.to_string())?;
        let conflict = service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                conflicting_source.to_string_lossy().as_ref(),
            )
            .await
            .err()
            .ok_or_else(|| "conflicting install operation was accepted".to_string())?;
        assert!(matches!(
            conflict,
            SkillInstallError::Enable(SkillMutationError::OperationConflict { .. })
        ));
        assert!(
            !fixture
                .temp
                .path()
                .join("skills/different-operation-skill")
                .exists()
        );

        let duplicate_uninstall = service
            .uninstall_skill_with_operation(
                &fixture.state,
                "old-uninstall-operation",
                "operation-skill",
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate_uninstall.settlement.idempotent);
        assert!(!duplicate_uninstall.artifact_removed);
        assert!(installed_path.exists());
        let after_uninstall_retry =
            std::fs::read_to_string(installed_path).map_err(|error| error.to_string())?;
        assert!(after_uninstall_retry.contains("keep-me"));
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("operation-skill")
                .is_some_and(|entry| entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn install_degraded_settlement_keeps_installed_directory_and_desired_state()
    -> Result<(), String> {
        let fixture = fanout_fixture(2).await?;
        let source = fixture.temp.path().join("install-failure");
        std::fs::create_dir_all(&source).map_err(|error| error.to_string())?;
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: install-failure\ndescription: degraded install fixture\n---\ndegraded",
        )
        .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let failing = targets
            .iter()
            .find(|target| target.scope() != "global")
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        let installed = fixture
            .state
            .extension_control
            .install_skill(&fixture.state, source.to_string_lossy().as_ref())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(installed.name, "install-failure");
        assert_eq!(installed.settlement.status, SkillSettlementStatus::Degraded);
        assert!(
            fixture
                .state
                .skills_hub
                .read()
                .await
                .root()
                .join("install-failure")
                .exists()
        );
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("install-failure")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn uninstall_returns_typed_degraded_after_durable_disable() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let failing = targets
            .iter()
            .find(|target| target.scope() != "global")
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .uninstall_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.settlement.status, SkillSettlementStatus::Degraded);
        assert!(receipt.artifact_removed);
        assert!(receipt.artifact_error.is_none());
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| !entry.enabled)
        );
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn uninstall_of_absent_artifact_reports_not_removed() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        std::fs::remove_dir_all(fixture.temp.path().join("skills/fanout-skill"))
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .uninstall_skill_with_operation(
                &fixture.state,
                "absent-artifact-uninstall",
                "fanout-skill",
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(!receipt.artifact_removed);
        assert!(receipt.artifact_error.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn mutation_permit_serializes_two_surface_commands() -> Result<(), String> {
        let service = Arc::new(ExtensionControlService::default());
        let first = service.mutation.lock().await;
        let contender = Arc::clone(&service);
        let second = tokio::spawn(async move {
            let _permit = contender.mutation.lock().await;
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        drop(first);
        second.await.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_plugin_authority_or_follower_reload() -> Result<(), String>
    {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let authority = control.plugin_runtime();
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let follower = targets
            .iter()
            .map(|target| target.plugin_runtime())
            .find(|runtime| !Arc::ptr_eq(runtime, &authority))
            .ok_or_else(|| "plugin follower fixture missing".to_string())?;
        let authority_before = authority.generation_for_test().await;
        let follower_before = follower.generation_for_test().await;
        drop(targets);
        drop(control);

        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller =
            tokio::spawn(async move { caller_service.reload_plugins(&caller_state).await });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("plugin settlement failed during shutdown: {error}"))?;

        assert!(authority.generation_for_test().await > authority_before);
        assert!(follower.generation_for_test().await > follower_before);
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_mcp_commit_and_reconcile_handoff() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller = tokio::spawn(async move {
            caller_service
                .upsert_mcp_server(
                    &caller_state,
                    "caller-drop-mcp".to_string(),
                    echo_agent::mcp::McpServerEntry {
                        command: Some("unused-disabled-mcp".to_string()),
                        disabled: true,
                        ..Default::default()
                    },
                )
                .await
        });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("MCP settlement failed during shutdown: {error}"))?;

        let snapshot = fixture.state.plugins.mcp_config.snapshot().await;
        assert!(
            snapshot
                .mcp_servers
                .get("caller-drop-mcp")
                .is_some_and(|entry| entry.disabled)
        );
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            let pool_snapshot = target
                .pool()
                .mcp_config_snapshot_for_test()
                .await
                .ok_or_else(|| format!("{} pool MCP snapshot missing", target.scope()))?;
            assert!(pool_snapshot.mcp_servers.contains_key("caller-drop-mcp"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_hook_reload() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let hook_dir = control.project_root().join(".eko");
        std::fs::create_dir_all(&hook_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            hook_dir.join("hooks.yaml"),
            "SessionStart:\n  - matcher: \"caller-drop-hook\"\n    hooks:\n      - type: prompt\n        prompt: \"settled\"\n",
        )
        .map_err(|error| error.to_string())?;
        let agent = control.runtime().primary_agent();
        drop(control);

        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller = tokio::spawn(async move { caller_service.reload_hooks(&caller_state).await });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("Hook settlement failed during shutdown: {error}"))?;

        let sources = agent
            .read_async(|agent| {
                Box::pin(async move { agent.hook_registry().read().await.list_sources() })
            })
            .await;
        assert!(
            sources
                .iter()
                .any(|(source, rules)| source == "user_config" && *rules > 0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn plugin_reload_and_skill_enable_share_one_admission() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let reload_state = Arc::clone(&fixture.state);
        let reload_service = Arc::clone(&service);
        let reload =
            tokio::spawn(async move { reload_service.reload_plugins(&reload_state).await });
        let enable_state = Arc::clone(&fixture.state);
        let enable_service = Arc::clone(&service);
        let enable = tokio::spawn(async move {
            enable_service
                .enable_skill(&enable_state, "fanout-skill")
                .await
        });
        tokio::task::yield_now().await;
        assert!(!reload.is_finished());
        assert!(!enable.is_finished());
        drop(permit);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        enable
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            assert!(
                skill_source_present(
                    &target.primary_agent(),
                    "fanout-skill",
                    &user_skill_source("fanout-skill"),
                )
                .await
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn lsp_rebind_and_gui_cli_mutations_share_one_admission() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let target = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let runtime = target.plugin_runtime();
        let root = target.project_root().to_path_buf();
        drop(target);
        let permit = service.mutation.lock().await;
        let watcher_service = Arc::clone(&service);
        let watcher =
            tokio::spawn(async move { watcher_service.rebind_plugin_runtime(runtime, root).await });
        let gui_state = Arc::clone(&fixture.state);
        let gui_service = Arc::clone(&service);
        let gui = tokio::spawn(async move { gui_service.reload_plugins(&gui_state).await });
        let cli_state = Arc::clone(&fixture.state);
        let cli_service = Arc::clone(&service);
        let cli =
            tokio::spawn(async move { cli_service.enable_skill(&cli_state, "fanout-skill").await });
        tokio::task::yield_now().await;
        assert!(!watcher.is_finished());
        assert!(!gui.is_finished());
        assert!(!cli.is_finished());
        drop(permit);
        watcher
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        gui.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        cli.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_snapshot_keeps_exact_focus_through_concurrent_switch() -> Result<(), String> {
        let fixture = fanout_fixture(2).await?;
        let expected = fixture
            .workspaces
            .get(1)
            .ok_or_else(|| "focused workspace fixture missing".to_string())?
            .id
            .to_string();
        let next = fixture
            .workspaces
            .first()
            .cloned()
            .ok_or_else(|| "switch target fixture missing".to_string())?;
        let (entered, release) = fixture
            .state
            .park_next_workspace_control_acquire_for_test()?;
        let read_state = Arc::clone(&fixture.state);
        let read = tokio::spawn(async move {
            read_state
                .extension_control
                .plugin_catalog(&read_state)
                .await
        });
        entered
            .await
            .map_err(|_| "plugin read did not enter control acquisition".to_string())?;
        let switch_state = Arc::clone(&fixture.state);
        let switch = tokio::spawn(async move { switch_state.switch_workspace(next).await });
        release
            .send(())
            .map_err(|_| "plugin read control release was dropped".to_string())?;
        let snapshot = read
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(snapshot.authority_scope, expected);
        switch
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_snapshot_settles_before_concurrent_host_eviction() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let workspace_id = fixture
            .workspaces
            .first()
            .ok_or_else(|| "workspace fixture missing".to_string())?
            .id
            .clone();
        let expected = workspace_id.to_string();
        let (entered, release) = fixture
            .state
            .park_next_workspace_control_acquire_for_test()?;
        let read_state = Arc::clone(&fixture.state);
        let read = tokio::spawn(async move {
            read_state
                .extension_control
                .plugin_catalog(&read_state)
                .await
        });
        entered
            .await
            .map_err(|_| "plugin read did not enter control acquisition".to_string())?;
        let evict_state = Arc::clone(&fixture.state);
        let evict = tokio::spawn(async move {
            evict_state
                .evict_workspace_runtime_if_idle_for_test(&workspace_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!evict.is_finished());
        release
            .send(())
            .map_err(|_| "plugin read control release was dropped".to_string())?;
        let snapshot = read
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(snapshot.authority_scope, expected);
        let _eviction = evict.await.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_install_receipt_carries_authority_and_entry_snapshot() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let source = fixture.temp.path().join("receipt-plugin-source");
        crate::plugin_runtime::PluginRuntimeService::scaffold(&source, "receipt-plugin")
            .map_err(|error| error.to_string())?;
        let expected = fixture
            .workspaces
            .first()
            .ok_or_else(|| "workspace fixture missing".to_string())?
            .id
            .to_string();
        let receipt = fixture
            .state
            .extension_control
            .install_plugin(
                &fixture.state,
                &echo_agent::plugin::InstallSource::Local(source),
                echo_agent::plugin::PluginScope::Project,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.authority_scope, expected);
        assert_eq!(receipt.plugin_id.as_deref(), Some("receipt-plugin"));
        assert_eq!(
            receipt
                .entry
                .as_ref()
                .map(|entry| entry.manifest.name.as_str()),
            Some("receipt-plugin")
        );
        assert_eq!(receipt.target_receipts.len(), 2);
        assert_eq!(receipt.status, PluginSettlementStatus::Settled);
        for target in &receipt.target_receipts {
            assert!(!target.workspace_generation.is_empty());
            assert!(!target.previous_prepared_generation.is_empty());
            assert!(
                target
                    .candidate_prepared_generation
                    .as_deref()
                    .is_some_and(|generation| !generation.is_empty())
            );
            assert_eq!(target.status, PluginTargetSettlementStatus::Settled);
            assert!(target.diagnostics.is_empty());
        }
        Ok(())
    }
}
