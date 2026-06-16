//! Top status bar widget — modern design with adaptive theme.

use crate::tui::TuiApp;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::Widget;

pub struct StatusBar;

impl Widget for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;
        let mode_color = match app.mode.as_str() {
            "coding" => t.blue,
            "research" => t.mauve,
            "data" => t.teal,
            "writing" => t.peach,
            "plan" => t.yellow,
            _ => t.green,
        };

        let state = if app.is_processing {
            "thinking"
        } else {
            "ready"
        };
        let state_color = if app.is_processing { t.yellow } else { t.green };
        let tokens = if app.tokens.2 > 0 {
            format!(" · {}k tokens", app.tokens.2 / 1000)
        } else {
            String::new()
        };
        let sidebar_hint = if app.sidebar_visible {
            "side"
        } else {
            "Ctrl+B side"
        };

        let line = Line::from(vec![
            Span::styled(
                " EchoCoWork",
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", app.mode), Style::default().fg(t.text)),
            Span::styled(format!("  {}", app.model), Style::default().fg(t.subtext)),
            Span::styled(format!("  {}", state), Style::default().fg(state_color)),
            Span::styled(tokens, Style::default().fg(t.subtext)),
            Span::styled(
                format!("  {}", sidebar_hint),
                Style::default().fg(t.overlay0),
            ),
        ]);

        let paragraph = Paragraph::new(line).style(Style::default().bg(t.bg));
        f.render_widget(paragraph, area);
    }
}
