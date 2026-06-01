//! TUI rendering — top-level layout and draw function.
//!
//! Delegates to individual widgets for each panel.

use super::TuiApp;
use super::widgets::Widget;
use super::widgets::chat::Chat;
use super::widgets::input::Input;
use super::widgets::popup::{draw_approval_popup, draw_diff_popup};
use super::widgets::sidebar::Sidebar;
use super::widgets::status_bar::StatusBar;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

/// Main draw function — renders the complete TUI layout.
pub fn draw(f: &mut Frame, app: &TuiApp) {
    let size = f.area();

    // Main layout: compact status + chat + compact input.
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
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

    // ── Popups (drawn on top) ──────────────────────────────────────────
    if let Some(ref diff) = app.diff_popup {
        draw_diff_popup(f, diff, size, &app.theme);
    }
    if let Some(ref approval) = app.approval {
        draw_approval_popup(f, approval, size, &app.theme);
    }

    // ── Picker (drawn on top of everything) ────────────────────────────
    if let Some(ref picker) = app.picker {
        if picker.visible {
            let picker_area = super::widgets::popup::centered_rect(50, 60, size);
            picker.render(f, picker_area);
        }
    }
}
