//! 本地技能注册表
//!
//! 扫描 `~/.echo-agent/skills/` 目录结构，索引每个子目录中的 SKILL.md，
//! 提供搜索和详情查询。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 技能市场条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHubEntry {
    /// 技能名称（kebab-case）
    pub name: String,
    /// 描述
    pub description: String,
    /// 安装路径
    pub path: PathBuf,
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
    /// 是否已加载到 agent
    pub loaded: bool,
    /// 是否声明了沙箱策略
    #[serde(default)]
    pub has_sandbox: bool,
    /// 依赖的其他技能
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// 本地技能注册表
#[derive(Debug, Clone)]
pub struct SkillsHub {
    /// 扫描根目录
    root: PathBuf,
    /// 索引：name -> entry
    entries: HashMap<String, SkillHubEntry>,
    /// 已加载到 agent 的技能名集合
    loaded_skills: Vec<String>,
}

impl SkillsHub {
    /// 创建新的 SkillsHub，指向 `~/.echo-agent/skills/`
    pub fn new() -> Self {
        let root = Self::default_skills_dir();
        let mut hub = Self {
            root,
            entries: HashMap::new(),
            loaded_skills: Vec::new(),
        };
        hub.scan();
        hub
    }

    /// 使用自定义根目录
    pub fn with_root(root: PathBuf) -> Self {
        let mut hub = Self {
            root,
            entries: HashMap::new(),
            loaded_skills: Vec::new(),
        };
        hub.scan();
        hub
    }

    /// 默认技能目录
    pub fn default_skills_dir() -> PathBuf {
        let home = std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".echo-agent").join("skills")
    }

    /// 重新扫描目录
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.scan();
    }

    /// 更新已加载技能列表
    pub fn set_loaded_skills(&mut self, names: Vec<String>) {
        self.loaded_skills = names;
        for entry in self.entries.values_mut() {
            entry.loaded = self.loaded_skills.contains(&entry.name);
        }
    }

    /// 启用单个技能（添加到已加载列表）
    pub fn enable_skill(&mut self, name: &str) -> Result<(), String> {
        if !self.entries.contains_key(name) {
            return Err(format!("Skill '{}' not found", name));
        }
        if !self.loaded_skills.contains(&name.to_string()) {
            self.loaded_skills.push(name.to_string());
            if let Some(entry) = self.entries.get_mut(name) {
                entry.loaded = true;
            }
        }
        Ok(())
    }

    /// 禁用单个技能（从已加载列表移除）
    pub fn disable_skill(&mut self, name: &str) -> Result<(), String> {
        if !self.entries.contains_key(name) {
            return Err(format!("Skill '{}' not found", name));
        }
        self.loaded_skills.retain(|s| s != name);
        if let Some(entry) = self.entries.get_mut(name) {
            entry.loaded = false;
        }
        Ok(())
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

    /// 扫描目录
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
            let skill_md = path.join("SKILL.md");
            if skill_md.exists()
                && let Some(hub_entry) = Self::parse_skill_dir(&path, &skill_md)
            {
                self.entries.insert(hub_entry.name.clone(), hub_entry);
            }
        }
    }

    /// 解析技能目录
    fn parse_skill_dir(dir: &Path, skill_md: &Path) -> Option<SkillHubEntry> {
        let content = std::fs::read_to_string(skill_md).ok()?;
        let (frontmatter, _body, list_fields) = parse_frontmatter(&content);

        let dir_name = dir.file_name()?.to_str()?.to_string();

        Some(SkillHubEntry {
            name: frontmatter.get("name").cloned().unwrap_or(dir_name),
            description: frontmatter.get("description").cloned().unwrap_or_default(),
            path: dir.to_path_buf(),
            license: frontmatter.get("license").cloned(),
            compatibility: frontmatter.get("compatibility").cloned(),
            version: frontmatter.get("version").cloned(),
            author: frontmatter.get("author").cloned(),
            tags: frontmatter
                .get("tags")
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
            loaded: false,
            has_sandbox: frontmatter.contains_key("sandbox"),
            depends_on: list_fields.get("depends_on").cloned().unwrap_or_default(),
        })
    }
}

/// 简单 YAML frontmatter 解析器
///
/// 支持 `---` 包围的键值对，格式: `key: value`
/// Also extracts simple YAML lists (items starting with `  - value`) into `list_fields`.
fn parse_frontmatter(
    content: &str,
) -> (
    HashMap<String, String>,
    String,
    HashMap<String, Vec<String>>,
) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), content.to_string(), HashMap::new());
    }

    let after_delim = &trimmed[3..];
    let end = after_delim.find("---").unwrap_or(after_delim.len());
    let fm_str = &after_delim[..end];
    let body = after_delim.get(end + 3..).unwrap_or("").trim().to_string();

    let mut map = HashMap::new();
    let mut lists: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_list_key: Option<String> = None;

    for line in fm_str.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() || trimmed_line.starts_with('#') {
            continue;
        }

        // Check if this is a list item (starts with `- `)
        if let Some(stripped) = trimmed_line.strip_prefix("- ") {
            if let Some(ref key) = current_list_key {
                let val = stripped
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !val.is_empty() {
                    lists.entry(key.clone()).or_default().push(val);
                }
            }
            continue;
        }

        // Regular key: value line
        current_list_key = None;
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if val.is_empty() {
                // This might be a list key (value on subsequent lines)
                current_list_key = Some(key.clone());
                lists.insert(key.clone(), Vec::new());
            } else {
                map.insert(key, val);
            }
        }
    }
    (map, body, lists)
}
