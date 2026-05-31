//! Prompt 版本化管理
//!
//! 支持 Prompt 的 CRUD、版本号、A/B 分组

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Prompt 版本记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVersion {
    pub id: String,
    pub name: String,
    pub content: String,
    pub version: u32,
    pub group: Option<String>, // A/B 分组
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub is_active: bool,
    pub metadata: HashMap<String, String>,
}

/// Prompt 管理器
pub struct PromptManager {
    storage: HashMap<String, Vec<PromptVersion>>,
}

impl PromptManager {
    pub fn new() -> Self {
        Self {
            storage: HashMap::new(),
        }
    }

    /// 创建新 Prompt
    pub fn create(&mut self, name: &str, content: &str, group: Option<String>) -> PromptVersion {
        let now = chrono::Utc::now();
        let version = PromptVersion {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            content: content.to_string(),
            version: 1,
            group,
            created_at: now,
            updated_at: now,
            is_active: true,
            metadata: HashMap::new(),
        };

        self.storage
            .entry(name.to_string())
            .or_default()
            .push(version.clone());

        version
    }

    /// 更新 Prompt（创建新版本）
    pub fn update(&mut self, name: &str, content: &str) -> Option<PromptVersion> {
        let versions = self.storage.get_mut(name)?;
        let last = versions.last()?;

        let new_version = PromptVersion {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            content: content.to_string(),
            version: last.version + 1,
            group: last.group.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_active: true,
            metadata: last.metadata.clone(),
        };

        versions.push(new_version.clone());
        Some(new_version)
    }

    /// 获取最新版本
    pub fn get_latest(&self, name: &str) -> Option<&PromptVersion> {
        self.storage.get(name)?.last()
    }

    /// 获取指定版本
    pub fn get_version(&self, name: &str, version: u32) -> Option<&PromptVersion> {
        self.storage.get(name)?.iter().find(|v| v.version == version)
    }

    /// 列出所有 Prompt
    pub fn list(&self) -> Vec<&String> {
        self.storage.keys().collect()
    }

    /// 获取 Prompt 的所有版本
    pub fn get_versions(&self, name: &str) -> Option<&Vec<PromptVersion>> {
        self.storage.get(name)
    }

    /// 回滚到指定版本
    pub fn rollback(&mut self, name: &str, version: u32) -> Option<PromptVersion> {
        let versions = self.storage.get(name)?;
        let target = versions.iter().find(|v| v.version == version)?.clone();

        let rollback = PromptVersion {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            content: target.content.clone(),
            version: versions.last().map(|v| v.version + 1).unwrap_or(1),
            group: target.group.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_active: true,
            metadata: target.metadata.clone(),
        };

        self.storage.get_mut(name)?.push(rollback.clone());
        Some(rollback)
    }

    /// 删除 Prompt
    pub fn delete(&mut self, name: &str) -> bool {
        self.storage.remove(name).is_some()
    }
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new()
    }
}
