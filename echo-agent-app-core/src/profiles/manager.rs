//! 档案管理器
//!
//! 提供档案的 CRUD 操作，数据存储在 `~/.echo-agent/profiles/`。

use std::fs;
use std::path::PathBuf;

use chrono::Utc;

use super::types::{Profile, ProfileSummary};

/// 档案持久化管理器
pub struct ProfileManager {
    base_dir: PathBuf,
}

impl ProfileManager {
    /// 创建管理器，自动创建存储目录
    pub fn new() -> Self {
        let base_dir = Self::base_dir();
        fs::create_dir_all(&base_dir).ok();
        Self { base_dir }
    }

    /// 存储基础路径
    pub fn base_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".echo-agent").join("profiles")
    }

    // ── CRUD 操作 ────────────────────────────────────────

    /// 列出所有档案摘要
    pub fn list(&self) -> anyhow::Result<Vec<ProfileSummary>> {
        let mut profiles = Vec::new();
        let entries = match fs::read_dir(&self.base_dir) {
            Ok(e) => e,
            Err(_) => return Ok(profiles),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Ok(data) = fs::read_to_string(&path)
                && let Ok(profile) = serde_json::from_str::<Profile>(&data)
            {
                profiles.push(ProfileSummary {
                    name: profile.name,
                    model: profile.model,
                    theme: profile.theme,
                    active: profile.active,
                    updated_at: profile.updated_at,
                });
            }
        }

        profiles.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(profiles)
    }

    /// 获取档案详情
    pub fn get(&self, name: &str) -> anyhow::Result<Profile> {
        let path = self.profile_path(name);
        let data = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&data)?)
    }

    /// 创建或更新档案
    pub fn save(&self, profile: &Profile) -> anyhow::Result<()> {
        let path = self.profile_path(&profile.name);
        let mut updated = profile.clone();
        updated.updated_at = Utc::now().to_rfc3339();

        // 如果是新文件，设置 created_at
        if !path.exists() {
            if updated.created_at.is_empty() {
                updated.created_at = Utc::now().to_rfc3339();
            }
        } else if let Ok(existing) = self.get(&profile.name) {
            updated.created_at = existing.created_at;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&updated)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// 删除档案
    pub fn delete(&self, name: &str) -> anyhow::Result<()> {
        let path = self.profile_path(name);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// 激活档案（将其设为当前使用，取消其他档案的激活状态）
    pub fn activate(&self, name: &str) -> anyhow::Result<Profile> {
        // 取消所有激活
        let profiles = self.list()?;
        for summary in &profiles {
            if summary.active
                && summary.name != name
                && let Ok(mut p) = self.get(&summary.name)
            {
                p.active = false;
                self.save(&p)?;
            }
        }

        // 激活目标
        let mut profile = self.get(name)?;
        profile.active = true;
        self.save(&profile)?;
        Ok(profile)
    }

    /// 获取当前激活的档案
    pub fn get_active(&self) -> Option<Profile> {
        self.list()
            .ok()?
            .into_iter()
            .find(|s| s.active)
            .and_then(|s| self.get(&s.name).ok())
    }

    // ── 内部辅助 ────────────────────────────────────────

    fn profile_path(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join(format!("{}.json", safe))
    }
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_crud() {
        let manager = ProfileManager::new();
        let profile = Profile::new("test-profile", "qwen3.6-plus");
        manager.save(&profile).unwrap();

        let loaded = manager.get("test-profile").unwrap();
        assert_eq!(loaded.model, "qwen3.6-plus");

        let list = manager.list().unwrap();
        assert!(list.iter().any(|s| s.name == "test-profile"));

        manager.delete("test-profile").unwrap();
    }

    #[test]
    fn test_activate_profile() {
        let manager = ProfileManager::new();

        let a = Profile::new("prof-a", "model-a");
        let b = Profile::new("prof-b", "model-b");
        manager.save(&a).unwrap();
        manager.save(&b).unwrap();

        manager.activate("prof-a").unwrap();
        let active = manager.get_active().unwrap();
        assert_eq!(active.name, "prof-a");

        manager.delete("prof-a").unwrap();
        manager.delete("prof-b").unwrap();
    }
}
