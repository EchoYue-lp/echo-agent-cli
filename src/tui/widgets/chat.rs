//! Chat area widget — displays messages with markdown rendering.

use crate::tui::{MessageRole, ToolCallStatus, TuiApp};
use crate::tui::markdown::render_markdown;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

use super::Widget;

pub struct Chat;

impl Widget for Chat {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let block = Block::default()
            .title(format!(
                " Chat | {} messages ",
                app.messages.len()
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Build all chat lines.
        let mut lines: Vec<Line<'static>> = Vec::new();

        for msg in &app.messages {
            render_message(&mut lines, &msg.role, &msg.content, &msg.tool_calls);
        }

        // Show streaming text if processing.
        if app.is_processing && !app.streaming_text.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " Agent ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " streaming...",
                    Style::default().fg(Color::Yellow),
                ),
            ]));
            let md_lines = render_markdown(&app.streaming_text);
            for line in md_lines {
                lines.push(indent_line(line));
            }
        } else if app.is_processing {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " Agent ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " thinking...",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }

        // Auto-scroll: if chat_scroll is 0, show the bottom.
        let total_lines = lines.len();
        let visible = inner.height as usize;
        let scroll = if app.chat_scroll == 0 && total_lines > visible {
            (total_lines - visible) as u16
        } else {
            app.chat_scroll
        };

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, inner);

        // Scrollbar
        if total_lines > visible {
            let mut state = ScrollbarState::new(total_lines).position(scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(Color::DarkGray));
            f.render_stateful_widget(
                scrollbar,
                Rect {
                    x: inner.x + inner.width - 1,
                    y: inner.y,
                    width: 1,
                    height: inner.height,
                },
                &mut state,
            );
        }
    }
}

fn render_message(
    lines: &mut Vec<Line<'static>>,
    role: &MessageRole,
    content: &str,
    tool_calls: &[crate::tui::ToolCallInfo],
) {
    match role {
        MessageRole::User => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  You  ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // Render user content as plain text (no markdown).
            for line in content.lines() {
                lines.push(Line::from(format!("    {}", line)));
            }
        }
        MessageRole::Assistant => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " Agent ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // Render with markdown.
            let md_lines = render_markdown(content);
            for line in md_lines {
                lines.push(indent_line(line));
            }
            // Show tool calls.
            for tc in tool_calls {
                let (icon, color) = match tc.status {
                    ToolCallStatus::Running => ("~", Color::Yellow),
                    ToolCallStatus::Success => ("+", Color::Green),
                    ToolCallStatus::Failed => ("x", Color::Red),
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        format!("{} {}", icon, tc.name),
                        Style::default().fg(color),
                    ),
                ]));
            }
        }
        MessageRole::System => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " i ",
                    Style::default().fg(Color::Black).bg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {}", content),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        MessageRole::Tool => {
            lines.push(Line::from(vec![
                Span::styled(
                    " Tool ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Magenta),
                ),
                Span::styled(
                    format!(" {}", content),
                    Style::default().fg(Color::Magenta),
                ),
            ]));
        }
    }
}

/// Indent a rendered markdown line by 4 spaces.
fn indent_line(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("    ")];
    spans.extend(line.spans.into_iter());
    Line::from(spans)
}
