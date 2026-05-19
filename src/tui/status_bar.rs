//! 状态栏
//!
//! 显示当前模型、Token 用量、连接状态等信息。

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::TuiColors;

/// 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connected,
    Streaming,
    Error,
    Idle,
}

impl ConnectionStatus {
    fn icon(&self) -> &str {
        match self {
            ConnectionStatus::Connected => "●",
            ConnectionStatus::Streaming => "◉",
            ConnectionStatus::Error => "✕",
            ConnectionStatus::Idle => "○",
        }
    }
}

/// 状态栏
pub struct StatusBar {
    pub model: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub status: ConnectionStatus,
    pub message_count: usize,
    pub help_text: String,
}

impl StatusBar {
    pub fn new(model: String) -> Self {
        Self {
            model,
            prompt_tokens: 0,
            completion_tokens: 0,
            status: ConnectionStatus::Idle,
            message_count: 0,
            help_text: String::new(),
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect, colors: &TuiColors) {
        let status_style = match self.status {
            ConnectionStatus::Connected => Style::default().fg(colors.success),
            ConnectionStatus::Streaming => Style::default()
                .fg(colors.warning)
                .add_modifier(Modifier::SLOW_BLINK),
            ConnectionStatus::Error => Style::default().fg(colors.error),
            ConnectionStatus::Idle => Style::default().fg(colors.muted),
        };

        let status_text = format!(
            " {} {}  |  模型: {}  |  消息: {}  |  Tokens: {}/{}  |  {} ",
            self.status.icon(),
            match self.status {
                ConnectionStatus::Connected => "已连接",
                ConnectionStatus::Streaming => "生成中",
                ConnectionStatus::Error => "错误",
                ConnectionStatus::Idle => "就绪",
            },
            self.model,
            self.message_count,
            self.prompt_tokens,
            self.completion_tokens,
            self.help_text,
        );

        let spans = vec![
            Span::styled(status_text, status_style),
        ];

        let paragraph = Paragraph::new(Line::from(spans))
            .style(Style::default().fg(colors.bg).bg(colors.surface));

        f.render_widget(paragraph, area);
    }
}
