//! TUI rendering — draws all panels to the terminal.

use super::*;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Tabs, Wrap,
    },
    Frame,
};

/// Main draw function — renders the complete TUI layout.
pub fn draw(f: &mut Frame, app: &TuiApp) {
    let size = f.area();

    // Main layout: status bar + body + input
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // Status bar
            Constraint::Min(10),     // Body (sidebar + chat)
            Constraint::Length(3),   // Input box
        ])
        .split(size);

    draw_status_bar(f, app, main_chunks[0]);

    if app.sidebar_visible {
        // Body with sidebar
        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(28),  // Sidebar
                Constraint::Min(30),     // Chat area
            ])
            .split(main_chunks[1]);

        draw_sidebar(f, app, body_chunks[0]);
        draw_chat(f, app, body_chunks[1]);
    } else {
        draw_chat(f, app, main_chunks[1]);
    }

    draw_input(f, app, main_chunks[2]);

    // Draw popups on top
    if let Some(ref diff) = app.diff_popup {
        draw_diff_popup(f, diff, size);
    }
    if let Some(ref approval) = app.approval {
        draw_approval_popup(f, approval, size);
    }
}

/// Draw the status bar at the top.
fn draw_status_bar(f: &mut Frame, app: &TuiApp, area: Rect) {
    let status_text = format!(
        " EchoCoWork │ {} │ {} │ tokens: {}/{}/{} │ tools: {} │ {} ",
        app.model,
        app.mode,
        app.tokens.0,
        app.tokens.1,
        app.tokens.2,
        app.tool_count,
        app.status_msg,
    );

    let style = if app.is_processing {
        Style::default().fg(Color::Yellow).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };

    let status = Paragraph::new(status_text).style(style);
    f.render_widget(status, area);
}

/// Draw the sidebar with tabs (Files, Tools, Tasks).
fn draw_sidebar(f: &mut Frame, app: &TuiApp, area: Rect) {
    let block = Block::default()
        .title(" Sidebar ")
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray));

    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),   // Tabs
            Constraint::Min(5),      // Content
        ])
        .split(area);

    // Tab selector
    let titles = vec!["📁 Files", "🛠 Tools", "📋 Tasks"];
    let tabs = Tabs::new(titles)
        .select(app.sidebar_tab)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(tabs, sidebar_chunks[0]);

    // Tab content
    match app.sidebar_tab {
        0 => draw_file_tree(f, app, sidebar_chunks[1]),
        1 => draw_tools_list(f, app, sidebar_chunks[1]),
        2 => draw_tasks_list(f, app, sidebar_chunks[1]),
        _ => {}
    }

    f.render_widget(block, area);
}

/// Draw a simple file tree placeholder.
fn draw_file_tree(f: &mut Frame, _app: &TuiApp, area: Rect) {
    let items = vec![
        ListItem::new("📁 src/"),
        ListItem::new("  📁 cli/"),
        ListItem::new("  📁 tui/"),
        ListItem::new("  📄 main.rs"),
        ListItem::new("  📄 lib.rs"),
        ListItem::new("📁 echo-agent-app-core/"),
        ListItem::new("📁 echo-agent-server/"),
        ListItem::new("📄 Cargo.toml"),
        ListItem::new("📄 README.md"),
    ];
    let list = List::new(items)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Cyan));
    f.render_widget(list, area);
}

/// Draw the tools list.
fn draw_tools_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let tools = [
        ("✓", "read_file"), ("✓", "write_file"), ("✓", "edit_file"),
        ("✓", "shell"), ("✓", "code_search"), ("✓", "web_fetch"),
        ("✓", "arxiv_search"), ("✓", "chart"), ("✓", "data_analyze"),
    ];
    let items: Vec<ListItem> = tools
        .iter()
        .map(|(icon, name)| {
            ListItem::new(format!(" {} {}", icon, name))
                .style(Style::default().fg(Color::Green))
        })
        .collect();
    let header = ListItem::new(format!(" Tools ({})", app.tool_count))
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    let mut all_items = vec![header];
    all_items.extend(items);
    let list = List::new(all_items);
    f.render_widget(list, area);
}

/// Draw the tasks list.
fn draw_tasks_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let task_text = if let Some(ref task) = app.active_task {
        format!("▶ Running: {}", task)
    } else {
        "No active tasks".to_string()
    };
    let items = vec![
        ListItem::new(" Active Tasks")
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ListItem::new(format!("  {}", task_text))
            .style(Style::default().fg(if app.active_task.is_some() {
                Color::Green
            } else {
                Color::DarkGray
            })),
    ];
    let list = List::new(items);
    f.render_widget(list, area);
}

/// Draw the main chat area.
fn draw_chat(f: &mut Frame, app: &TuiApp, area: Rect) {
    let block = Block::default()
        .title(format!(
            " Chat │ {} messages │ scroll: {} ",
            app.messages.len(),
            app.chat_scroll
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Build chat lines
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        match msg.role {
            MessageRole::User => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  You  ", Style::default().fg(Color::Black).bg(Color::Blue).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                ]));
                for line in msg.content.lines() {
                    lines.push(Line::from(format!("    {}", line)));
                }
            }
            MessageRole::Assistant => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(" Agent ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                ]));
                for line in msg.content.lines() {
                    lines.push(Line::from(format!("    {}", line)));
                }
                // Show tool calls
                for tc in &msg.tool_calls {
                    let (icon, color) = match tc.status {
                        ToolCallStatus::Running => ("⟳", Color::Yellow),
                        ToolCallStatus::Success => ("✓", Color::Green),
                        ToolCallStatus::Failed => ("✗", Color::Red),
                    };
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            format!("{} {}", icon, tc.name),
                            Style::default().fg(color),
                        ),
                    ]));
                }
            }
            MessageRole::System => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(" ℹ ", Style::default().fg(Color::Black).bg(Color::DarkGray)),
                    Span::styled(
                        format!(" {}", msg.content),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            MessageRole::Tool => {
                lines.push(Line::from(vec![
                    Span::styled(" Tool ", Style::default().fg(Color::Black).bg(Color::Magenta)),
                    Span::styled(
                        format!(" {}", msg.content),
                        Style::default().fg(Color::Magenta),
                    ),
                ]));
            }
        }
    }

    // Show streaming text if processing
    if app.is_processing && !app.streaming_text.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" Agent ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(" (streaming...)", Style::default().fg(Color::Yellow)),
        ]));
        for line in app.streaming_text.lines() {
            lines.push(Line::from(format!("    {}", line)));
        }
    } else if app.is_processing {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" Agent ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(" thinking...", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC)),
        ]));
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.chat_scroll, 0));
    f.render_widget(paragraph, inner);
}

/// Draw the input box at the bottom.
fn draw_input(f: &mut Frame, app: &TuiApp, area: Rect) {
    let title = if app.is_processing {
        " Input (Esc to cancel) "
    } else {
        " Input (Enter to send, / for commands, Ctrl+C to quit) "
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if app.is_processing {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        });

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Draw suggestions popup if any
    if !app.suggestions.is_empty() {
        let sug_height = (app.suggestions.len() as u16 + 2).min(10);
        let sug_area = Rect {
            x: inner.x,
            y: inner.y.saturating_sub(sug_height),
            width: inner.width.min(40),
            height: sug_height,
        };

        f.render_widget(Clear, sug_area);

        let items: Vec<ListItem> = app
            .suggestions
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let style = if i == app.selected_suggestion {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                ListItem::new(format!("  {} ", cmd)).style(style)
            })
            .collect();

        let sug_block = Block::default()
            .title(" Commands ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        let sug_list = List::new(items).block(sug_block);
        f.render_widget(sug_list, sug_area);
    }

    // Input text
    let input_text = if app.input.is_empty() && !app.is_processing {
        "Type a message or / for commands..."
    } else {
        &app.input
    };

    let input_style = if app.input.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    let input = Paragraph::new(input_text).style(input_style);
    f.render_widget(input, inner);

    // Show cursor
    if !app.is_processing {
        f.set_cursor_position((
            inner.x + app.cursor as u16,
            inner.y,
        ));
    }
}

/// Draw a centered diff popup.
fn draw_diff_popup(f: &mut Frame, diff: &DiffPopup, area: Rect) {
    let popup_area = centered_rect(80, 70, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Diff: {} (Esc to close) ", diff.file_path))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    // Render diff lines with colors
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
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
fn draw_approval_popup(f: &mut Frame, approval: &ApprovalRequest, area: Rect) {
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
            Span::styled("Tool: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&approval.tool_name),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Args: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&approval.args),
        ]),
        Line::from(""),
        Line::from(approval.prompt.clone()),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [y] Approve  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(" [n] Deny  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ]),
    ];

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

/// Helper: create a centered rectangle.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
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
