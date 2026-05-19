//! 输出格式控制
//!
//! 支持 Text / Json / Markdown / Table 四种输出模式。

use serde::Serialize;
use crate::types::ToolCallInfo;

/// 输出格式 (用于 --output / -o 标志)
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[derive(Default)]
pub enum OutputFormat {
    /// 人类可读文本 (默认)
    #[clap(name = "text")]
    #[default]
    Text,
    /// 结构化 JSON 输出
    #[clap(name = "json")]
    Json,
    /// 原始 Markdown
    #[clap(name = "markdown")]
    Markdown,
    /// 格式化表格
    #[clap(name = "table")]
    Table,
}


/// 格式化上下文信息
#[derive(Debug, Clone)]
pub struct FormatContext {
    pub model: String,
    pub tokens: usize,
    pub message_count: usize,
    pub tool_calls: Vec<ToolCallInfo>,
    pub duration_ms: Option<u64>,
}

/// 格式化输出结果
#[derive(Debug, Serialize)]
pub struct FormattedOutput {
    pub answer: String,
    pub model: String,
    pub token_estimate: usize,
    pub message_count: usize,
    pub tool_calls: Vec<FormattedToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct FormattedToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub result: String,
    pub success: bool,
}

impl OutputFormat {
    /// 格式化 agent 响应
    pub fn format_response(
        &self,
        answer: &str,
        context: &FormatContext,
    ) -> String {
        match self {
            OutputFormat::Text => answer.to_string(),
            OutputFormat::Json => {
                let output = FormattedOutput {
                    answer: answer.to_string(),
                    model: context.model.clone(),
                    token_estimate: context.tokens,
                    message_count: context.message_count,
                    tool_calls: context
                        .tool_calls
                        .iter()
                        .map(|tc| FormattedToolCall {
                            name: tc.name.clone(),
                            args: tc.args.clone(),
                            result: tc.result.clone(),
                            success: tc.success,
                        })
                        .collect(),
                    duration_ms: context.duration_ms,
                };
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            OutputFormat::Markdown => answer.to_string(),
            OutputFormat::Table => {
                // 简化为纯文本表格
                let mut out = String::new();
                out.push_str(&format!("Answer: {}\n", answer));
                if !context.tool_calls.is_empty() {
                    out.push_str("\nTool Calls:\n");
                    for tc in &context.tool_calls {
                        out.push_str(&format!(
                            "  {} ({})",
                            tc.name,
                            if tc.success { "success" } else { "failed" }
                        ));
                    }
                }
                out
            }
        }
    }
}

impl FormatContext {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            tokens: 0,
            message_count: 0,
            tool_calls: Vec::new(),
            duration_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_text() {
        let ctx = FormatContext::new("test-model");
        let output = OutputFormat::Text.format_response("hello", &ctx);
        assert_eq!(output, "hello");
    }

    #[test]
    fn test_format_json() {
        let ctx = FormatContext::new("test-model");
        let output = OutputFormat::Json.format_response("hello", &ctx);
        assert!(output.contains("\"answer\": \"hello\""));
        assert!(output.contains("\"model\": \"test-model\""));
    }

    #[test]
    fn test_format_markdown() {
        let ctx = FormatContext::new("test-model");
        let output = OutputFormat::Markdown.format_response("**bold**", &ctx);
        assert!(output.contains("**bold**"));
    }
}
