//! Popup widget — modal dialogs for diff preview and tool approval.

use crate::tui::{ApprovalRequest, DiffPopup, Theme};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Draw a centered diff popup.
pub fn draw_diff_popup(f: &mut Frame, diff: &DiffPopup, area: Rect, theme: &Theme) {
    let t = theme;
    let popup_area = centered_rect(80, 70, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Diff: {} (Esc to close) ", diff.file_path))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.cyan));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let lines: Vec<Line> = diff
        .diff_text
        .lines()
        .map(|line| {
            if line.starts_with('+') && !line.starts_with("+++") {
                Line::from(Span::styled(line, Style::default().fg(t.green)))
            } else if line.starts_with('-') && !line.starts_with("---") {
                Line::from(Span::styled(line, Style::default().fg(t.red)))
            } else if line.starts_with("@@") {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(t.cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(line, Style::default().fg(t.text)))
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(t.bg))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

/// Draw approval popup for human-in-the-loop.
pub fn draw_approval_popup(f: &mut Frame, approval: &ApprovalRequest, area: Rect, theme: &Theme) {
    let t = theme;
    let popup_area = centered_rect(60, 40, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Approve: {} (y/n) ", approval.tool_name))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.yellow));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let text = vec![
        Line::from(vec![
            Span::styled(
                "Tool: ",
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&approval.tool_name, Style::default().fg(t.text)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Args: ",
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&approval.args, Style::default().fg(t.text)),
        ]),
        Line::from(""),
        Line::from(Span::styled(approval.prompt.clone(), Style::default().fg(t.text))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " [y] Approve  ",
                Style::default()
                    .fg(t.green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " [n] Deny  ",
                Style::default()
                    .fg(t.red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .style(Style::default().bg(t.bg))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

/// Helper: create a centered rectangle (percentage-based).
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
