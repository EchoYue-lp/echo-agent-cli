//! Task strip widget — shows parallel task progress below the chat area.
//!
//! Inspired by Claude Code's parallel agent progress display:
//! ```text
//! ◯ Research papers     2m 08s  ↓ 8.2k tokens
//! ● Generate report     2m 05s  ↓ 8.1k tokens
//! ◯ Compile results     2m 02s  ↓ 8.3k tokens
//! ```

use crate::tui::{TaskStripStatus, TuiApp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph};

use super::Widget;

pub struct TaskStrip;

impl Widget for TaskStrip {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;

        if app.parallel_tasks.is_empty() {
            return;
        }

        // Show at most `area.height` tasks (typically 3–5 rows).
        let max_rows = area.height as usize;
        for (i, entry) in app.parallel_tasks.iter().take(max_rows).enumerate() {
            let row_y = area.y + i as u16;
            let row_area = Rect::new(area.x, row_y, area.width, 1);

            // ── Status icon + color ──────────────────────────────────
            let (icon, icon_color, name_color) = match &entry.status {
                TaskStripStatus::Pending => ("◯", t.overlay0, t.subtext),
                TaskStripStatus::Running => ("●", t.blue, t.text),
                TaskStripStatus::Completed => ("✔", t.green, t.subtext),
                TaskStripStatus::Failed(_) => ("✗", t.red, t.red),
                TaskStripStatus::Cancelled => ("⊘", t.overlay0, t.overlay0),
            };

            // ── Build the line ───────────────────────────────────────
            let elapsed = &entry.elapsed_label;
            let phase_info = if !entry.phase.is_empty() {
                format!(" · {}", entry.phase)
            } else {
                String::new()
            };
            let msg_info = entry
                .message
                .as_ref()
                .map(|m| format!(" · {}", truncate_str(m, 25)))
                .unwrap_or_default();

            // Progress percentage (for running tasks)
            let pct_label = if entry.status == TaskStripStatus::Running && entry.progress_pct > 0.0
            {
                format!(" {:.0}%", entry.progress_pct)
            } else {
                String::new()
            };

            let line = Line::from(vec![
                Span::styled(format!(" {} ", icon), Style::default().fg(icon_color)),
                Span::styled(
                    format!("{:<24}", entry.name),
                    Style::default().fg(name_color),
                ),
                Span::styled(format!("{:>6}", elapsed), Style::default().fg(t.overlay0)),
                Span::styled(pct_label, Style::default().fg(t.blue)),
                Span::styled(phase_info, Style::default().fg(t.subtext)),
                Span::styled(msg_info, Style::default().fg(t.overlay0)),
            ]);

            // ── Render with optional gauge bar for running tasks ─────
            if entry.status == TaskStripStatus::Running
                && entry.progress_pct > 0.0
                && area.width > 60
            {
                // Two-part render: text on left, gauge on right
                let text_width = 50.min(area.width / 2);
                let gauge_area = Rect::new(
                    area.x + text_width,
                    row_y,
                    area.width.saturating_sub(text_width),
                    1,
                );
                let text_area = Rect::new(area.x, row_y, text_width, 1);

                let text_para = Paragraph::new(line);
                f.render_widget(text_para, text_area);

                let gauge = Gauge::default()
                    .gauge_style(Style::default().fg(t.blue).bg(t.surface0))
                    .percent(entry.progress_pct.clamp(0.0, 100.0) as u16)
                    .label("");
                f.render_widget(gauge, gauge_area);
            } else {
                let para = Paragraph::new(line);
                f.render_widget(para, row_area);
            }
        }

        // ── Overflow indicator ───────────────────────────────────────
        if app.parallel_tasks.len() > max_rows {
            let overflow = app.parallel_tasks.len() - max_rows;
            let overflow_area = Rect::new(area.x, area.y + max_rows as u16, area.width, 1);
            if overflow_area.y < area.y + area.height {
                let line = Line::from(vec![Span::styled(
                    format!("  +{} more task(s)…", overflow),
                    Style::default()
                        .fg(t.overlay0)
                        .add_modifier(Modifier::ITALIC),
                )]);
                f.render_widget(Paragraph::new(line), overflow_area);
            }
        }
    }
}

/// Truncate a string to fit within max_len characters, adding "…" if needed.
pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
