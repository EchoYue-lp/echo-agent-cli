//! 本地技能注册表
//!
//! 扫描 `~/.eko/skills/` 目录结构，索引每个子目录中的 SKILL.md，
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
    /// 是否已加载到 agent
    pub loaded: bool,
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
    /// 已加载到 agent 的技能名集合
    loaded_skills: Vec<String>,
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
        echo_agent::paths::user_data_path("skills")
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
        let content = std::fs::read_to_string(skill_md).ok()?;
        let (frontmatter, _body, list_fields) = parse_frontmatter(&content);

        let dir_name = dir.file_name()?.to_str()?.to_string();

        // Extract metadata sub-map. The frontmatter parser flattens YAML
        // nested maps: `metadata:\n  category: x` → key `category` = `x`.
        let metadata_category = frontmatter.get("category").cloned().unwrap_or_default();
        let source_record = super::install::read_source_record(dir).ok().flatten();
        let metadata_source = source_record
            .as_ref()
            .map(|record| {
                record.subdir.as_ref().map_or_else(
                    || format!("git:{}", record.repo_url),
                    |subdir| format!("git:{}#{subdir}", record.repo_url),
                )
            })
            .or_else(|| frontmatter.get("source").cloned());
        let upstream_version = source_record
            .as_ref()
            .map(|record| record.revision.chars().take(12).collect::<String>())
            .or_else(|| frontmatter.get("upstream-version").cloned());

        // Determine baseline status from enabled-skills.json (checked later)
        let is_baseline = match metadata_category.as_str() {
            "methodology" => {
                // Core 4 methodology skills default to baseline
                matches!(
                    dir_name.as_str(),
                    "brainstorming"
                        | "systematic-debugging"
                        | "verification-before-completion"
                        | "writing-plans"
                )
            }
            _ => false,
        };

        // 探测 requires-binaries 中本机缺失的(missing_dependencies 链路)。
        // frontmatter.get 能拿到 "soffice, pdftoppm" 这种逗号串(parse_frontmatter
        // 已把 metadata 子映射扁平化)。inline 探测,不引入跨 crate 类型耦合。
        let missing_dependencies = frontmatter
            .get("requires-binaries")
            .map(|raw| {
                raw.split(',')
                    .map(|s| {
                        s.trim()
                            .trim_matches(|c: char| c == '"' || c == '\'')
                            .to_string()
                    })
                    .filter(|n| !n.is_empty() && !binary_available(n))
                    .collect()
            })
            .unwrap_or_default();

        Some(SkillHubEntry {
            name: frontmatter.get("name").cloned().unwrap_or(dir_name),
            description: frontmatter.get("description").cloned().unwrap_or_default(),
            path: dir.to_path_buf(),
            category: metadata_category,
            is_baseline,
            is_builtin: true, // skills in CARGO_MANIFEST_DIR/skills are built-in
            upstream_version,
            source: metadata_source,
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
            missing_dependencies,
        })
    }
}

/// 探测单个二进制是否在 PATH 上(走 `which` 子进程)。
/// 与 echo-execution/src/skills/dependency_probe.rs 的 binary_available 同实现,
/// 此处 inline 一份避免 registry 反向依赖 echo_execution 的类型。
fn binary_available(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
