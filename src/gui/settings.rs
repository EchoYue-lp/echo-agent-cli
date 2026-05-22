//! 设置面板 — 全部 Tab 可交互
//!
//! 对于不存在于 ReactAgent 上的方法，采用降级策略：
//! - 使用已有方法（context_stats, force_compress_with, extract_json 等）
//! - 对于 audit/workflow/sandbox/permissions 等未在 ReactAgent 上暴露的，
//!   显示静态描述 + 说明需通过 Web API 操作

use echo_agent::agent::Agent;
use egui::{CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, TextEdit};
use crate::agent_handle::AgentHandle;
use super::app::BgResult;
use super::theme::Theme;

#[derive(Clone, PartialEq)]
pub enum Tab {
    Config, Tools, Mcp, Skills, Memory, Sessions,
    Compress, Extract, Permissions, Audit, Workflow, Sandbox,
}

pub struct SettingsState {
    pub model_input: String,
    pub iters_input: String,
    pub mcp_json: String,
    // 压缩 Tab
    pub compress_keep: String,
    pub compress_result: Option<String>,
    pub context_stats_display: Option<String>,
    // 提取 Tab
    pub extract_input: String,
    pub extract_schema: String,
    pub extract_result: Option<String>,
    // 权限 Tab
    pub perm_mode: String,
    pub new_rule_matcher: String,
    pub new_rule_behavior: String,
    // 审计 Tab
    pub audit_refreshed: bool,
    // 工作流 Tab
    pub wf_create_id: String,
    pub wf_create_def: String,
    pub wf_execute_input: String,
    pub wf_result: Option<String>,
    // 沙箱 Tab
    pub sandbox_lang: String,
    pub sandbox_code: String,
    pub sandbox_result: Option<String>,
    // MCP Tab
    pub mcp_connect_feedback: Option<String>,
    pub mcp_connecting: bool,
    // 技能 Tab
    pub skills_feedback: Option<String>,
    // 配置 Tab — 系统提示词
    pub system_prompt_input: String,
    pub system_prompt_current: Option<String>,
    pub system_prompt_feedback: Option<String>,
}

impl SettingsState {
    pub fn new() -> Self {
        Self {
            model_input: String::new(),
            iters_input: String::new(),
            mcp_json: String::new(),
            compress_keep: "10".into(),
            compress_result: None,
            context_stats_display: None,
            extract_input: String::new(),
            extract_schema: String::new(),
            extract_result: None,
            perm_mode: "default".into(),
            new_rule_matcher: String::new(),
            new_rule_behavior: "ask".into(),
            audit_refreshed: false,
            wf_create_id: String::new(),
            wf_create_def: String::new(),
            wf_execute_input: String::new(),
            wf_result: None,
            sandbox_lang: "python".into(),
            sandbox_code: String::new(),
            sandbox_result: None,
            mcp_connect_feedback: None,
            mcp_connecting: false,
            skills_feedback: None,
            system_prompt_input: String::new(),
            system_prompt_current: None,
            system_prompt_feedback: None,
        }
    }
}

impl Tab {
    pub fn all() -> Vec<(&'static str, Tab)> {
        vec![
            ("配置", Tab::Config), ("工具", Tab::Tools), ("MCP", Tab::Mcp), ("技能", Tab::Skills), ("记忆", Tab::Memory),
            ("会话", Tab::Sessions), ("压缩", Tab::Compress), ("提取", Tab::Extract),
            ("权限", Tab::Permissions), ("审计", Tab::Audit), ("工作流", Tab::Workflow), ("沙箱", Tab::Sandbox),
        ]
    }
    pub fn groups() -> Vec<(&'static str, Vec<(&'static str, Tab)>)> {
        vec![
            ("智能体", vec![("配置", Tab::Config), ("工具", Tab::Tools), ("MCP", Tab::Mcp), ("技能", Tab::Skills), ("记忆", Tab::Memory)]),
            ("数据", vec![("会话", Tab::Sessions), ("压缩", Tab::Compress), ("提取", Tab::Extract)]),
            ("安全", vec![("权限", Tab::Permissions), ("审计", Tab::Audit)]),
            ("运行时", vec![("工作流", Tab::Workflow), ("沙箱", Tab::Sandbox)]),
        ]
    }
}

pub fn render(ui: &mut egui::Ui, tab: &Tab, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    match tab {
        Tab::Config => config(ui, agent, t, s, bg_tx),
        Tab::Tools => tools(ui, agent, t),
        Tab::Mcp => mcp(ui, agent, t, s, bg_tx),
        Tab::Skills => skills(ui, agent, t, s, bg_tx),
        Tab::Memory => memory(ui, agent, t),
        Tab::Sessions => sessions(ui, agent, t),
        Tab::Compress => compress(ui, agent, t, s, bg_tx),
        Tab::Extract => extract(ui, agent, t, s, bg_tx),
        Tab::Permissions => perms(ui, t),
        Tab::Audit => audit(ui, t),
        Tab::Workflow => workflow(ui, t),
        Tab::Sandbox => sandbox(ui, t, s),
    }
}

fn h(ui: &mut egui::Ui, text: &str, t: &Theme) {
    ui.label(RichText::new(text).size(15.0).color(t.text).strong());
    ui.add_space(10.0);
}
fn d(ui: &mut egui::Ui, text: &str, t: &Theme) {
    ui.label(RichText::new(text).size(12.0).color(t.text2));
}
fn kv(ui: &mut egui::Ui, k: &str, v: &str, t: &Theme) {
    ui.colored_label(t.text2, format!("{}：", k));
    ui.label(RichText::new(v).color(t.text));
    ui.end_row();
}
fn card(ui: &mut egui::Ui, t: &Theme, f: impl FnOnce(&mut egui::Ui)) {
    Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(8)).stroke(Stroke::new(1.0, t.border)).inner_margin(Margin::same(16)).show(ui, f);
}

fn info_card(ui: &mut egui::Ui, t: &Theme, icon: &str, title: &str, desc: &str) {
    card(ui, t, |ui| {
        ui.label(RichText::new(format!("{} {}", icon, title)).size(13.0).color(t.text).strong());
        ui.add_space(4.0);
        d(ui, desc, t);
    });
}

// ── 配置 ──────────────────────────────────────────

fn config(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    h(ui, "基础配置", t);
    card(ui, t, |ui| {
        if let Some(g) = agent.inner().try_read().ok() {
            egui::Grid::new("cfg").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                kv(ui, "智能体", g.name(), t);
                kv(ui, "当前模型", g.model_name(), t);
                kv(ui, "工具数", &g.tool_names().len().to_string(), t);
                kv(ui, "MCP 服务器", &g.mcp_server_names().len().to_string(), t);
                kv(ui, "技能数", &g.skill_names().len().to_string(), t);
            });
        }
    });
    ui.add_space(12.0);
    h(ui, "模型切换", t);
    card(ui, t, |ui| {
        let current = agent.inner().try_read().ok().map(|g| g.model_name().to_string()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.colored_label(t.text2, "模型名称：");
            ui.add(TextEdit::singleline(&mut s.model_input).hint_text(&current).desired_width(200.0));
        });
        ui.add_space(4.0);
        if ui.button("应用 (重启生效)").clicked() && !s.model_input.is_empty() {
            if let Some(llm_cfg) = agent.inner().try_read().ok().and_then(|g| g.llm_config().cloned()) {
                let mut new_cfg = llm_cfg;
                new_cfg.model = s.model_input.clone();
                let arc = agent.inner().clone();
                tokio::spawn(async move { let mut g = arc.write().await; g.set_llm_config(new_cfg); });
                s.model_input.clear();
            }
        }
        ui.add_space(4.0);
        d(ui, "模型名称修改后立即生效。其他参数 (temperature, max_tokens) 请在 echo-agent.yaml 中修改。", t);
    });
    ui.add_space(12.0);
    h(ui, "系统提示词", t);
    card(ui, t, |ui| {
        // 显示当前系统提示词
        if s.system_prompt_current.is_none() {
            s.system_prompt_current = agent.inner().try_read().ok().map(|g| g.system_prompt().to_string());
        }
        if let Some(ref current) = s.system_prompt_current {
            ui.colored_label(t.text2, "当前提示词 (前200字)：");
            let preview = current.chars().take(200).collect::<String>();
            ui.label(RichText::new(&preview).size(12.0).color(t.text).monospace());
        }
        ui.add_space(6.0);
        ui.colored_label(t.text2, "新提示词：");
        ui.add(TextEdit::multiline(&mut s.system_prompt_input)
            .hint_text("输入新的系统提示词…")
            .desired_rows(4)
            .desired_width(ui.available_width()));
        ui.add_space(4.0);
        if ui.button("应用系统提示词").clicked() && !s.system_prompt_input.is_empty() {
            let arc = agent.inner().clone();
            let prompt = s.system_prompt_input.clone();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let mut g = arc.write().await;
                g.set_system_prompt(prompt.clone()).await;
                let _ = tx.send(BgResult::SystemPromptSet { prompt });
            });
            s.system_prompt_input.clear();
        }
        if let Some(ref fb) = s.system_prompt_feedback {
            ui.add_space(4.0);
            ui.label(RichText::new(fb).size(12.0).color(t.green));
        }
    });
    ui.add_space(12.0);
    h(ui, "迭代控制", t);
    card(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(t.text2, "最大迭代次数：");
            ui.add(TextEdit::singleline(&mut s.iters_input).hint_text("10").desired_width(80.0));
        });
        if ui.button("应用").clicked() {
            if let Ok(n) = s.iters_input.parse::<usize>() {
                let arc = agent.inner().clone();
                tokio::spawn(async move { let mut g = arc.write().await; g.set_max_iterations(n); });
                s.iters_input.clear();
            }
        }
        d(ui, "控制 Agent 每个请求最多执行多少轮思考+工具调用（默认 10）", t);
    });
}

// ── 工具 ──────────────────────────────────────────

fn tools(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme) {
    h(ui, "已加载工具", t);
    if let Some(names) = agent.inner().try_read().ok().map(|g| g.tool_names().into_iter().map(|s| s.to_string()).collect::<Vec<_>>()) {
        card(ui, t, |ui| {
            if names.is_empty() { d(ui, "暂无工具", t); }
            else {
                egui::Grid::new("tools_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for n in &names { ui.label(RichText::new(format!("🔧 {}", n)).size(12.5).color(t.text)); }
                });
            }
        });
        ui.add_space(4.0);
        d(ui, &format!("共 {} 个工具（在 echo-agent.yaml 中配置启用/禁用）", names.len()), t);
    } else { d(ui, "无法读取", t); }
}

// ── MCP ──────────────────────────────────────────

fn mcp(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    h(ui, "已连接的 MCP 服务器", t);
    if let Some(g) = agent.inner().try_read().ok() {
        let names: Vec<String> = g.list_mcp_servers().into_iter().map(|s| s.to_string()).collect();
        if names.is_empty() { d(ui, "未连接任何 MCP 服务器", t); }
        else {
            ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for name in &names {
                    let has_tools = g.mcp_client(name).map(|c| c.tools().len() > 0).unwrap_or(false);
                    let (status_icon, status_color) = if has_tools { ("🟢", t.green) } else { ("🔴", t.red) };
                    let tool_count = g.mcp_client(name).map(|c| c.tools().len()).unwrap_or(0);
                    Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{} {}", status_icon, name)).size(12.5).color(status_color));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("断开").clicked() {
                                        let arc = agent.inner().clone(); let n = name.clone();
                                        tokio::spawn(async move { let mut g = arc.write().await; let _ = g.disconnect_mcp(&n).await; });
                                    }
                                    ui.label(RichText::new(format!("{} tools", tool_count)).size(11.0).color(t.text2));
                                });
                            });
                        });
                }
            });
        }
    }

    ui.add_space(12.0);
    h(ui, "MCP 配置 (mcp.json)", t);
    d(ui, "粘贴完整的 mcpServers JSON 配置，点击应用连接新服务器（不会断开已有服务器）", t);
    ui.add_space(4.0);
    ui.add(TextEdit::multiline(&mut s.mcp_json).hint_text(r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@playwright/mcp@latest"]
    },
    "my-server": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer xxx" }
    }
  }
}"#).desired_rows(10).desired_width(ui.available_width()));
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui.button("应用配置").clicked() && !s.mcp_json.is_empty() && !s.mcp_connecting {
            let arc = agent.inner().clone(); let json = s.mcp_json.clone();
            let tx = bg_tx.clone();
            s.mcp_connecting = true;
            s.mcp_connect_feedback = Some("正在连接…".to_string());
            tokio::spawn(async move {
                match echo_agent::mcp::McpConfigFile::parse(&json) {
                    Ok(cfg) => match cfg.to_server_configs() {
                        Ok(configs) => {
                            let mut g = arc.write().await;
                            let mut results = Vec::new();
                            for c in configs {
                                let name = c.name.clone();
                                match g.connect_mcp_from_config(c).await {
                                    Ok(_) => results.push(format!("✅ {} 连接成功", name)),
                                    Err(e) => results.push(format!("❌ {} 连接失败: {}", name, e)),
                                }
                            }
                            if results.is_empty() {
                                let _ = tx.send(BgResult::McpConnectFeedback { message: "没有新的服务器配置".into() });
                            } else {
                                let _ = tx.send(BgResult::McpConnectFeedback { message: results.join("\n") });
                            }
                        }
                        Err(e) => { let _ = tx.send(BgResult::McpConnectFeedback { message: format!("配置解析错误: {}", e) }); }
                    },
                    Err(e) => { let _ = tx.send(BgResult::McpConnectFeedback { message: format!("JSON 解析错误: {}", e) }); }
                }
            });
        }
        if s.mcp_connecting {
            ui.spinner();
        }
        if ui.button("📂 从文件加载").clicked() {
            let arc = agent.inner().clone();
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    if let Some(p) = path.to_str() {
                        let path_str = p.to_string();
                        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
                            let mut g = arc.write().await;
                            let result = g.load_mcp_from_file(&path_str).await;
                            match result {
                                Ok(clients) => {
                                    let count = clients.len();
                                    let _ = tx.send(BgResult::McpConnectFeedback {
                                        message: format!("✅ 从文件加载 {} 个服务器", count),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(BgResult::McpConnectFeedback {
                                        message: format!("❌ 文件加载失败: {}", e),
                                    });
                                }
                            }
                        });
                    }
                }
            });
        }
    });

    if let Some(ref fb) = s.mcp_connect_feedback {
        ui.add_space(4.0);
        Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                    for line in fb.split('\n') {
                        let color = if line.starts_with("✅") { t.green } else if line.starts_with("❌") { t.red } else { t.text };
                        ui.label(RichText::new(line).size(12.0).color(color));
                    }
                });
            });
    }
}

// ── 技能 ──────────────────────────────────────────

fn skills(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    h(ui, "技能管理", t);
    if ui.button("📂 从目录加载 Skill").clicked() {
        let arc = agent.inner().clone();
        let tx = bg_tx.clone();
        std::thread::spawn(move || {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                if let Some(p) = dir.to_str() {
                    let path = p.to_string();
                    let names = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
                        let mut g = arc.write().await;
                        g.load_skills_from_dir(&path).await.unwrap_or_default()
                    });
                    let _ = tx.send(BgResult::SkillsLoaded { names });
                }
            }
        });
    }
    ui.add_space(8.0);
    if let Some(ref fb) = s.skills_feedback {
        ui.add_space(4.0);
        ui.label(RichText::new(fb).size(12.0).color(t.text));
    }

    if let Some(g) = agent.inner().try_read().ok() {
        let code_skills = g.list_skills();
        let reg = g.skill_registry();
        let file_skills = reg.list_descriptors();

        if code_skills.is_empty() && file_skills.is_empty() {
            d(ui, "暂无技能。点击上方按钮从目录加载。", t);
        } else {
            ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for s in &code_skills {
                    Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.label(RichText::new(format!("📄 {}", s.name)).size(12.5).color(t.text));
                            ui.label(RichText::new(&s.description).size(11.0).color(t.text2));
                        });
                }
                for d in &file_skills {
                    Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.label(RichText::new(format!("📁 {}", d.name)).size(12.5).color(t.text));
                            ui.label(RichText::new(&d.description).size(11.0).color(t.text2));
                        });
                }
            });
            let total = code_skills.len() + file_skills.len();
            d(ui, &format!("共 {} 个技能 ({} 内置 + {} 文件)", total, code_skills.len(), file_skills.len()), t);
        }
    }
}

// ── 记忆 ──────────────────────────────────────────

fn memory(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme) {
    h(ui, "长期记忆", t);
    card(ui, t, |ui| {
        if let Some(g) = agent.inner().try_read().ok() {
            egui::Grid::new("mem").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                kv(ui, "快照数", &g.snapshots().len().to_string(), t);
                kv(ui, "记忆存储", if g.store().is_some() { "已启用" } else { "未启用 (需配置 embedding)" }, t);
            });
        }
        ui.add_space(4.0);
        d(ui, "长期记忆在对话中自动检索并注入上下文", t);
    });
}

// ── 会话 ──────────────────────────────────────────

fn sessions(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme) {
    h(ui, "会话快照", t);
    if ui.button("📸 创建快照").clicked() {
        let arc = agent.inner().clone();
        tokio::spawn(async move { let _ = arc.read().await.snapshot().await; });
    }
    ui.add_space(8.0);
    if let Some(snaps) = agent.inner().try_read().ok().map(|g| g.snapshots()) {
        if snaps.is_empty() {
            d(ui, "暂无快照。在对话过程中点击上方按钮创建。", t);
        } else {
            ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for s in &snaps {
                    Frame::default().fill(t.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("📸 迭代 #{}", s.iteration)).size(12.5).color(t.text));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("回滚").clicked() {
                                        let arc = agent.inner().clone(); let id = s.id.clone();
                                        tokio::spawn(async move { let _ = arc.read().await.rollback_to(&id).await; });
                                    }
                                });
                            });
                            ui.label(RichText::new(&s.id).size(10.5).color(t.text2));
                        });
                }
            });
        }
    }
}

// ── 压缩（可交互）───────────────────────────────────

fn compress(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    h(ui, "上下文压缩", t);

    // 上下文统计 — 通过 bg_tx 通道获取
    card(ui, t, |ui| {
        if let Some(g) = agent.inner().try_read().ok() {
            let tool_count = g.tool_names().len();
            let model = g.model_name().to_string();
            kv(ui, "当前模型", &model, t);
            kv(ui, "工具数", &tool_count.to_string(), t);
        }
        ui.add_space(6.0);
        if ui.button("刷新统计").clicked() {
            let arc = agent.inner().clone();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let guard = arc.read().await;
                let (msg_count, token_est) = guard.context_stats().await;
                let _ = tx.send(BgResult::ContextStats { msg_count, token_est });
            });
        }
        if let Some(ref stats) = s.context_stats_display {
            ui.label(RichText::new(stats).size(13.0).color(t.text).strong());
        } else {
            d(ui, "点击「刷新统计」获取上下文信息", t);
        }
    });

    ui.add_space(12.0);
    h(ui, "手动压缩", t);
    card(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(t.text2, "保留最近消息数：");
            ui.add(TextEdit::singleline(&mut s.compress_keep).desired_width(80.0));
        });
        ui.add_space(4.0);
        if ui.button("执行压缩").clicked() {
            if let Ok(keep) = s.compress_keep.parse::<usize>() {
                let arc = agent.inner().clone();
                let tx = bg_tx.clone();
                tokio::spawn(async move {
                    let guard = arc.read().await;
                    let compressor = echo_agent::compression::compressor::SlidingWindowCompressor::new(keep);
                    let result = guard.force_compress_with(&compressor).await;
                    match result {
                        Ok(stats) => {
                            let _ = tx.send(BgResult::CompressResult {
                                before_count: stats.before_count, after_count: stats.after_count,
                                before_tokens: stats.before_tokens, after_tokens: stats.after_tokens, evicted: stats.evicted,
                            });
                        }
                        Err(e) => { let _ = tx.send(BgResult::CompressError { error: e.to_string() }); }
                    };
                });
                s.compress_result = Some("压缩正在执行…".into());
            }
        }
        if let Some(r) = &s.compress_result {
            ui.add_space(4.0);
            let color = if r.starts_with("✅") { t.green } else if r.starts_with("❌") { t.red } else { t.accent };
            ui.label(RichText::new(r).size(12.0).color(color));
        }
    });
    ui.add_space(4.0);
    d(ui, "对话历史超过 token 限制时自动压缩。保留系统提示词和最近消息，将更早的消息压缩为摘要。", t);
}

// ── 提取（可交互）───────────────────────────────────

fn extract(ui: &mut egui::Ui, agent: &AgentHandle, t: &Theme, s: &mut SettingsState, bg_tx: &tokio::sync::mpsc::UnboundedSender<BgResult>) {
    h(ui, "结构化提取", t);

    // 预设示例
    ui.horizontal(|ui| {
        d(ui, "快速示例：", t);
        if ui.small_button("人物信息").clicked() {
            s.extract_input = "张三，28岁，工程师，居住在北京海淀区".into();
            s.extract_schema = r#"{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"},"occupation":{"type":"string"},"location":{"type":"string"}}}"#.into();
        }
        if ui.small_button("情感分析").clicked() {
            s.extract_input = "今天天气真好，心情很愉快，但是路上堵车让人有点烦".into();
            s.extract_schema = r#"{"type":"object","properties":{"sentiment":{"type":"string","enum":["positive","negative","mixed"]},"emotions":{"type":"array","items":{"type":"string"}}}}"#.into();
        }
        if ui.small_button("事件列表").clicked() {
            s.extract_input = "上周一参加了项目评审，周三提交了代码，周五部署上线，周末去了公园".into();
            s.extract_schema = r#"{"type":"object","properties":{"events":{"type":"array","items":{"type":"object","properties":{"date":{"type":"string"},"action":{"type":"string"}}}}}}"#.into();
        }
    });
    ui.add_space(8.0);

    card(ui, t, |ui| {
        ui.colored_label(t.text2, "输入文本：");
        ui.add(TextEdit::multiline(&mut s.extract_input).hint_text("输入要提取的文本内容…").desired_rows(3).desired_width(ui.available_width()));
        ui.add_space(6.0);
        ui.colored_label(t.text2, "JSON Schema：");
        ui.add(TextEdit::multiline(&mut s.extract_schema).hint_text("定义提取的结构…").desired_rows(4).desired_width(ui.available_width()));
        ui.add_space(4.0);
        if ui.button("提取").clicked() && !s.extract_input.is_empty() && !s.extract_schema.is_empty() {
            let arc = agent.inner().clone();
            let input = s.extract_input.clone();
            let schema_str = s.extract_schema.clone();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let schema: serde_json::Value = serde_json::from_str(&schema_str).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let response_format = echo_agent::llm::types::ResponseFormat::json_schema("extract", schema);
                let guard = arc.read().await;
                let result = guard.extract_json(&input, response_format).await;
                match result {
                    Ok(val) => { let _ = tx.send(BgResult::ExtractResult { value: val }); }
                    Err(e) => { let _ = tx.send(BgResult::ExtractError { error: e.to_string() }); }
                }
            });
            s.extract_result = Some("正在提取…".into());
        }
    });

    if let Some(r) = &s.extract_result {
        ui.add_space(8.0);
        h(ui, "提取结果", t);
        card(ui, t, |ui| {
            ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                ui.label(RichText::new(r).size(12.0).color(t.text).monospace());
            });
        });
    }
}

// ── 权限（描述 + 模式说明）─────────────────────────
// ReactAgent 上没有 set_permission_mode/add_permission_rule 等方法
// 这些功能在 Web API 模式下通过 REST API 操作

fn perms(ui: &mut egui::Ui, t: &Theme) {
    h(ui, "权限策略", t);
    info_card(ui, t, "🛡", "三级权限模式", "Agent 支持三种权限模式：\n\n• 默认 — 按规则判断，危险操作需人工批准\n• 自动批准 — 所有工具调用自动执行（适合可信环境）\n• 严格 — 所有工具调用需人工批准\n\n权限变更需批准时，聊天界面弹出对话框。");
    ui.add_space(12.0);
    info_card(ui, t, "📋", "规则管理", "权限规则由匹配器（工具名/glob模式）和行为（允许/拒绝/询问）组成。\n\n在 Web 模式下通过 REST API 管理：\n• GET /api/permissions — 获取当前模式\n• PUT /api/permissions/mode — 切换模式\n• POST/DELETE /api/permissions/rules — 添加/删除规则");
    ui.add_space(12.0);
    info_card(ui, t, "💡", "使用建议", "对于 GUI 模式，权限弹窗会自动在聊天界面出现。建议使用「默认」模式，既保证安全又减少干扰。");
}

// ── 审计（描述 + Web API 指引）─────────────────────

fn audit(ui: &mut egui::Ui, t: &Theme) {
    h(ui, "审计日志", t);
    info_card(ui, t, "📝", "审计日志记录", "记录所有工具调用决策：\n\n• 工具名称和参数哈希\n• 审批决策（允许/拒绝/询问）\n• 执行时间和耗时\n• 操作时间戳\n\n审计数据通过 Web API 查询：\n• GET /api/audit/logs — 获取日志列表\n• GET /api/audit/stats — 获取统计摘要\n• DELETE /api/audit/logs — 清空日志");
    ui.add_space(12.0);
    info_card(ui, t, "📊", "统计维度", "按决策类型统计：允许 ✅ / 拒绝 ❌ / 询问 ⚠\n\n可按时间范围和工具类型过滤。在 Web 模式下启动时审计自动开启。");
}

// ── 工作流（描述 + 示例）───────────────────────────

fn workflow(ui: &mut egui::Ui, t: &Theme) {
    h(ui, "工作流", t);
    info_card(ui, t, "🔗", "DAG 工作流编排", "YAML 定义 DAG 工作流，编排多个 Agent 协同：\n\n• 顺序执行 — 步骤按序进行\n• 并行执行 — 多步骤同时运行\n• 条件分支 — 根据结果选择路径\n\n示例定义：\n```yaml\nsteps:\n  - id: step1\n    agent: main\n    prompt: \"分析数据\"\n  - id: step2\n    agent: main\n    prompt: \"生成报告\"\n    depends_on: [step1]\n```");
    ui.add_space(12.0);
    info_card(ui, t, "⚙", "Web API 操作", "• POST /api/workflow/create — 创建工作流\n• GET /api/workflow — 列出工作流\n• POST /api/workflow/:id/execute — 执行工作流");
}

// ── 沙箱（描述 + 代码执行区）─────────────────────────

fn sandbox(ui: &mut egui::Ui, t: &Theme, s: &mut SettingsState) {
    h(ui, "代码沙箱", t);

    info_card(ui, t, "🔒", "安全执行环境", "安全执行 AI 生成的代码：\n\n• 支持语言：Shell / Python / Ruby / Node.js / Perl\n• 隔离方式：本地 / Docker / Kubernetes\n• 安全级别：低（无限制）/ 中（限制网络）/ 高（完全隔离）\n\n在 Web 模式下通过 API 执行：\n• GET /api/sandbox/status — 沙箱状态\n• POST /api/sandbox/execute — 执行代码");

    ui.add_space(12.0);
    h(ui, "代码执行（本地模式）", t);
    card(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(t.text2, "语言：");
            let languages = ["python", "javascript", "shell", "ruby"];
            egui::ComboBox::from_id_salt("sandbox_lang")
                .selected_text(&s.sandbox_lang)
                .show_ui(ui, |ui| {
                    for lang in languages {
                        ui.selectable_value(&mut s.sandbox_lang, lang.to_string(), lang);
                    }
                });
        });
        ui.add_space(4.0);
        ui.colored_label(t.text2, "代码：");
        ui.add(TextEdit::multiline(&mut s.sandbox_code)
            .hint_text(if s.sandbox_lang == "python" { "print('Hello, World!')" } else { "// Enter code" })
            .desired_rows(6)
            .desired_width(ui.available_width()));
        ui.add_space(4.0);
        if ui.button("▶ 执行（本地沙箱）").clicked() && !s.sandbox_code.is_empty() {
            // 本地直接执行（简单沙箱模式）
            let lang = s.sandbox_lang.clone();
            let code = s.sandbox_code.clone();
            let result = execute_local(&lang, &code);
            s.sandbox_result = Some(format!("退出码: {}\nstdout: {}\nstderr: {}",
                result.exit_code,
                result.stdout.chars().take(2000).collect::<String>(),
                result.stderr.chars().take(2000).collect::<String>(),
            ));
        }
    });

    if let Some(r) = &s.sandbox_result {
        ui.add_space(8.0);
        h(ui, "执行结果", t);
        card(ui, t, |ui| {
            ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                ui.label(RichText::new(r).size(12.0).color(t.text).monospace());
            });
        });
    }
}

struct LocalResult { exit_code: i32, stdout: String, stderr: String }

fn execute_local(lang: &str, code: &str) -> LocalResult {
    use std::process::Command;
    let (cmd, args) = match lang {
        "python" => ("python3", vec!["-c", code]),
        "javascript" => ("node", vec!["-e", code]),
        "shell" => ("sh", vec!["-c", code]),
        "ruby" => ("ruby", vec!["-e", code]),
        _ => ("sh", vec!["-c", code]),
    };
    let output = Command::new(cmd)
        .args(&args)
        .output();
    match output {
        Ok(o) => LocalResult {
            exit_code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        },
        Err(e) => LocalResult {
            exit_code: -1,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}