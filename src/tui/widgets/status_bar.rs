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

        let state = if app.is_processing && app.conversation_input_queue_len() > 0 {
            "thinking+queued"
        } else if app.is_processing {
            "thinking"
        } else {
            "ready"
        };
        let state_color = if app.is_processing { t.yellow } else { t.green };
        // 上下文窗口占用（对齐 Claude Code statusline）+ 会话缓存命中率。
        let ctx = &app.context_snapshot;
        let pct = if ctx.is_available() {
            ctx.used_percentage()
        } else {
            None
        };
        let ring = echo_agent_app_core::context_window::render_ring_char(pct);
        let tier = echo_agent_app_core::context_window::usage_tier(pct);
        let ctx_color = match tier {
            echo_agent_app_core::context_window::UsageTier::Critical => t.red,
            echo_agent_app_core::context_window::UsageTier::High => t.yellow,
            _ => t.subtext,
        };
        let cache_span = match app.usage_accumulator.cache_hit_rate() {
            Some(rate) => {
                let pct_i = (rate * 100.0).round().clamp(0.0, 100.0) as u16;
                format!(" · cache {}%", pct_i)
            }
            None => " · cache --".to_string(),
        };
        let context_span = if ctx.is_available() {
            let used_str =
                echo_agent_app_core::context_window::format_token_count(ctx.input_tokens);
            let win_str =
                echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            match pct {
                Some(p) => format!("{} {}/{} {}%{}", ring, used_str, win_str, p, cache_span),
                None => format!("{} {}{}", ring, used_str, cache_span),
            }
        } else {
            // 首次响应前 / 刚压缩后：占用占位；cache 率可能仍有值（压缩不清 Accumulator）。
            let win_str =
                echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            format!("{} --/{}{}", ring, win_str, cache_span)
        };
        let sidebar_hint = if app.sidebar_visible {
            "side"
        } else {
            "Ctrl+B side"
        };

        // Segments separated by a dim vertical bar — Claude Code statusline style.
        let sep = || Span::styled("  │  ", Style::default().fg(t.surface0));
        let state_icon = if app.is_processing { "●" } else { "○" };

        let line = Line::from(vec![
            Span::styled(
                " ✻ EKO",
                Style::default().fg(t.peach).add_modifier(Modifier::BOLD),
            ),
            sep(),
            Span::styled(app.mode.clone(), Style::default().fg(mode_color)),
            sep(),
            Span::styled(app.model.clone(), Style::default().fg(t.subtext)),
            sep(),
            Span::styled(
                format!("{} {}", state_icon, state),
                Style::default().fg(state_color),
            ),
            Span::styled(context_span, Style::default().fg(ctx_color)),
            sep(),
            Span::styled(sidebar_hint.to_string(), Style::default().fg(t.overlay0)),
        ]);

        let paragraph = Paragraph::new(line).style(Style::default().bg(t.bg));
        f.render_widget(paragraph, area);
    }
}
