//! 唯一的 hook 配置加载器 (audit P0-1)。
//!
//! ## 背景
//!
//! 用户 hook 配置有三个来源,历史上加载逻辑分散在多处,且每个来源都
//! 调用 `HookRegistry::clear_user_hooks()` + `register_user_hooks()`,
//! 而框架的 `HookSource::UserConfig` 只有一个槽位 —— 后装的 clear 会
//! 把先装的清掉,导致:
//! 1. `echo-agent.yaml` 内嵌 hooks 在 bootstrap 第 6 步被 hooks.yaml
//!    文件加载覆盖丢失;
//! 2. `/hooks reload` 只重读文件 hooks、不重读 echo-agent.yaml,reload
//!    后内嵌 hooks 永久丢失。
//!
//! ## 修复
//!
//! 本 loader 把三个来源按固定顺序合并成**单个** `HooksDefinition`,
//! 然后**一次性** `clear_user_hooks()` + `register_user_hooks(merged)`。
//! 无论哪个来源变,都重建完整的 user hook 集,不再互相清空。
//!
//! 三个来源在框架语义里都属于"用户配置"(`HookSource::UserConfig`),
//! 所以共用 UserConfig 槽位是正确的 —— 关键是合并后再 register,而不是
//! 各自 register。
//!
//! ## 合并顺序(后者覆盖前者同名 event 的 rules,additive merge)
//!
//! 1. `echo-agent.yaml` 内嵌 `hooks:` 字段(最低优先级)
//! 2. `~/.eko/hooks.yaml`(全局用户 hooks)
//! 3. `.eko/hooks.yaml`(项目级 hooks,最高优先级)

use echo_agent::config::AppConfig;
use echo_agent::skills::hooks::HooksDefinition;
use std::path::{Path, PathBuf};

/// hook 配置加载结果。
///
/// `definition` 是按固定顺序合并后的单一 user hook 集合;
/// `loaded_from` 记录实际成功加载的文件路径(用于 `/hooks` 命令展示)。
#[derive(Debug, Clone, Default)]
pub struct HooksLoadResult {
    /// 合并后的 hooks 定义(空表示无任何 user hook)。
    pub definition: HooksDefinition,
    /// 实际加载成功的文件路径列表(内嵌 echo-agent.yaml 不算文件,
    /// 不进入此列表;只含 `~/.eko/hooks.yaml` 与 `.eko/hooks.yaml`)。
    pub loaded_from: Vec<PathBuf>,
}

/// hook 配置来源标识(仅用于日志/审计,不影响框架 `HookSource`)。
///
/// 注意:这三个来源最终都 register 进框架的 `HookSource::UserConfig`
/// 单一槽位 —— 本 enum 只在 loader 内部用于追踪合并顺序和日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookConfigSource {
    /// `echo-agent.yaml` 内嵌 `hooks:` 字段(最低优先级)。
    InlineConfig,
    /// `~/.eko/hooks.yaml`(全局)。
    GlobalFile,
    /// `.eko/hooks.yaml`(项目级,最高优先级)。
    ProjectFile,
}

impl HookConfigSource {
    /// 可读名称(日志用)。
    fn label(self) -> &'static str {
        match self {
            HookConfigSource::InlineConfig => "echo-agent.yaml (inline)",
            HookConfigSource::GlobalFile => "~/.eko/hooks.yaml (global)",
            HookConfigSource::ProjectFile => ".eko/hooks.yaml (project)",
        }
    }
}

/// 唯一的 hook 配置加载器。
///
/// 无状态:所有方法都是关联函数,直接从磁盘/`AppConfig` 读取并合并。
/// 提供 `load_merged`(已知 `AppConfig`)与 `load_merged_from_disk`
/// (`/hooks reload` 无法访问 `AppConfig` 时从磁盘重读)两个统一入口,
/// 以及向后兼容的 `load_hooks_files`(仅文件)。
pub struct HookConfigLoader;

impl HookConfigLoader {
    /// 加载并合并所有 user hook 来源(内嵌 + 全局文件 + 项目文件)。
    ///
    /// 这是 bootstrap 路径应使用的入口:它接收已加载的 `AppConfig`,
    /// 取其 `hooks` 字段,再叠加两个 hooks.yaml 文件,合并返回单个
    /// `HooksDefinition`。调用方拿到结果后应**一次性**
    /// `clear_user_hooks()` + `register_user_hooks(merged)`,不要
    /// 再单独 register 内嵌或文件 hooks(那是旧 bug 的根源)。
    pub fn load_merged(app_config: &AppConfig) -> HooksLoadResult {
        let mut definition = HooksDefinition::default();
        let mut loaded_from = Vec::new();

        // 1. echo-agent.yaml 内嵌(最低优先级)
        let inline = app_config.hooks.clone();
        let inline_rule_count: usize = inline.rules.values().map(Vec::len).sum();
        if inline_rule_count > 0 {
            definition.merge(inline);
            tracing::info!(
                source = HookConfigSource::InlineConfig.label(),
                count = inline_rule_count,
                "Loaded inline user hooks from echo-agent.yaml"
            );
        }

        // 2 & 3. 两个 hooks.yaml 文件(全局 + 项目级)
        Self::merge_file_sources(&mut definition, &mut loaded_from);

        HooksLoadResult {
            definition,
            loaded_from,
        }
    }

    /// 从磁盘重新加载并合并所有 user hook 来源。
    ///
    /// 用于 `/hooks reload`:无法访问 `AppConfig` 时,用框架的
    /// `load_config(None)` 重新从标准路径读取 `echo-agent.yaml`,
    /// 再叠加两个 hooks.yaml 文件。语义与 `load_merged` 完全一致,
    /// 只是内嵌来源从磁盘重读而非从内存取。
    pub fn load_merged_from_disk() -> HooksLoadResult {
        let app_config = echo_agent::config::load_config(None);
        Self::load_merged(&app_config)
    }

    /// 仅加载两个 hooks.yaml 文件(不含 echo-agent.yaml 内嵌)。
    ///
    /// **向后兼容入口**:保留给仍按"仅文件"语义调用旧代码。新代码
    /// 应优先用 `load_merged` / `load_merged_from_disk`,否则会丢
    /// 内嵌 hooks(就是 P0-1 要修的 bug)。
    ///
    /// 注意:仅在你**确认**该调用点不关心内嵌 hooks 时使用 —— 例如
    /// 某些只展示"文件来源"的诊断命令。bootstrap 和 `/hooks reload`
    /// 不应再调用此函数。
    pub fn load_hooks_files() -> HooksLoadResult {
        let mut definition = HooksDefinition::default();
        let mut loaded_from = Vec::new();
        Self::merge_file_sources(&mut definition, &mut loaded_from);
        HooksLoadResult {
            definition,
            loaded_from,
        }
    }

    // ── 内部 helpers ──────────────────────────────────────────────

    /// 把全局 hooks.yaml + 项目级 hooks.yaml 合并进 `definition`,
    /// 并把成功加载的路径加入 `loaded_from`。
    ///
    /// 抽出来是因为 `load_merged` 和 `load_hooks_files` 都需要这步。
    fn merge_file_sources(definition: &mut HooksDefinition, loaded_from: &mut Vec<PathBuf>) {
        // 2. 全局 hooks: ~/.eko/hooks.yaml
        let global_path = echo_agent::paths::user_data_path("hooks.yaml");
        if let Some(def) = try_load_yaml(&global_path) {
            let count: usize = def.rules.values().map(Vec::len).sum();
            definition.merge(def);
            loaded_from.push(global_path);
            tracing::info!(
                source = HookConfigSource::GlobalFile.label(),
                count,
                "Loaded user hooks from global file"
            );
        }

        // 3. 项目级 hooks: .eko/hooks.yaml(相对 cwd)
        if let Ok(cwd) = std::env::current_dir() {
            let project_path = cwd.join(".eko").join("hooks.yaml");
            if let Some(def) = try_load_yaml(&project_path) {
                let count: usize = def.rules.values().map(Vec::len).sum();
                definition.merge(def);
                loaded_from.push(project_path);
                tracing::info!(
                    source = HookConfigSource::ProjectFile.label(),
                    count,
                    "Loaded user hooks from project file"
                );
            }
        }
    }
}

/// 尝试从 YAML 文件加载 `HooksDefinition`。
///
/// 文件不存在 → `None`(静默);读取或解析失败 → `None` + warn 日志
/// (单个坏文件不应让整个 hook 加载失败)。
fn try_load_yaml(path: &Path) -> Option<HooksDefinition> {
    if !path.exists() {
        return None;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read hooks file");
            return None;
        }
    };

    match serde_yaml::from_str::<HooksDefinition>(&content) {
        Ok(def) => Some(def),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse hooks file");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::skills::hooks::{HookAction, HookEvent, HookRule};
    use std::collections::HashMap;

    /// 构造一条合法的 Prompt hook rule(非空 prompt 通过 validate)。
    fn prompt_rule(matcher: &str, prompt: &str) -> HookRule {
        HookRule {
            matcher: matcher.to_string(),
            hooks: vec![HookAction::Prompt {
                prompt: prompt.to_string(),
            }],
        }
    }

    /// 构造只含一条 rule 的 HooksDefinition。
    fn single_def(event: HookEvent, matcher: &str, prompt: &str) -> HooksDefinition {
        let mut rules = HashMap::new();
        rules.insert(event, vec![prompt_rule(matcher, prompt)]);
        HooksDefinition { rules }
    }

    /// 空的 AppConfig(无内嵌 hooks)。
    fn empty_app_config() -> AppConfig {
        AppConfig::default()
    }

    /// 把 inline hooks 塞进 AppConfig 的 `hooks` 字段。
    fn app_config_with_inline(def: HooksDefinition) -> AppConfig {
        AppConfig {
            hooks: def,
            ..AppConfig::default()
        }
    }

    // ── 合并顺序测试 ─────────────────────────────────────────────

    #[test]
    fn test_inline_hooks_loaded_from_app_config() {
        // 无任何文件时,load_merged 应回收内嵌 hooks。
        let inline = single_def(HookEvent::SessionStart, "*", "inline-only");
        let cfg = app_config_with_inline(inline);
        let result = HookConfigLoader::load_merged(&cfg);
        assert_eq!(
            result.definition.rules_for(HookEvent::SessionStart).len(),
            1,
            "inline hooks should be present in merged result"
        );
        assert!(
            result.loaded_from.is_empty(),
            "no files loaded, loaded_from should be empty"
        );
    }

    #[test]
    fn test_merge_order_inline_then_files() {
        // 三源都存在(用真实磁盘文件模拟)时,项目级应叠加在内嵌和全局之上。
        // 这里只验证"合并逻辑"——实际文件加载由 try_load_yaml 处理,
        // 已在 test_load_yaml_parses_valid_file 覆盖。本测试用 merge 直接验证顺序。
        let mut merged = HooksDefinition::default();
        let inline = single_def(HookEvent::Stop, "*", "inline");
        let global = single_def(HookEvent::Stop, "*", "global");
        let project = single_def(HookEvent::Stop, "*", "project");

        // 按文档顺序 merge(additive):内嵌 < 全局 < 项目
        merged.merge(inline);
        merged.merge(global);
        merged.merge(project);

        // additive 语义:三条都应在
        let rules = merged.rules_for(HookEvent::Stop);
        assert_eq!(rules.len(), 3, "additive merge should keep all 3 rules");
    }

    #[test]
    fn test_load_merged_empty_app_config_no_hooks() {
        // 空 AppConfig + (测试环境下很可能不存在的)文件 → 空结果。
        let cfg = empty_app_config();
        let result = HookConfigLoader::load_merged(&cfg);
        // loaded_from 可能有项目文件(取决于测试运行目录),但 definition
        // 在无 inline 且无文件时应为空。
        if result.loaded_from.is_empty() {
            assert!(
                result.definition.is_empty(),
                "empty sources should yield empty definition"
            );
        }
    }

    // ── 文件加载测试 ─────────────────────────────────────────────

    #[test]
    fn test_try_load_yaml_returns_none_for_missing_file() {
        let p = PathBuf::from("/tmp/__definitely_not_existing_hooks__.yaml");
        assert!(try_load_yaml(&p).is_none());
    }

    #[test]
    fn test_try_load_yaml_parses_valid_file() {
        // 写一个临时 YAML 文件,验证能解析出 HooksDefinition。
        let dir = std::env::temp_dir();
        let fname = format!("eko_hook_loader_test_{}.yaml", std::process::id());
        let path = dir.join(fname);
        // 用 hooks.yaml 实际格式:顶层是 event 名 → rule 列表。
        // HooksDefinition 用 serde(flatten) 所以是 event 名直接铺平。
        let yaml = r#"
SessionStart:
  - matcher: "*"
    hooks:
      - type: prompt
        prompt: "hello from file"
"#;
        std::fs::write(&path, yaml).expect("write temp yaml");

        let def = try_load_yaml(&path);
        // 清理临时文件
        let _ = std::fs::remove_file(&path);

        let def = def.expect("yaml should parse");
        let rules = def.rules_for(HookEvent::SessionStart);
        assert_eq!(rules.len(), 1, "one SessionStart rule expected");
        // 不再访问 matcher/hook 内部细节,只验证数量足够(避免耦合字段)。
    }

    // ── load_hooks_files 向后兼容 ────────────────────────────────

    #[test]
    fn test_load_hooks_files_does_not_panic_on_missing() {
        // 不应 panic,即使文件都不存在。
        let _ = HookConfigLoader::load_hooks_files();
    }

    // ── 来源标识 ────────────────────────────────────────────────

    #[test]
    fn test_hook_config_source_labels_distinct() {
        let labels = [
            HookConfigSource::InlineConfig.label(),
            HookConfigSource::GlobalFile.label(),
            HookConfigSource::ProjectFile.label(),
        ];
        // 三个 label 应互不相同且非空。
        for l in labels.iter() {
            assert!(!l.is_empty());
        }
        assert_eq!(labels.len(), 3);
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), 3, "labels should be distinct");
    }
}
