//! Top status bar widget — modern design with Catppuccin Mocha palette.
//!
//! Visual: `[ mode ] • model • tokens • permission • status`

use crate::tui::TuiApp;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::Widget;

// Catppuccin Mocha palette
const BASE: Color = Color::Rgb(30, 30, 46);        // #1e1e2e
const SURFACE0: Color = Color::Rgb(49, 50, 68);    // #313244
const SURFACE1: Color = Color::Rgb(69, 71, 90);    // #45475a
const TEXT: Color = Color::Rgb(205, 214, 244);      // #cdd6f4
const SUBTEXT: Color = Color::Rgb(166, 173, 200);   // #a6adc8
const BLUE: Color = Color::Rgb(137, 180, 250);      // #89b4fa
const GREEN: Color = Color::Rgb(166, 227, 161);     // #a6e3a1
const YELLOW: Color = Color::Rgb(249, 226, 175);    // #f9e2af
const PEACH: Color = Color::Rgb(250, 179, 135);     // #fab387
const MAUVE: Color = Color::Rgb(203, 166, 247);     // #cba6f7
const TEAL: Color = Color::Rgb(148, 226, 213);      // #94e2d5
const RED: Color = Color::Rgb(243, 139, 168);       // #f38ba8

pub struct StatusBar;

impl Widget for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let bg = BASE;

        // Mode badge — colored pill
        let mode_color = match app.mode.as_str() {
            "coding" => BLUE,
            "research" => MAUVE,
            "data" => TEAL,
            "writing" => PEACH,
            "plan" => YELLOW,
            _ => GREEN,
        };
        let mode_badge = Span::styled(
            format!(" {} ", app.mode),
            Style::default()
                .fg(BASE)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        );

        // Model name with icon
        let model_span = Span::styled(
            format!("  {} {}", "\u{25c8}", app.model),
            Style::default().fg(TEXT).bg(bg),
        );

        // Token usage with visual indicator
        let token_color = if app.tokens.2 > 150_000 {
            RED
        } else if app.tokens.2 > 100_000 {
            YELLOW
        } else {
            SUBTEXT
        };
        let tokens_span = Span::styled(
            format!("{} {}k/{}k", "\u{25cf}", app.tokens.0 / 1000, app.tokens.2 / 1000),
            Style::default().fg(token_color).bg(bg),
        );

        // Permission mode
        let perm_color = match app.permission_mode.as_str() {
            "auto" => GREEN,
            "ask" => YELLOW,
            "deny" => RED,
            _ => SUBTEXT,
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
                format!("{} {} ", "\u{25dc}", app.status_msg),
                Style::default()
                    .fg(YELLOW)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )
        } else {
            Span::styled(
                format!("{} {}", "\u{2714}", app.status_msg),
                Style::default().fg(GREEN).bg(bg),
            )
        };

        // Right-aligned info (tools count)
        let tools_span = Span::styled(
            format!("{} {} tools ", "\u{2692}", app.tool_count),
            Style::default().fg(SUBTEXT).bg(bg),
        );

        let sep = Span::styled(" \u{2502} ", Style::default().fg(SURFACE1).bg(bg));

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
            let right_para = Paragraph::new(Line::from(vec![tools_span]))
                .style(Style::default().bg(bg));
            f.render_widget(right_para, right_area);
        }
    }
}
