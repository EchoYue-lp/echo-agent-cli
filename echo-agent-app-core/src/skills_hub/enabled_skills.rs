//! 独立 Skill 的启用状态管理。
//!
//! EKO 不再携带内置 Skill。`~/.eko/enabled-skills.json` 只保存用户安装
//! Skill 的启用选择；Skill 内容、分类和来源仍由各自 `SKILL.md` 与安装记录负责。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// `enabled-skills.json` 中一个独立 Skill 的状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnableEntry {
    pub enabled: bool,
}

/// `~/.eko/enabled-skills.json` 的根对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnabledSkillsConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub skills: HashMap<String, SkillEnableEntry>,
}

fn default_version() -> u32 {
    3
}

impl Default for EnabledSkillsConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            skills: HashMap::new(),
        }
    }
}

impl EnabledSkillsConfig {
    /// 查询一个已安装 Skill 是否启用；没有持久条目时默认不启用。
    pub fn is_enabled(&self, name: &str) -> bool {
        self.skills.get(name).is_some_and(|entry| entry.enabled)
    }

    /// 读取启用状态。旧版本多余字段会被 serde 忽略，以保留独立 Skill 的启用选择。
    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            let config = Self::default();
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                tracing::warn!(path = %path.display(), %error, "Unable to persist empty enabled-skills.json; using empty state in memory");
                return Ok(config);
            }
            if let Err(error) = config.save(path) {
                tracing::warn!(path = %path.display(), %error, "Unable to persist empty enabled-skills.json; using empty state in memory");
            }
            return Ok(config);
        }

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Unable to read enabled-skills.json; using empty state");
                return Ok(Self::default());
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut config) => {
                config.version = default_version();
                Ok(config)
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Ignoring malformed enabled-skills.json; using empty state");
                Ok(Self::default())
            }
        }
    }

    /// 原子写入启用状态。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        echo_agent::utils::fs::atomic_write(path, &bytes)
    }

    /// 设置一个独立 Skill 的启用状态。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        self.skills
            .insert(name.to_string(), SkillEnableEntry { enabled });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_bundled_skills() {
        let config = EnabledSkillsConfig::default();
        assert_eq!(config.version, 3);
        assert!(config.skills.is_empty());
        assert!(!config.is_enabled("verification-before-completion"));
    }

    #[test]
    fn set_enabled_creates_and_updates_external_entry() {
        let mut config = EnabledSkillsConfig::default();
        config.set_enabled("project-review", true);
        assert!(config.is_enabled("project-review"));
        config.set_enabled("project-review", false);
        assert!(!config.is_enabled("project-review"));
    }

    #[test]
    fn legacy_extra_fields_preserve_external_enablement() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("enabled-skills.json");
        std::fs::write(
            &path,
            r#"{"version":2,"skills":{"project-review":{"category":"methodology","enabled":true,"baseline":true}},"desired_generation":9}"#,
        )
        .map_err(|error| error.to_string())?;

        let config = EnabledSkillsConfig::load(&path).map_err(|error| error.to_string())?;
        assert_eq!(config.version, 3);
        assert!(config.is_enabled("project-review"));
        Ok(())
    }

    #[test]
    fn malformed_config_falls_back_to_empty_state() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("enabled-skills.json");
        std::fs::write(&path, "{ not valid json").map_err(|error| error.to_string())?;

        let config = EnabledSkillsConfig::load(&path).map_err(|error| error.to_string())?;
        assert!(config.skills.is_empty());
        Ok(())
    }
}
