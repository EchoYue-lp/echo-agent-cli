//! 代码语法高亮（使用 syntect） — 支持主题感知

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// 全局语法高亮器（懒加载）
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    /// 语法高亮后的单行 token 列表。每个元素是 (color, bold, italic, text)
    /// dark_mode 为 true 时使用 base16-ocean.dark，否则使用 base16-ocean.light
    pub fn highlight(&self, code: &str, language: &str, dark_mode: bool) -> Vec<Vec<(Color32, bool, bool, String)>> {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme_name = if dark_mode { "base16-ocean.dark" } else { "base16-ocean.light" };
        let theme = &self.theme_set.themes[theme_name];
        let mut h = HighlightLines::new(syntax, theme);
        let mut result: Vec<Vec<(Color32, bool, bool, String)>> = Vec::new();

        for line in LinesWithEndings::from(code) {
            let mut tokens = Vec::new();
            if let Ok(highlights) = h.highlight_line(line, &self.syntax_set) {
                for (style, text) in highlights {
                    let color = Color32::from_rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                    let bold = style.font_style.contains(syntect::highlighting::FontStyle::BOLD);
                    let italic = style.font_style.contains(syntect::highlighting::FontStyle::ITALIC);
                    tokens.push((color, bold, italic, text.to_string()));
                }
            }
            result.push(tokens);
        }
        result
    }
}

/// 从代码块语言标签映射到 syntect 语言标识符
pub fn lang_to_syntax(lang: &str) -> &str {
    match lang.to_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "py" | "python" => "python",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "go" | "golang" => "go",
        "java" => "java",
        "c" => "c",
        "cpp" | "c++" => "c++",
        "sh" | "bash" | "shell" => "bash",
        "zsh" => "bash",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        "md" | "markdown" => "markdown",
        "txt" | "text" | "plain" => "plain text",
        "rb" | "ruby" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kotlin" => "kotlin",
        "scala" => "scala",
        "r" => "r",
        "lua" => "lua",
        "perl" => "perl",
        "h" => "c",
        "hpp" | "h++" => "c++",
        _ => lang,
    }
}