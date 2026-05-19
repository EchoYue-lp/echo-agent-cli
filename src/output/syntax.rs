//! 代码语法高亮
//!
//! 基于 `syntect` 的语法高亮引擎，支持 100+ 编程语言。
//! 语法定义和主题从内嵌资源加载，零外部依赖。

use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::{SyntaxSet, SyntaxReference};
use syntect::util::LinesWithEndings;

/// 高亮后的代码片段
#[derive(Debug, Clone)]
pub struct HighlightedSpan {
    pub text: String,
    pub style: nu_ansi_term::Style,
}

/// 语法高亮器
pub struct SyntaxHighlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    /// 对代码字符串进行语法高亮
    ///
    /// `language` 参数支持常见语言名 (如 "rust", "python", "javascript", "toml", "json" 等)。
    /// 如果语言不被识别，回退到纯文本渲染。
    pub fn highlight(&self, code: &str, language: &str) -> Vec<HighlightedSpan> {
        let syntax = self
            .find_syntax(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];

        let mut spans: Vec<HighlightedSpan> = Vec::new();
        let mut highlighter = HighlightLines::new(syntax, theme);

        for line in LinesWithEndings::from(code) {
            if let Ok(ranges) = highlighter.highlight_line(line, &self.syntax_set) {
                for (style, text) in ranges {
                    let ansi_style = syntect_style_to_ansi(style);
                    // Merge with previous span if same style
                    if let Some(last) = spans.last_mut()
                        && last.style == ansi_style {
                            last.text.push_str(text);
                            continue;
                        }
                    spans.push(HighlightedSpan {
                        text: text.to_string(),
                        style: ansi_style,
                    });
                }
            }
        }

        spans
    }

    fn find_syntax(&self, language: &str) -> Option<&SyntaxReference> {
        // Try exact match first, then case-insensitive, then by extension
        self.syntax_set
            .find_syntax_by_name(language)
            .or_else(|| self.syntax_set.find_syntax_by_extension(language))
            .or_else(|| {
                // Common aliases
                let lookup = match language.to_lowercase().as_str() {
                    "js" => "javascript",
                    "ts" => "typescript",
                    "py" => "python",
                    "rb" => "ruby",
                    "sh" => "bash",
                    "zsh" => "bash",
                    "yml" => "yaml",
                    "md" => "markdown",
                    "rs" => "rust",
                    "go" => "golang",
                    "c++" | "cpp" => "c++",
                    "c#" | "cs" => "c#",
                    "kt" => "kotlin",
                    "swift" => "swift",
                    _ => language,
                };
                self.syntax_set.find_syntax_by_name(lookup)
            })
    }
}

fn syntect_style_to_ansi(style: Style) -> nu_ansi_term::Style {
    use nu_ansi_term::Color as AnsiColor;

    let mut ansi = nu_ansi_term::Style::new();

    ansi = ansi.fg(AnsiColor::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));

    if style.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
        ansi = ansi.bold();
    }
    if style.font_style.contains(syntect::highlighting::FontStyle::ITALIC) {
        ansi = ansi.italic();
    }
    if style.font_style.contains(syntect::highlighting::FontStyle::UNDERLINE) {
        ansi = ansi.underline();
    }

    ansi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_rust() {
        let h = SyntaxHighlighter::new();
        let spans = h.highlight("fn main() {\n    println!(\"hello\");\n}\n", "rust");
        assert!(!spans.is_empty());
    }

    #[test]
    fn test_highlight_plain_text_fallback() {
        let h = SyntaxHighlighter::new();
        let spans = h.highlight("just some text", "nonexistent-lang");
        assert!(!spans.is_empty());
    }

    #[test]
    fn test_highlight_js_alias() {
        let h = SyntaxHighlighter::new();
        let spans = h.highlight("const x = 1;", "js");
        assert!(!spans.is_empty());
    }
}
