//! Chat area widget — modern message display with adaptive theme.

use crate::tui::markdown::render_markdown;
use crate::tui::{MessageRole, Theme, ToolCallStatus, TuiApp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::Widget;

pub struct Chat;

impl Widget for Chat {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;

        // No border — use full area for a seamless look
        let inner = area;

        // Build all chat lines.
        let mut lines: Vec<Line<'static>> = Vec::new();

        for msg in &app.messages {
            render_message(&mut lines, &msg.role, &msg.content, &msg.tool_calls, t);
        }

        // Show streaming text if processing.
        if app.is_processing && !app.streaming_text.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default().fg(t.green).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " streaming...",
                    Style::default().fg(t.yellow).add_modifier(Modifier::ITALIC),
                ),
            ]));
            let md_lines = render_markdown(&app.streaming_text);
            for line in md_lines {
                lines.push(indent_line(line, t.surface0));
            }
        } else if app.is_processing {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default().fg(t.green).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} thinking...", "\u{25dc}"),
                    Style::default().fg(t.yellow).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }

        // chat_scroll is measured from the bottom: 0 = auto-scroll/latest.
        let total_lines = lines.len();
        let visible = inner.height as usize;
        let max_scroll = total_lines.saturating_sub(visible) as u16;
        let scroll = max_scroll.saturating_sub(app.chat_scroll.min(max_scroll));

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, inner);

        // Scrollbar with theme colors
        if total_lines > visible {
            let mut state = ScrollbarState::new(total_lines).position(scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(t.surface0))
                .thumb_style(Style::default().fg(t.overlay0));
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
    t: &Theme,
) {
    match role {
        MessageRole::User => {
            lines.push(Line::from(""));
            // User badge with icon
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} You ", "\u{1f464}"),
                    Style::default().fg(t.blue).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // User content as plain text with subtle indent
            for line in content.lines() {
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(t.subtext)),
                    Span::raw(line.to_string()),
                ]));
            }
        }
        MessageRole::Assistant => {
            lines.push(Line::from(""));
            // Agent badge with icon
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default().fg(t.green).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // Render with markdown
            let md_lines = render_markdown(content);
            for line in md_lines {
                lines.push(indent_line(line, t.surface0));
            }
            // Show tool calls with better icons
            for tc in tool_calls {
                let (icon, color) = match tc.status {
                    ToolCallStatus::Running => ("\u{25dc}", t.yellow),
                    ToolCallStatus::Success => ("\u{2714}", t.green),
                    ToolCallStatus::Failed => ("\u{2718}", t.red),
                };
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(t.subtext)),
                    Span::styled(format!("{} ", icon), Style::default().fg(color)),
                    Span::styled(
                        tc.name.clone(),
                        Style::default().fg(t.lavender).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }
        MessageRole::System => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", "\u{2139}"), Style::default().fg(t.subtext)),
                Span::styled(
                    format!(" {}", content.to_string()),
                    Style::default()
                        .fg(t.subtext)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        MessageRole::Tool => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Tool ", "\u{1f527}"),
                    Style::default().fg(t.mauve).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", content.to_string()),
                    Style::default().fg(t.mauve),
                ),
            ]));
        }
    }
}

/// Indent a rendered markdown line with a subtle guide character.
fn indent_line(line: Line<'static>, guide_color: Color) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "  \u{2502} ",
        Style::default().fg(guide_color),
    )];
    spans.extend(line.spans.into_iter());
    Line::from(spans)
}
