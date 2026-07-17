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
        let Some(tab_area) = sidebar_chunks.first().copied() else {
            return;
        };
        let Some(content_area) = sidebar_chunks.get(1).copied() else {
            return;
        };
        f.render_widget(tabs, tab_area);

        // Tab content
        match app.sidebar_tab {
            0 => render_file_tree(f, content_area, t),
            1 => render_tools_list(f, app, content_area, t),
            2 => render_tasks_list(f, app, content_area, t),
            _ => {}
        }
    }
}

fn render_file_tree(_f: &mut Frame, area: Rect, t: &Theme) {
    let hint = Style::default()
        .fg(t.overlay0)
        .add_modifier(Modifier::ITALIC);
    let items = vec![ListItem::new(Line::from(vec![Span::styled(
        "  Project files will appear\n  here once a workspace\n  is loaded via /project",
        hint,
    )]))];
    let list = List::new(items);
    // safe: ratatui requires a mutable Frame ref
    _f.render_widget(list, area);
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
                format!("    {} tools available. Use /tools", app.tool_count),
                Style::default()
                    .fg(t.overlay0)
                    .add_modifier(Modifier::ITALIC),
            )])),
        ]
    };

    let list = List::new(items);
    f.render_widget(list, area);
}

fn render_tasks_list(_f: &mut Frame, app: &TuiApp, area: Rect, t: &Theme) {
    let mut items = vec![ListItem::new(Line::from(vec![Span::styled(
        format!("  {} TaskRuntime", "\u{1f4cb}"),
        Style::default().fg(t.cyan).add_modifier(Modifier::BOLD),
    )]))];
    if let Some(view) = &app.task_runtime_view {
        items.push(ListItem::new(Line::from(vec![
            Span::styled("  Run ", Style::default().fg(t.overlay0)),
            Span::styled(
                super::task_strip::truncate_str(&view.run_id, 14),
                Style::default().fg(t.text),
            ),
        ])));
        items.push(ListItem::new(Line::from(Span::styled(
            format!("  [{}]", view.status),
            Style::default().fg(status_color(&view.status, t)),
        ))));
        items.push(ListItem::new(Line::from(Span::styled(
            format!("  {}", super::task_strip::truncate_str(&view.goal, 20)),
            Style::default().fg(t.subtext),
        ))));
        items.push(ListItem::new(Line::from("")));
        for task in view
            .tasks
            .iter()
            .take(area.height.saturating_sub(6).saturating_div(2) as usize)
        {
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {} ", task_icon(&task.status)),
                    Style::default().fg(status_color(&task.status, t)),
                ),
                Span::styled(
                    super::task_strip::truncate_str(&task.title, 17),
                    Style::default().fg(t.text),
                ),
            ])));
            items.push(ListItem::new(Line::from(Span::styled(
                format!("      {}", task.agent_role),
                Style::default().fg(t.overlay0),
            ))));
        }
    } else if let Some(ref task) = app.active_task {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("    {} ", "\u{25b6}"), Style::default().fg(t.green)),
            Span::styled(task.as_str(), Style::default().fg(t.text)),
        ])));
    } else {
        items.push(ListItem::new(Line::from(vec![Span::styled(
            "    No task run for this conversation",
            Style::default()
                .fg(t.overlay0)
                .add_modifier(Modifier::ITALIC),
        )])));
    }

    if !app.subagent_runs.is_empty() {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(Line::from(Span::styled(
            "  Subagents",
            Style::default().fg(t.cyan).add_modifier(Modifier::BOLD),
        ))));
        for run in app.subagent_runs.iter().rev().take(4).rev() {
            let background = if run.background { " bg" } else { "" };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {} ", task_icon(&run.status)),
                    Style::default().fg(status_color(&run.status, t)),
                ),
                Span::styled(
                    super::task_strip::truncate_str(&run.agent, 12),
                    Style::default().fg(t.text),
                ),
                Span::styled(
                    format!(" {}t{}", run.tool_calls, background),
                    Style::default().fg(t.overlay0),
                ),
            ])));
            if !run.summary.trim().is_empty() {
                items.push(ListItem::new(Line::from(Span::styled(
                    format!(
                        "    {}",
                        super::task_strip::truncate_str(run.summary.trim(), 24)
                    ),
                    Style::default().fg(t.subtext),
                ))));
            }
            let mut evidence = Vec::new();
            if !run.artifacts.is_empty() {
                evidence.push(format!("{} artifacts", run.artifacts.len()));
            }
            if !run.verification.is_empty() {
                evidence.push(format!("{} checks", run.verification.len()));
            }
            if !run.remaining_work.is_empty() {
                evidence.push(format!("{} remaining", run.remaining_work.len()));
            }
            if !run.files_read.is_empty() || !run.files_written.is_empty() {
                evidence.push(format!(
                    "{}r/{}w files",
                    run.files_read.len(),
                    run.files_written.len()
                ));
            }
            if !evidence.is_empty() {
                let evidence = evidence.join(" · ");
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("    {}", super::task_strip::truncate_str(&evidence, 24)),
                    Style::default().fg(t.overlay0),
                ))));
            }
        }
    }

    let list = List::new(items);
    _f.render_widget(list, area);
}

fn task_icon(status: &str) -> &'static str {
    match status {
        "running" => "▶",
        "completed" => "✓",
        "failed" | "timed_out" => "×",
        "blocked" => "!",
        "skipped" => "-",
        _ => "○",
    }
}

fn status_color(status: &str, t: &Theme) -> ratatui::style::Color {
    match status {
        "running" => t.yellow,
        "completed" => t.green,
        "failed" | "blocked" | "cancelled" | "timed_out" => t.red,
        _ => t.overlay0,
    }
}
