//! Markdown rendering for the TUI chat area.
//!
//! Converts markdown text into ratatui `Line`/`Span` with proper styling.
//! Uses `pulldown-cmark` for parsing and `syntect` for code syntax highlighting.
//! Handles partial/incomplete markdown gracefully (streaming-safe).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use std::sync::OnceLock;

// ── Lazy singletons for syntect ─────────────────────────────────────────────

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

fn syntect_theme() -> &'static syntect::highlighting::Theme {
    // "base16-ocean.dark" is a good dark theme.
    &theme_set().themes["base16-ocean.dark"]
}

// ── Color helpers ───────────────────────────────────────────────────────────

fn syntect_color_to_ratatui(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Render markdown text into a vector of ratatui Lines.
///
/// Safe to call on partial / streaming markdown -- incomplete code blocks and
/// unclosed tags are handled gracefully.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut renderer = MarkdownRenderer::new();
    renderer.render(text);
    renderer.lines
}

// ── Renderer state machine ──────────────────────────────────────────────────

struct MarkdownRenderer {
    lines: Vec<Line<'static>>,
    current_spans: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    in_code_block: bool,
    code_block_lang: Option<String>,
    code_block_text: String,
    heading_level: u8,
    list_depth: usize,
    in_blockquote: bool,
    // Table rendering state
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    // Task list state
    task_list_checked: Option<bool>,
}

impl MarkdownRenderer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: Vec::new(),
            in_code_block: false,
            code_block_lang: None,
            code_block_text: String::new(),
            heading_level: 0,
            list_depth: 0,
            in_blockquote: false,
            in_table: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            task_list_checked: None,
        }
    }

    fn render(&mut self, text: &str) {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TABLES);

        let parser = Parser::new_ext(text, opts);

        for event in parser {
            self.handle_event(event);
        }

        // Flush any pending inline spans.
        self.flush_line();

        // If still in a code block (streaming: incomplete fence), render what we have.
        if self.in_code_block {
            self.render_code_block();
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            // ── Block-level tags ──────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                self.heading_level = level as u8;
                self.lines.push(Line::from(""));
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush_line();
                self.heading_level = 0;
            }
            Event::Start(Tag::Paragraph) => {
                self.lines.push(Line::from(""));
            }
            Event::End(TagEnd::Paragraph) => {
                self.flush_line();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.in_blockquote = true;
                self.lines.push(Line::from(""));
            }
            Event::End(TagEnd::BlockQuote) => {
                self.flush_line();
                self.in_blockquote = false;
            }

            // ── Code blocks ──────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                self.in_code_block = true;
                self.code_block_text.clear();
                self.code_block_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => {
                        let l = lang.to_string();
                        if l.is_empty() { None } else { Some(l) }
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                self.render_code_block();
                self.in_code_block = false;
                self.code_block_lang = None;
                self.code_block_text.clear();
            }

            // ── Lists ────────────────────────────────────────────────────────
            Event::Start(Tag::List(_)) => {
                self.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                self.list_depth = self.list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                self.lines.push(Line::from(""));
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                // Check if this is a task list item
                if let Some(checked) = self.task_list_checked {
                    let marker = if checked { "[x] " } else { "[ ] " };
                    self.push_span(Span::styled(
                        format!("{}{}", indent, marker),
                        Style::default().fg(Color::Cyan),
                    ));
                    self.task_list_checked = None;
                } else {
                    let bullet = format!("{}  - ", indent);
                    self.push_span(Span::styled(bullet, Style::default().fg(Color::Cyan)));
                }
            }
            Event::End(TagEnd::Item) => {
                self.flush_line();
            }

            // ── Task list markers ────────────────────────────────────────────
            Event::TaskListMarker(checked) => {
                self.task_list_checked = Some(checked);
            }

            // ── Tables ───────────────────────────────────────────────────────
            Event::Start(Tag::Table { .. }) => {
                self.in_table = true;
                self.table_rows.clear();
                self.current_row.clear();
                self.current_cell.clear();
            }
            Event::End(TagEnd::Table) => {
                self.render_table();
                self.in_table = false;
                self.table_rows.clear();
                self.current_row.clear();
                self.current_cell.clear();
            }
            Event::Start(Tag::TableHead) => {
                // Table head begins a new row
            }
            Event::End(TagEnd::TableHead)
                // End of header row - add to rows
                if !self.current_row.is_empty() => {
                    self.table_rows.push(std::mem::take(&mut self.current_row));
                }
            Event::Start(Tag::TableRow) => {
                self.current_row.clear();
            }
            Event::End(TagEnd::TableRow)
                if !self.current_row.is_empty() => {
                    self.table_rows.push(std::mem::take(&mut self.current_row));
                }
            Event::Start(Tag::TableCell { .. }) => {
                self.current_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                self.current_row.push(std::mem::take(&mut self.current_cell));
            }

            // ── Inline formatting ────────────────────────────────────────────
            Event::Start(Tag::Strong) => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                self.pop_style();
            }
            Event::Start(Tag::Emphasis) => {
                self.push_style(Style::default().add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                self.pop_style();
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT),
                );
            }
            Event::End(TagEnd::Strikethrough) => {
                self.pop_style();
            }

            // ── Links ────────────────────────────────────────────────────────
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.push_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
                // We'll append the URL after the link text in End.
                // Store it in the style stack via a trick: we don't have a place for it,
                // so we just render the text and hope the URL isn't needed inline.
                // (For a full implementation, we'd store it; here the text is enough.)
                let _ = dest_url;
            }
            Event::End(TagEnd::Link) => {
                self.pop_style();
            }

            // ── Text content ─────────────────────────────────────────────────
            Event::Text(text) => {
                if self.in_code_block {
                    self.code_block_text.push_str(&text);
                } else if self.in_table {
                    self.current_cell.push_str(&text);
                } else {
                    let style = self.current_style();
                    let style = if self.heading_level > 0 {
                        self.heading_style(style)
                    } else if self.in_blockquote {
                        style.fg(Color::DarkGray)
                    } else {
                        style
                    };
                    // Split on newlines for multi-line text events.
                    for (i, part) in text.split('\n').enumerate() {
                        if i > 0 {
                            self.flush_line();
                        }
                        if !part.is_empty() {
                            self.push_span(Span::styled(part.to_string(), style));
                        }
                    }
                }
            }

            Event::Code(code) => {
                // Inline code.
                let style = Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD);
                self.push_span(Span::styled(format!(" {} ", code), style));
            }

            Event::SoftBreak => {
                self.flush_line();
            }
            Event::HardBreak => {
                self.flush_line();
                self.lines.push(Line::from(""));
            }

            // ── Rules, tables, etc. ──────────────────────────────────────────
            Event::Rule => {
                self.lines.push(Line::from(""));
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
                self.lines.push(Line::from(""));
            }

            // Ignore other events (tables, footnotes, HTML, etc.) for now.
            _ => {}
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn push_span(&mut self, span: Span<'static>) {
        self.current_spans.push(span);
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn current_style(&self) -> Style {
        self.style_stack
            .iter()
            .copied()
            .fold(Style::default(), |acc, s| acc.patch(s))
    }

    fn heading_style(&self, base: Style) -> Style {
        match self.heading_level {
            1 => base.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            2 => base.fg(Color::Green).add_modifier(Modifier::BOLD),
            3 => base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            _ => base.add_modifier(Modifier::BOLD),
        }
    }

    fn flush_line(&mut self) {
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            self.lines.push(Line::from(spans));
        }
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        // Determine column widths
        let num_cols = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return;
        }

        let mut col_widths = vec![0usize; num_cols];
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate().take(num_cols) {
                col_widths[i] = col_widths[i].max(cell.len().min(40));
            }
        }

        // Clamp column widths to a reasonable total
        let total_width: usize = col_widths.iter().sum::<usize>() + num_cols * 3 + 1;
        if total_width > 120 {
            // Scale down proportionally
            let scale = 120.0 / total_width as f64;
            for w in &mut col_widths {
                *w = (*w as f64 * scale).max(3.0) as usize;
            }
        }

        let mut table_lines = Vec::new();

        // Separator line
        let sep_parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(*w + 2)).collect();
        let sep = format!("┌{}┐", sep_parts.join("┬"));
        table_lines.push(sep);

        for (row_idx, row) in self.table_rows.iter().enumerate() {
            let mut line = String::from("│");
            for (col_idx, cell) in row.iter().enumerate() {
                let width = col_widths.get(col_idx).copied().unwrap_or(10);
                let truncated = if cell.len() > width {
                    format!("{:.width$}...", cell, width = width.saturating_sub(3))
                } else {
                    cell.to_string()
                };
                line.push_str(&format!(" {:<width$} │", truncated, width = width));
            }
            table_lines.push(line);

            // Separator after header row
            if row_idx == 0 {
                let sep_inner: Vec<String> =
                    col_widths.iter().map(|w| "─".repeat(*w + 2)).collect();
                table_lines.push(format!("├{}┤", sep_inner.join("┼")));
            }
        }

        // Bottom border
        let bottom_parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(*w + 2)).collect();
        table_lines.push(format!("└{}┘", bottom_parts.join("┴")));

        for line in table_lines {
            self.lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    fn render_code_block(&mut self) {
        if self.code_block_text.is_empty() {
            return;
        }

        self.lines.push(Line::from(""));

        // Language label
        if let Some(ref lang) = self.code_block_lang {
            self.lines.push(Line::from(Span::styled(
                format!("  {}", lang),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        }

        // Try syntax highlighting
        let highlighted = self.highlight_code();
        for hl_line in highlighted {
            self.lines.push(hl_line);
        }

        self.lines.push(Line::from(""));
    }

    fn highlight_code(&self) -> Vec<Line<'static>> {
        let ss = syntax_set();
        let theme = syntect_theme();

        // Find syntax by language name, fallback to plain text.
        let syntax = self
            .code_block_lang
            .as_deref()
            .and_then(|lang| ss.find_syntax_by_token(lang))
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        let mut h = HighlightLines::new(syntax, theme);
        let mut result = Vec::new();

        for line in LinesWithEndings::from(&self.code_block_text) {
            let mut spans = Vec::new();
            // Indent for code block
            spans.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));

            match h.highlight_line(line, ss) {
                Ok(regions) => {
                    for (style, text) in regions {
                        let fg = syntect_color_to_ratatui(style.foreground);
                        let text = text.trim_end_matches('\n');
                        if !text.is_empty() {
                            spans.push(Span::styled(text.to_string(), Style::default().fg(fg)));
                        }
                    }
                }
                Err(_) => {
                    let text = line.trim_end_matches('\n');
                    spans.push(Span::styled(
                        text.to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            result.push(Line::from(spans));
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plain_text() {
        let lines = render_markdown("Hello world");
        assert!(!lines.is_empty());
    }

    #[test]
    fn render_heading() {
        let lines = render_markdown("# Title\n\nSome text.");
        assert!(lines.len() >= 2);
    }

    #[test]
    fn render_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md);
        assert!(lines.len() >= 3);
    }

    #[test]
    fn render_partial_code_block() {
        // Streaming: incomplete fence
        let md = "```rust\nfn main() {";
        let lines = render_markdown(md);
        assert!(!lines.is_empty());
    }

    #[test]
    fn render_inline_code() {
        let lines = render_markdown("Use `println!` macro.");
        assert!(!lines.is_empty());
    }

    #[test]
    fn render_list() {
        let md = "- item 1\n- item 2\n- item 3";
        let lines = render_markdown(md);
        assert!(lines.len() >= 3);
    }

    #[test]
    fn render_bold_italic() {
        let lines = render_markdown("**bold** and *italic*");
        assert!(!lines.is_empty());
    }
}
