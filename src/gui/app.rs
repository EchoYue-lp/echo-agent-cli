//! 主界面 — ChatGPT 风格重构

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use echo_agent::agent::Agent;
use echo_agent::prelude::AgentEvent;
use egui::{Align, Color32, CornerRadius, Frame, Key, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit, Vec2};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tokio::sync::mpsc;
use crate::agent_handle::AgentHandle;
use super::human_loop::{GuiHumanLoopHandler, GuiRequestKind};
use super::message::ChatMessage;
use super::render::{self, RenderAction};
use super::settings::{self, Tab as Stab};
use super::syntax::Highlighter;
use super::theme::Theme;

/// 后台异步操作的结果 — 通过 mpsc channel 回传给 UI
pub enum BgResult {
    ContextStats { msg_count: usize, token_est: usize },
    CompressResult { before_count: usize, after_count: usize, before_tokens: usize, after_tokens: usize, evicted: usize },
    CompressError { error: String },
    ExtractResult { value: serde_json::Value },
    ExtractError { error: String },
    McpConnectFeedback { message: String },
    SkillsLoaded { names: Vec<String> },
    SystemPromptSet { prompt: String },
}

#[derive(Clone)]
struct Conv {
    id: String,
    title: String,
}

pub struct EchoGuiApp {
    agent: AgentHandle,
    messages: Vec<ChatMessage>,
    streaming_idx: Option<usize>,
    input: String,
    convs: Vec<Conv>,
    active_conv: String,
    sidebar_open: bool,
    streaming: bool,
    event_rx: Option<mpsc::UnboundedReceiver<Result<AgentEvent, echo_agent::error::ReactError>>>,
    cancel_token: Option<CancellationToken>,
    human_loop: Arc<GuiHumanLoopHandler>,
    hl_pending: Vec<super::human_loop::GuiHumanLoopRequest>,
    hl_input_buffers: HashMap<String, String>,
    deny_reason: String,
    pending_files: Arc<Mutex<Vec<String>>>,
    hl: Highlighter,
    settings_open: bool,
    settings_tab: Stab,
    settings_state: settings::SettingsState,
    persistence: crate::persistence::Persistence,

    // 新增字段
    dark_mode: bool,
    search_input: String,
    collapse_states: HashMap<String, Vec<bool>>,   // msg_id → 每个思考块的折叠状态
    tool_expand_states: HashMap<String, bool>,       // msg_id:tool_name → 是否展开
    hovered_msgs: HashMap<String, bool>,             // msg_id → 是否hover
    bg_tx: mpsc::UnboundedSender<BgResult>,
    bg_result_rx: mpsc::UnboundedReceiver<BgResult>,
}

impl EchoGuiApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, agent: AgentHandle, persistence: crate::persistence::Persistence) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let (bg_tx, bg_result_rx) = mpsc::unbounded_channel();
        let mut app = Self {
            agent, messages: Vec::new(), streaming_idx: None, input: String::new(),
            convs: vec![Conv { id: id.clone(), title: "新对话".into() }], active_conv: id, sidebar_open: true,
            streaming: false, event_rx: None, cancel_token: None,
            human_loop: Arc::new(GuiHumanLoopHandler::new()), hl_pending: Vec::new(), hl_input_buffers: HashMap::new(), deny_reason: String::new(),
            pending_files: Arc::new(Mutex::new(Vec::new())),
            hl: Highlighter::new(),
            settings_open: false, settings_tab: Stab::Config, settings_state: settings::SettingsState::new(),
            persistence,

            dark_mode: true,
            search_input: String::new(),
            collapse_states: HashMap::new(),
            tool_expand_states: HashMap::new(),
            hovered_msgs: HashMap::new(),
            bg_tx, bg_result_rx,
        };
        app.load_conversations();
        app
    }

    fn send(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.streaming { return; }
        self.input.clear();
        if let Some(c) = self.convs.iter_mut().find(|c| c.id == self.active_conv) {
            c.title = text.chars().take(40).collect();
        }
        self.messages.push(ChatMessage::new_user(text.clone()));
        let ai = self.messages.len();
        self.messages.push(ChatMessage::new_assistant());
        self.streaming_idx = Some(ai);
        self.streaming = true;
        let ct = CancellationToken::new();
        self.cancel_token = Some(ct.clone());
        let (tx, rx) = mpsc::unbounded_channel();
        self.event_rx = Some(rx);
        let agent_arc = self.agent.inner().clone();
        let hl = self.human_loop.clone();
        let files: Vec<String> = self.pending_files.lock().unwrap().drain(..).collect();
        tokio::spawn(async move {
            { let mut w = agent_arc.write().await; w.set_human_loop_provider(hl.clone()); }
            let guard = agent_arc.read().await;
            let stream = if files.is_empty() {
                guard.chat_stream_with_cancel(&text, ct).await
            } else {
                let mut parts = vec![echo_agent::llm::types::ContentPart::Text { text: text.clone() }];
                for p in &files {
                    if let Ok(bytes) = std::fs::read(p) {
                        let mime = guess_mime(p);
                        if mime.starts_with("image/") {
                            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
                            parts.push(echo_agent::llm::types::ContentPart::ImageUrl {
                                image_url: echo_agent::llm::types::ImageUrl {
                                    url: format!("data:{};base64,{}", mime, b64),
                                    detail: None,
                                },
                            });
                        } else {
                            let preview: String = String::from_utf8_lossy(&bytes).lines().take(50).collect::<Vec<_>>().join("\n");
                            parts.push(echo_agent::llm::types::ContentPart::Text {
                                text: format!("\n[{} ({} B, {})]\n{}\n", p, bytes.len(), mime, preview),
                            });
                        }
                    }
                }
                let msg = echo_agent::llm::types::Message::user_multimodal(parts);
                guard.chat_stream_message(msg).await
            };
            if let Ok(mut s) = stream { while let Some(e) = s.next().await { if tx.send(e).is_err() { break; } } }
            else if let Err(e) = stream { let _ = tx.send(Err(e)); }
        });
    }

    fn handle(&mut self, e: Result<AgentEvent, echo_agent::error::ReactError>) {
        let Some(idx) = self.streaming_idx.filter(|&i| i < self.messages.len()) else { return };
        let m = &mut self.messages[idx];
        match e {
            Ok(AgentEvent::Token(d)) => m.append_token(&d),
            Ok(AgentEvent::ThinkStart) => m.start_thinking(),
            Ok(AgentEvent::ThinkEnd { prompt_tokens, completion_tokens }) => m.end_thinking(prompt_tokens, completion_tokens),
            Ok(AgentEvent::ToolCall { name, args }) => m.add_tool_call(name, args),
            Ok(AgentEvent::ToolResult { name, output }) => m.complete_tool_call(&name, output, true),
            Ok(AgentEvent::ToolError { name, error }) => m.complete_tool_call(&name, error, false),
            Ok(AgentEvent::FinalAnswer(d)) => {
                if !d.is_empty() { m.append_token(&d); }
                // 更新对话标题为助手回复前40字
                if let Some(c) = self.convs.iter_mut().find(|c| c.id == self.active_conv) {
                    if c.title == "新对话" || c.title.chars().take(5).collect::<String>() == self.input.chars().take(5).collect::<String>() {
                        let title_text = m.content.chars().take(40).collect::<String>();
                        if !title_text.is_empty() { c.title = title_text; }
                    }
                }
                m.finished = true;
                self.streaming = false;
                self.streaming_idx = None;
                self.event_rx = None;
                self.cancel_token = None;
                self.save_current_conversation();
            }
            Ok(AgentEvent::Cancelled) => {
                m.finished = true;
                self.streaming = false;
                self.streaming_idx = None;
                self.event_rx = None;
                self.cancel_token = None;
            }
            Ok(_) => {}
            Err(e) => {
                m.error = Some(e.to_string());
                m.finished = true;
                self.streaming = false;
                self.streaming_idx = None;
                self.event_rx = None;
                self.cancel_token = None;
            }
        }
    }

    fn new_conv(&mut self) {
        self.messages.clear();
        self.streaming_idx = None;
        self.streaming = false;
        self.event_rx = None;
        self.cancel_token = None;
        let id = uuid::Uuid::new_v4().to_string();
        self.convs.push(Conv { id: id.clone(), title: "新对话".into() });
        self.active_conv = id;
        let a = self.agent.inner().clone();
        tokio::spawn(async move { a.read().await.reset().await; });
    }

    fn delete_conv(&mut self, id: String) {
        // 删除持久化文件
        let path = self.persistence.conversations_dir().join(format!("{}.json", id));
        std::fs::remove_file(path).ok();
        self.convs.retain(|c| c.id != id);
        if self.active_conv == id {
            if let Some(first) = self.convs.first() {
                self.active_conv = first.id.clone();
            } else {
                self.new_conv();
            }
            self.messages.clear();
            self.streaming_idx = None;
            self.streaming = false;
            self.event_rx = None;
        }
    }

    fn cancel(&mut self) {
        if let Some(ct) = self.cancel_token.take() {
            ct.cancel();
        }
        self.event_rx = None;
        if let Some(i) = self.streaming_idx.filter(|&i| i < self.messages.len()) {
            let m = &mut self.messages[i];
            m.finished = true;
            if m.content.is_empty() { m.content = "[已取消]".into(); }
        }
        self.streaming = false;
        self.streaming_idx = None;
    }

    fn regenerate(&mut self) {
        if self.streaming { return; }
        self.cancel_token = None;
        if let Some((i, _)) = self.messages.iter().enumerate().rev().find(|(_, m)| m.role == super::message::Role::User) {
            let txt = self.messages[i].content.clone();
            self.messages.truncate(i + 1);
            self.streaming = false;
            self.event_rx = None;
            self.input = txt;
            self.send();
        }
    }

    fn edit(&mut self, msg_id: String) {
        if self.streaming { return; }
        self.cancel_token = None;
        if let Some(idx) = self.messages.iter().position(|m| m.id == msg_id && m.role == super::message::Role::User) {
            let txt = self.messages[idx].content.clone();
            self.messages.truncate(idx);
            self.streaming = false;
            self.event_rx = None;
            self.input = txt;
        }
    }

    fn copy(&self, ctx: &egui::Context, text: &str) { ctx.copy_text(text.to_owned()); }
    fn open_files(&self) {
        let pf = self.pending_files.clone();
        std::thread::spawn(move || {
            if let Some(files) = rfd::FileDialog::new().pick_files() {
                *pf.lock().unwrap() = files.iter().filter_map(|p| p.to_str().map(String::from)).collect();
            }
        });
    }

    fn save_current_conversation(&self) {
        let conv_id = self.active_conv.clone();
        let title = self.convs.iter()
            .find(|c| c.id == conv_id)
            .map(|c| c.title.clone())
            .unwrap_or_default();
        let model = self.agent.inner().try_read().ok()
            .map(|g| g.model_name().to_string())
            .unwrap_or_default();
        let data = super::message::GuiConversationData {
            version: 1,
            id: conv_id.clone(),
            title,
            messages: self.messages.clone(),
            model,
        };
        let path = self.persistence.conversations_dir().join(format!("{}.json", conv_id));
        if let Ok(json) = serde_json::to_string_pretty(&data) {
            std::fs::write(path, json).ok();
        }
    }

    fn load_conversations(&mut self) {
        let dir = self.persistence.conversations_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        if let Ok(conv) = serde_json::from_str::<super::message::GuiConversationData>(&data) {
                            if !self.convs.iter().any(|c| c.id == conv.id) {
                                self.convs.push(Conv { id: conv.id, title: conv.title });
                            }
                        }
                    }
                }
            }
        }
    }

    fn switch_conversation(&mut self, conv_id: String) {
        if self.streaming { return; }
        self.save_current_conversation();
        self.active_conv = conv_id.clone();
        self.collapse_states.clear();
        self.tool_expand_states.clear();
        let path = self.persistence.conversations_dir().join(format!("{}.json", conv_id));
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(conv) = serde_json::from_str::<super::message::GuiConversationData>(&data) {
                self.messages = conv.messages.clone();
                // 将对话历史加载到 agent context
                let agent_messages: Vec<echo_agent::llm::types::Message> = conv.messages.iter()
                    .filter_map(|m| match m.role {
                        super::message::Role::User => Some(echo_agent::llm::types::Message::user(m.content.clone())),
                        super::message::Role::Assistant if m.finished => Some(echo_agent::llm::types::Message::assistant(m.content.clone())),
                        _ => None,
                    })
                    .collect();
                if !agent_messages.is_empty() {
                    let arc = self.agent.inner().clone();
                    tokio::spawn(async move {
                        let g = arc.write().await;
                        // reset_messages is pub(crate), use reset() which clears context
                        g.reset().await;
                        g.load_messages(agent_messages).await;
                    });
                }
            }
        } else {
            self.messages.clear();
            let arc = self.agent.inner().clone();
            tokio::spawn(async move { let g = arc.read().await; g.reset().await; });
        }
        self.streaming_idx = None;
        self.streaming = false;
        self.event_rx = None;
        self.cancel_token = None;
    }

    fn handle_render_actions(&mut self, actions: Vec<RenderAction>, ctx: &egui::Context) {
        for action in actions {
            match action {
                RenderAction::ToggleThinking { msg_id, block_idx } => {
                    let states = self.collapse_states.entry(msg_id.clone()).or_insert_with(|| Vec::new());
                    if block_idx < states.len() {
                        states[block_idx] = !states[block_idx];
                    } else {
                        // 初始化所有为 true (collapsed)，然后 toggle 指定索引
                        states.clear();
                        let msg = self.messages.iter().find(|m| m.id == msg_id);
                        let n = msg.map(|m| m.thinking.len()).unwrap_or(0);
                        for i in 0..n { states.push(i != block_idx); }
                    }
                }
                RenderAction::ToggleTool { msg_id, tool_name } => {
                    let key = format!("{}:{}", msg_id, tool_name);
                    let current = self.tool_expand_states.get(&key).copied().unwrap_or(false);
                    self.tool_expand_states.insert(key, !current);
                }
                RenderAction::Copy { msg_id } => {
                    if let Some(m) = self.messages.iter().find(|m| m.id == msg_id) {
                        self.copy(ctx, &m.content);
                    }
                }
                RenderAction::Regenerate => { self.regenerate(); }
                RenderAction::Edit { msg_id } => { self.edit(msg_id); }
            }
        }
    }
}

impl eframe::App for EchoGuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = Theme::get(self.dark_mode);

        self.hl_pending = self.human_loop.drain_requests();
        let mut events = Vec::new();
        if let Some(ref mut rx) = self.event_rx {
            while let Ok(e) = rx.try_recv() { events.push(e); }
        }
        for e in events { self.handle(e); }

        // 轮询后台异步操作结果
        {
            while let Ok(result) = self.bg_result_rx.try_recv() {
                match result {
                    BgResult::ContextStats { msg_count, token_est } => {
                        self.settings_state.context_stats_display = Some(
                            format!("{} 条消息, ~{} tokens", msg_count, token_est)
                        );
                    }
                    BgResult::CompressResult { before_count, after_count, before_tokens, after_tokens, evicted } => {
                        self.settings_state.compress_result = Some(
                            format!("✅ 压缩完成: {}→{} 条消息, {}→{} tokens, 淘汰 {}", before_count, after_count, before_tokens, after_tokens, evicted)
                        );
                    }
                    BgResult::CompressError { error } => {
                        self.settings_state.compress_result = Some(format!("❌ 压缩失败: {}", error));
                    }
                    BgResult::ExtractResult { value } => {
                        self.settings_state.extract_result = Some(
                            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                        );
                    }
                    BgResult::ExtractError { error } => {
                        self.settings_state.extract_result = Some(format!("❌ 提取失败: {}", error));
                    }
                    BgResult::McpConnectFeedback { message } => {
                        self.settings_state.mcp_connect_feedback = Some(message);
                        self.settings_state.mcp_connecting = false;
                    }
                    BgResult::SkillsLoaded { names } => {
                        self.settings_state.skills_feedback = Some(
                            if names.is_empty() { "无新技能加载".into() }
                            else { format!("✅ 加载 {} 个技能: {}", names.len(), names.join(", ")) }
                        );
                    }
                    BgResult::SystemPromptSet { prompt } => {
                        self.settings_state.system_prompt_current = Some(prompt);
                        self.settings_state.system_prompt_feedback = Some("✅ 系统提示词已更新".into());
                    }
                }
            }
        }

        if self.streaming { ctx.request_repaint_after(std::time::Duration::from_millis(super::theme::anim::STREAMING_PULSE_MS)); }

        // ── 顶栏 ───────────────────────────────────
        egui::Panel::top("tb")
            .frame(Frame::default().fill(t.surface).inner_margin(Margin::symmetric(16, 8)))
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左侧：菜单 + 标题
                    if ui.button(RichText::new("☰").size(16.0).color(t.text)).clicked() {
                        self.sidebar_open = !self.sidebar_open;
                    }
                    ui.label(RichText::new("Echo Agent").size(15.0).color(t.text).strong());

                    // 中间：模型名称
                    if let Some(model) = self.agent.inner().try_read().ok().map(|g| g.model_name().to_string()) {
                        ui.label(RichText::new(model).size(12.0).color(t.text2));
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // 设置按钮
                        if ui.button(RichText::new("⚙").size(14.0).color(t.text)).clicked() {
                            self.settings_open = !self.settings_open;
                        }
                        // 连接状态指示灯
                        let status_color = if self.streaming { t.amber } else { t.green };
                        let cur_rect = ui.available_rect_before_wrap();
                        ui.painter().circle_filled(
                            egui::pos2(cur_rect.right() - 8.0, cur_rect.center().y),
                            5.0,
                            status_color,
                        );
                        ui.add_space(16.0);
                        // 主题切换
                        let theme_icon = if self.dark_mode { "☀" } else { "🌙" };
                        if ui.button(RichText::new(theme_icon).size(14.0).color(t.text)).clicked() {
                            self.dark_mode = !self.dark_mode;
                            super::theme::setup(&ctx, self.dark_mode);
                        }
                    });
                });
            });

        // ── 侧边栏 ─────────────────────────────────
        if self.sidebar_open {
            egui::Panel::left("sb")
                .resizable(true)
                .default_size(260.0)
                .min_size(160.0)
                .frame(Frame::default().fill(t.surface).inner_margin(Margin::same(12)))
                .show_inside(ui, |ui| {
                    // 新对话按钮（醒目）
                    let new_btn = egui::Button::new(RichText::new("＋ 新对话").size(13.0).color(Color32::WHITE))
                        .min_size(Vec2::new(ui.available_width(), 36.0))
                        .fill(t.accent)
                        .corner_radius(CornerRadius::same(8));
                    if ui.add(new_btn).clicked() { self.new_conv(); }
                    ui.add_space(8.0);

                    // 搜索框
                    ui.add(TextEdit::singleline(&mut self.search_input)
                        .hint_text("搜索对话…")
                        .desired_width(ui.available_width()));
                    ui.add_space(8.0);

                    // 对话列表（过滤后）
                    let filtered: Vec<Conv> = self.convs.clone().into_iter()
                        .filter(|c| self.search_input.is_empty() || c.title.contains(&self.search_input))
                        .collect();

                    ScrollArea::vertical().show(ui, |ui| {
                        for c in filtered {
                            let sel = c.id == self.active_conv;
                            let fill = if sel { mix_color(t.accent, 0.15, t.surface) } else { t.surface2 };
                            let row_resp = Frame::default()
                                .fill(fill)
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(Margin::symmetric(10, 6))
                                .stroke(Stroke::new(if sel { 1.0 } else { 0.0 }, t.accent))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&c.title).size(13.0).color(if sel { t.accent } else { t.text }));
                                        // hover 时显示删除按钮
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui.small_button(RichText::new("×").size(11.0).color(t.text2)).clicked() {
                                                self.delete_conv(c.id.clone());
                                            }
                                        });
                                    });
                                });
                            if row_resp.response.clicked() {
                                self.switch_conversation(c.id);
                            }
                        }
                    });

                    // 底部设置按钮
                    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                        ui.add_space(8.0);
                        if ui.button(RichText::new("⚙ 设置").size(13.0).color(t.text2)).clicked() {
                            self.settings_open = !self.settings_open;
                        }
                    });
                });
        }

        // ── 底部输入栏 ──────────────────────────────
        egui::Panel::bottom("input")
            .frame(Frame::default().fill(t.bg).inner_margin(Margin::symmetric(20, 14)))
            .show_inside(ui, |ui| {
                // 文件附件显示
                let file_names: Vec<String> = self.pending_files.lock().unwrap().clone();
                if !file_names.is_empty() {
                    ui.horizontal(|ui| {
                        for f in &file_names {
                            let short = std::path::Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or(f);
                            Frame::default()
                                .fill(t.surface2)
                                .corner_radius(CornerRadius::same(4))
                                .inner_margin(Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(short).size(11.0).color(t.accent));
                                    if ui.small_button("×").clicked() {
                                        self.pending_files.lock().unwrap().retain(|p| p != f);
                                    }
                                });
                        }
                    });
                    ui.add_space(4.0);
                }

                Frame::default()
                    .fill(t.surface)
                    .corner_radius(CornerRadius::same(12))
                    .stroke(Stroke::new(1.0, t.border))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("📎").size(14.0).color(t.text2)).clicked() {
                                self.open_files();
                            }
                            let hint = if self.streaming { "等待回复…" } else { "输入消息… (Enter 发送, Shift+Enter 换行)" };
                            let ed = TextEdit::multiline(&mut self.input)
                                .hint_text(hint)
                                .desired_rows(2)
                                .desired_width(ui.available_width() - 80.0);
                            let r = ui.add(ed);
                            if r.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift) {
                                r.request_focus();
                                self.send();
                            }
                            // 字符数估算
                            let char_count = self.input.len();
                            ui.label(RichText::new(format!("{} chars", char_count)).size(10.0).color(t.text2));

                            let enabled = !self.input.trim().is_empty() || self.streaming;
                            let btn = egui::Button::new(
                                RichText::new(if self.streaming { "⏹" } else { "➤" }).size(15.0).color(Color32::WHITE)
                            )
                            .min_size(Vec2::new(44.0, 40.0))
                            .fill(if enabled { t.accent } else { t.border })
                            .corner_radius(CornerRadius::same(10));
                            if ui.add_enabled(enabled, btn).clicked() {
                                if self.streaming { self.cancel(); } else { self.send(); }
                            }
                        });
                    });
            });

        // ── 中央聊天区 ──────────────────────────────
        egui::CentralPanel::default().frame(Frame::default().fill(t.bg)).show_inside(ui, |ui| {
            ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.add_space(16.0);

                if self.messages.is_empty() {
                    // ── 空状态 — 2列网格快速开始卡片 ──
                    ui.vertical_centered(|ui| {
                        ui.add_space(60.0);
                        ui.label(RichText::new("Echo Agent").size(36.0).color(t.text).strong());
                        ui.add_space(8.0);
                        ui.label(RichText::new("有什么我可以帮你的？").size(14.0).color(t.text2));
                        ui.add_space(32.0);

                        let suggestions = [
                            ("💻", "帮我写一段代码", "生成、调试或解释代码", "请帮我写一段 Python 代码，实现一个简单的 HTTP 服务器"),
                            ("📖", "解释一个概念", "理解技术概念和原理", "请解释一下什么是 React 框架，以及它和 Vue 的区别"),
                            ("📊", "分析一份数据", "处理和分析数据文件", "请帮我分析一份 CSV 数据文件，找出其中的趋势和模式"),
                            ("📝", "写一篇文档", "起草文档、报告或邮件", "请帮我写一篇关于项目架构设计的技术文档"),
                        ];
                        let cols = 2;
                        for chunk in suggestions.chunks(cols) {
                            ui.horizontal(|ui| {
                                for (icon, title, desc, prompt) in chunk {
                                    let card = Frame::default()
                                        .fill(t.surface)
                                        .corner_radius(CornerRadius::same(10))
                                        .stroke(Stroke::new(1.0, t.border))
                                        .inner_margin(Margin::same(16))
                                        .show(ui, |ui| {
                                            ui.set_min_size(Vec2::new(200.0, 70.0));
                                            ui.label(RichText::new(format!("{} {}", icon, title)).size(13.0).color(t.text));
                                            ui.add_space(4.0);
                                            ui.label(RichText::new(*desc).size(11.0).color(t.text2));
                                        });
                                    if card.response.clicked() {
                                        self.input = prompt.to_string();
                                        self.send();
                                    }
                                }
                            });
                            ui.add_space(10.0);
                        }
                    });
                }

                // ── 渲染消息 ──
                let mut all_actions = Vec::new();
                self.hovered_msgs.clear();
                for m in self.messages.clone().iter() {
                    let actions = render::render_message(
                        ui, m, t, &self.hl, self.dark_mode,
                        &self.collapse_states, &self.tool_expand_states,
                        &mut self.hovered_msgs,
                    );
                    all_actions.extend(actions);
                }
                self.handle_render_actions(all_actions, &ctx);

                // ── 人工介入 ──
                for req in &self.hl_pending.clone() {
                    ui.add_space(8.0);
                    match &req.kind {
                        GuiRequestKind::Approval { tool_name, args, prompt } => {
                            Frame::default()
                                .fill(t.surface)
                                .corner_radius(CornerRadius::same(8))
                                .stroke(Stroke::new(1.5, t.amber))
                                .inner_margin(Margin::same(14))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(format!("⚠ 需要批准: {}", tool_name)).color(t.text).size(14.0));
                                    if let Some(p) = prompt {
                                        ui.label(RichText::new(p).size(12.5).color(t.text2));
                                    }
                                    ui.label(RichText::new(serde_json::to_string_pretty(args).unwrap_or_default()).monospace().size(11.5).color(t.text2));
                                    ui.horizontal(|ui| {
                                        let approve_btn = egui::Button::new(RichText::new("✅ 批准").size(13.0).color(Color32::WHITE))
                                            .fill(t.green).min_size(Vec2::new(80.0, 32.0)).corner_radius(CornerRadius::same(6));
                                        if ui.add(approve_btn).clicked() {
                                            self.human_loop.send_approval(&req.request_id, true, None);
                                        }
                                        let deny_btn = egui::Button::new(RichText::new("❌ 拒绝").size(13.0).color(Color32::WHITE))
                                            .fill(t.red).min_size(Vec2::new(80.0, 32.0)).corner_radius(CornerRadius::same(6));
                                        if ui.add(deny_btn).clicked() {
                                            let r = if self.deny_reason.is_empty() { None } else { Some(self.deny_reason.clone()) };
                                            self.human_loop.send_approval(&req.request_id, false, r);
                                            self.deny_reason.clear();
                                        }
                                    });
                                    ui.add(TextEdit::singleline(&mut self.deny_reason).hint_text("拒绝理由（可选）"));
                                });
                        }
                        GuiRequestKind::Input { prompt } => {
                            Frame::default()
                                .fill(t.surface)
                                .corner_radius(CornerRadius::same(8))
                                .stroke(Stroke::new(1.5, t.accent))
                                .inner_margin(Margin::same(14))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("📝 提供更多信息").color(t.text).size(14.0));
                                    if let Some(p) = prompt {
                                        ui.label(RichText::new(p).size(12.5).color(t.text2));
                                    }
                                    let buf = self.hl_input_buffers.entry(req.request_id.clone()).or_insert_with(String::new);
                                    ui.add(TextEdit::multiline(buf).hint_text("输入…").desired_rows(2));
                                    if ui.button("提交").clicked() && !buf.is_empty() {
                                        self.human_loop.send_input(&req.request_id, buf.clone());
                                        self.hl_input_buffers.remove(&req.request_id);
                                    }
                                });
                        }
                    }
                }
                ui.add_space(20.0);
            });
        });

        // ── 设置窗口 ───────────────────────────────
        if self.settings_open {
            let mut open = true;
            egui::Window::new("⚙ 设置")
                .open(&mut open)
                .default_size(Vec2::new(640.0, 500.0))
                .resizable(true)
                .show(&ctx, |ui| {
                    // Tab 横向标签
                    ui.horizontal(|ui| {
                        for (n, tb) in Stab::all() {
                            let sel = self.settings_tab == tb;
                            let btn = egui::Button::new(RichText::new(n).size(12.0).color(if sel { Color32::WHITE } else { t.text2 }))
                                .fill(if sel { t.accent } else { Color32::TRANSPARENT })
                                .corner_radius(CornerRadius::same(6))
                                .min_size(Vec2::new(50.0, 28.0));
                            if ui.add(btn).clicked() { self.settings_tab = tb; }
                        }
                    });
                    ui.separator();
                    ScrollArea::vertical().show(ui, |ui| {
                        settings::render(ui, &self.settings_tab, &self.agent, t, &mut self.settings_state, &self.bg_tx);
                    });
                });
            self.settings_open = open;
        }
    }
}

fn guess_mime(path: &str) -> String {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif",
        "webp" => "image/webp", "svg" => "image/svg+xml", "bmp" => "image/bmp",
        "pdf" => "application/pdf", "txt" | "log" => "text/plain",
        "md" => "text/markdown", "json" => "application/json",
        "yaml" | "yml" => "text/yaml", "xml" => "text/xml",
        "html" => "text/html", "csv" => "text/csv",
        _ => "application/octet-stream",
    }.to_string()
}

fn mix_color(c: Color32, f: f32, base: Color32) -> Color32 {
    Color32::from_rgb(
        (base.r() as f32 + (c.r() as f32 - base.r() as f32) * f) as u8,
        (base.g() as f32 + (c.g() as f32 - base.g() as f32) * f) as u8,
        (base.b() as f32 + (c.b() as f32 - base.b() as f32) * f) as u8,
    )
}