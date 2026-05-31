//! Top status bar widget — shows model, mode, tokens, permission, status.

use crate::tui::TuiApp;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::Widget;

pub struct StatusBar;

impl Widget for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let bg = Color::DarkGray;
        let fg = Color::White;

        let mode_badge = Span::styled(
            format!(" {} ", app.mode),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        let model_span = Span::styled(
            format!(" {} ", app.model),
            Style::default().fg(fg).bg(bg),
        );

        let tokens_span = Span::styled(
            format!(
                " {} / {} / {} ",
                app.tokens.0, app.tokens.1, app.tokens.2
            ),
            Style::default().fg(Color::Rgb(170, 170, 170)).bg(bg),
        );

        let status_span = if app.is_processing {
            Span::styled(
                format!(" {} ", app.status_msg),
                Style::default()
                    .fg(Color::Yellow)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                format!(" {} ", app.status_msg),
                Style::default().fg(Color::Green).bg(bg),
            )
        };

        let perm_span = Span::styled(
            format!(" {} ", app.permission_mode),
            Style::default().fg(Color::Rgb(170, 170, 170)).bg(bg),
        );

        let sep = Span::styled(" | ", Style::default().fg(Color::Rgb(85, 85, 85)).bg(bg));

        let line = Line::from(vec![
            mode_badge,
            sep.clone(),
            model_span,
            sep.clone(),
            Span::styled(" tokens: ", Style::default().fg(Color::DarkGray).bg(bg)),
            tokens_span,
            sep.clone(),
            perm_span,
            sep,
            status_span,
        ]);

        let paragraph = Paragraph::new(line).style(Style::default().bg(bg));
        f.render_widget(paragraph, area);
    }
}
