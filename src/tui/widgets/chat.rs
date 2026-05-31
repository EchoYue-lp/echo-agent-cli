//! Chat area widget — modern message display with Catppuccin Mocha palette.

use crate::tui::markdown::render_markdown;
use crate::tui::{MessageRole, ToolCallStatus, TuiApp};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use super::Widget;

// Catppuccin Mocha palette
const BASE: Color = Color::Rgb(30, 30, 46);
const MANTLE: Color = Color::Rgb(24, 24, 37);
const SURFACE0: Color = Color::Rgb(49, 50, 68);
const SURFACE1: Color = Color::Rgb(69, 71, 90);
const OVERLAY0: Color = Color::Rgb(108, 112, 134);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT: Color = Color::Rgb(166, 173, 200);
const BLUE: Color = Color::Rgb(137, 180, 250);
const GREEN: Color = Color::Rgb(166, 227, 161);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const PEACH: Color = Color::Rgb(250, 179, 135);
const MAUVE: Color = Color::Rgb(203, 166, 247);
const TEAL: Color = Color::Rgb(148, 226, 213);
const RED: Color = Color::Rgb(243, 139, 168);
const LAVENDER: Color = Color::Rgb(180, 190, 254);

pub struct Chat;

impl Widget for Chat {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        // Rounded border with subtle color
        let block = Block::default()
            .title(format!(
                " {} {} messages ",
                "\u{1f4ac}",
                app.messages.len()
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SURFACE1));

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
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default()
                        .fg(BASE)
                        .bg(GREEN)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " streaming...",
                    Style::default().fg(YELLOW).add_modifier(Modifier::ITALIC),
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
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default()
                        .fg(BASE)
                        .bg(GREEN)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} thinking...", "\u{25dc}"),
                    Style::default()
                        .fg(YELLOW)
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

        // Scrollbar with Catppuccin colors
        if total_lines > visible {
            let mut state = ScrollbarState::new(total_lines).position(scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(SURFACE0))
                .thumb_style(Style::default().fg(OVERLAY0));
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
            // User badge with icon
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} You ", "\u{1f464}"),
                    Style::default()
                        .fg(BASE)
                        .bg(BLUE)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // User content as plain text with subtle indent
            for line in content.lines() {
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(SUBTEXT)),
                    Span::styled(line.to_string(), Style::default().fg(TEXT)),
                ]));
            }
        }
        MessageRole::Assistant => {
            lines.push(Line::from(""));
            // Agent badge with icon
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Agent ", "\u{2728}"),
                    Style::default()
                        .fg(BASE)
                        .bg(GREEN)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
            // Render with markdown
            let md_lines = render_markdown(content);
            for line in md_lines {
                lines.push(indent_line(line));
            }
            // Show tool calls with better icons
            for tc in tool_calls {
                let (icon, color) = match tc.status {
                    ToolCallStatus::Running => ("\u{25dc}", YELLOW),
                    ToolCallStatus::Success => ("\u{2714}", GREEN),
                    ToolCallStatus::Failed => ("\u{2718}", RED),
                };
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(SUBTEXT)),
                    Span::styled(
                        format!("{} ", icon),
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        tc.name.clone(),
                        Style::default()
                            .fg(LAVENDER)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }
        MessageRole::System => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", "\u{2139}"),
                    Style::default().fg(BASE).bg(SURFACE1),
                ),
                Span::styled(
                    format!(" {}", content.to_string()),
                    Style::default()
                        .fg(SUBTEXT)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        MessageRole::Tool => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} Tool ", "\u{1f527}"),
                    Style::default().fg(BASE).bg(MAUVE),
                ),
                Span::styled(
                    format!(" {}", content.to_string()),
                    Style::default().fg(MAUVE),
                ),
            ]));
        }
    }
}

/// Indent a rendered markdown line with a subtle guide character.
fn indent_line(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::styled("  \u{2502} ", Style::default().fg(SURFACE0))];
    spans.extend(line.spans.into_iter());
    Line::from(spans)
}
