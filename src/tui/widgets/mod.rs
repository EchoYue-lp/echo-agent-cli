//! Widget trait and module declarations.

pub mod chat;
pub mod input;
pub mod popup;
pub mod sidebar;
pub mod status_bar;

use ratatui::Frame;
use ratatui::layout::Rect;

use super::TuiApp;

/// Common trait for all TUI widgets.
pub trait Widget {
    fn render(&self, f: &mut Frame, area: Rect, app: &TuiApp);
}
