//! Markdown → Terminal 渲染器
//!
//! 基于 `pulldown-cmark` 解析 Markdown 并渲染为 ANSI 终端输出。
//! 支持标题、代码块、列表、表格、引用块、链接等常见元素。

use pulldown_cmark::{CowStr, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::syntax::SyntaxHighlighter;
use super::theme::ColorTheme;
use nu_ansi_term::Color;

/// 将 Markdown 文本渲染到终端
pub fn render_markdown_to_terminal(
    md: &str,
    color_enabled: bool,
    theme: ColorTheme,
    max_width: usize,
    highlighter: &SyntaxHighlighter,
) {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(md, options);

    let mut renderer = MarkdownRenderer {
        color_enabled,
        theme,
        max_width,
        highlighter,
        indent_level: 0,
        list_counter: Vec::new(),
        in_code_block: false,
        code_lang: String::new(),
        code_buffer: String::new(),
        table_headers: Vec::new(),
        table_rows: Vec::new(),
        in_table_head: false,
        in_table_row: false,
        current_cell: String::new(),
        current_row: Vec::new(),
    };

    for event in parser {
        renderer.handle_event(event);
    }

    // flush any remaining content
    renderer.flush();
    println!();
}

struct MarkdownRenderer<'a> {
    color_enabled: bool,
    theme: ColorTheme,
    max_width: usize,
    highlighter: &'a SyntaxHighlighter,
    indent_level: usize,
    list_counter: Vec<usize>,
    in_code_block: bool,
    code_lang: String,
    code_buffer: String,
    table_headers: Vec<String>,
    table_rows: Vec<Vec<String>>,
    in_table_head: bool,
    in_table_row: bool,
    current_cell: String,
    current_row: Vec<String>,
}

impl<'a> MarkdownRenderer<'a> {
    fn paint(&self, color: Color, text: &str) -> String {
        if self.color_enabled {
            color.paint(text).to_string()
        } else {
            text.to_string()
        }
    }

    fn bold(&self, text: &str) -> String {
        if self.color_enabled {
            nu_ansi_term::Style::new().bold().paint(text).to_string()
        } else {
            text.to_string()
        }
    }

    fn indent(&self) -> String {
        "  ".repeat(self.indent_level)
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Text(text) => self.handle_text(&text),
            Event::Code(code) => {
                let styled = self.paint(self.theme.code_block_bg, &format!(" `{}` ", code));
                print!("{}", styled);
            }
            Event::HardBreak => println!(),
            Event::SoftBreak => print!(" "),
            Event::Rule => {
                let rule = self.paint(self.theme.muted_color, &"—".repeat(self.max_width.min(80)));
                println!("\n{}", rule);
            }
            _ => {}
        }
    }

    fn handle_start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                println!();
                print!("{} ", self.indent());
                let heading_prefix = match level {
                    HeadingLevel::H1 => "#",
                    HeadingLevel::H2 => "##",
                    HeadingLevel::H3 => "###",
                    HeadingLevel::H4 => "####",
                    HeadingLevel::H5 => "#####",
                    HeadingLevel::H6 => "######",
                };
                print!("{} ", self.paint(self.theme.heading_color, heading_prefix));
            }
            Tag::Paragraph => {
                print!("{}", self.indent());
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_buffer.clear();
            }
            Tag::List(order) => {
                self.list_counter.push(0);
                if order.is_some() {
                    println!();
                }
            }
            Tag::Item => {
                if let Some(counter) = self.list_counter.last_mut() {
                    *counter += 1;
                }
                print!("{}", self.indent());
            }
            Tag::Emphasis => {}
            Tag::Strong => {}
            Tag::Strikethrough
                if self.color_enabled => {
                    print!("{}", nu_ansi_term::Style::new().strikethrough().prefix());
                }
            Tag::BlockQuote(_) => {
                self.indent_level += 1;
                print!("{}", self.paint(self.theme.muted_color, "▍ "));
            }
            Tag::Table(_) => {
                self.table_headers.clear();
                self.table_rows.clear();
            }
            Tag::TableHead => {
                self.in_table_head = true;
                self.current_row.clear();
            }
            Tag::TableRow => {
                self.in_table_row = true;
                self.current_row.clear();
            }
            Tag::TableCell => {
                self.current_cell.clear();
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                println!();
            }
            TagEnd::Paragraph => {
                println!();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.render_code_block();
                self.code_buffer.clear();
            }
            TagEnd::List(_) => {
                self.list_counter.pop();
                println!();
            }
            TagEnd::Item => {}
            TagEnd::Strikethrough
                if self.color_enabled => {
                    print!("{}", nu_ansi_term::Style::new().strikethrough().suffix());
                }
            TagEnd::BlockQuote => {
                self.indent_level = self.indent_level.saturating_sub(1);
                println!();
            }
            TagEnd::Table => {
                self.render_table();
            }
            TagEnd::TableHead => {
                self.in_table_head = false;
                self.table_headers = self.current_row.clone();
                self.current_row.clear();
            }
            TagEnd::TableRow => {
                self.in_table_row = false;
                if !self.in_table_head && !self.current_row.is_empty() {
                    self.table_rows.push(self.current_row.clone());
                }
                self.current_row.clear();
            }
            TagEnd::TableCell => {
                self.current_row.push(self.current_cell.clone());
                self.current_cell.clear();
            }
            _ => {}
        }
    }

    fn handle_text(&mut self, text: &CowStr) {
        if self.in_code_block {
            self.code_buffer.push_str(text);
            return;
        }
        print!("{}", text);
    }

    fn render_code_block(&self) {
        let code = self.code_buffer.trim();
        if code.is_empty() {
            return;
        }
        let lang = if self.code_lang.is_empty() {
            ""
        } else {
            &self.code_lang
        };

        let width = self.max_width.min(80);
        let top = self.paint(self.theme.border_color, &format!("┌─ {} ─┐", if lang.is_empty() { "code" } else { lang }));
        let bottom = self.paint(self.theme.border_color, &format!("└{}┘", "─".repeat(width.saturating_sub(2))));

        println!("{}", top);

        if self.color_enabled && !lang.is_empty() {
            let highlighted = self.highlighter.highlight(code, lang);
            for span in &highlighted {
                let styled = span.style.paint(&span.text);
                print!("{}", styled);
            }
            if !code.ends_with('\n') {
                println!();
            }
        } else {
            for line in code.lines() {
                println!(" {} {}", self.paint(self.theme.muted_color, "│"), line);
            }
        }
        println!("{}", bottom);
    }

    fn render_table(&self) {
        if self.table_headers.is_empty() && self.table_rows.is_empty() {
            return;
        }

        let all_headers: Vec<&str> = self.table_headers.iter().map(|s| s.as_str()).collect();
        let all_rows: Vec<Vec<String>> = self.table_headers.first().map_or_else(
            || self.table_rows.clone(),
            |_| {
                let mut rows = vec![self.table_headers.clone()];
                rows.extend(self.table_rows.clone());
                rows
            },
        );

        // Determine column widths
        let col_count = all_headers.len().max(
            all_rows.iter().map(|r| r.len()).max().unwrap_or(0),
        );
        if col_count == 0 {
            return;
        }

        let _available = self.max_width.saturating_sub(col_count * 3 + 1);
        let mut col_widths = vec![0usize; col_count];
        for row in &all_rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    col_widths[i] = col_widths[i].max(cell.chars().count().min(40));
                }
            }
        }
        for header in &all_headers {
            if let Some(i) = all_headers.iter().position(|h| h == header)
                && i < col_count {
                    col_widths[i] = col_widths[i].max(header.len().min(40));
                }
        }

        let total: usize = col_widths.iter().sum::<usize>() + col_count * 3 + 1;
        let scale = if total > self.max_width && self.max_width > 20 {
            self.max_width as f64 / total as f64
        } else {
            1.0
        };

        let col_widths: Vec<usize> = col_widths
            .iter()
            .map(|w| ((*w as f64) * scale) as usize)
            .collect();

        // Render
        let separator = || {
            let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(*w)).collect();
            format!("└{}┘", parts.join("─┴─"))
        };

        let header_sep = || {
            let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(*w)).collect();
            format!("├{}┤", parts.join("─┼─"))
        };

        // Top border
        let top = {
            let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(*w)).collect();
            self.paint(self.theme.border_color, &format!("┌{}┐", parts.join("─┬─")))
        };
        let bot = self.paint(self.theme.border_color, &separator());
        let sep = self.paint(self.theme.border_color, &header_sep());

        println!();
        println!("{}", top);

        if !self.table_headers.is_empty() {
            let header_cells: Vec<String> = self
                .table_headers
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let w = col_widths.get(i).copied().unwrap_or(10);
                    format!("{:^w$}", h)
                })
                .collect();
            let header_line = self.paint(self.theme.border_color, "│");
            println!(
                "{}{}{}",
                header_line,
                self.bold(&header_cells.join(&self.paint(self.theme.border_color, "│"))),
                header_line
            );
            println!("{}", sep);
        }

        for (row_idx, row) in self.table_rows.iter().enumerate() {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let w = col_widths.get(i).copied().unwrap_or(10);
                    format!("{:w$}", c)
                })
                .collect();
            let border = self.paint(self.theme.border_color, "│");
            let text = if self.color_enabled && row_idx % 2 == 1 {
                self.paint(Color::DarkGray, &cells.join(&self.paint(self.theme.border_color, "│")))
            } else {
                cells.join(&self.paint(self.theme.border_color, "│"))
            };
            println!("{}{}{}", border, text, border);
        }

        println!("{}", bot);
    }

    fn flush(&mut self) {
        if self.in_code_block {
            self.render_code_block();
            self.in_code_block = false;
            self.code_buffer.clear();
        }
    }
}
