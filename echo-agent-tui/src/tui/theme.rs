//! ratatui 颜色主题映射
//!
//! 将 `ColorTheme` 的 ANSI 颜色映射到 ratatui 的 `Color` 类型。

use ratatui::style::Color as RatColor;

use crate::output::ColorTheme;

/// 从 ANSI ColorTheme 创建 ratatui 兼容的颜色配置
pub struct TuiColors {
    pub user: RatColor,
    pub assistant: RatColor,
    pub tool: RatColor,
    pub error: RatColor,
    pub success: RatColor,
    pub warning: RatColor,
    pub info: RatColor,
    pub heading: RatColor,
    pub muted: RatColor,
    pub border: RatColor,
    pub highlight: RatColor,
    pub bg: RatColor,
    pub surface: RatColor,
}

impl TuiColors {
    pub fn from_theme(theme: &ColorTheme) -> Self {
        Self {
            user: ansi_to_rat(theme.user_color),
            assistant: ansi_to_rat(theme.assistant_color),
            tool: ansi_to_rat(theme.tool_color),
            error: ansi_to_rat(theme.error_color),
            success: ansi_to_rat(theme.success_color),
            warning: ansi_to_rat(theme.warning_color),
            info: ansi_to_rat(theme.info_color),
            heading: ansi_to_rat(theme.heading_color),
            muted: ansi_to_rat(theme.muted_color),
            border: ansi_to_rat(theme.border_color),
            highlight: ansi_to_rat(theme.highlight_color),
            bg: RatColor::Black,
            surface: RatColor::DarkGray,
        }
    }
}

fn ansi_to_rat(color: nu_ansi_term::Color) -> RatColor {
    match color {
        nu_ansi_term::Color::Black => RatColor::Black,
        nu_ansi_term::Color::Red => RatColor::Red,
        nu_ansi_term::Color::Green => RatColor::Green,
        nu_ansi_term::Color::Yellow => RatColor::Yellow,
        nu_ansi_term::Color::Blue => RatColor::Blue,
        nu_ansi_term::Color::Magenta => RatColor::Magenta,
        nu_ansi_term::Color::Cyan => RatColor::Cyan,
        nu_ansi_term::Color::White => RatColor::White,
        nu_ansi_term::Color::DarkGray => RatColor::DarkGray,
        nu_ansi_term::Color::LightRed => RatColor::LightRed,
        nu_ansi_term::Color::LightGreen => RatColor::LightGreen,
        nu_ansi_term::Color::LightYellow => RatColor::LightYellow,
        nu_ansi_term::Color::LightBlue => RatColor::LightBlue,
        nu_ansi_term::Color::LightMagenta => RatColor::LightMagenta,
        nu_ansi_term::Color::LightCyan => RatColor::LightCyan,
        nu_ansi_term::Color::LightGray => RatColor::Gray,
        nu_ansi_term::Color::Rgb(r, g, b) => RatColor::Rgb(r, g, b),
        nu_ansi_term::Color::Fixed(idx) => RatColor::Indexed(idx),
        _ => RatColor::White,
    }
}
