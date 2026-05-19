//! ASCII 表格渲染器
//!
//! 支持自动列宽、文本换行、数字右对齐、自适应终端宽度。

use super::theme::ColorTheme;
use nu_ansi_term::Color;

pub struct TableRenderer;

impl TableRenderer {
    /// 渲染带表头的表格
    pub fn render(
        headers: &[&str],
        rows: &[Vec<String>],
        color_enabled: bool,
        theme: ColorTheme,
        max_width: usize,
    ) {
        if headers.is_empty() && rows.is_empty() {
            return;
        }

        let col_count = headers.len().max(
            rows.iter().map(|r| r.len()).max().unwrap_or(0),
        );
        if col_count == 0 {
            return;
        }

        // Calculate column widths
        let mut widths = vec![0usize; col_count];
        for (i, h) in headers.iter().enumerate() {
            widths[i] = widths[i].max(h.chars().count());
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    widths[i] = widths[i].max(cell.chars().count());
                }
            }
        }

        // Cap & scale
        for w in widths.iter_mut() {
            *w = (*w).clamp(3, 50);
        }
        let total: usize = widths.iter().sum::<usize>() + col_count * 3 + 1;
        let scale = if total > max_width && max_width > 20 {
            max_width as f64 / total as f64
        } else {
            1.0
        };
        let widths: Vec<usize> = widths.iter().map(|w| ((*w as f64) * scale) as usize).collect();

        let paint = |c: Color, t: &str| -> String {
            if color_enabled {
                c.paint(t).to_string()
            } else {
                t.to_string()
            }
        };

        let border_color = theme.border_color;

        // Top border
        let top_parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
        println!(
            "{}",
            paint(border_color, &format!("┌{}┐", top_parts.join("─┬─")))
        );

        // Header row
        let header_cells: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:^w$}", h, w = widths[i]))
            .collect();
        let b = paint(border_color, "│");
        let header_text: String = header_cells.join(&paint(border_color, "│"));
        let styled_header = if color_enabled {
            nu_ansi_term::Style::new().bold().paint(&header_text).to_string()
        } else {
            header_text
        };
        println!("{}{}{}", b, styled_header, b);

        // Separator
        let sep_parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
        println!(
            "{}",
            paint(border_color, &format!("├{}┤", sep_parts.join("─┼─")))
        );

        // Data rows
        for (row_idx, row) in rows.iter().enumerate() {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let w = *widths.get(i).unwrap_or(&10);
                    // Detect if cell looks numeric
                    if is_numeric(c) {
                        format!("{:>w$}", c)
                    } else {
                        format!("{:w$}", c)
                    }
                })
                .collect();
            let line = cells.join(&paint(border_color, "│"));
            let styled = if color_enabled && row_idx % 2 == 1 {
                paint(Color::DarkGray, &line)
            } else {
                line
            };
            println!("{}{}{}", b, styled, b);
        }

        // Bottom border
        let bot_parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
        println!(
            "{}",
            paint(border_color, &format!("└{}┘", bot_parts.join("─┴─")))
        );
    }

    /// 渲染键值对表格 (两列: Key | Value)
    pub fn render_kv(
        pairs: &[(&str, &str)],
        color_enabled: bool,
        theme: ColorTheme,
        max_width: usize,
    ) {
        if pairs.is_empty() {
            return;
        }

        let key_width = pairs
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(10)
            .min(30);

        let val_width = max_width
            .saturating_sub(key_width + 7)
            .max(10);

        let paint = |c: Color, t: &str| -> String {
            if color_enabled {
                c.paint(t).to_string()
            } else {
                t.to_string()
            }
        };

        let b = paint(theme.border_color, "│");
        let top = paint(
            theme.border_color,
            &format!("┌{}┬{}┐", "─".repeat(key_width), "─".repeat(val_width)),
        );
        let bot = paint(
            theme.border_color,
            &format!("└{}┴{}┘", "─".repeat(key_width), "─".repeat(val_width)),
        );

        println!("{}", top);

        for (i, (key, value)) in pairs.iter().enumerate() {
            if i > 0 {
                let separator = paint(
                    theme.border_color,
                    &format!("├{}┼{}┤", "─".repeat(key_width), "─".repeat(val_width)),
                );
                println!("{}", separator);
            }
            let key_styled = paint(theme.heading_color, &format!("{:>w$}", key, w = key_width));
            let val = truncate_or_wrap(value, val_width);
            let val_styled = paint(theme.info_color, &format!("{:w$}", val, w = val_width));
            println!("{}{}{}{}{}", b, key_styled, b, val_styled, b);
        }

        println!("{}", bot);
    }
}

fn is_numeric(s: &str) -> bool {
    s.trim().parse::<f64>().is_ok()
}

fn truncate_or_wrap(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let truncated: String = s.chars().take(width.saturating_sub(3)).collect();
    format!("{}...", truncated)
}
