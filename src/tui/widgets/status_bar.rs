//! Top status bar widget — modern design with adaptive theme.

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
        let t = &app.theme;
        let bg = t.bg;

        // Mode badge — colored pill
        let mode_color = match app.mode.as_str() {
            "coding" => t.blue,
            "research" => t.mauve,
            "data" => t.teal,
            "writing" => t.peach,
            "plan" => t.yellow,
            _ => t.green,
        };
        let mode_badge = Span::styled(
            format!(" {} ", app.mode),
            Style::default()
                .fg(bg)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        );

        // Model name with icon
        let model_span = Span::styled(
            format!("  \u{25c8} {}", app.model),
            Style::default().fg(t.text).bg(bg),
        );

        // Token usage with visual indicator
        let token_color = if app.tokens.2 > 150_000 {
            t.red
        } else if app.tokens.2 > 100_000 {
            t.yellow
        } else {
            t.subtext
        };
        let tokens_span = Span::styled(
            format!("\u{25cf} {}k/{}k", app.tokens.0 / 1000, app.tokens.2 / 1000),
            Style::default().fg(token_color).bg(bg),
        );

        // Permission mode
        let perm_color = match app.permission_mode.as_str() {
            "auto" => t.green,
            "ask" => t.yellow,
            "deny" => t.red,
            _ => t.subtext,
        };
        let perm_icon = match app.permission_mode.as_str() {
            "auto" => "\u{2713}",
            "ask" => "\u{2753}",
            "deny" => "\u{2717}",
            _ => "\u{25cb}",
        };
        let perm_span = Span::styled(
            format!("{} {}", perm_icon, app.permission_mode),
            Style::default().fg(perm_color).bg(bg),
        );

        // Status indicator
        let status_span = if app.is_processing {
            Span::styled(
                format!("\u{25dc} {} ", app.status_msg),
                Style::default()
                    .fg(t.yellow)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )
        } else {
            Span::styled(
                format!("\u{2714} {}", app.status_msg),
                Style::default().fg(t.green).bg(bg),
            )
        };

        // Right-aligned info (tools count)
        let tools_span = Span::styled(
            format!("\u{2692} {} tools ", app.tool_count),
            Style::default().fg(t.subtext).bg(bg),
        );

        let sep = Span::styled(" \u{2502} ", Style::default().fg(t.surface1).bg(bg));

        let line = Line::from(vec![
            mode_badge,
            sep.clone(),
            model_span,
            sep.clone(),
            tokens_span,
            sep.clone(),
            perm_span,
            sep.clone(),
            status_span,
            Span::styled("  ", Style::default().bg(bg)),
        ]);

        let paragraph = Paragraph::new(line).style(Style::default().bg(bg));
        f.render_widget(paragraph, area);

        // Render tools count right-aligned if space permits
        if area.width > 60 {
            let right_x = area.x + area.width - 14;
            let right_area = Rect {
                x: right_x,
                y: area.y,
                width: 14,
                height: 1,
            };
            let right_para =
                Paragraph::new(Line::from(vec![tools_span])).style(Style::default().bg(bg));
            f.render_widget(right_para, right_area);
        }
    }
}
