//! 对话面板
//!
//! 渲染用户消息和助手回复，支持滚动回溯。

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use super::super::theme::TuiColors;

/// 单条消息
#[derive(Debug, Clone)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub is_streaming: bool,
}

/// 对话面板状态
pub struct ChatPanel {
    /// 所有消息
    messages: Vec<ChatMessage>,
    /// 滚动偏移（从顶部算起的行数）
    scroll_offset: u16,
    /// 是否自动跟随最新消息
    auto_scroll: bool,
}

impl Default for ChatPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatPanel {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// 添加消息
    pub fn add_message(&mut self, role: MessageRole, content: String) {
        self.messages.push(ChatMessage {
            role,
            content,
            is_streaming: false,
        });
    }

    /// 追加 token 到最后一条消息（流式渲染）
    pub fn append_token(&mut self, token: &str) {
        if let Some(last) = self.messages.last_mut() {
            last.content.push_str(token);
            last.is_streaming = true;
        }
    }

    /// 结束流式输出
    pub fn finish_streaming(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            last.is_streaming = false;
        }
    }

    /// 确保最后一条消息是助手消息（用于流式输出）
    pub fn ensure_assistant_msg(&mut self) {
        if self.messages.last().map(|m| matches!(m.role, MessageRole::Assistant)) != Some(true) {
            self.messages.push(ChatMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                is_streaming: true,
            });
        }
    }

    /// 向上滚动
    pub fn scroll_up(&mut self, lines: u16) {
        self.auto_scroll = false;
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    /// 向下滚动
    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// 滚动到底部
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// 清空消息
    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// 获取消息数量
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// 渲染面板
    pub fn render(&self, f: &mut Frame, area: Rect, colors: &TuiColors, focused: bool) {
        let border_style = if focused {
            Style::default().fg(colors.highlight)
        } else {
            Style::default().fg(colors.border)
        };

        let block = Block::default()
            .title(" 对话 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        // 生成对话文本
        let mut lines: Vec<Line> = Vec::new();

        for msg in &self.messages {
            let (role_prefix, color) = match msg.role {
                MessageRole::User => ("👤 You", colors.user),
                MessageRole::Assistant => ("🤖 Assistant", colors.assistant),
                MessageRole::System => ("ℹ System", colors.info),
            };

            lines.push(Line::from(Span::styled(
                format!("{}:", role_prefix),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));

            // 按换行拆分内容
            for content_line in msg.content.lines() {
                let style = if msg.is_streaming {
                    Style::default().fg(colors.assistant)
                } else {
                    match msg.role {
                        MessageRole::User => Style::default().fg(colors.user),
                        MessageRole::Assistant => Style::default().fg(colors.assistant),
                        MessageRole::System => Style::default().fg(colors.info),
                    }
                };
                lines.push(Line::from(Span::styled(format!("  {}", content_line), style)));
            }

            lines.push(Line::from(""));
        }

        // 流式光标
        if let Some(last) = self.messages.last()
            && last.is_streaming
        {
            lines.push(Line::from(Span::styled(
                "▌",
                Style::default().fg(colors.assistant),
            )));
        }

        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: true })
            .scroll((self.scroll_offset, 0));

        f.render_widget(paragraph, area);
    }
}
