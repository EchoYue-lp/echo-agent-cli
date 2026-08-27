//! Input box widget with slash-command completion popup.
//! Adaptive theme, scrollable suggestions with descriptions.

use crate::tui::TuiApp;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::Widget;

/// Max visible items in the suggestion popup (not counting category headers).
const MAX_VISIBLE: usize = 8;

pub struct Input;

impl Widget for Input {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;

        // Draw a subtle horizontal separator line at the top
        let sep_style = if app.is_processing {
            Style::default().fg(t.yellow)
        } else if !app.suggestions.is_empty() {
            Style::default().fg(t.peach)
        } else {
            Style::default().fg(t.surface0)
        };
        let separator = Paragraph::new("─".repeat(area.width as usize)).style(sep_style);
        let sep_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        f.render_widget(separator, sep_area);

        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(2),
        };
        let footer = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };

        // Draw suggestions popup above the input if any.
        if !app.suggestions.is_empty() {
            render_suggestions(f, app, body);
        }

        // Prompt indicator
        let prompt_icon = if app.is_processing {
            Span::styled(
                "… ",
                Style::default().fg(t.yellow).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "❯ ",
                Style::default().fg(t.peach).add_modifier(Modifier::BOLD),
            )
        };

        // Render the input text or placeholder.
        let (text_span, style) = if app.input.is_empty() && !app.is_processing {
            (
                Span::styled(
                    "输入消息，或输入 / 查看命令",
                    Style::default()
                        .fg(t.overlay0)
                        .add_modifier(Modifier::ITALIC),
                ),
                Style::default(),
            )
        } else {
            (Span::raw(app.input.clone()), Style::default())
        };

        let prompt_area = Rect {
            x: body.x,
            y: body.y,
            width: body.width.min(2),
            height: body.height,
        };
        let text_area = Rect {
            x: body.x.saturating_add(2),
            y: body.y,
            width: body.width.saturating_sub(2),
            height: body.height,
        };
        f.render_widget(Paragraph::new(Line::from(prompt_icon)), prompt_area);
        let before_cursor = app.input.get(..app.cursor).unwrap_or("");
        let content_width = text_area.width.max(1) as usize;
        let mut cursor_row = 0usize;
        let mut cursor_col = 0usize;
        for (idx, line) in before_cursor.split('\n').enumerate() {
            let line_width = UnicodeWidthStr::width(line);
            if idx > 0 {
                cursor_row = cursor_row.saturating_add(1);
            }
            cursor_row = cursor_row.saturating_add(line_width / content_width);
            cursor_col = line_width % content_width;
        }
        let scroll_y = cursor_row.saturating_sub(text_area.height.saturating_sub(1) as usize);
        let input = Paragraph::new(Line::from(text_span))
            .style(style)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y as u16, 0));
        f.render_widget(input, text_area);

        if footer.height > 0 {
            let queue_len = app.conversation_input_queue_len();
            let queued = app.next_conversation_input_preview().map_or_else(
                || "队列 0".to_string(),
                |preview| format!("队列 {queue_len} · next: {preview}"),
            );
            let footer_line = Line::from(vec![
                Span::styled("  Enter 发送", Style::default().fg(t.overlay0)),
                Span::styled("  Shift+Enter 换行", Style::default().fg(t.overlay0)),
                Span::styled(
                    format!("  {queued}"),
                    if queue_len == 0 {
                        Style::default().fg(t.overlay0)
                    } else {
                        Style::default().fg(t.yellow).add_modifier(Modifier::BOLD)
                    },
                ),
            ]);
            f.render_widget(Paragraph::new(footer_line), footer);
        }

        // Show cursor (offset by prompt display width).
        if !app.is_processing {
            let visible_row = cursor_row.saturating_sub(scroll_y);
            f.set_cursor_position((
                text_area.x.saturating_add(cursor_col as u16),
                text_area.y.saturating_add(visible_row as u16),
            ));
        }
    }
}

fn render_suggestions(f: &mut Frame, app: &TuiApp, input_inner: Rect) {
    let t = &app.theme;

    // Count actual items (commands, not headers) to determine height.
    let total_items = app.suggestions.len();
    let visible_items = total_items.min(MAX_VISIBLE);
    // Add space for category headers (estimate: 1 header per 4 items + borders).
    let estimated_headers = (total_items / 4).max(1);
    let sug_height = ((visible_items + estimated_headers + 2) as u16).min(16);
    let sug_width = (60u16).min(input_inner.width);

    let sug_area = Rect {
        x: input_inner.x,
        y: input_inner.y.saturating_sub(sug_height),
        width: sug_width,
        height: sug_height,
    };

    f.render_widget(Clear, sug_area);

    // Ensure selected_suggestion is within scroll window.
    let scroll = app.suggestion_scroll;
    let visible_end = scroll + visible_items;

    // Build items with category grouping and scrolling.
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_cat = None;

    for (i, cmd) in app.suggestions.iter().enumerate() {
        // Skip items outside the scroll window.
        if i < scroll || i >= visible_end {
            // But still track category changes for items we skip.
            let cat = cmd.category();
            if last_cat != Some(cat) {
                last_cat = Some(cat);
            }
            continue;
        }

        let cat = cmd.category();
        // Category header
        if last_cat != Some(cat) {
            if !items.is_empty() {
                items.push(ListItem::new(Line::from(Span::styled(
                    "  \u{2500}".repeat((sug_width as usize / 3).min(12)),
                    Style::default().fg(t.surface0),
                ))));
            }
            items.push(ListItem::new(Line::from(Span::styled(
                format!("  {} {}", cat.icon(), cat.label()),
                Style::default().fg(t.overlay0).add_modifier(Modifier::BOLD),
            ))));
            last_cat = Some(cat);
        }

        let is_selected = i == app.selected_suggestion;
        let name = cmd.slash_name();
        let desc = cmd.description();
        let usage = cmd.usage();

        let mut spans = vec![Span::styled(
            format!("    {}", name),
            Style::default().fg(t.cyan).add_modifier(Modifier::BOLD),
        )];

        if !usage.is_empty() {
            spans.push(Span::styled(
                format!(" {}", usage),
                if is_selected {
                    Style::default().fg(t.cyan)
                } else {
                    Style::default().fg(t.overlay0)
                },
            ));
        }

        // Pad to align descriptions.
        let left_len = name.len() + 1 + if usage.is_empty() { 0 } else { usage.len() + 1 };
        let pad = (24usize).saturating_sub(left_len);
        spans.push(Span::styled(
            format!("{}{}", " ".repeat(pad), desc),
            if is_selected {
                Style::default().fg(t.cyan)
            } else {
                Style::default().fg(t.subtext)
            },
        ));

        items.push(ListItem::new(Line::from(spans)));
    }

    // Scroll indicator
    let scroll_info = if total_items > visible_items {
        let indicator = format!(
            " {} {}/{} ",
            "\u{2195}",
            app.selected_suggestion + 1,
            total_items
        );
        Span::styled(indicator, Style::default().fg(t.overlay0))
    } else {
        Span::raw("")
    };

    let sug_block = Block::default()
        .title(format!(" {} Commands ", "\u{2318}"))
        .title_bottom(Line::from(vec![scroll_info]).right_aligned())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.cyan))
        .style(Style::default());
    let sug_list = List::new(items).block(sug_block);
    f.render_widget(sug_list, sug_area);
}
