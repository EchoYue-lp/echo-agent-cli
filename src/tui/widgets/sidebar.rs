//! Left sidebar widget — tabs for Files, Tools, Tasks.
//! Catppuccin Mocha palette with Unicode icons.

use crate::tui::TuiApp;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Tabs};
use ratatui::Frame;

use super::Widget;

// Catppuccin Mocha
const BASE: Color = Color::Rgb(30, 30, 46);
const SURFACE0: Color = Color::Rgb(49, 50, 68);
const SURFACE1: Color = Color::Rgb(69, 71, 90);
const OVERLAY0: Color = Color::Rgb(108, 112, 134);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT: Color = Color::Rgb(166, 173, 200);
const BLUE: Color = Color::Rgb(137, 180, 250);
const GREEN: Color = Color::Rgb(166, 227, 161);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const CYAN: Color = Color::Rgb(137, 220, 235);
const LAVENDER: Color = Color::Rgb(180, 190, 254);
const TEAL: Color = Color::Rgb(148, 226, 213);

pub struct Sidebar;

impl Widget for Sidebar {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SURFACE1));

        let sidebar_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(5)])
            .split(area);

        // Tab selector with icons
        let titles = vec![
            Line::from(format!(" {} Files ", "\u{1f4c1}")),
            Line::from(format!(" {} Tools ", "\u{1f527}")),
            Line::from(format!(" {} Tasks ", "\u{1f4cb}")),
        ];
        let tabs = Tabs::new(titles)
            .select(app.sidebar_tab)
            .style(Style::default().fg(OVERLAY0))
            .highlight_style(
                Style::default()
                    .fg(CYAN)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::styled(" \u{2502} ", Style::default().fg(SURFACE0)));
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
    let dir = Style::default().fg(BLUE).add_modifier(Modifier::BOLD);
    let file = Style::default().fg(TEXT);
    let toml = Style::default().fg(YELLOW);

    let items = vec![
        ListItem::new(Line::from(vec![
            Span::styled("  \u{1f4c2} ", dir),
            Span::styled("src/", dir),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("     \u{1f4c2} ", dir),
            Span::styled("tui/", dir),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("     \u{1f4c2} ", dir),
            Span::styled("cli/", dir),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("     \u{1f980} ", Style::default().fg(TEAL)),
            Span::styled("main.rs", file),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("     \u{1f980} ", Style::default().fg(TEAL)),
            Span::styled("lib.rs", file),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("  \u{1f4c2} ", dir),
            Span::styled("echo-agent-app-core/", dir),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("  \u{1f4c2} ", dir),
            Span::styled("echo-agent-server/", dir),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("  \u{2699} ", toml),
            Span::styled("Cargo.toml", toml),
        ])),
    ];
    let list = List::new(items);
    f.render_widget(list, area);
}

fn render_tools_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let tools = [
        ("read_file", "\u{1f4d6}"),
        ("write_file", "\u{1f4dd}"),
        ("edit_file", "\u{270f}"),
        ("shell", "\u{1f4bb}"),
        ("code_search", "\u{1f50d}"),
        ("web_fetch", "\u{1f310}"),
        ("arxiv_search", "\u{1f4da}"),
        ("chart", "\u{1f4c8}"),
        ("data_analyze", "\u{1f4ca}"),
    ];
    let header = ListItem::new(Line::from(vec![
        Span::styled(
            format!("  {} Tools ", "\u{1f527}"),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", app.tool_count),
            Style::default().fg(OVERLAY0),
        ),
    ]));
    let mut all_items = vec![header];
    for (name, icon) in &tools {
        all_items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("    {} ", icon), Style::default().fg(SUBTEXT)),
            Span::styled(*name, Style::default().fg(LAVENDER)),
        ])));
    }
    let list = List::new(all_items);
    f.render_widget(list, area);
}

fn render_tasks_list(f: &mut Frame, app: &TuiApp, area: Rect) {
    let header = ListItem::new(Line::from(vec![
        Span::styled(
            format!("  {} Active Tasks", "\u{1f4cb}"),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
    ]));

    let task_item = if let Some(ref task) = app.active_task {
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("    {} ", "\u{25b6}"),
                Style::default().fg(GREEN),
            ),
            Span::styled(task.clone(), Style::default().fg(TEXT)),
        ]))
    } else {
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("    {} No active tasks", "\u{25cb}"),
                Style::default().fg(OVERLAY0).add_modifier(Modifier::ITALIC),
            ),
        ]))
    };

    let list = List::new(vec![header, task_item]);
    f.render_widget(list, area);
}
