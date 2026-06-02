//! 丰富输出模块
//!
//! 提供统一的终端输出接口，支持 Markdown 渲染、语法高亮、表格格式化、
//! 进度动画、颜色主题等功能。
//!
//! `OutputRenderer` 是整个 CLI 输出的唯一外观 (Facade),
//! REPL 模式和 TUI 模式都通过它输出，保证样式一致。

#![allow(dead_code)]

pub mod format;
pub mod markdown;
pub mod spinner;
pub mod syntax;
pub mod table;
pub mod theme;

use std::io::Write;
use std::sync::RwLock;

use nu_ansi_term::Color;

pub use format::{FormatContext, OutputFormat};
pub use theme::ColorTheme;

/// 输出渲染器 — CLI 输出的唯一入口
///
/// 封装了所有终端渲染逻辑，在 REPL 和 TUI 模式间共享。
pub struct OutputRenderer {
    config: RwLock<OutputConfig>,
    highlighter: syntax::SyntaxHighlighter,
}

/// 输出配置
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// 是否启用彩色输出
    pub color: bool,
    /// 当前颜色主题
    pub theme: ColorTheme,
    /// 默认输出格式
    pub default_format: OutputFormat,
    /// 终端最大宽度 (字符)
    pub max_width: usize,
    /// 是否显示工具调用详情
    pub show_tool_details: bool,
    /// 是否显示 Token 统计
    pub show_token_stats: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        let width = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(80);

        Self {
            color: true,
            theme: ColorTheme::dark(),
            default_format: OutputFormat::Text,
            max_width: width,
            show_tool_details: true,
            show_token_stats: false,
        }
    }
}

impl OutputRenderer {
    pub fn new(config: OutputConfig) -> Self {
        Self {
            config: RwLock::new(config),
            highlighter: syntax::SyntaxHighlighter::new(),
        }
    }

    // ── 配置方法 ────────────────────────────────────────

    /// 切换颜色开关
    pub fn set_color(&self, enabled: bool) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.color = enabled;
        }
    }

    /// 切换主题
    pub fn set_theme(&self, theme: ColorTheme) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.theme = theme;
        }
    }

    /// 设置默认输出格式
    pub fn set_default_format(&self, format: OutputFormat) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.default_format = format;
        }
    }

    /// 切换 Token 统计显示
    pub fn set_show_token_stats(&self, show: bool) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.show_token_stats = show;
        }
    }

    /// 切换工具调用详情显示
    pub fn set_show_tool_details(&self, show: bool) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.show_tool_details = show;
        }
    }

    /// 获取当前主题
    pub fn theme(&self) -> ColorTheme {
        self.config.read().map(|c| c.theme).unwrap_or_default()
    }

    /// 获取当前配置快照
    pub fn config(&self) -> OutputConfig {
        self.config
            .read()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    // ── 颜色包装 ────────────────────────────────────────

    fn color_enabled(&self) -> bool {
        self.config.read().map(|c| c.color).unwrap_or(true)
    }

    fn paint(&self, color_fn: fn(&ColorTheme) -> Color, text: &str) -> String {
        if !self.color_enabled() {
            return text.to_string();
        }
        let theme = self.theme();
        color_fn(&theme).paint(text).to_string()
    }

    // ── 输出方法 ────────────────────────────────────────

    /// 渲染 Markdown 文本
    pub fn render_markdown(&self, md: &str) {
        let config = self.config();
        markdown::render_markdown_to_terminal(
            md,
            config.color,
            config.theme,
            config.max_width,
            &self.highlighter,
        );
    }

    /// 渲染代码块 (带语法高亮)
    pub fn render_code_block(&self, code: &str, language: &str) {
        let config = self.config();
        if config.color {
            let highlighted = self.highlighter.highlight(code, language);
            for span in &highlighted {
                let styled = span.style.paint(&span.text);
                print!("{}", styled);
            }
            println!();
        } else {
            println!("{}", code);
        }
    }

    /// 渲染格式化表格
    pub fn render_table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let config = self.config();
        table::TableRenderer::render(headers, rows, config.color, config.theme, config.max_width);
    }

    /// 渲染键值对表格
    pub fn render_kv_table(&self, pairs: &[(&str, &str)]) {
        let config = self.config();
        table::TableRenderer::render_kv(pairs, config.color, config.theme, config.max_width);
    }

    // ── 快捷打印 ────────────────────────────────────────

    /// 打印用户消息
    pub fn print_user_message(&self, message: &str) {
        let prefix = self.paint(|t| t.user_color, "👤 You");
        println!("\n{}: {}", prefix, message);
    }

    /// 打印助手前缀
    pub fn print_assistant_prefix(&self) {
        let prefix = self.paint(|t| t.assistant_color, "🤖 Assistant");
        print!("{}: ", prefix);
        std::io::stdout().flush().ok();
    }

    /// 打印流式 Token
    pub fn print_token(&self, token: &str) {
        print!("{}", token);
        std::io::stdout().flush().ok();
    }

    /// 打印工具调用通知
    pub fn print_tool_call(&self, name: &str, args: &serde_json::Value) {
        let label = self.paint(|t| t.tool_color, &format!("🔧 调用工具: {}", name));
        let args_str = serde_json::to_string(args).unwrap_or_default();
        let args_display = if args_str.len() > 200 {
            format!("{}...", &args_str[..200])
        } else {
            args_str
        };
        let args_muted = self.paint(|t| t.muted_color, &args_display);
        println!("\n  {} {}", label, args_muted);
    }

    /// 打印工具结果
    pub fn print_tool_result(&self, name: &str, output: &str, success: bool) {
        let status = if success {
            self.paint(|t| t.success_color, "✓")
        } else {
            self.paint(|t| t.error_color, "✗")
        };
        let preview: String = output.chars().take(300).collect();
        let suffix = if output.len() > 300 { "..." } else { "" };
        let preview_text = self.paint(|t| t.muted_color, &format!("{}{}", preview, suffix));
        println!("  {} {}: {}", status, name, preview_text);
    }

    /// 打印带边框的信息框
    pub fn print_info_box(&self, title: &str, content: &str) {
        let width = self.config().max_width.min(80);
        let border = self.paint(|t| t.border_color, "─".repeat(width - 2).as_str());
        let title_styled = self.paint(|t| t.heading_color, title);

        println!("\n┌{}┐", border);
        println!("│ {} │", title_styled);
        println!("├{}┤", border);
        for line in content.lines() {
            let line_styled = self.paint(|t| t.info_color, line);
            println!("│ {} │", line_styled);
        }
        println!("└{}┘", border);
    }

    /// 打印错误消息
    pub fn print_error(&self, message: &str) {
        let text = self.paint(|t| t.error_color, &format!("❌ {}", message));
        println!("\n{}", text);
    }

    /// 打印成功消息
    pub fn print_success(&self, message: &str) {
        let text = self.paint(|t| t.success_color, &format!("✅ {}", message));
        println!("{}", text);
    }

    /// 打印警告
    pub fn print_warning(&self, message: &str) {
        let text = self.paint(|t| t.warning_color, &format!("⚠️  {}", message));
        println!("{}", text);
    }

    /// 打印信息
    pub fn print_info(&self, message: &str) {
        let text = self.paint(|t| t.info_color, message);
        println!("{}", text);
    }

    /// 打印分割线
    pub fn print_separator(&self) {
        let width = self.config().max_width.min(80);
        let line = self.paint(|t| t.muted_color, "─".repeat(width).as_str());
        println!("{}", line);
    }

    /// 启动进度动画
    pub fn start_spinner(&self, message: &str) -> spinner::SpinnerHandle {
        spinner::SpinnerHandle::new(message)
    }

    /// 打印欢迎横幅
    pub fn print_banner(&self, version: &str) {
        let width = 65;
        let top_border = format!("╭{}╮", "─".repeat(width - 2));
        let border = self.paint(|t| t.heading_color, &top_border);

        println!();
        println!("{}", border);
        self.print_banner_line("", width);
        let title = format!("EchoCoWork v{}", version);
        self.print_banner_line(&format!("{:^width$}", title, width = width - 2), width);
        self.print_banner_line("", width);
        let tagline = "Production-grade AI Agent — ReAct / MCP / Multi-Modal";
        self.print_banner_line(&format!("{:^width$}", tagline, width = width - 2), width);
        self.print_banner_line("", width);
        let hint = "输入消息开始对话，或输入 /help 查看帮助";
        self.print_banner_line(&format!("{:^width$}", hint, width = width - 2), width);
        self.print_banner_line("", width);
        let bottom_border = format!("╰{}╯", "─".repeat(width - 2));
        let border_end = self.paint(|t| t.heading_color, &bottom_border);
        println!("{}", border_end);
        println!();
    }

    /// 打印模式与项目信息
    pub fn print_session_info(
        &self,
        mode: &str,
        model: &str,
        project: Option<&str>,
        instructions: usize,
    ) {
        let mode_icon = match mode {
            "coding" => "💻",
            "research" => "🔬",
            "data" => "📊",
            "writing" => "✍️",
            _ => "💬",
        };
        let mode_label = Color::Cyan
            .bold()
            .paint(format!("  模式: {} {}", mode_icon, mode));
        let model_label = Color::Fixed(12).paint(format!("  模型: {}", model));

        println!("{}", mode_label);
        println!("{}", model_label);

        if let Some(proj) = project {
            let proj_label = Color::Green.paint(format!("  项目: {}", proj));
            println!("{}", proj_label);
        }

        if instructions > 0 {
            let inst_label =
                Color::Yellow.paint(format!("  指令: {} 个项目指令已加载", instructions));
            println!("{}", inst_label);
        }

        println!();
    }

    fn print_banner_line(&self, content: &str, width: usize) {
        let border = self.paint(|t| t.heading_color, "│");
        let padded = format!("{:^width$}", content, width = width - 2);
        let styled = self.paint(|t| t.info_color, &padded);
        println!("{}{}{}", border, styled, border);
    }
}

impl Default for OutputRenderer {
    fn default() -> Self {
        Self::new(OutputConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_config_default() {
        let config = OutputConfig::default();
        assert!(config.color);
        assert!(config.show_tool_details);
        assert!(!config.show_token_stats);
        assert!(config.max_width > 0);
    }

    #[test]
    fn test_output_renderer_creation() {
        let renderer = OutputRenderer::default();
        let theme = renderer.theme();
        assert_eq!(theme.name, "dark");
    }

    #[test]
    fn test_output_renderer_no_color() {
        let renderer = OutputRenderer::new(OutputConfig {
            color: false,
            ..Default::default()
        });
        assert!(!renderer.color_enabled());
    }

    #[test]
    fn test_output_renderer_theme_switch() {
        let renderer = OutputRenderer::default();
        renderer.set_theme(ColorTheme::light());
        assert_eq!(renderer.theme().name, "light");
    }
}
