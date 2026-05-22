//! ChatGPT 风格消息渲染 — 助手无边框，用户气泡，头像圆圈，时间戳，折叠思考块

use egui::{Align, Align2, Color32, CornerRadius, FontId, Frame, Margin, RichText, ScrollArea, Stroke, Ui};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use super::message::{ChatMessage, Role, ThinkingBlock, ToolCallRecord, format_timestamp};
use super::syntax::Highlighter;
use super::theme::Theme;

/// 渲染动作 — 由渲染器返回给 app.rs 处理
#[derive(Clone)]
pub enum RenderAction {
    ToggleThinking { msg_id: String, block_idx: usize },
    ToggleTool { msg_id: String, tool_name: String },
    Copy { msg_id: String },
    Regenerate,
    Edit { msg_id: String },
}

/// 渲染一条消息，返回可能的用户操作
pub fn render_message(
    ui: &mut Ui,
    msg: &ChatMessage,
    t: &Theme,
    hl: &Highlighter,
    dark_mode: bool,
    collapse_states: &HashMap<String, Vec<bool>>,
    tool_expand_states: &HashMap<String, bool>,
    hovered_msgs: &mut HashMap<String, bool>,
) -> Vec<RenderAction> {
    let mut actions = Vec::new();
    match msg.role {
        Role::User => {
            let h = user_bubble(ui, msg, t);
            hovered_msgs.insert(msg.id.clone(), h);
            if h && !msg.content.is_empty() {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(Align::TOP), |ui| {
                        if ui.small_button(RichText::new("✏ 编辑").size(11.0).color(t.text2)).clicked() {
                            actions.push(RenderAction::Edit { msg_id: msg.id.clone() });
                        }
                    });
                });
            }
        }
        Role::Assistant => {
            let h = asst_bubble(ui, msg, t, hl, dark_mode, collapse_states, tool_expand_states, &mut actions);
            hovered_msgs.insert(msg.id.clone(), h);
            if h && msg.finished && !msg.content.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(46.0);
                    if ui.small_button(RichText::new("📋 复制").size(11.0).color(t.text2)).clicked() {
                        actions.push(RenderAction::Copy { msg_id: msg.id.clone() });
                    }
                    if ui.small_button(RichText::new("🔄 重试").size(11.0).color(t.text2)).clicked() {
                        actions.push(RenderAction::Regenerate);
                    }
                });
            }
        }
    }
    actions
}

/// 用户气泡 — 右对齐，accent色，不对称圆角
fn user_bubble(ui: &mut Ui, msg: &ChatMessage, t: &Theme) -> bool {
    let mut hovered = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        let w = ui.available_width() * 0.70;
        let response = Frame::default()
            .fill(t.accent)
            .corner_radius(CornerRadius { nw: 12, ne: 4, sw: 12, se: 4 })
            .inner_margin(Margin::symmetric(16, 10))
            .shadow(egui::Shadow { offset: [0, 2], blur: 6, spread: 4, color: t.shadow_color.linear_multiply(0.3) })
            .show(ui, |ui| {
                ui.set_max_width(w);
                ui.label(RichText::new(&msg.content).color(Color32::WHITE).size(14.0));
            });
        hovered = response.response.hovered();
        ui.add_space(2.0);
        ui.label(RichText::new(format_timestamp(&msg.timestamp)).size(10.0).color(t.text2));
        let rect = ui.available_rect_before_wrap();
        let center = egui::pos2(rect.right() - 20.0, rect.top() + 14.0);
        ui.painter().circle_filled(center, 14.0, t.user_avatar);
        ui.painter().text(center, Align2::CENTER_CENTER, "U", FontId::proportional(12.0), Color32::WHITE);
    });
    ui.add_space(12.0);
    hovered
}

/// 助手消息 — ChatGPT 风格，无边框，直接在背景上显示
fn asst_bubble(
    ui: &mut Ui,
    msg: &ChatMessage,
    t: &Theme,
    hl: &Highlighter,
    dark_mode: bool,
    collapse_states: &HashMap<String, Vec<bool>>,
    tool_expand_states: &HashMap<String, bool>,
    actions: &mut Vec<RenderAction>,
) -> bool {
    // 头像圆圈（左侧）
    let avatar_rect = ui.available_rect_before_wrap();
    let center = egui::pos2(avatar_rect.left() + 20.0, avatar_rect.top() + 14.0);
    ui.painter().circle_filled(center, 14.0, t.asst_avatar);
    ui.painter().text(center, Align2::CENTER_CENTER, "A", FontId::proportional(12.0), Color32::WHITE);

    let mut hovered = false;
    ui.horizontal(|ui| {
        ui.add_space(46.0);
        let w = ui.available_width() * 0.90;
        ui.set_max_width(w);

        // 思考块（可折叠）
        for (i, think) in msg.thinking.iter().enumerate() {
            let is_collapsed = collapse_states
                .get(&msg.id)
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(true);
            think_block(ui, think, i + 1, msg.thinking.len(), is_collapsed, t, actions, &msg.id);
        }

        // 工具调用卡片
        for tc in &msg.tool_calls {
            let key = format!("{}:{}", msg.id, tc.name);
            let is_expanded = tool_expand_states.get(&key).copied().unwrap_or(false);
            tool_card(ui, tc, is_expanded, t, actions, &msg.id);
        }

        // 正文内容
        if !msg.content.is_empty() {
            markdown(ui, &msg.content, hl, dark_mode, t);
        }

        // 流式脉动点
        if !msg.finished {
            let time = ui.input(|i| i.time) as f32;
            let phase = (time * 2.0 * std::f32::consts::PI).sin().abs();
            let alpha = 0.3 + 0.7 * phase;
            ui.label(RichText::new("●").color(t.accent.linear_multiply(alpha)).size(11.0));
        }

        // 错误显示
        if let Some(e) = &msg.error {
            ui.label(RichText::new(format!("❌ {}", e)).color(t.red).size(12.5));
        }

        // 时间戳
        if msg.finished {
            ui.add_space(4.0);
            ui.label(RichText::new(format_timestamp(&msg.timestamp)).size(10.0).color(t.text2));
        }
    });
    // 检测 hover：使用消息区域的整体rect
    let _msg_rect = ui.min_rect();
    hovered = ui.ui_contains_pointer();
    ui.add_space(16.0);
    hovered
}

/// 思考块 — 可折叠，默认折叠只显示摘要
fn think_block(
    ui: &mut Ui,
    think: &ThinkingBlock,
    i: usize,
    n: usize,
    collapsed: bool,
    t: &Theme,
    actions: &mut Vec<RenderAction>,
    msg_id: &str,
) {
    let token_total = think.prompt_tokens + think.completion_tokens;
    let header_text = if collapsed {
        format!("🧠 思考过程 ({}/{}) — {} tokens  ▶", i, n, token_total)
    } else {
        format!("🧠 思考过程 ({}/{}) — {} tokens  ▼", i, n, token_total)
    };

    Frame::default()
        .fill(t.surface2)
        .corner_radius(CornerRadius::same(6))
        .stroke(Stroke::new(1.0, t.border))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            let header_resp = ui.label(RichText::new(&header_text).size(12.0).color(t.text2));
            if header_resp.clicked() {
                actions.push(RenderAction::ToggleThinking {
                    msg_id: msg_id.to_string(),
                    block_idx: i - 1,
                });
            }
            if !collapsed {
                ui.add_space(2.0);
                ui.label(RichText::new(&think.tokens).size(12.0).color(t.text2).monospace());
            }
        });
    ui.add_space(6.0);
}

/// 工具卡片 — 左侧色条 + 状态图标 + 可展开折叠长结果
fn tool_card(
    ui: &mut Ui,
    tc: &ToolCallRecord,
    expanded: bool,
    t: &Theme,
    actions: &mut Vec<RenderAction>,
    msg_id: &str,
) {
    let (icon, border_color, fill) = if tc.finished {
        if tc.success { ("✅", t.green, t.success_bg) }
        else { ("❌", t.red, t.error_bg) }
    } else {
        ("⏳", t.amber, t.surface2)
    };

    Frame::default()
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .stroke(Stroke::new(1.0, t.border))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            // 左侧色条
            let rect = ui.min_rect();
            ui.painter().line_segment(
                [egui::pos2(rect.left() - 10.0, rect.top()), egui::pos2(rect.left() - 10.0, rect.bottom())],
                Stroke::new(3.0, border_color),
            );

            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} {}", icon, tc.name)).size(12.0).color(t.text));
                if tc.finished {
                    let toggle_text = if expanded { "收起 ▲" } else { "展开 ▼" };
                    if ui.small_button(RichText::new(toggle_text).size(10.0).color(t.text2)).clicked() {
                        actions.push(RenderAction::ToggleTool {
                            msg_id: msg_id.to_string(),
                            tool_name: tc.name.clone(),
                        });
                    }
                }
            });

            if let Some(r) = &tc.result {
                if expanded {
                    ui.add_space(2.0);
                    ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        ui.label(RichText::new(r).size(11.5).color(t.text2).monospace());
                    });
                } else {
                    let preview = if r.len() > 200 { format!("{}…", &r[..200]) } else { r.clone() };
                    ui.add_space(2.0);
                    ui.label(RichText::new(preview).size(11.5).color(t.text2).monospace());
                }
            }
        });
    ui.add_space(6.0);
}

/// Markdown 渲染
fn markdown(ui: &mut Ui, src: &str, hl: &Highlighter, dark_mode: bool, t: &Theme) {
    let mut parser = Parser::new_ext(src, Options::all());
    let mut in_code = false;
    let mut code_text = String::new();
    let mut code_lang = String::new();

    while let Some(ev) = parser.next() {
        match ev {
            Event::Text(tx) => {
                if in_code { code_text.push_str(&tx); }
                else { inline(ui, &tx, t); }
            }
            Event::Code(tx) => {
                ui.code(RichText::new(tx.to_string()).color(t.accent).size(13.0));
            }
            Event::Start(tag) => match tag {
                Tag::CodeBlock(CodeBlockKind::Fenced(l)) => {
                    in_code = true;
                    code_text.clear();
                    code_lang = l.to_string();
                }
                Tag::Heading { level: _, .. } => {
                    ui.add_space(6.0);
                }
                Tag::List(..) => { ui.add_space(2.0); }
                Tag::Item => { ui.label(RichText::new("  • ").color(t.text2)); }
                Tag::BlockQuote(_) => { ui.colored_label(t.text2, "▎ "); ui.add_space(2.0); }
                Tag::Table(_) => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::CodeBlock => {
                    in_code = false;
                    let tokens = hl.highlight(&code_text, super::syntax::lang_to_syntax(&code_lang), dark_mode);
                    ui.add_space(4.0);
                    let lang_label = if code_lang.is_empty() { "code" } else { &code_lang };
                    Frame::default()
                        .fill(t.code)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(lang_label).size(10.0).color(t.text2).monospace());
                            });
                            ui.add_space(2.0);
                            ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                for line in &tokens {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        for (c, b, it, tx) in line {
                                            let mut rt = RichText::new(tx).color(*c).size(12.0).monospace();
                                            if *b { rt = rt.strong(); }
                                            if *it { rt = rt.italics(); }
                                            ui.label(rt);
                                        }
                                    });
                                }
                            });
                        });
                    ui.add_space(4.0);
                }
                TagEnd::Paragraph => { ui.add_space(6.0); }
                TagEnd::Heading(_) => { ui.add_space(4.0); }
                TagEnd::List(_) => { ui.add_space(4.0); }
                TagEnd::Item => {}
                TagEnd::BlockQuote => { ui.add_space(2.0); }
                _ => {}
            },
            Event::HardBreak | Event::SoftBreak => { ui.add_space(2.0); }
            _ => {}
        }
    }

    if in_code && !code_text.is_empty() {
        let tokens = hl.highlight(&code_text, super::syntax::lang_to_syntax(&code_lang), dark_mode);
        Frame::default()
            .fill(t.code)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for line in &tokens {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (c, b, it, tx) in line {
                                let mut rt = RichText::new(tx).color(*c).size(12.0).monospace();
                if *b { rt = rt.strong(); }
                if *it { rt = rt.italics(); }
                ui.label(rt);
            }
                        });
                    }
                });
            });
    }
}

/// Inline 文本渲染
fn inline(ui: &mut Ui, text: &str, t: &Theme) {
    let mut i = 0;
    let bytes = text.as_bytes();
    let mut start = 0usize;

    while i < bytes.len() {
        let ch = text[i..].chars().next().unwrap();
        let cl = ch.len_utf8();
        if ch == '*' || ch == '`' {
            if i > start {
                ui.label(RichText::new(&text[start..i]).color(t.text).size(14.0));
            }
            if ch == '*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                if let Some(e) = text[i + 2..].find("**") {
                    ui.label(RichText::new(&text[i + 2..i + 2 + e]).color(t.text).size(14.0).strong());
                    i += 2 + e + 2; start = i; continue;
                }
            } else if ch == '*' {
                if let Some(e) = text[i + 1..].find('*') {
                    if !text[i + 1..i + 1 + e].contains(' ') {
                        ui.label(RichText::new(&text[i + 1..i + 1 + e]).color(t.text).size(14.0).italics());
                        i += 1 + e + 1; start = i; continue;
                    }
                }
            } else if ch == '`' {
                if let Some(e) = text[i + 1..].find('`') {
                    ui.code(RichText::new(&text[i + 1..i + 1 + e]).color(t.accent).size(13.0));
                    i += 1 + e + 1; start = i; continue;
                }
            }
            i += cl; start = i;
        } else {
            i += cl;
        }
    }
    if i > start {
        ui.label(RichText::new(&text[start..i]).color(t.text).size(14.0));
    }
}