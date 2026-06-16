//! Chat area widget — modern message display with adaptive theme.

use crate::tui::markdown::render_markdown;
use crate::tui::{MessageRole, Theme, TuiApp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use unicode_width::UnicodeWidthChar;

use super::Widget;

/// Selection highlight background color.
const SELECTION_BG: Color = Color::Rgb(50, 80, 140);

pub struct Chat;

impl Widget for Chat {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;

        // Only clone lines when selection highlighting is needed (rare: mouse drag).
        let mut lines: Vec<Line<'static>> = if app.normalized_selection().is_some() {
            let mut owned: Vec<Line<'static>> = app.chat_cached_messages_lines.clone();
            owned.extend(app.chat_cached_stream_lines.iter().cloned());
            owned
        } else {
            app.chat_cached_messages_lines
                .iter()
                .chain(app.chat_cached_stream_lines.iter())
                .cloned()
                .collect()
        };

        // chat_scroll is measured from the bottom: 0 = auto-scroll/latest.
        let total_lines = lines.len();
        let visible = inner_height(area);
        let max_scroll = total_lines.saturating_sub(visible);
        let scroll_u16 = max_scroll.saturating_sub(app.chat_scroll.min(max_scroll));
        // ratatui scroll API uses u16; cap at u16::MAX for extremely long conversations.
        let scroll = (scroll_u16.min(u16::MAX as usize)) as u16;

        // Apply selection highlighting to visible lines.
        if let Some((sel_start, sel_end)) = app.normalized_selection() {
            let vis_start = scroll as usize;
            let vis_end = (scroll as usize + visible).min(total_lines);
            for vis_idx in vis_start..vis_end {
                if vis_idx < sel_start.0 || vis_idx > sel_end.0 {
                    continue;
                }
                let start_col = if vis_idx == sel_start.0 {
                    sel_start.1
                } else {
                    0
                };
                let end_col = if vis_idx == sel_end.0 {
                    sel_end.1
                } else {
                    usize::MAX
                };
                let line_idx = vis_idx - vis_start;
                if line_idx < lines.len() {
                    apply_highlight(&mut lines[line_idx], start_col, end_col);
                }
            }
        }

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);

        // Scrollbar with theme colors
        if total_lines > visible {
            let mut state = ScrollbarState::new(total_lines).position(scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(t.surface0))
                .thumb_style(Style::default().fg(t.overlay0));
            f.render_stateful_widget(
                scrollbar,
                Rect {
                    x: area.x + area.width - 1,
                    y: area.y,
                    width: 1,
                    height: area.height,
                },
                &mut state,
            );
        }
    }
}

fn inner_height(area: Rect) -> usize {
    area.height as usize
}

/// Apply selection highlight to a line's spans within [start_col, end_col) visual columns.
fn apply_highlight(line: &mut Line<'static>, start_col: usize, end_col: usize) {
    if start_col >= end_col {
        return;
    }

    let mut new_spans = Vec::new();
    let mut vcol = 0usize;

    for span in line.spans.drain(..) {
        let span_width: usize = span
            .content
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(1))
            .sum();
        let span_start = vcol;
        let span_end = vcol + span_width;

        // Check if this span overlaps with the selection range
        if span_end <= start_col || span_start >= end_col {
            // No overlap — keep original style
            new_spans.push(span);
        } else {
            // Overlap — split into up to 3 parts: before, selected, after
            let text = span.content.as_ref();
            let style = span.style;
            let hl_style = style.bg(SELECTION_BG);

            let mut char_vcol = span_start;
            let mut parts: Vec<(usize, usize, bool)> = Vec::new();

            for (byte_idx, ch) in text.char_indices() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(1);
                let ch_vcol_start = char_vcol;
                let ch_vcol_end = char_vcol + ch_width;

                let is_selected = ch_vcol_start < end_col && ch_vcol_end > start_col;

                // Merge with previous part if same highlight state
                if let Some(last) = parts.last_mut() {
                    if last.2 == is_selected {
                        last.1 = byte_idx + ch.len_utf8();
                    } else {
                        parts.push((byte_idx, byte_idx + ch.len_utf8(), is_selected));
                    }
                } else {
                    parts.push((byte_idx, byte_idx + ch.len_utf8(), is_selected));
                }

                char_vcol = ch_vcol_end;
            }

            // Handle empty span
            if parts.is_empty() {
                new_spans.push(Span::styled(text.to_string(), style));
            } else {
                for (bs, be, highlighted) in &parts {
                    let part_text = &text[*bs..*be];
                    if !part_text.is_empty() {
                        let part_style = if *highlighted { hl_style } else { style };
                        new_spans.push(Span::styled(part_text.to_string(), part_style));
                    }
                }
            }
        }

        vcol = span_end;
    }

    line.spans = new_spans;
}

/// Build ratatui lines for a single chat message (used by cache builder).
pub fn build_chat_lines(
    lines: &mut Vec<Line<'static>>,
    role: &MessageRole,
    content: &str,
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
        }
        MessageRole::System => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", "\u{2139}"), Style::default().fg(t.subtext)),
                Span::styled(
                    format!(" {}", content),
                    Style::default()
                        .fg(t.subtext)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        MessageRole::ToolResult { tool_name } => {
            lines.push(Line::from(""));
            // Header: tool icon + name + summary (first line of output)
            let summary = content.lines().next().unwrap_or(content);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", "\u{1f4dd}", tool_name),
                    Style::default().fg(t.yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(summary.to_string(), Style::default().fg(t.text)),
            ]));

            // Render diff lines from the output
            let diff_lines = render_diff_content(content, t);
            for line in diff_lines {
                lines.push(line);
            }
        }
    }
}

/// Indent a rendered markdown line with a subtle guide character.
pub fn indent_line(line: Line<'static>, guide_color: Color) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "  \u{2502} ",
        Style::default().fg(guide_color),
    )];
    spans.extend(line.spans);
    Line::from(spans)
}

/// Render unified diff content from tool output as styled ratatui lines.
///
/// Parses lines starting with `+`, `-`, `@@`, `---`, `+++` and colors them:
/// - `+` lines → green
/// - `-` lines → red
/// - `@@` hunk headers → cyan
/// - `---`/`+++` file headers → bold
/// - Context lines → dimmed
fn render_diff_content(output: &str, t: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_diff = false;

    for raw_line in output.lines().skip(1) {
        // Detect start of diff section
        if raw_line.starts_with("---") || raw_line.starts_with("+++") {
            in_diff = true;
        }
        if !in_diff {
            continue;
        }

        // Strip ANSI escape codes if present (the tool output may be pre-colored)
        let clean = strip_ansi(raw_line);

        if clean.starts_with("@@") {
            // Hunk header — cyan
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(
                    clean.to_string(),
                    Style::default().fg(t.blue).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else if clean.starts_with('+') {
            // Added line — green background
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(clean.to_string(), Style::default().fg(t.green)),
            ]));
        } else if clean.starts_with('-') {
            // Removed line — red
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(clean.to_string(), Style::default().fg(t.red)),
            ]));
        } else if clean.starts_with("---") || clean.starts_with("+++") {
            // File headers — bold
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    clean.to_string(),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            // Context line — dimmed
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(clean.to_string(), Style::default().fg(t.subtext)),
            ]));
        }
    }
    lines
}

/// Strip ANSI escape sequences from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip escape sequence: ESC [ ... m
            if chars.peek() == Some(&'[') {
                chars.next();
                // Skip until 'm' or end
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}
