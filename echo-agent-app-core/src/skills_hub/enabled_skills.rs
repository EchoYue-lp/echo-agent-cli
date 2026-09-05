//! Enabled-skills.json configuration management.
//!
//! Manages per-skill enable state and baseline eligibility in
//! `~/.eko/enabled-skills.json`.  Methodology skills that are both
//! `enabled` and `baseline` have their full SKILL.md body injected as a named
//! system-context projection on primary and pooled conversation Agents.
//!
//! 2026-09 简化(ADR 取代 0032):本地单用户场景下,原 durable 状态机
//! (desired/settled generation、operation identities、repair debt 的崩溃
//! 恢复对账)是分布式系统思维的超重配置;现在只保留平铺的
//! {category, enabled, baseline} + 原子写,配置解析失败回退默认值
//! (fail-open),旧文件中的多余字段被直接忽略。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
use echo_agent::skills::external::SkillDocument;
use echo_agent::skills::external::{SkillDescriptor, SkillLoadPolicy};

/// Per-skill entry in enabled-skills.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnableEntry {
    pub category: String,
    pub enabled: bool,
    #[serde(default)]
    pub baseline: bool,
}

/// Root config for `~/.eko/enabled-skills.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnabledSkillsConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub skills: HashMap<String, SkillEnableEntry>,
}

fn default_version() -> u32 {
    2
}

/// Core methodology skill that is baseline-injected by default.
///
/// 2026-09 收敛:前沿模型的 brainstorming/调试/写计划能力已原生化,常驻注入
/// 只保留 verification-before-completion(防"声称完成未验证"仍是真实痛点);
/// 其余 methodology skill 保持可激活,不再全文注入 system prompt。
pub const DEFAULT_BASELINE_SKILLS: &[&str] = &["verification-before-completion"];

const METHODOLOGY_SKILLS: &[&str] = &[
    "brainstorming",
    "plugin-creator",
    "receiving-code-review",
    "requesting-code-review",
    "skill-creator",
    "systematic-debugging",
    "test-driven-development",
    "verification-before-completion",
    "writing-plans",
];

/// Built-in skills shipped with EKO. They are cataloged in the source tree but
/// only the active subset enters an Agent runtime.
///
/// 2026-09 收敛:删除通用能力型(coding/translation/doc-writing/web-search,
/// 内容收编进基础 prompt contract)与 vendored Anthropic 示例(design/
/// automation/research);保留领域特定、methodology、development 与 document
/// 四件套。Anthropic 示例可经 SkillsHub 从 anthropics/skills 安装。
pub const BUILTIN_SKILL_NAMES: &[&str] = &[
    "brainstorming",
    "data-visualization",
    "data-wrangling",
    "dispatching-parallel-agents",
    "docx",
    "evidence-medicine",
    "executing-plans",
    "finishing-a-development-branch",
    "git-workflow",
    "paper-reader",
    "paper-search",
    "pdf",
    "plugin-creator",
    "pptx",
    "receiving-code-review",
    "requesting-code-review",
    "statistical-analysis",
    "subagent-driven-development",
    "skill-creator",
    "systematic-debugging",
    "test-driven-development",
    "using-git-worktrees",
    "verification-before-completion",
    "writing-plans",
    "xlsx",
];

/// Small always-on set. Other shipped skills remain discoverable in the
/// source/catalog but are opt-in through `enabled-skills.json`.
pub const DEFAULT_ACTIVE_BUILTIN_SKILLS: &[&str] = &[
    "brainstorming",
    "git-workflow",
    "plugin-creator",
    "skill-creator",
    "systematic-debugging",
    "verification-before-completion",
    "writing-plans",
];

/// Source-tree fallback root used when the binary runs from a dev checkout.
const DEV_SKILLS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../skills");
static BUNDLED_SKILLS_ROOT: OnceLock<PathBuf> = OnceLock::new();
pub(crate) const METHODOLOGY_BASELINE_PROJECTION: &str = "eko:methodology-baseline";

fn is_bundled_skills_root(root: &Path) -> bool {
    root.join("methodology/verification-before-completion/SKILL.md")
        .is_file()
}

/// Publish the platform resource root resolved by the Tauri host.
///
/// The GUI calls this before Agent bootstrap. Returning `Ok(false)` lets a
/// source-tree `cargo run` fall back to [`DEV_SKILLS_ROOT`] when Tauri has not
/// copied bundle resources next to the development binary.
pub fn configure_bundled_skills_root(root: PathBuf) -> Result<bool, String> {
    if !is_bundled_skills_root(&root) {
        return Ok(false);
    }
    let canonical = std::fs::canonicalize(&root).unwrap_or(root);
    match BUNDLED_SKILLS_ROOT.set(canonical.clone()) {
        Ok(()) => Ok(true),
        Err(_) if BUNDLED_SKILLS_ROOT.get() == Some(&canonical) => Ok(true),
        Err(_) => Err("bundled Skill root was already configured to another path".to_string()),
    }
}

/// Filesystem root of the skills shipped with the EKO application.
///
/// Resolution order (the compile-time `CARGO_MANIFEST_DIR` path only exists on
/// the build machine, so an installed app would otherwise lose all built-in
/// skills):
/// 1. `$EKO_SKILLS_ROOT` — explicit override (packaging scripts, tests).
/// 2. Tauri resource dir — the GUI host resolves it with Tauri's platform API
///    and publishes it before Agent bootstrap.
/// 3. Source-tree path — dev checkout / CLI run from the repo.
///
/// Canonicalized because the framework loader canonicalizes every discovered
/// `SKILL.md` location (resolving symlinks such as `/tmp` → `/private/tmp` on
/// macOS); the policy boundary must use the same form or every builtin would
/// compare as a user skill and bypass `enabled-skills.json`.
pub fn builtin_skills_root() -> PathBuf {
    if let Ok(root) = std::env::var("EKO_SKILLS_ROOT")
        && !root.trim().is_empty()
    {
        let raw = PathBuf::from(root);
        return std::fs::canonicalize(&raw).unwrap_or(raw);
    }

    if let Some(resource) = BUNDLED_SKILLS_ROOT.get() {
        return resource.clone();
    }

    let raw = PathBuf::from(DEV_SKILLS_ROOT);
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Whether a descriptor path belongs to the shipped application Skill tree.
pub fn is_builtin_skill_path(path: &Path) -> bool {
    path.starts_with(builtin_skills_root())
}

/// Non-baseline methodology skills (catalog-only by default).
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
    for name in METHODOLOGY_SKILLS {
        if DEFAULT_BASELINE_SKILLS.contains(name) {
            continue;
        }
        skills.insert(
            name.to_string(),
            SkillEnableEntry {
                category: "methodology".into(),
                enabled: DEFAULT_ACTIVE_BUILTIN_SKILLS.contains(name),
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
        }
    }
}

impl EnabledSkillsConfig {
    fn normalize(&mut self) {
        for (name, entry) in default_skills() {
            self.skills.entry(name).or_insert(entry);
        }
        for (name, entry) in &mut self.skills {
            if METHODOLOGY_SKILLS.contains(&name.as_str()) {
                entry.category = "methodology".to_string();
            }
            if !DEFAULT_BASELINE_SKILLS.contains(&name.as_str()) {
                entry.baseline = false;
            }
        }
    }

    /// Whether a skill may enter the current Agent runtime. Missing entries
    /// use the shipped default only for built-ins so older config files gain
    /// newly shipped core skills without enabling optional bundles.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.skills
            .get(name)
            .map(|entry| entry.enabled)
            .unwrap_or_else(|| DEFAULT_ACTIVE_BUILTIN_SKILLS.contains(&name))
    }

    /// Load the flat policy, failing open to the shipped defaults.
    ///
    /// A missing file is persisted best-effort. Existing unreadable or malformed
    /// files are left untouched so the next explicit mutation can atomically
    /// replace them after applying the user's requested change.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            let config = Self::default();
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                tracing::warn!(path = %path.display(), %error, "Unable to persist default enabled-skills.json; using defaults in memory");
                return Ok(config);
            }
            if let Err(error) = config.save(path) {
                tracing::warn!(path = %path.display(), %error, "Unable to persist default enabled-skills.json; using defaults in memory");
            }
            return Ok(config);
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Unable to read enabled-skills.json; falling back to defaults");
                return Ok(Self::default());
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut config) => {
                config.normalize();
                Ok(config)
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Ignoring malformed enabled-skills.json; falling back to defaults");
                Ok(Self::default())
            }
        }
    }

    /// Write to disk.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        echo_agent::utils::fs::atomic_write(path, &bytes)
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
            Some(entry)
                if entry.category == "methodology"
                    && (!baseline || DEFAULT_BASELINE_SKILLS.contains(&name)) =>
            {
                entry.baseline = baseline;
                Ok(())
            }
            Some(entry) if entry.category == "methodology" => Err(format!(
                "Skill '{name}' is not in the configured methodology baseline"
            )),
            Some(_) => Err(format!(
                "Skill '{name}' is not methodology, cannot be baseline"
            )),
            None => Err(format!("Skill '{name}' not found in config")),
        }
    }
}

/// Apply the configured methodology baseline as a replaceable system projection.
///
/// Primary and pooled conversation Agents call this after creation and every
/// builtin reconcile. Disabling the baseline therefore removes its previous
/// projection immediately instead of leaving text embedded until restart.
pub async fn apply_methodology_baseline(
    agent: &mut echo_agent::agent::ReactAgent,
    config_path: &Path,
) -> Vec<String> {
    let config = EnabledSkillsConfig::load(config_path).unwrap_or_else(|error| {
        tracing::warn!(path = %config_path.display(), %error, "Unable to load enabled Skill policy; using defaults");
        EnabledSkillsConfig::default()
    });
    let names = config
        .enabled_baseline_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut projection = String::new();
    let references = names.iter().map(String::as_str).collect::<Vec<_>>();
    agent
        .skill_registry()
        .inject_methodology_baseline(&mut projection, &references);
    let projection = (!projection.trim().is_empty()).then_some(projection);
    agent
        .replace_system_context_projection(METHODOLOGY_BASELINE_PROJECTION, projection)
        .await;
    names
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

    fn enabled_config(&self) -> EnabledSkillsConfig {
        EnabledSkillsConfig::load(&self.enabled_config_path).unwrap_or_else(|error| {
            tracing::warn!(path = %self.enabled_config_path.display(), %error, "Unable to load enabled Skill policy; using defaults");
            EnabledSkillsConfig::default()
        })
    }
}

impl SkillLoadPolicy for ActiveSkillLoadPolicy {
    fn allows(&self, descriptor: &SkillDescriptor) -> bool {
        if descriptor.location.starts_with(&self.builtin_root) {
            return self.enabled_config().is_enabled(&descriptor.name);
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
    fn default_config_has_single_methodology_baseline() {
        let config = EnabledSkillsConfig::default();
        let base = config.enabled_baseline_names();
        assert_eq!(base, vec!["verification-before-completion".to_string()]);
    }

    #[test]
    fn default_config_activates_only_core_builtin_bundle() {
        let config = EnabledSkillsConfig::default();
        assert!(config.is_enabled("git-workflow"));
        assert!(config.is_enabled("brainstorming"));
        assert!(config.is_enabled("skill-creator"));
        assert!(config.is_enabled("plugin-creator"));
        assert_eq!(
            config
                .skills
                .get("skill-creator")
                .map(|entry| entry.baseline),
            Some(false)
        );
        assert_eq!(
            config
                .skills
                .get("plugin-creator")
                .map(|entry| entry.baseline),
            Some(false)
        );
        assert!(!config.is_enabled("docx"));
        assert!(!config.is_enabled("paper-search"));
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
            SkillDocument::parse("---\nname: paper-search\ndescription: optional\n---\nbody")
                .map_err(|error| error.to_string())?
                .into_descriptor();
        optional.location = builtin_root.join("paper-search/SKILL.md");
        assert!(!policy.allows(&optional));
        let mut core =
            SkillDocument::parse("---\nname: git-workflow\ndescription: core\n---\nbody")
                .map_err(|error| error.to_string())?
                .into_descriptor();
        core.location = builtin_root.join("git-workflow/SKILL.md");
        assert!(policy.allows(&core));
        let mut user = SkillDocument::parse("---\nname: my-skill\ndescription: user\n---\nbody")
            .map_err(|error| error.to_string())?
            .into_descriptor();
        user.location = temp.path().join("user/my-skill/SKILL.md");
        assert!(policy.allows(&user));
        Ok(())
    }

    #[test]
    fn malformed_config_falls_back_to_defaults() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join("enabled-skills.json");
        std::fs::write(&config_path, "{ not valid json").map_err(|error| error.to_string())?;
        let builtin_root = temp.path().join("builtin");
        let policy = ActiveSkillLoadPolicy::new(config_path, builtin_root.clone(), None);
        let mut core =
            SkillDocument::parse("---\nname: git-workflow\ndescription: core\n---\nbody")
                .map_err(|error| error.to_string())?
                .into_descriptor();
        core.location = builtin_root.join("git-workflow/SKILL.md");
        assert!(
            policy.allows(&core),
            "corrupt config must fall back to the default active set"
        );
        Ok(())
    }

    #[test]
    fn load_normalizes_legacy_baselines_and_methodology_categories() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("enabled-skills.json");
        std::fs::write(
            &path,
            r#"{"version":2,"skills":{"brainstorming":{"category":"builtin","enabled":true,"baseline":true},"systematic-debugging":{"category":"methodology","enabled":true,"baseline":true},"verification-before-completion":{"category":"methodology","enabled":true,"baseline":true},"writing-plans":{"category":"methodology","enabled":true,"baseline":true}}}"#,
        )
        .map_err(|error| error.to_string())?;

        let config = EnabledSkillsConfig::load(&path).map_err(|error| error.to_string())?;
        assert_eq!(
            config.enabled_baseline_names(),
            vec!["verification-before-completion"]
        );
        assert_eq!(
            config
                .skills
                .get("brainstorming")
                .map(|entry| entry.category.as_str()),
            Some("methodology")
        );
        Ok(())
    }

    #[test]
    fn bundled_root_accepts_the_nested_catalog_layout() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let marker = temp
            .path()
            .join("methodology/verification-before-completion/SKILL.md");
        let parent = marker
            .parent()
            .ok_or_else(|| "bundle marker has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(marker, "# verification").map_err(|error| error.to_string())?;
        assert!(is_bundled_skills_root(temp.path()));
        Ok(())
    }

    #[test]
    fn legacy_config_extra_fields_are_ignored() -> Result<(), String> {
        // 旧 durable 状态机留下的 desired_generation/repair_debt 等字段直接忽略。
        let config: EnabledSkillsConfig = serde_json::from_str(
            r#"{"version":2,"skills":{"git-workflow":{"category":"builtin","enabled":false,"baseline":false}},"desired_generation":9,"repair_debt":null}"#,
        )
        .map_err(|error| error.to_string())?;
        assert!(!config.is_enabled("git-workflow"));
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
        config.set_enabled("verification-before-completion", false);
        assert!(config.enabled_baseline_names().is_empty());
        config.set_enabled("verification-before-completion", true);
        assert_eq!(
            config.enabled_baseline_names(),
            vec!["verification-before-completion"]
        );
    }
}
