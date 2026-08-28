//! 上下文感知的自动补全器
//!
//! 支持斜杠命令、工具名、文件名、模型名的智能补全。

use reedline::{Completer, Span};
use std::collections::BTreeSet;

/// 增强的上下文感知补全器
pub struct EnhancedCompleter {
    /// 斜杠命令列表
    commands: BTreeSet<String>,
    /// 工具名列表 (动态更新)
    tool_names: BTreeSet<String>,
    /// 可用模型列表
    model_names: BTreeSet<String>,
    /// MCP 服务名列表
    mcp_servers: BTreeSet<String>,
    /// 技能名列表
    skill_names: BTreeSet<String>,
}

impl EnhancedCompleter {
    pub fn new() -> Self {
        let commands = BTreeSet::from_iter(
            [
                "/help",
                "/h",
                "/?",
                "/exit",
                "/quit",
                "/q",
                "/reset",
                "/r",
                "/clear",
                "/cls",
                "/tools",
                "/t",
                "/skills",
                "/sk",
                "/mcp",
                "/m",
                "/history",
                "/hist",
                "/compress",
                "/cp",
                "/stats",
                "/st",
                "/model",
                "/attach",
                "/remember",
                "/forget",
                "/system",
                "/sys",
                "/save",
                "/sessions",
                "/ss",
                "/verbose",
                "/inspect",
                "/export",
                "/profile",
                "/prof",
                "/debug",
                "/dbg",
                "/project",
                "/proj",
                "/cost",
                "/undo",
                "/u",
            ]
            .iter()
            .map(|s| s.to_string()),
        );

        Self {
            commands,
            tool_names: BTreeSet::new(),
            model_names: BTreeSet::new(),
            mcp_servers: BTreeSet::new(),
            skill_names: BTreeSet::new(),
        }
    }

    /// 更新工具名列表
    pub fn set_tool_names(&mut self, names: Vec<String>) {
        self.tool_names = names.into_iter().collect();
    }

    /// 更新 MCP 服务列表
    pub fn set_mcp_servers(&mut self, names: Vec<String>) {
        self.mcp_servers = names.into_iter().collect();
    }

    /// 更新技能列表
    pub fn set_skill_names(&mut self, names: Vec<String>) {
        self.skill_names = names.into_iter().collect();
    }

    /// 根据输入上下文决定补全源
    fn get_candidates(&self, input: &str) -> Vec<String> {
        let input = input.trim();

        // 斜杠命令补全 (仅当没有空格时，即正在输入命令本身)
        if input.starts_with('/') && !input.contains(' ') {
            return self
                .commands
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect();
        }

        // /model 后补全模型名
        if input.starts_with("/model ") || input.starts_with("/m ") {
            let prefix = input.split_whitespace().last().unwrap_or("");
            return self
                .model_names
                .iter()
                .filter(|m| m.starts_with(prefix))
                .cloned()
                .collect();
        }

        Vec::new()
    }
}

impl Completer for EnhancedCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<reedline::Suggestion> {
        let prefix = &line[..pos];
        let text = extract_word_before_cursor(prefix);

        if text.is_empty() {
            return Vec::new();
        }

        // Pass the full prefix for context-aware completion (for example, `/model qw`).
        let candidates = self.get_candidates(prefix.trim());

        let start = pos.saturating_sub(text.len());
        let offset = start;

        candidates
            .into_iter()
            .map(|c| reedline::Suggestion {
                value: c,
                description: None,
                extra: None,
                style: None,
                span: Span {
                    start: offset,
                    end: pos,
                },
                append_whitespace: !text.ends_with(' '),
            })
            .collect()
    }
}

impl Default for EnhancedCompleter {
    fn default() -> Self {
        Self::new()
    }
}

/// 提取光标前的单词 (用于补全)
fn extract_word_before_cursor(line: &str) -> &str {
    let trimmed = line.trim_end();
    trimmed
        .rsplit_once(|c: char| c.is_whitespace())
        .map(|(_, last)| last)
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_completion() {
        let completer = EnhancedCompleter::new();
        let candidates = completer.get_candidates("/hel");
        assert!(candidates.contains(&"/help".to_string()));
    }

    #[test]
    fn completion_keeps_canonical_sessions_and_drops_legacy_load() {
        let completer = EnhancedCompleter::new();

        assert_eq!(
            completer.get_candidates("/sess"),
            vec!["/sessions".to_string()]
        );
        assert!(completer.get_candidates("/lo").is_empty());
    }

    #[test]
    fn test_model_completion() {
        let mut completer = EnhancedCompleter::new();
        assert!(completer.get_candidates("/model qw").is_empty());

        completer.model_names.insert("qwen3.5-plus".to_string());
        let candidates = completer.get_candidates("/model qw");
        assert_eq!(candidates, vec!["qwen3.5-plus"]);
    }
}
