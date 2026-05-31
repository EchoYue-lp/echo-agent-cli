//! Input box widget with slash-command completion popup.

use crate::tui::TuiApp;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::Widget;

pub struct Input;

impl Widget for Input {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let title = if app.is_processing {
            " Input (Esc to cancel) "
        } else {
            " Input (Enter to send, / for commands, C-c to quit) "
        };

        let border_color = if app.is_processing {
            Color::Yellow
        } else if !app.suggestions.is_empty() {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
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
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (app.input.clone(), Style::default().fg(Color::White))
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
    let sug_height = (app.suggestions.len() as u16 + 2).min(12);
    let sug_width = {
        let max_len = app
            .suggestions
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(20);
        (max_len as u16 + 4).min(input_inner.width)
    };

    let sug_area = Rect {
        x: input_inner.x,
        y: input_inner.y.saturating_sub(sug_height),
        width: sug_width,
        height: sug_height,
    };

    f.render_widget(Clear, sug_area);

    let items: Vec<ListItem> = app
        .suggestions
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let is_selected = i == app.selected_suggestion;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            ListItem::new(Line::from(Span::styled(
                format!("  {} ", cmd),
                style,
            )))
        })
        .collect();

    let sug_block = Block::default()
        .title(" Commands ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let sug_list = List::new(items).block(sug_block);
    f.render_widget(sug_list, sug_area);
}
