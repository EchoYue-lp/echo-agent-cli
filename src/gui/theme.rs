//! 视觉主题 — 支持 Dark/Light 动态切换

use egui::{Color32, CornerRadius, Stroke, Style, Visuals};

pub struct Theme {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub accent: Color32,
    pub green: Color32,
    pub red: Color32,
    pub amber: Color32,
    pub code: Color32,
    pub shadow_color: Color32,     // 阴影色（用于手动绘制）
    pub success_bg: Color32,       // 成功工具卡片背景
    pub error_bg: Color32,         // 失败工具卡片背景
    pub user_avatar: Color32,      // 用户头像圆圈色
    pub asst_avatar: Color32,      // 助手头像圆圈色
}

const LIGHT: Theme = Theme {
    bg: Color32::from_rgb(245,246,248),
    surface: Color32::WHITE,
    surface2: Color32::from_rgb(250,251,253),
    border: Color32::from_rgb(230,232,236),
    text: Color32::from_rgb(22,24,28),
    text2: Color32::from_rgb(120,126,138),
    accent: Color32::from_rgb(88,101,242),
    green: Color32::from_rgb(35,165,90),
    red: Color32::from_rgb(220,55,60),
    amber: Color32::from_rgb(240,150,20),
    code: Color32::from_rgb(246,248,250),
    shadow_color: Color32::from_rgb(180, 180, 190),
    success_bg: Color32::from_rgb(230, 248, 235),
    error_bg: Color32::from_rgb(252, 235, 235),
    user_avatar: Color32::from_rgb(88,101,242),
    asst_avatar: Color32::from_rgb(120,126,138),
};

const DARK: Theme = Theme {
    bg: Color32::from_rgb(24,26,34),
    surface: Color32::from_rgb(38,40,52),
    surface2: Color32::from_rgb(44,46,60),
    border: Color32::from_rgb(60,64,80),
    text: Color32::from_rgb(232,234,240),
    text2: Color32::from_rgb(140,146,162),
    accent: Color32::from_rgb(108,128,255),
    green: Color32::from_rgb(40,185,100),
    red: Color32::from_rgb(235,75,80),
    amber: Color32::from_rgb(245,160,25),
    code: Color32::from_rgb(30,32,42),
    shadow_color: Color32::from_rgb(0, 0, 0),
    success_bg: Color32::from_rgb(20,40,30),
    error_bg: Color32::from_rgb(40,20,20),
    user_avatar: Color32::from_rgb(108,128,255),
    asst_avatar: Color32::from_rgb(140,146,162),
};

impl Theme {
    pub fn get(dark_mode: bool) -> &'static Self {
        if dark_mode { &DARK } else { &LIGHT }
    }
}

/// 动画常量
pub mod anim {
    pub const STREAMING_PULSE_MS: u64 = 500;
    pub const SIDEBAR_ANIM_SPEED: f32 = 0.3;
    pub const COLLAPSE_ANIM_SPEED: f32 = 0.15;
}

/// 根据 dark_mode 设置全局样式
pub fn setup(ctx: &egui::Context, dark_mode: bool) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(d) = find_cjk() {
        fonts.font_data.insert("CJK".into(), std::sync::Arc::new(d));
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "CJK".into());
        fonts.families.get_mut(&egui::FontFamily::Monospace).unwrap().push("CJK".into());
    }
    ctx.set_fonts(fonts);

    let t = Theme::get(dark_mode);
    ctx.set_global_style(Style {
        visuals: Visuals {
            dark_mode,
            window_corner_radius: CornerRadius::same(12),
            window_fill: t.surface,
            panel_fill: t.bg,
            window_stroke: Stroke::new(1.0, t.border),
            widgets: egui::style::Widgets {
                noninteractive: wv(t.surface, t.surface, Stroke::NONE, CornerRadius::same(8), Stroke::new(1.0, t.text), 0.0),
                inactive: wv(t.surface2, t.surface2, Stroke::NONE, CornerRadius::same(8), Stroke::new(1.0, t.text), 0.0),
                hovered: wv(mix(t.accent, 0.12, t.surface2), mix(t.accent, 0.12, t.surface2), Stroke::new(1.0, t.accent), CornerRadius::same(8), Stroke::new(1.0, t.text), 0.0),
                active: wv(mix(t.accent, 0.20, t.surface2), mix(t.accent, 0.20, t.surface2), Stroke::new(1.0, t.accent), CornerRadius::same(8), Stroke::new(2.0, t.accent), 0.0),
                open: wv(mix(t.accent, 0.20, t.surface2), mix(t.accent, 0.20, t.surface2), Stroke::new(1.0, t.accent), CornerRadius::same(8), Stroke::new(2.0, t.accent), 0.0),
            },
            selection: egui::style::Selection {
                bg_fill: t.accent.linear_multiply(0.25),
                stroke: Stroke::new(1.0, t.accent),
            },
            ..if dark_mode { Visuals::dark() } else { Visuals::light() }
        },
        spacing: egui::Spacing {
            item_spacing: egui::vec2(12.0, 8.0),
            button_padding: egui::vec2(16.0, 8.0),
            ..Default::default()
        },
        ..Default::default()
    });
}

fn wv(bg: Color32, wbg: Color32, bs: Stroke, cr: CornerRadius, fg: Stroke, exp: f32) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals { bg_fill: bg, weak_bg_fill: wbg, bg_stroke: bs, corner_radius: cr, fg_stroke: fg, expansion: exp }
}

fn mix(c: Color32, f: f32, base: Color32) -> Color32 {
    Color32::from_rgb(
        (base.r() as f32 + (c.r() as f32 - base.r() as f32) * f) as u8,
        (base.g() as f32 + (c.g() as f32 - base.g() as f32) * f) as u8,
        (base.b() as f32 + (c.b() as f32 - base.b() as f32) * f) as u8,
    )
}

fn find_cjk() -> Option<egui::FontData> {
    let paths: &[&str] = if cfg!(target_os = "macos") {
        &["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Medium.ttc"]
    } else if cfg!(target_os = "windows") {
        &["C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\simhei.ttf"]
    } else {
        &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"]
    };
    for p in paths {
        if let Ok(d) = std::fs::read(p) {
            return Some(egui::FontData::from_owned(d));
        }
    }
    None
}