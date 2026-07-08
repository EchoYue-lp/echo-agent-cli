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
        // 上下文窗口占用（对齐 Claude Code statusline）。
        let ctx = &app.context_snapshot;
        let pct = ctx.used_percentage();
        let bar = echo_agent_app_core::context_window::render_progress_bar(pct);
        let tier = echo_agent_app_core::context_window::usage_tier(pct);
        let ctx_color = match tier {
            echo_agent_app_core::context_window::UsageTier::Critical => t.red,
            echo_agent_app_core::context_window::UsageTier::High => t.yellow,
            _ => t.subtext,
        };
        let context_span = if ctx.is_available() {
            let used_str =
                echo_agent_app_core::context_window::format_token_count(ctx.input_tokens);
            let win_str =
                echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            match pct {
                Some(p) => format!("  {} {}/{} {}%", bar, used_str, win_str, p),
                None => format!("  {} {}", bar, used_str),
            }
        } else {
            // 首次响应前：显示占位（不显示 0%，避免误导）。
            let win_str =
                echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            format!("  · --/{}", win_str)
        };
        let sidebar_hint = if app.sidebar_visible {
            "side"
        } else {
            "Ctrl+B side"
        };

        let line = Line::from(vec![
            Span::styled(
                " EKO",
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", app.mode), Style::default().fg(t.text)),
            Span::styled(format!("  {}", app.model), Style::default().fg(t.subtext)),
            Span::styled(format!("  {}", state), Style::default().fg(state_color)),
            Span::styled(context_span, Style::default().fg(ctx_color)),
            Span::styled(
                format!("  {}", sidebar_hint),
                Style::default().fg(t.overlay0),
            ),
        ]);

        let paragraph = Paragraph::new(line).style(Style::default().bg(t.bg));
        f.render_widget(paragraph, area);
    }
}
