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
                "/system",
                "/sys",
                "/save",
                "/load",
                "/sessions",
                "/ss",
                "/theme",
                "/output",
                "/verbose",
                "/inspect",
                "/tui",
                "/export",
                "/profile",
                "/prof",
                "/debug",
                "/dbg",
                "/mode",
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
            model_names: BTreeSet::from_iter(
                [
                    "qwen-max",
                    "qwen-plus",
                    "qwen-turbo",
                    "gpt-4",
                    "gpt-3.5-turbo",
                    "claude-3-opus",
                ]
                .iter()
                .map(|s| s.to_string()),
            ),
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

        // /theme 后补全主题名
        if input.starts_with("/theme ") {
            let prefix = input.split_whitespace().last().unwrap_or("");
            return [
                "dark",
                "light",
                "monokai",
                "solarized",
                "dracula",
                "one-dark",
            ]
            .iter()
            .filter(|t| t.starts_with(prefix))
            .map(|s| s.to_string())
            .collect();
        }

        // /output 后补全输出格式
        if input.starts_with("/output ") {
            let prefix = input.split_whitespace().last().unwrap_or("");
            return ["text", "json", "markdown", "table"]
                .iter()
                .filter(|f| f.starts_with(prefix))
                .map(|s| s.to_string())
                .collect();
        }

        // /mode 后补全模式名
        if input.starts_with("/mode ") {
            let prefix = input.split_whitespace().last().unwrap_or("");
            return ["general", "coding", "research", "data", "writing"]
                .iter()
                .filter(|m| m.starts_with(prefix))
                .map(|s| s.to_string())
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

        // Pass full prefix for context-aware completion (e.g. "/theme dar")
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
    fn test_model_completion() {
        let completer = EnhancedCompleter::new();
        let candidates = completer.get_candidates("/model qw");
        assert!(!candidates.is_empty());
    }

    #[test]
    fn test_theme_completion() {
        let completer = EnhancedCompleter::new();
        let candidates = completer.get_candidates("/theme dar");
        assert!(candidates.contains(&"dark".to_string()));
    }

    #[test]
    fn test_output_completion() {
        let completer = EnhancedCompleter::new();
        let candidates = completer.get_candidates("/output js");
        assert!(candidates.contains(&"json".to_string()));
    }
}
