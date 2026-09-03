//! 本地技能注册表
//!
//! 扫描 `~/.eko/skills/` 目录结构，索引每个子目录中的 SKILL.md，
//! 提供搜索和详情查询。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use echo_agent::skills::dependency_probe::missing_binary_names;
use echo_agent::skills::external::{SkillDocument, validate_skill_dir};
use serde::{Deserialize, Serialize};

/// 技能市场条目
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "SkillHubEntry")]
pub struct SkillHubEntry {
    /// 技能名称（kebab-case）
    pub name: String,
    /// 描述
    pub description: String,
    /// 安装路径
    #[ts(type = "string")]
    pub path: PathBuf,
    /// 分类
    #[serde(default)]
    pub category: String,
    /// 是否 baseline 注入
    #[serde(default)]
    pub is_baseline: bool,
    /// 是否内置技能
    #[serde(default)]
    pub is_builtin: bool,
    /// 上游版本
    #[serde(default)]
    pub upstream_version: Option<String>,
    /// 上游来源
    #[serde(default)]
    pub source: Option<String>,
    /// 许可证
    pub license: Option<String>,
    /// 兼容性
    pub compatibility: Option<String>,
    /// 版本
    pub version: Option<String>,
    /// 作者
    pub author: Option<String>,
    /// 标签
    pub tags: Vec<String>,
    /// 是否声明了沙箱策略
    #[serde(default)]
    pub has_sandbox: bool,
    /// 依赖的其他技能
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 缺失的系统二进制(scan 时探测 requires-binaries,前端据此显示 ⚠️)。
    /// 仅含 required 且本机 PATH 上找不到的二进制名。
    #[serde(default)]
    pub missing_dependencies: Vec<String>,
}

/// 本地技能注册表
#[derive(Debug, Clone)]
pub struct SkillsHub {
    /// 扫描根目录
    root: PathBuf,
    /// 索引：name -> entry
    entries: HashMap<String, SkillHubEntry>,
}

impl Default for SkillsHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillsHub {
    /// 创建新的 SkillsHub，指向 `~/.eko/skills/`
    pub fn new() -> Self {
        let root = Self::default_skills_dir();
        let mut hub = Self {
            root,
            entries: HashMap::new(),
        };
        hub.scan();
        hub
    }

    /// 使用自定义根目录
    pub fn with_root(root: PathBuf) -> Self {
        let mut hub = Self {
            root,
            entries: HashMap::new(),
        };
        hub.scan();
        hub
    }

    /// 默认技能目录
    pub fn default_skills_dir() -> PathBuf {
        crate::data_root::user_data_path("skills")
    }

    /// 重新扫描目录
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.scan();
    }

    /// 列出所有条目
    pub fn list(&self) -> Vec<&SkillHubEntry> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// 按名称查找
    pub fn get(&self, name: &str) -> Option<&SkillHubEntry> {
        self.entries.get(name)
    }

    /// 搜索：匹配 name、description、tags
    pub fn search(&self, query: &str) -> Vec<&SkillHubEntry> {
        let q = query.to_lowercase();
        let mut results: Vec<_> = self
            .entries
            .values()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect();
        results.sort_by(|a, b| {
            // name 精确匹配优先
            let a_exact = a.name.to_lowercase() == q;
            let b_exact = b.name.to_lowercase() == q;
            b_exact.cmp(&a_exact).then(a.name.cmp(&b.name))
        });
        results
    }

    /// 获取技能根目录
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 扫描目录，支持扁平结构和 category/<skill>/ 子目录结构。
    fn scan(&mut self) {
        if !self.root.exists() {
            return;
        }
        let Ok(dirs) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in dirs.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Check if this is a category directory (contains subdirectories with SKILL.md)
            // or a skill directory directly (contains SKILL.md).
            let direct_skill_md = path.join("SKILL.md");
            if direct_skill_md.exists() {
                if let Some(hub_entry) = Self::parse_skill_dir(&path, &direct_skill_md) {
                    self.entries.insert(hub_entry.name.clone(), hub_entry);
                }
            } else {
                // Category subdirectory: scan each child for SKILL.md
                let Ok(sub_dirs) = std::fs::read_dir(&path) else {
                    continue;
                };
                for sub in sub_dirs.flatten() {
                    let sub_path = sub.path();
                    if !sub_path.is_dir() {
                        continue;
                    }
                    let skill_md = sub_path.join("SKILL.md");
                    if skill_md.exists()
                        && let Some(mut hub_entry) = Self::parse_skill_dir(&sub_path, &skill_md)
                    {
                        // Inherit category from parent directory name if not set
                        if hub_entry.category.is_empty() {
                            hub_entry.category = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .to_string();
                        }
                        self.entries.insert(hub_entry.name.clone(), hub_entry);
                    }
                }
            }
        }
    }

    /// 解析技能目录
    fn parse_skill_dir(dir: &Path, skill_md: &Path) -> Option<SkillHubEntry> {
        if !validate_skill_dir(dir).is_valid() {
            return None;
        }
        let content = std::fs::read_to_string(skill_md).ok()?;
        let document = SkillDocument::parse_at(&content, skill_md).ok()?;
        let descriptor = document.into_descriptor();
        let metadata_category = descriptor
            .metadata
            .get("category")
            .cloned()
            .unwrap_or_default();
        let source_record = super::install::read_source_record(dir).ok().flatten();
        let metadata_source = source_record
            .as_ref()
            .map(|record| {
                record.subdir.as_ref().map_or_else(
                    || format!("git:{}", record.repo_url),
                    |subdir| format!("git:{}#{subdir}", record.repo_url),
                )
            })
            .or_else(|| descriptor.metadata.get("source").cloned());
        let upstream_version = source_record
            .as_ref()
            .map(|record| record.revision.chars().take(12).collect::<String>())
            .or_else(|| descriptor.metadata.get("upstream-version").cloned());

        let is_baseline = metadata_category == "methodology"
            && super::enabled_skills::DEFAULT_BASELINE_SKILLS.contains(&descriptor.name.as_str());

        let missing_dependencies = missing_binary_names(&descriptor);

        let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let is_builtin = canonical_dir.starts_with(super::enabled_skills::builtin_skills_root());

        Some(SkillHubEntry {
            name: descriptor.name,
            description: descriptor.description,
            path: dir.to_path_buf(),
            category: metadata_category,
            is_baseline,
            is_builtin,
            upstream_version,
            source: metadata_source,
            license: descriptor.license,
            compatibility: descriptor.compatibility,
            version: descriptor.metadata.get("version").cloned(),
            author: descriptor.metadata.get("author").cloned(),
            tags: descriptor
                .metadata
                .get("tags")
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
            has_sandbox: descriptor.sandbox.is_some(),
            depends_on: descriptor.depends_on,
            missing_dependencies,
        })
    }
}
