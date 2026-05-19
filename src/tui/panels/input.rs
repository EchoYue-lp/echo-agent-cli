//! 输入面板
//!
//! 提供多行文本输入区域，支持光标移动和基本的编辑操作。

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use super::super::theme::TuiColors;

/// 输入面板状态
pub struct InputPanel {
    /// 输入文本
    text: String,
    /// 光标位置（字节偏移）
    cursor_pos: usize,
    /// 是否处于激活状态
    pub focused: bool,
}

impl Default for InputPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl InputPanel {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
            focused: true,
        }
    }

    /// 获取当前输入文本
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 设置文本
    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor_pos = self.text.len();
    }

    /// 清空输入
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    /// 获取输入并清空（用于提交）
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor_pos = 0;
        text
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// 插入字符
    pub fn insert_char(&mut self, c: char) {
        if self.cursor_pos <= self.text.len() {
            self.text.insert(self.cursor_pos, c);
            self.cursor_pos += c.len_utf8();
        }
    }

    /// 删除光标前的字符
    pub fn delete_backward(&mut self) {
        if self.cursor_pos > 0 {
            // Find the char boundary before cursor
            if let Some(pos) = self.prev_char_boundary() {
                self.text.remove(pos);
                self.cursor_pos = pos;
            }
        }
    }

    /// 删除光标后的字符
    pub fn delete_forward(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.text.remove(self.cursor_pos);
        }
    }

    /// 删除光标前的单词
    pub fn delete_word_backward(&mut self) {
        let before = &self.text[..self.cursor_pos];
        let trimmed = before.trim_end();
        if let Some(pos) = before.rfind(|c: char| c.is_whitespace()) {
            if trimmed.is_empty() {
                // Delete whitespace
                self.text.drain(..self.cursor_pos);
                self.cursor_pos = 0;
            } else {
                let start = if trimmed.as_ptr() != before.as_ptr() {
                    pos + 1
                } else if let Some(word_start) = trimmed.rfind(|c: char| c.is_whitespace()) {
                    word_start + 1
                } else {
                    0
                };
                self.text.drain(start..self.cursor_pos);
                self.cursor_pos = start;
            }
        } else {
            self.text.drain(..self.cursor_pos);
            self.cursor_pos = 0;
        }
    }

    /// 光标左移
    pub fn cursor_left(&mut self) {
        if let Some(pos) = self.prev_char_boundary() {
            self.cursor_pos = pos;
        }
    }

    /// 光标右移
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.cursor_pos = self.next_char_boundary();
        }
    }

    /// 光标移到行首
    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// 光标移到行尾
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.text.len();
    }

    fn prev_char_boundary(&self) -> Option<usize> {
        let mut pos = self.cursor_pos;
        while pos > 0 {
            pos -= 1;
            if self.text.is_char_boundary(pos) {
                return Some(pos);
            }
        }
        None
    }

    fn next_char_boundary(&self) -> usize {
        let mut pos = self.cursor_pos;
        while pos < self.text.len() {
            pos += 1;
            if self.text.is_char_boundary(pos) {
                return pos;
            }
        }
        self.text.len()
    }

    /// 渲染面板
    pub fn render(&self, f: &mut Frame, area: Rect, colors: &TuiColors, _focused: bool) {
        let border_style = if self.focused {
            Style::default().fg(colors.highlight)
        } else {
            Style::default().fg(colors.border)
        };

        let block = Block::default()
            .title(" 输入 (Enter 发送, Esc 菜单, Tab 切换面板) ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        // 构建带光标的文本显示
        let display_text = if self.focused {
            let before = &self.text[..self.cursor_pos];
            let after = &self.text[self.cursor_pos..];
            format!("{}▌{}", before, after)
        } else {
            self.text.clone()
        };

        let content = if display_text.is_empty() {
            Text::from(Line::from(Span::styled(
                "输入消息...",
                Style::default().fg(colors.muted),
            )))
        } else {
            Text::from(Line::from(Span::styled(
                &display_text,
                Style::default().fg(colors.user),
            )))
        };

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }
}
