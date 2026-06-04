//! Left sidebar widget — tabs for Files, Tools, Tasks.
//! Adaptive theme with Unicode icons.

use crate::tui::{Theme, TuiApp};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Tabs};

use super::Widget;

pub struct Sidebar;

impl Widget for Sidebar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let t = &app.theme;

        // No border block — draw a subtle vertical line separator on the right edge
        let sep_line = Paragraph::new("\u{2502}\n".repeat(area.height as usize))
            .style(Style::default().fg(t.surface0));
        let sep_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        f.render_widget(sep_line, sep_area);

        // Use area minus the separator column
        let content_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width.saturating_sub(1),
            height: area.height,
        };

        let sidebar_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(5)])
            .split(content_area);

        // Tab selector with icons
        let titles = vec![
            Line::from(format!(" {} Files ", "\u{1f4c1}")),
            Line::from(format!(" {} Tools ", "\u{1f527}")),
            Line::from(format!(" {} Tasks ", "\u{1f4cb}")),
        ];
        let tabs = Tabs::new(titles)
            .select(app.sidebar_tab)
            .style(Style::default().fg(t.overlay0))
            .highlight_style(Style::default().fg(t.cyan).add_modifier(Modifier::BOLD))
            .divider(Span::styled(" \u{2502} ", Style::default().fg(t.surface0)));
        f.render_widget(tabs, sidebar_chunks[0]);

        // Tab content
        match app.sidebar_tab {
            0 => render_file_tree(f, sidebar_chunks[1], t),
            1 => render_tools_list(f, app, sidebar_chunks[1], t),
            2 => render_tasks_list(f, app, sidebar_chunks[1], t),
            _ => {}
        }
    }
}

fn render_file_tree(f: &mut Frame, area: Rect, t: &Theme) {
    let hint = Style::default()
        .fg(t.overlay0)
        .add_modifier(Modifier::ITALIC);
    let items = vec![ListItem::new(Line::from(vec![Span::styled(
        "  No project loaded\n  Press /workspace to open",
        hint,
    )]))];
    let list = List::new(items);
    f.render_widget(list, area);
}

fn render_tools_list(f: &mut Frame, app: &TuiApp, area: Rect, t: &Theme) {
    let header = ListItem::new(Line::from(vec![
        Span::styled(
            format!("  {} Tools ", "\u{1f527}"),
            Style::default().fg(t.cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", app.tool_count),
            Style::default().fg(t.overlay0),
        ),
    ]));

    let items = if app.tool_count == 0 {
        vec![
            header,
            ListItem::new(Line::from(vec![Span::styled(
                "    No tools registered",
                Style::default()
                    .fg(t.overlay0)
                    .add_modifier(Modifier::ITALIC),
            )])),
        ]
    } else {
        vec![
            header,
            ListItem::new(Line::from(vec![Span::styled(
                "    Tool list available via API",
                Style::default()
                    .fg(t.overlay0)
                    .add_modifier(Modifier::ITALIC),
            )])),
        ]
    };

    let list = List::new(items);
    f.render_widget(list, area);
}

fn render_tasks_list(f: &mut Frame, app: &TuiApp, area: Rect, t: &Theme) {
    let header = ListItem::new(Line::from(vec![Span::styled(
        format!("  {} Active Tasks", "\u{1f4cb}"),
        Style::default().fg(t.cyan).add_modifier(Modifier::BOLD),
    )]));

    let task_item = if let Some(ref task) = app.active_task {
        ListItem::new(Line::from(vec![
            Span::styled(format!("    {} ", "\u{25b6}"), Style::default().fg(t.green)),
            Span::styled(task.as_str(), Style::default().fg(t.text)),
        ]))
    } else {
        ListItem::new(Line::from(vec![Span::styled(
            format!("    {} No active tasks", "\u{25cb}"),
            Style::default()
                .fg(t.overlay0)
                .add_modifier(Modifier::ITALIC),
        )]))
    };

    let list = List::new(vec![header, task_item]);
    f.render_widget(list, area);
}
