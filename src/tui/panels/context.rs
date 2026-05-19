//! 上下文信息面板
//!
//! 展示 Token 用量、模型信息、MCP 状态等上下文数据。

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use super::super::theme::TuiColors;

/// 上下文信息
#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    pub model: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub theme_name: String,
}

/// 上下文面板
pub struct ContextPanel {
    info: ContextInfo,
}

impl Default for ContextPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextPanel {
    pub fn new() -> Self {
        Self {
            info: ContextInfo::default(),
        }
    }

    /// 更新上下文信息
    pub fn update(&mut self, info: ContextInfo) {
        self.info = info;
    }

    /// 更新 Token 计数
    pub fn update_tokens(&mut self, prompt: usize, completion: usize) {
        self.info.prompt_tokens += prompt;
        self.info.completion_tokens += completion;
        self.info.total_tokens = self.info.prompt_tokens + self.info.completion_tokens;
    }

    pub fn set_model(&mut self, model: String) {
        self.info.model = model;
    }

    pub fn set_mcp_servers(&mut self, servers: Vec<String>) {
        self.info.mcp_servers = servers;
    }

    pub fn set_skills(&mut self, skills: Vec<String>) {
        self.info.skills = skills;
    }

    pub fn set_tools(&mut self, tools: Vec<String>) {
        self.info.tools = tools;
    }

    pub fn set_theme(&mut self, name: &str) {
        self.info.theme_name = name.to_string();
    }

    /// 渲染面板
    pub fn render(&self, f: &mut Frame, area: Rect, colors: &TuiColors, _focused: bool) {
        let block = Block::default()
            .title(" 上下文 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(colors.border));

        let mut lines: Vec<Line> = Vec::new();

        // 模型
        lines.push(Line::from(vec![
            Span::styled("模型: ", Style::default().fg(colors.muted)),
            Span::styled(
                &self.info.model,
                Style::default().fg(colors.highlight).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));

        // Token 统计
        let token_style = Style::default().fg(colors.info);
        lines.push(Line::from(Span::styled("━ Token 统计 ━", token_style)));
        lines.push(Line::from(Span::styled(
            format!("  提示:  {}", format_tokens(self.info.prompt_tokens)),
            token_style,
        )));
        lines.push(Line::from(Span::styled(
            format!("  补全:  {}", format_tokens(self.info.completion_tokens)),
            token_style,
        )));
        lines.push(Line::from(Span::styled(
            format!("  合计:  {}", format_tokens(self.info.total_tokens)),
            Style::default().fg(colors.highlight).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // MCP 服务
        let mcp_count = self.info.mcp_servers.len();
        lines.push(Line::from(Span::styled(
            format!("━ MCP 服务 ({} 个) ━", mcp_count),
            Style::default().fg(colors.info),
        )));
        if mcp_count == 0 {
            lines.push(Line::from(Span::styled("  (无)", Style::default().fg(colors.muted))));
        } else {
            for server in &self.info.mcp_servers {
                lines.push(Line::from(Span::styled(
                    format!("  ● {}", server),
                    Style::default().fg(colors.success),
                )));
            }
        }
        lines.push(Line::from(""));

        // 技能
        if !self.info.skills.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("━ 技能 ({} 个) ━", self.info.skills.len()),
                Style::default().fg(colors.info),
            )));
            for skill in &self.info.skills {
                lines.push(Line::from(Span::styled(
                    format!("  ⚡ {}", skill),
                    Style::default().fg(colors.highlight),
                )));
            }
            lines.push(Line::from(""));
        }

        // 工具
        if !self.info.tools.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("━ 工具 ({} 个) ━", self.info.tools.len()),
                Style::default().fg(colors.info),
            )));
            for tool in &self.info.tools {
                lines.push(Line::from(Span::styled(
                    format!("  🔧 {}", tool),
                    Style::default().fg(colors.tool),
                )));
            }
            lines.push(Line::from(""));
        }

        // 主题
        lines.push(Line::from(vec![
            Span::styled("主题: ", Style::default().fg(colors.muted)),
            Span::styled(&self.info.theme_name, Style::default().fg(colors.info)),
        ]));

        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }
}

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
