//! Input box widget with slash-command completion popup.
//! Catppuccin Mocha palette, scrollable suggestions with descriptions.

use crate::tui::TuiApp;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::Widget;

// Catppuccin Mocha
const BASE: Color = Color::Rgb(30, 30, 46);
const SURFACE0: Color = Color::Rgb(49, 50, 68);
const SURFACE1: Color = Color::Rgb(69, 71, 90);
const OVERLAY0: Color = Color::Rgb(108, 112, 134);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT: Color = Color::Rgb(166, 173, 200);
const BLUE: Color = Color::Rgb(137, 180, 250);
const CYAN: Color = Color::Rgb(137, 220, 235);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const TEAL: Color = Color::Rgb(148, 226, 213);

/// Max visible items in the suggestion popup (not counting category headers).
const MAX_VISIBLE: usize = 8;

pub struct Input;

impl Widget for Input {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let title = if app.is_processing {
            format!(" {} Press Esc to cancel ", "\u{25dc}")
        } else {
            format!(" {} Type / for commands, Enter to send ", "\u{270e}")
        };

        let border_color = if app.is_processing {
            YELLOW
        } else if !app.suggestions.is_empty() {
            CYAN
        } else {
            SURFACE1
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Draw suggestions popup above the input if any.
        if !app.suggestions.is_empty() {
            render_suggestions(f, app, inner);
        }

        // Render the input text or placeholder.
        let (display_text, style) = if app.input.is_empty() && !app.is_processing {
            (
                "Type a message or / for commands...".to_string(),
                Style::default().fg(OVERLAY0).add_modifier(Modifier::ITALIC),
            )
        } else {
            (app.input.clone(), Style::default().fg(TEXT))
        };

        let input = Paragraph::new(display_text).style(style);
        f.render_widget(input, inner);

        // Show cursor.
        if !app.is_processing {
            let cursor_x = inner.x + (app.cursor as u16).min(inner.width.saturating_sub(1));
            f.set_cursor_position((cursor_x, inner.y));
        }
    }
}

fn render_suggestions(f: &mut Frame, app: &TuiApp, input_inner: Rect) {
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
                    Style::default().fg(SURFACE0),
                ))));
            }
            items.push(ListItem::new(Line::from(Span::styled(
                format!("  {} {}", cat.icon(), cat.label()),
                Style::default()
                    .fg(OVERLAY0)
                    .add_modifier(Modifier::BOLD),
            ))));
            last_cat = Some(cat);
        }

        let is_selected = i == app.selected_suggestion;
        let name = cmd.slash_name();
        let desc = cmd.description();
        let usage = cmd.usage();

        let mut spans = vec![Span::styled(
            format!("    {}", name),
            if is_selected {
                Style::default()
                    .fg(BASE)
                    .bg(CYAN)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(CYAN)
                    .add_modifier(Modifier::BOLD)
            },
        )];

        if !usage.is_empty() {
            spans.push(Span::styled(
                format!(" {}", usage),
                if is_selected {
                    Style::default().fg(BASE).bg(CYAN)
                } else {
                    Style::default().fg(OVERLAY0)
                },
            ));
        }

        // Pad to align descriptions.
        let left_len = name.len() + 1 + if usage.is_empty() { 0 } else { usage.len() + 1 };
        let pad = (24usize).saturating_sub(left_len);
        spans.push(Span::styled(
            format!("{}{}", " ".repeat(pad), desc),
            if is_selected {
                Style::default().fg(BASE).bg(CYAN)
            } else {
                Style::default().fg(SUBTEXT)
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
        Span::styled(indicator, Style::default().fg(OVERLAY0))
    } else {
        Span::raw("")
    };

    let sug_block = Block::default()
        .title(format!(" {} Commands ", "\u{2318}"))
        .title_bottom(Line::from(vec![scroll_info]).right_aligned())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CYAN));
    let sug_list = List::new(items).block(sug_block);
    f.render_widget(sug_list, sug_area);
}
