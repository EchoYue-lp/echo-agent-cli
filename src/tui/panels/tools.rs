//! 工具调用面板
//!
//! 实时展示工具调用及其结果。

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use super::super::theme::TuiColors;

/// 工具调用状态
#[derive(Debug, Clone)]
pub enum ToolStatus {
    /// 调用中
    Running,
    /// 成功完成
    Success,
    /// 执行出错
    Error,
}

/// 工具调用记录
#[derive(Debug, Clone)]
pub struct ToolCallEntry {
    pub name: String,
    pub args: String,
    pub output: Option<String>,
    pub status: ToolStatus,
}

/// 工具面板状态
pub struct ToolsPanel {
    /// 活跃的工具调用
    entries: Vec<ToolCallEntry>,
    /// 是否展开详情
    expanded: bool,
}

impl Default for ToolsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsPanel {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            expanded: false,
        }
    }

    /// 切换展开/折叠
    pub fn toggle_expand(&mut self) {
        self.expanded = !self.expanded;
    }

    /// 添加工具调用（运行中）
    pub fn add_tool_call(&mut self, name: &str, args: &str) {
        self.entries.push(ToolCallEntry {
            name: name.to_string(),
            args: args.to_string(),
            output: None,
            status: ToolStatus::Running,
        });
    }

    /// 更新工具调用结果（成功）
    pub fn set_tool_result(&mut self, name: &str, output: &str) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.name == name && e.output.is_none()) {
            entry.output = Some(output.to_string());
            entry.status = ToolStatus::Success;
        }
    }

    /// 更新工具调用结果（错误）
    pub fn set_tool_error(&mut self, name: &str, error: &str) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.name == name && e.output.is_none()) {
            entry.output = Some(error.to_string());
            entry.status = ToolStatus::Error;
        }
    }

    /// 清空工具调用
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 活跃工具数量
    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|e| matches!(e.status, ToolStatus::Running)).count()
    }

    /// 渲染面板
    pub fn render(&self, f: &mut Frame, area: Rect, colors: &TuiColors, _focused: bool) {
        let block = Block::default()
            .title(format!(" 工具 ({}) ", self.entries.len()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(colors.border));

        if self.entries.is_empty() {
            let text = Text::from(Line::from(Span::styled(
                "暂无工具调用",
                Style::default().fg(colors.muted),
            )));
            let paragraph = Paragraph::new(text).block(block);
            f.render_widget(paragraph, area);
            return;
        }

        let mut lines: Vec<Line> = Vec::new();

        for entry in &self.entries {
            let (status_icon, status_color) = match entry.status {
                ToolStatus::Running => ("◌", colors.warning),
                ToolStatus::Success => ("✓", colors.success),
                ToolStatus::Error => ("✗", colors.error),
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", status_icon),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    &entry.name,
                    Style::default().fg(colors.tool).add_modifier(Modifier::BOLD),
                ),
            ]));

            if self.expanded {
                let args_display = if entry.args.len() > 60 {
                    format!("{}...", &entry.args[..57])
                } else {
                    entry.args.clone()
                };
                lines.push(Line::from(Span::styled(
                    format!("  参数: {}", args_display),
                    Style::default().fg(colors.muted),
                )));

                if let Some(ref output) = entry.output {
                    let output_preview: String = output.chars().take(80).collect();
                    let suffix = if output.len() > 80 { "..." } else { "" };
                    lines.push(Line::from(Span::styled(
                        format!("  结果: {}{}", output_preview, suffix),
                        Style::default().fg(colors.info),
                    )));
                }
            }

            lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }
}
