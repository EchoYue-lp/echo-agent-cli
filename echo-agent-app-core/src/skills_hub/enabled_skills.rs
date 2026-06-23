//! Enabled-skills.json configuration management.
//!
//! Manages per-skill enable state and baseline eligibility in
//! `~/.echo-agent/enabled-skills.json`.  Methodology skills that are both
//! `enabled` and `baseline` have their full SKILL.md body injected into the
//! system prompt at session start.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Per-skill entry in enabled-skills.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnableEntry {
    pub category: String,
    pub enabled: bool,
    #[serde(default)]
    pub baseline: bool,
}

/// Root config for `~/.echo-agent/enabled-skills.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnabledSkillsConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub skills: HashMap<String, SkillEnableEntry>,
}

fn default_version() -> u32 {
    1
}

/// Core methodology skills that are baseline-injected by default.
pub const DEFAULT_BASELINE_SKILLS: &[&str] = &[
    "brainstorming",
    "systematic-debugging",
    "verification-before-completion",
    "writing-plans",
];

/// Non-baseline methodology skills (catalog-only by default).
const OTHER_METHODOLOGY: &[&str] = &[
    "test-driven-development",
    "using-superpowers",
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
    skills
}

impl Default for EnabledSkillsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            skills: default_skills(),
        }
    }
}

impl EnabledSkillsConfig {
    /// Load from disk; returns default (and persists it) when file is missing.
    pub fn load(path: &PathBuf) -> std::io::Result<Self> {
        if !path.exists() {
            let config = Self::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = serde_json::to_string_pretty(&config)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            std::fs::write(path, text)?;
            return Ok(config);
        }
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }

    /// Write to disk.
    pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, text)
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
                .unwrap()
                .enabled
        );
    }
}
