//! Terminal presentation used by the interactive REPL.
//!
//! The TUI owns an independent ratatui renderer and palette. This module only
//! keeps the small ANSI surface exercised by `src/cli/repl.rs`.

use nu_ansi_term::Color;

/// REPL terminal renderer.
#[derive(Default)]
pub struct OutputRenderer {
    config: OutputConfig,
}

/// REPL output options consumed by the live event sink.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub show_tool_details: bool,
    pub show_token_stats: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            show_tool_details: true,
            show_token_stats: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OutputColors {
    error: Color,
    success: Color,
    warning: Color,
    info: Color,
    heading: Color,
}

impl OutputColors {
    const fn dark() -> Self {
        Self {
            error: Color::Red,
            success: Color::Green,
            warning: Color::Yellow,
            info: Color::Blue,
            heading: Color::LightCyan,
        }
    }
}

impl OutputRenderer {
    /// Return the REPL options captured by a newly-created live event sink.
    pub fn config(&self) -> OutputConfig {
        self.config.clone()
    }

    fn paint(&self, color: Color, text: &str) -> String {
        color.paint(text).to_string()
    }

    pub fn print_error(&self, message: &str) {
        println!(
            "\n{}",
            self.paint(OutputColors::dark().error, &format!("❌ {message}"))
        );
    }

    pub fn print_success(&self, message: &str) {
        println!(
            "{}",
            self.paint(OutputColors::dark().success, &format!("✅ {message}"))
        );
    }

    pub fn print_warning(&self, message: &str) {
        println!(
            "{}",
            self.paint(OutputColors::dark().warning, &format!("⚠️  {message}"))
        );
    }

    pub fn print_info(&self, message: &str) {
        println!("{}", self.paint(OutputColors::dark().info, message));
    }

    pub fn print_banner(&self, version: &str) {
        let width = 65;
        let top_border = format!("╭{}╮", "─".repeat(width - 2));
        println!(
            "\n{}",
            self.paint(OutputColors::dark().heading, &top_border)
        );
        self.print_banner_line("", width);
        let title = format!("EKO v{version}");
        self.print_banner_line(&format!("{title:^width$}", width = width - 2), width);
        self.print_banner_line("", width);
        self.print_banner_line(
            &format!(
                "{:^width$}",
                "Production-grade AI Agent — ReAct / MCP / Multi-Modal",
                width = width - 2
            ),
            width,
        );
        self.print_banner_line("", width);
        self.print_banner_line(
            &format!(
                "{:^width$}",
                "输入消息开始对话，或输入 /help 查看帮助",
                width = width - 2
            ),
            width,
        );
        self.print_banner_line("", width);
        let bottom_border = format!("╰{}╯", "─".repeat(width - 2));
        println!(
            "{}\n",
            self.paint(OutputColors::dark().heading, &bottom_border)
        );
    }

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
        println!(
            "{}",
            Color::Cyan
                .bold()
                .paint(format!("  模式: {mode_icon} {mode}"))
        );
        println!("{}", Color::Fixed(12).paint(format!("  模型: {model}")));
        if let Some(project) = project {
            println!("{}", Color::Green.paint(format!("  项目: {project}")));
        }
        if instructions > 0 {
            println!(
                "{}",
                Color::Yellow.paint(format!("  指令: {instructions} 个项目指令已加载"))
            );
        }
        println!();
    }

    fn print_banner_line(&self, content: &str, width: usize) {
        let border = self.paint(OutputColors::dark().heading, "│");
        let padded = format!("{:^width$}", content, width = width - 2);
        let styled = self.paint(OutputColors::dark().info, &padded);
        println!("{border}{styled}{border}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_config_defaults_match_live_repl_projection() {
        let config = OutputConfig::default();
        assert!(config.show_tool_details);
        assert!(!config.show_token_stats);
    }

    #[test]
    fn renderer_config_returns_an_independent_snapshot() {
        let renderer = OutputRenderer::default();
        let mut snapshot = renderer.config();
        snapshot.show_tool_details = false;
        assert!(!snapshot.show_tool_details);
        assert!(renderer.config().show_tool_details);
    }
}
