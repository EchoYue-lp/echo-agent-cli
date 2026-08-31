//! Enabled-skills.json configuration management.
//!
//! Manages per-skill enable state and baseline eligibility in
//! `~/.eko/enabled-skills.json`.  Methodology skills that are both
//! `enabled` and `baseline` have their full SKILL.md body injected into the
//! system prompt at session start.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use echo_agent::skills::external::SkillDocument;
use echo_agent::skills::external::{SkillDescriptor, SkillLoadPolicy};

pub(crate) mod u64_string {
    use serde::{Deserialize, Deserializer, Serializer, de};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum WireValue {
        Number(u64),
        String(String),
    }

    fn parse<E: de::Error>(value: WireValue) -> Result<u64, E> {
        match value {
            WireValue::Number(value) => Ok(value),
            WireValue::String(value) => value.parse::<u64>().map_err(E::custom),
        }
    }

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        parse(WireValue::deserialize(deserializer)?)
    }
}

pub(crate) mod option_u64_string {
    use serde::{Deserialize, Deserializer, Serializer, de};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum WireValue {
        Number(u64),
        String(String),
    }

    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        Option::<WireValue>::deserialize(deserializer)?
            .map(|value| match value {
                WireValue::Number(value) => Ok(value),
                WireValue::String(value) => value.parse::<u64>().map_err(de::Error::custom),
            })
            .transpose()
    }
}

pub const MAX_OPERATION_IDENTITIES: usize = 64;
pub const MAX_REPAIR_FAILURES: usize = 64;

/// Per-skill entry in enabled-skills.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnableEntry {
    pub category: String,
    pub enabled: bool,
    #[serde(default)]
    pub baseline: bool,
}

/// Bounded idempotency record stored beside the desired skill policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillOperationIdentity")]
pub struct SkillOperationIdentity {
    pub operation_id: String,
    /// Identity of the admitted command, independent of unrelated policy entries.
    #[serde(default)]
    pub command_identity: String,
    /// Artifact selected by install/uninstall operations. This lets duplicate
    /// retries return the admitted result without touching a newer artifact.
    #[serde(default)]
    pub artifact_name: Option<String>,
    pub content_identity: String,
    #[serde(with = "u64_string")]
    #[ts(type = "string")]
    pub generation: u64,
}

/// One artifact synchronization action that must be retried after restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillArtifactSyncDebt")]
pub struct SkillArtifactSyncDebt {
    pub name: String,
    pub force: bool,
}

/// One structured repair obligation derived from a degraded target receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillRepairTargetDebt")]
pub struct SkillRepairTargetDebt {
    pub target: String,
    pub component: String,
    #[serde(with = "u64_string")]
    #[ts(type = "string")]
    pub expected_generation: u64,
    #[serde(with = "option_u64_string")]
    #[ts(type = "string | null")]
    pub observed_generation: Option<u64>,
    pub reason: String,
    pub retryable: bool,
}

/// Durable evidence that the file policy committed but runtime settlement did not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SkillRepairDebt")]
pub struct SkillRepairDebt {
    #[serde(with = "u64_string")]
    #[ts(type = "string")]
    pub generation: u64,
    pub content_identity: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub target_failures: Vec<SkillRepairTargetDebt>,
    #[serde(default)]
    pub artifact_removals: Vec<String>,
    #[serde(default)]
    pub artifact_syncs: Vec<SkillArtifactSyncDebt>,
    /// Installed artifacts whose enabled policy could not be committed yet.
    #[serde(default)]
    pub artifact_enablements: Vec<String>,
}

/// Root config for `~/.eko/enabled-skills.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnabledSkillsConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub skills: HashMap<String, SkillEnableEntry>,
    #[serde(default, with = "u64_string")]
    pub desired_generation: u64,
    #[serde(default, with = "u64_string")]
    pub settled_generation: u64,
    #[serde(default)]
    pub content_identity: String,
    #[serde(default)]
    pub operation_identities: Vec<SkillOperationIdentity>,
    #[serde(default)]
    pub repair_debt: Option<SkillRepairDebt>,
}

fn default_version() -> u32 {
    2
}

/// Core methodology skills that are baseline-injected by default.
pub const DEFAULT_BASELINE_SKILLS: &[&str] = &[
    "brainstorming",
    "systematic-debugging",
    "verification-before-completion",
    "writing-plans",
];

/// Built-in skills shipped with EKO. They are cataloged in the source tree but
/// only the active subset enters an Agent runtime.
pub const BUILTIN_SKILL_NAMES: &[&str] = &[
    "algorithmic-art",
    "brainstorming",
    "brand-guidelines",
    "canvas-design",
    "claude-api",
    "coding",
    "data-visualization",
    "data-wrangling",
    "dispatching-parallel-agents",
    "doc-writing",
    "docx",
    "evidence-medicine",
    "executing-plans",
    "finishing-a-development-branch",
    "frontend-design",
    "git-workflow",
    "internal-comms",
    "mcp-builder",
    "paper-reader",
    "paper-search",
    "pdf",
    "pptx",
    "receiving-code-review",
    "requesting-code-review",
    "slack-gif-creator",
    "statistical-analysis",
    "subagent-driven-development",
    "systematic-debugging",
    "test-driven-development",
    "theme-factory",
    "translation",
    "using-git-worktrees",
    "verification-before-completion",
    "web-artifacts-builder",
    "web-search",
    "webapp-testing",
    "writing-plans",
    "writing-skills",
    "xlsx",
];

/// Small always-on set. Other shipped skills remain discoverable in the
/// source/catalog but are opt-in through `enabled-skills.json`.
pub const DEFAULT_ACTIVE_BUILTIN_SKILLS: &[&str] = &[
    "coding",
    "brainstorming",
    "systematic-debugging",
    "verification-before-completion",
    "writing-plans",
    "git-workflow",
    "web-search",
    "translation",
];

/// Filesystem root of the skills shipped with the EKO application.
///
/// Canonicalized because the framework loader canonicalizes every discovered
/// `SKILL.md` location (resolving symlinks such as `/tmp` → `/private/tmp` on
/// macOS); the policy boundary must use the same form or every builtin would
/// compare as a user skill and bypass `enabled-skills.json`.
pub fn builtin_skills_root() -> PathBuf {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills");
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Whether a descriptor path belongs to the shipped application Skill tree.
pub fn is_builtin_skill_path(path: &Path) -> bool {
    path.starts_with(builtin_skills_root())
}

/// Non-baseline methodology skills (catalog-only by default).
const OTHER_METHODOLOGY: &[&str] = &[
    "test-driven-development",
    "writing-skills",
    "requesting-code-review",
    "receiving-code-review",
];

fn default_skills() -> HashMap<String, SkillEnableEntry> {
    let mut skills = HashMap::new();
    for name in DEFAULT_BASELINE_SKILLS {
        skills.insert(
            name.to_string(),
            SkillEnableEntry {
                category: "methodology".into(),
                enabled: true,
                baseline: true,
            },
        );
    }
    for name in OTHER_METHODOLOGY {
        skills.insert(
            name.to_string(),
            SkillEnableEntry {
                category: "methodology".into(),
                enabled: false,
                baseline: false,
            },
        );
    }
    for name in BUILTIN_SKILL_NAMES {
        skills
            .entry((*name).to_string())
            .or_insert_with(|| SkillEnableEntry {
                category: "builtin".into(),
                enabled: DEFAULT_ACTIVE_BUILTIN_SKILLS.contains(name),
                baseline: false,
            });
    }
    skills
}

impl Default for EnabledSkillsConfig {
    fn default() -> Self {
        Self {
            version: 2,
            skills: default_skills(),
            desired_generation: 0,
            settled_generation: 0,
            content_identity: String::new(),
            operation_identities: Vec::new(),
            repair_debt: None,
        }
    }
}

impl EnabledSkillsConfig {
    /// Whether a skill may enter the current Agent runtime. Missing entries
    /// use the shipped default only for built-ins so older config files gain
    /// newly shipped core skills without enabling optional bundles.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.skills
            .get(name)
            .map(|entry| entry.enabled)
            .unwrap_or_else(|| DEFAULT_ACTIVE_BUILTIN_SKILLS.contains(&name))
    }

    /// Load from disk; returns default (and persists it) when file is missing.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            let config = Self::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            config.save(path)?;
            return Ok(config);
        }
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::other)
    }

    /// Write to disk.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        echo_agent::utils::fs::atomic_write(path, &bytes)
    }

    /// Retain the newest operation identities without allowing unbounded growth.
    pub fn record_operation(&mut self, identity: SkillOperationIdentity) {
        self.operation_identities.push(identity);
        let overflow = self
            .operation_identities
            .len()
            .saturating_sub(MAX_OPERATION_IDENTITIES);
        if overflow > 0 {
            self.operation_identities.drain(..overflow);
        }
    }

    pub fn operation(&self, operation_id: &str) -> Option<&SkillOperationIdentity> {
        self.operation_identities
            .iter()
            .rev()
            .find(|identity| identity.operation_id == operation_id)
    }

    pub fn set_repair_debt(&mut self, mut debt: SkillRepairDebt) {
        let overflow = debt
            .target_failures
            .len()
            .saturating_sub(MAX_REPAIR_FAILURES);
        if overflow > 0 {
            debt.target_failures.drain(..overflow);
        }
        debt.target_failures.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then(left.component.cmp(&right.component))
                .then(left.expected_generation.cmp(&right.expected_generation))
        });
        debt.target_failures.dedup();
        debt.artifact_removals.sort();
        debt.artifact_removals.dedup();
        let artifact_overflow = debt
            .artifact_removals
            .len()
            .saturating_sub(MAX_REPAIR_FAILURES);
        if artifact_overflow > 0 {
            debt.artifact_removals.drain(..artifact_overflow);
        }
        debt.artifact_syncs.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.force.cmp(&right.force))
        });
        debt.artifact_syncs.dedup();
        let sync_overflow = debt
            .artifact_syncs
            .len()
            .saturating_sub(MAX_REPAIR_FAILURES);
        if sync_overflow > 0 {
            debt.artifact_syncs.drain(..sync_overflow);
        }
        debt.artifact_enablements.sort();
        debt.artifact_enablements.dedup();
        let enablement_overflow = debt
            .artifact_enablements
            .len()
            .saturating_sub(MAX_REPAIR_FAILURES);
        if enablement_overflow > 0 {
            debt.artifact_enablements.drain(..enablement_overflow);
        }
        self.repair_debt = Some(debt);
    }

    /// Names of enabled baseline skills (methodology + enabled + baseline).
    pub fn enabled_baseline_names(&self) -> Vec<&str> {
        self.skills
            .iter()
            .filter(|(_, e)| e.enabled && e.baseline && e.category == "methodology")
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Names of all enabled skills.
    pub fn enabled_skill_names(&self) -> Vec<&str> {
        self.skills
            .iter()
            .filter(|(_, e)| e.enabled)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Toggle a skill on/off.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if let Some(entry) = self.skills.get_mut(name) {
            entry.enabled = enabled;
        }
    }

    /// Toggle baseline for a methodology skill.
    pub fn set_baseline(&mut self, name: &str, baseline: bool) -> Result<(), String> {
        match self.skills.get_mut(name) {
            Some(entry) if entry.category == "methodology" => {
                entry.baseline = baseline;
                Ok(())
            }
            Some(_) => Err(format!(
                "Skill '{name}' is not methodology, cannot be baseline"
            )),
            None => Err(format!("Skill '{name}' not found in config")),
        }
    }
}

/// EKO's runtime activation policy for compiled-in skills plus the existing
/// product policy for user/plugin skills. Discovery and SkillsHub may still
/// catalog every installed artifact; only accepted descriptors are registered
/// into an Agent's runtime, hooks, and intent router.
pub struct ActiveSkillLoadPolicy {
    enabled_config_path: std::path::PathBuf,
    builtin_root: std::path::PathBuf,
    product_policy: Option<Arc<dyn SkillLoadPolicy>>,
}

impl ActiveSkillLoadPolicy {
    pub fn new(
        enabled_config_path: std::path::PathBuf,
        builtin_root: std::path::PathBuf,
        product_policy: Option<Arc<dyn SkillLoadPolicy>>,
    ) -> Self {
        // Same canonicalization rationale as `builtin_skills_root`: the
        // loader canonicalizes descriptor locations, so the boundary must too.
        let builtin_root = std::fs::canonicalize(&builtin_root).unwrap_or(builtin_root);
        Self {
            enabled_config_path,
            builtin_root,
            product_policy,
        }
    }

    fn enabled_config(&self) -> Option<EnabledSkillsConfig> {
        match std::fs::read_to_string(&self.enabled_config_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(config) => Some(config),
                Err(error) => {
                    tracing::warn!(
                        path = %self.enabled_config_path.display(),
                        %error,
                        "Ignoring malformed enabled-skills.json; built-in Skills fail closed"
                    );
                    None
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(EnabledSkillsConfig::default())
            }
            Err(error) => {
                tracing::warn!(
                    path = %self.enabled_config_path.display(),
                    %error,
                    "Unable to read enabled-skills.json; built-in Skills fail closed"
                );
                None
            }
        }
    }
}

impl SkillLoadPolicy for ActiveSkillLoadPolicy {
    fn allows(&self, descriptor: &SkillDescriptor) -> bool {
        if descriptor.location.starts_with(&self.builtin_root) {
            return self
                .enabled_config()
                .is_some_and(|config| config.is_enabled(&descriptor.name));
        }
        self.product_policy
            .as_ref()
            .is_none_or(|policy| policy.allows(descriptor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_core_4_baseline() {
        let config = EnabledSkillsConfig::default();
        let base = config.enabled_baseline_names();
        assert_eq!(base.len(), 4);
        assert!(base.contains(&"brainstorming"));
        assert!(base.contains(&"systematic-debugging"));
        assert!(base.contains(&"verification-before-completion"));
        assert!(base.contains(&"writing-plans"));
    }

    #[test]
    fn default_config_activates_only_core_builtin_bundle() {
        let config = EnabledSkillsConfig::default();
        assert!(config.is_enabled("coding"));
        assert!(config.is_enabled("web-search"));
        assert!(!config.is_enabled("frontend-design"));
        assert!(!config.is_enabled("docx"));
        assert_eq!(
            BUILTIN_SKILL_NAMES
                .iter()
                .filter(|name| config.is_enabled(name))
                .count(),
            DEFAULT_ACTIVE_BUILTIN_SKILLS.len()
        );
    }

    #[test]
    fn active_policy_filters_only_builtin_paths() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join("enabled-skills.json");
        EnabledSkillsConfig::default()
            .save(&config_path)
            .map_err(|error| error.to_string())?;
        let builtin_root = temp.path().join("builtin");
        let policy = ActiveSkillLoadPolicy::new(config_path, builtin_root.clone(), None);
        let mut optional =
            SkillDocument::parse("---\nname: frontend-design\ndescription: optional\n---\nbody")
                .map_err(|error| error.to_string())?
                .into_descriptor();
        optional.location = builtin_root.join("frontend-design/SKILL.md");
        assert!(!policy.allows(&optional));
        let mut core = SkillDocument::parse("---\nname: coding\ndescription: core\n---\nbody")
            .map_err(|error| error.to_string())?
            .into_descriptor();
        core.location = builtin_root.join("coding/SKILL.md");
        assert!(policy.allows(&core));
        let mut user = SkillDocument::parse("---\nname: my-skill\ndescription: user\n---\nbody")
            .map_err(|error| error.to_string())?
            .into_descriptor();
        user.location = temp.path().join("user/my-skill/SKILL.md");
        assert!(policy.allows(&user));
        Ok(())
    }

    #[test]
    fn non_methodology_cannot_be_baseline() {
        let mut config = EnabledSkillsConfig::default();
        config.skills.insert(
            "docx".into(),
            SkillEnableEntry {
                category: "document".into(),
                enabled: true,
                baseline: false,
            },
        );
        assert!(config.set_baseline("docx", true).is_err());
    }

    #[test]
    fn set_enabled_toggles_correctly() {
        let mut config = EnabledSkillsConfig::default();
        config.set_enabled("test-driven-development", true);
        assert!(
            config
                .skills
                .get("test-driven-development")
                .is_some_and(|entry| entry.enabled)
        );
    }

    #[test]
    fn operation_history_is_bounded_to_newest_identities() {
        let mut config = EnabledSkillsConfig::default();
        for index in 0..(MAX_OPERATION_IDENTITIES + 3) {
            config.record_operation(SkillOperationIdentity {
                operation_id: format!("operation-{index}"),
                command_identity: format!("command-{index}"),
                artifact_name: None,
                content_identity: format!("content-{index}"),
                generation: index as u64,
            });
        }
        assert_eq!(config.operation_identities.len(), MAX_OPERATION_IDENTITIES);
        assert!(config.operation("operation-0").is_none());
        assert!(config.operation("operation-3").is_some());
    }

    #[test]
    fn legacy_config_defaults_settlement_metadata() -> Result<(), String> {
        let config: EnabledSkillsConfig = serde_json::from_str(
            r#"{"version":1,"skills":{"legacy":{"category":"methodology","enabled":true,"baseline":false}}}"#,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(config.desired_generation, 0);
        assert_eq!(config.settled_generation, 0);
        assert!(config.content_identity.is_empty());
        assert!(config.operation_identities.is_empty());
        assert!(config.repair_debt.is_none());
        Ok(())
    }

    #[test]
    fn generation_wire_accepts_numbers_and_serializes_lossless_strings() -> Result<(), String> {
        let config: EnabledSkillsConfig = serde_json::from_str(
            r#"{
                "version": 2,
                "skills": {},
                "desired_generation": 9007199254740993,
                "settled_generation": "9007199254740992",
                "content_identity": "content",
                "operation_identities": [{
                    "operation_id": "operation",
                    "command_identity": "command",
                    "content_identity": "content",
                    "generation": 9007199254740993
                }],
                "repair_debt": null
            }"#,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(config.desired_generation, 9_007_199_254_740_993);
        assert_eq!(config.settled_generation, 9_007_199_254_740_992);
        assert!(
            config
                .operation("operation")
                .is_some_and(|identity| identity.artifact_name.is_none())
        );

        let value = serde_json::to_value(config).map_err(|error| error.to_string())?;
        assert_eq!(
            value
                .get("desired_generation")
                .and_then(serde_json::Value::as_str),
            Some("9007199254740993")
        );
        assert_eq!(
            value
                .pointer("/operation_identities/0/generation")
                .and_then(serde_json::Value::as_str),
            Some("9007199254740993")
        );
        assert!(
            value
                .pointer("/operation_identities/0/artifact_name")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
    }
}
