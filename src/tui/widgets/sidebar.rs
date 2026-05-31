//! Left sidebar widget — tabs for Files, Tools, Tasks.

use crate::tui::TuiApp;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Tabs};
use ratatui::Frame;

use super::Widget;

pub struct Sidebar;

impl Widget for Sidebar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(Color::DarkGray));

        let sidebar_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(5)])
            .split(area);

        // Tab selector
        let titles = vec![
            Line::from(" Files "),
            Line::from(" Tools "),
            Line::from(" Tasks "),
        ];
        let tabs = Tabs::new(titles)
            .select(app.sidebar_tab)
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::styled("|", Style::default().fg(Color::DarkGray)));
        f.render_widget(tabs, sidebar_chunks[0]);

        // Tab content
        match app.sidebar_tab {
            0 => render_file_tree(f, sidebar_chunks[1]),
            1 => render_tools_list(f, app, sidebar_chunks[1]),
            2 => render_tasks_list(f, app, sidebar_chunks[1]),
            _ => {}
        }

        f.render_widget(block, area);
    }
}

fn render_file_tree(f: &mut Frame, area: Rect) {
    let items = vec![
        ListItem::new(Span::styled(
            "  src/",
            Style::default().fg(Color::Cyan),
        )),
        ListItem::new(Span::styled(
            "    tui/",
            Style::default().fg(Color::Cyan),
        )),
        ListItem::new(Span::styled(
            "    cli/",
            Style::default().fg(Color::Cyan),
        )),
        ListItem::new(Span::raw("    main.rs")),
        ListItem::new(Span::raw("    lib.rs")),
        ListItem::new(Span::styled(
            "  echo-agent-app-core/",
            Style::default().fg(Color::Cyan),
        )),
        ListItem::new(Span::styled(
            "  echo-agent-server/",
            Style::default().fg(Color::Cyan),
        )),
        ListItem::new(Span::raw("  Cargo.toml")),
    ];
    let list = List::new(items).style(Style::default().fg(Color::White));
    f.render_widget(list, area);
}

fn render_tools_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let tools = [
        "read_file",
        "write_file",
        "edit_file",
        "shell",
        "code_search",
        "web_fetch",
        "arxiv_search",
        "chart",
        "data_analyze",
    ];
    let header = ListItem::new(Span::styled(
        format!(" Tools ({})", app.tool_count),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));
    let mut all_items = vec![header];
    for name in &tools {
        all_items.push(ListItem::new(Line::from(vec![
            Span::styled(" + ", Style::default().fg(Color::Green)),
            Span::raw(*name),
        ])));
    }
    let list = List::new(all_items);
    f.render_widget(list, area);
}

fn render_tasks_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let header = ListItem::new(Span::styled(
        " Active Tasks",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));

    let task_item = if let Some(ref task) = app.active_task {
        ListItem::new(Span::styled(
            format!("  > {}", task),
            Style::default().fg(Color::Green),
        ))
    } else {
        ListItem::new(Span::styled(
            "  No active tasks",
            Style::default().fg(Color::DarkGray),
        ))
    };

    let list = List::new(vec![header, task_item]);
    f.render_widget(list, area);
}
