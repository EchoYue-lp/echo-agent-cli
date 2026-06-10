//! TUI rendering — top-level layout and draw function.
//!
//! Delegates to individual widgets for each panel.

use super::TuiApp;
use super::widgets::Widget;
use super::widgets::chat::Chat;
use super::widgets::input::Input;
use super::widgets::sidebar::Sidebar;
use super::widgets::status_bar::StatusBar;
use super::widgets::task_strip::TaskStrip;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

/// Main draw function — renders the complete TUI layout.
///
/// Layout (top to bottom):
/// ```text
/// ┌─────────────────────────────────────────────┐
/// │  StatusBar (1 row)                          │
/// ├──────────┬──────────────────────────────────┤
/// │ Sidebar  │  Chat                            │
/// │ (opt)    │  (flexible)                      │
/// ├──────────┴──────────────────────────────────┤
/// │  Input (2 rows)                             │
/// ├─────────────────────────────────────────────┤
/// │  TaskStrip (conditional, 1–5 rows)          │
/// └─────────────────────────────────────────────┘
/// ```
pub fn draw(f: &mut Frame, app: &TuiApp) {
    let size = f.area();

    // Conditionally show task strip below input when there are active parallel tasks.
    let task_strip_rows = app.parallel_tasks.len().min(5) as u16;
    let has_tasks = !app.parallel_tasks.is_empty();

    // Main layout: status bar + body (sidebar+chat) + input + [task strip].
    let constraints = if has_tasks {
        vec![
            Constraint::Length(1),               // StatusBar
            Constraint::Min(8),                  // Chat (+ sidebar)
            Constraint::Length(2),               // Input
            Constraint::Length(task_strip_rows), // TaskStrip (dynamic, bottom)
        ]
    } else {
        vec![
            Constraint::Length(1), // StatusBar
            Constraint::Min(8),    // Chat (+ sidebar)
            Constraint::Length(2), // Input
        ]
    };

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(size);

    // ── Status bar ─────────────────────────────────────────────────────
    StatusBar.render(f, main_chunks[0], app);

    // ── Body (sidebar + chat) ──────────────────────────────────────────
    if app.sidebar_visible {
        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(24), // Sidebar
                Constraint::Min(40),    // Chat area
            ])
            .split(main_chunks[1]);

        Sidebar.render(f, body_chunks[0], app);
        Chat.render(f, body_chunks[1], app);
    } else {
        Chat.render(f, main_chunks[1], app);
    }

    // ── Input box ──────────────────────────────────────────────────────
    Input.render(f, main_chunks[2], app);

    // ── Task strip (conditional, below input) ──────────────────────────
    if has_tasks {
        TaskStrip.render(f, main_chunks[3], app);
    }

    // ── Approval card (inline overlay, bottom of chat area) ────────────
    render_approval_card(f, app, main_chunks[1]);
}

/// Render the approval request card as an inline overlay at the bottom of the chat area.
fn render_approval_card(f: &mut Frame, app: &TuiApp, chat_area: Rect) {
    use echo_agent_app_core::hitl::PendingApproval;

    // Check if there's a pending approval
    let pending_handle = match &app.pending_approval {
        Some(h) => h,
        None => return,
    };
    let guard = match pending_handle.try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let approval = match guard.as_ref() {
        Some(a) => a,
        None => return,
    };

    let theme = &app.theme;

    // Compute card dimensions
    let card_height = if approval.input_mode { 10u16 } else { 8u16 };
    let card_width = chat_area.width.min(70);
    let card_x = chat_area.x + (chat_area.width.saturating_sub(card_width)) / 2;
    let card_y = chat_area.y + chat_area.height.saturating_sub(card_height + 1);

    let card_area = Rect::new(card_x, card_y, card_width, card_height);

    // Clear background
    f.render_widget(Clear, card_area);

    // Card border
    let block = Block::default()
        .title(format!(" 🛡️ {} 需要确认 ", approval.tool_name))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.yellow));

    let inner = block.inner(card_area);
    f.render_widget(block, card_area);

    // Split inner area: args display + options/input
    let inner_height = inner.height as usize;
    let args_max_lines = if approval.input_mode {
        inner_height.saturating_sub(5)
    } else {
        inner_height.saturating_sub(3)
    };

    // Build lines
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Risk + prompt line
    lines.push(Line::from(vec![
        Span::styled(
            format!("[{}] ", approval.risk_label),
            Style::default().fg(theme.subtext),
        ),
        Span::styled(approval.prompt.clone(), Style::default().fg(theme.text)),
    ]));

    // Arguments display (truncated)
    if !approval.args_display.is_empty() {
        let arg_lines: Vec<&str> = approval.args_display.lines().collect();
        let show_lines = arg_lines.len().min(args_max_lines);
        for line in &arg_lines[..show_lines] {
            lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(theme.subtext),
            )));
        }
        if arg_lines.len() > show_lines {
            lines.push(Line::from(Span::styled(
                format!("  ... ({} more lines)", arg_lines.len() - show_lines),
                Style::default().fg(theme.subtext),
            )));
        }
    }

    // Empty separator
    lines.push(Line::from(""));

    if approval.input_mode {
        // ── Feedback input mode ──
        lines.push(Line::from(Span::styled(
            format!("  {}: ", approval.input_label),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  > {}_", approval.feedback_input),
            Style::default().fg(theme.text),
        )));
        lines.push(Line::from(Span::styled(
            "  Enter=提交  Esc=取消",
            Style::default().fg(theme.subtext),
        )));
    } else {
        // ── Option selection mode ──
        let options = PendingApproval::OPTION_LABELS;
        let spans: Vec<Span<'static>> = options
            .iter()
            .enumerate()
            .flat_map(|(i, label)| {
                let style = if i == approval.selected_option {
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme.green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                let prefix = if i > 0 { "  " } else { "  " };
                vec![
                    Span::styled(prefix.to_string(), Style::default()),
                    Span::styled(label.to_string(), style),
                ]
            })
            .collect();
        lines.push(Line::from(spans));
        lines.push(Line::from(Span::styled(
            "  ←/→=选择  Enter=确认  Esc=拒绝",
            Style::default().fg(theme.subtext),
        )));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}
