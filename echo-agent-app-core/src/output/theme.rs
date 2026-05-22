//! 颜色主题系统
//!
//! 提供6个内置主题,支持自定义主题。
//! `ColorTheme` 实现了 `Copy`,在闭包中可高效使用。

use nu_ansi_term::Color;

/// ANSI 颜色主题
#[derive(Debug, Clone, Copy)]
pub struct ColorTheme {
    pub name: &'static str,
    pub user_color: Color,
    pub assistant_color: Color,
    pub tool_color: Color,
    pub error_color: Color,
    pub success_color: Color,
    pub warning_color: Color,
    pub info_color: Color,
    pub heading_color: Color,
    pub code_block_bg: Color,
    pub border_color: Color,
    pub muted_color: Color,
    pub highlight_color: Color,
}

impl ColorTheme {
    /// 默认深色主题 (暗色终端背景)
    pub const fn dark() -> Self {
        Self {
            name: "dark",
            user_color: Color::Cyan,
            assistant_color: Color::Green,
            tool_color: Color::Yellow,
            error_color: Color::Red,
            success_color: Color::Green,
            warning_color: Color::Yellow,
            info_color: Color::Blue,
            heading_color: Color::LightCyan,
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::LightYellow,
        }
    }

    /// 浅色主题 (亮色终端背景)
    pub const fn light() -> Self {
        Self {
            name: "light",
            user_color: Color::Blue,
            assistant_color: Color::Green,
            tool_color: Color::Yellow,
            error_color: Color::Red,
            success_color: Color::Green,
            warning_color: Color::Yellow,
            info_color: Color::Blue,
            heading_color: Color::Cyan,
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::Yellow,
        }
    }

    /// Monokai 风格主题
    pub const fn monokai() -> Self {
        Self {
            name: "monokai",
            user_color: Color::Fixed(81),      // #66d9ef
            assistant_color: Color::Fixed(118), // #a6e22e
            tool_color: Color::Fixed(227),       // #e6db74
            error_color: Color::Fixed(197),      // #f92672
            success_color: Color::Fixed(118),
            warning_color: Color::Fixed(227),
            info_color: Color::Fixed(81),
            heading_color: Color::Fixed(208),    // #fd971f
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::Fixed(227),
        }
    }

    /// Solarized 风格主题
    pub const fn solarized() -> Self {
        Self {
            name: "solarized",
            user_color: Color::Fixed(33),       // #268bd2
            assistant_color: Color::Fixed(64),   // #859900
            tool_color: Color::Fixed(136),       // #b58900
            error_color: Color::Fixed(160),      // #dc322f
            success_color: Color::Fixed(64),
            warning_color: Color::Fixed(136),
            info_color: Color::Fixed(33),
            heading_color: Color::Fixed(37),     // #2aa198
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::Fixed(136),
        }
    }

    /// Dracula 风格主题
    pub const fn dracula() -> Self {
        Self {
            name: "dracula",
            user_color: Color::Fixed(117),      // #8be9fd
            assistant_color: Color::Fixed(84),   // #50fa7b
            tool_color: Color::Fixed(228),       // #f1fa8c
            error_color: Color::Fixed(203),      // #ff5555
            success_color: Color::Fixed(84),
            warning_color: Color::Fixed(228),
            info_color: Color::Fixed(141),       // #bd93f9
            heading_color: Color::Fixed(212),    // #ff79c6
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::Fixed(228),
        }
    }

    /// One Dark 风格主题 (类似 VS Code)
    pub const fn one_dark() -> Self {
        Self {
            name: "one-dark",
            user_color: Color::Fixed(75),       // #61afef
            assistant_color: Color::Fixed(114),  // #98c379
            tool_color: Color::Fixed(180),       // #e5c07b
            error_color: Color::Fixed(168),      // #e06c75
            success_color: Color::Fixed(114),
            warning_color: Color::Fixed(180),
            info_color: Color::Fixed(75),
            heading_color: Color::Fixed(177),    // #c678dd
            code_block_bg: Color::DarkGray,
            border_color: Color::DarkGray,
            muted_color: Color::DarkGray,
            highlight_color: Color::Fixed(180),
        }
    }

    /// 根据名称获取主题
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "dark" | "default" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "monokai" => Some(Self::monokai()),
            "solarized" => Some(Self::solarized()),
            "dracula" => Some(Self::dracula()),
            "one-dark" | "onedark" | "one_dark" => Some(Self::one_dark()),
            _ => None,
        }
    }

    /// 列出所有可用主题名
    pub fn available_names() -> &'static [&'static str] {
        &["dark", "light", "monokai", "solarized", "dracula", "one-dark"]
    }

    pub fn format_user(&self, text: &str) -> String {
        self.user_color.paint(text).to_string()
    }

    pub fn format_assistant(&self, text: &str) -> String {
        self.assistant_color.paint(text).to_string()
    }

    pub fn format_tool(&self, text: &str) -> String {
        self.tool_color.paint(text).to_string()
    }

    pub fn format_error(&self, text: &str) -> String {
        self.error_color.paint(text).to_string()
    }

    pub fn format_success(&self, text: &str) -> String {
        self.success_color.paint(text).to_string()
    }

    pub fn format_warning(&self, text: &str) -> String {
        self.warning_color.paint(text).to_string()
    }

    pub fn format_info(&self, text: &str) -> String {
        self.info_color.paint(text).to_string()
    }

    pub fn format_heading(&self, text: &str) -> String {
        self.heading_color.bold().paint(text).to_string()
    }

    pub fn format_muted(&self, text: &str) -> String {
        self.muted_color.paint(text).to_string()
    }

    pub fn format_highlight(&self, text: &str) -> String {
        self.highlight_color.bold().paint(text).to_string()
    }
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_from_name() {
        assert!(ColorTheme::from_name("dark").is_some());
        assert!(ColorTheme::from_name("light").is_some());
        assert!(ColorTheme::from_name("monokai").is_some());
        assert!(ColorTheme::from_name("solarized").is_some());
        assert!(ColorTheme::from_name("dracula").is_some());
        assert!(ColorTheme::from_name("one-dark").is_some());
        assert!(ColorTheme::from_name("unknown").is_none());
    }

    #[test]
    fn test_theme_names_all_available() {
        for name in ColorTheme::available_names() {
            assert!(ColorTheme::from_name(name).is_some());
        }
    }

    #[test]
    fn test_theme_formatting_does_not_panic() {
        let theme = ColorTheme::dark();
        let _ = theme.format_user("test");
        let _ = theme.format_assistant("test");
        let _ = theme.format_tool("test");
        let _ = theme.format_error("test");
        let _ = theme.format_success("test");
        let _ = theme.format_warning("test");
        let _ = theme.format_info("test");
        let _ = theme.format_heading("test");
        let _ = theme.format_muted("test");
        let _ = theme.format_highlight("test");
    }

    #[test]
    fn test_default_theme_is_dark() {
        let theme = ColorTheme::default();
        assert_eq!(theme.name, "dark");
    }
}
