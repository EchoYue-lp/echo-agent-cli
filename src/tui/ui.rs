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
use ratatui::layout::{Constraint, Direction, Layout};

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
}
