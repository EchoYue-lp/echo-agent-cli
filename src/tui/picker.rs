//! Reusable scrollable list picker for the TUI.
//!
//! Used for session resume, model selection, mode selection, etc.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;

/// A single item in the picker list.
#[derive(Debug, Clone)]
pub struct PickerItem {
    /// Primary display text.
    pub label: String,
    /// Optional secondary text (right-aligned or dimmed).
    pub detail: Option<String>,
    /// Optional preview / description.
    pub preview: Option<String>,
    /// Opaque value for the caller to identify the item.
    pub value: String,
}

impl PickerItem {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            preview: None,
            value: value.into(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }
}

/// State for a scrollable list picker.
#[derive(Debug, Clone)]
pub struct Picker {
    /// Title shown at the top of the picker.
    pub title: String,
    /// All items.
    pub items: Vec<PickerItem>,
    /// Currently highlighted index.
    pub selected: usize,
    /// Scroll offset.
    pub scroll: usize,
    /// Whether the picker is visible.
    pub visible: bool,
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            items,
            selected: 0,
            scroll: 0,
            visible: true,
        }
    }

    /// Move selection up by one.
    pub fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        } else {
            self.selected = self.items.len() - 1;
        }
        self.adjust_scroll();
    }

    /// Move selection down by one.
    pub fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected < self.items.len() - 1 {
            self.selected += 1;
        } else {
            self.selected = 0;
        }
        self.adjust_scroll();
    }

    /// Get the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.items.get(self.selected)
    }

    /// Get the value of the currently selected item.
    pub fn selected_value(&self) -> Option<&str> {
        self.items.get(self.selected).map(|i| i.value.as_str())
    }

    /// Render the picker into the given frame area.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);

        let block = Block::default()
            .title(format!(" {} (Up/Down, Enter, Esc) ", self.title))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        f.render_widget(block, area);

        if self.items.is_empty() {
            let empty = Line::from(Span::styled(
                "  No items",
                Style::default().fg(Color::DarkGray),
            ));
            let p = ratatui::widgets::Paragraph::new(empty);
            f.render_widget(p, inner);
            return;
        }

        let visible_height = inner.height as usize;
        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(visible_height)
            .map(|(i, item)| {
                let is_selected = i == self.selected;
                let label_style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let mut spans = vec![Span::styled(
                    format!("  {}  ", item.label),
                    label_style,
                )];

                if let Some(ref detail) = item.detail {
                    let detail_style = if is_selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    spans.push(Span::styled(format!(" {}", detail), detail_style));
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items);
        f.render_widget(list, inner);

        // Scrollbar if needed.
        if self.items.len() > visible_height {
            let mut state = ScrollbarState::new(self.items.len())
                .position(self.selected);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(Color::DarkGray));
            f.render_stateful_widget(
                scrollbar,
                Rect {
                    x: area.x + area.width - 1,
                    y: inner.y,
                    width: 1,
                    height: inner.height,
                },
                &mut state,
            );
        }
    }

    fn adjust_scroll(&mut self) {
        // Keep selected item visible.
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        // We don't know the visible height here; the render function handles
        // the actual windowing. This is a best-effort adjustment.
    }

    /// Adjust scroll given a visible height.
    pub fn adjust_scroll_for_height(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible_height {
            self.scroll = self.selected - visible_height + 1;
        }
    }
}
