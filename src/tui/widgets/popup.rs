//! Popup widget — modal dialogs for diff preview and tool approval.

use crate::tui::{ApprovalRequest, DiffPopup};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Draw a centered diff popup.
pub fn draw_diff_popup(f: &mut Frame, diff: &DiffPopup, area: Rect) {
    let popup_area = centered_rect(80, 70, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Diff: {} (Esc to close) ", diff.file_path))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let lines: Vec<Line> = diff
        .diff_text
        .lines()
        .map(|line| {
            if line.starts_with('+') && !line.starts_with("+++") {
                Line::from(Span::styled(line, Style::default().fg(Color::Green)))
            } else if line.starts_with('-') && !line.starts_with("---") {
                Line::from(Span::styled(line, Style::default().fg(Color::Red)))
            } else if line.starts_with("@@") {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(line)
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

/// Draw approval popup for human-in-the-loop.
pub fn draw_approval_popup(f: &mut Frame, approval: &ApprovalRequest, area: Rect) {
    let popup_area = centered_rect(60, 40, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Approve: {} (y/n) ", approval.tool_name))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let text = vec![
        Line::from(vec![
            Span::styled(
                "Tool: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(&approval.tool_name),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Args: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(&approval.args),
        ]),
        Line::from(""),
        Line::from(approval.prompt.clone()),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " [y] Approve  ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " [n] Deny  ",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
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
