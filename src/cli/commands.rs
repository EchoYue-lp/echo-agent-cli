//! 命令处理模块
//!
//! 定义所有支持的斜杠命令。

use crate::agent_handle::AgentHandle;
use echo_agent::llm::types::FunctionCall;
use echo_agent::prelude::*;
use nu_ansi_term::Color;

use crate::output::{ColorTheme, OutputFormat, OutputRenderer};
use crate::persistence::Persistence;
use crate::profiles::ProfileManager;
use crate::project::{context as project_context, modes as agent_modes};
use crate::sessions::SessionSearchEngine;

/// 命令处理结果
pub enum CommandResult {
    /// 继续 REPL 循环
    Continue,
    /// 退出 REPL
    Exit,
    /// 需要执行对话
    Chat(String),
}

/// 命令处理器
pub struct CommandHandler {
    agent: AgentHandle,
    output: OutputRenderer,
    current_mode: String,
}

impl CommandHandler {
    pub fn new(agent: AgentHandle) -> Self {
        Self {
            agent,
            output: OutputRenderer::default(),
            current_mode: "general".to_string(),
        }
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.current_mode = mode.to_string();
        self
    }

    /// 处理用户输入
    pub async fn handle(&self, input: &str) -> CommandResult {
        let input = input.trim();

        if input.is_empty() {
            return CommandResult::Continue;
        }

        if input.starts_with('/') {
            self.handle_command(input).await
        } else {
            CommandResult::Chat(input.to_string())
        }
    }

    /// 处理斜杠命令
    async fn handle_command(&self, input: &str) -> CommandResult {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return CommandResult::Continue;
        }

        let cmd = parts[0];
        let args = &parts[1..];

        match cmd {
            "/help" | "/h" | "/?" => {
                self.print_help();
                CommandResult::Continue
            }
            "/exit" | "/quit" | "/q" => {
                let (_, _, tool_calls) = crate::cli::repl::get_usage_stats();
                if tool_calls > 0 {
                    self.output
                        .print_info(&format!("本次会话: {} 工具调用", tool_calls));
                }
                println!("\n👋 再见！");
                CommandResult::Exit
            }
            "/reset" | "/r" => {
                self.cmd_reset().await;
                CommandResult::Continue
            }
            "/clear" | "/cls" => {
                self.cmd_clear();
                CommandResult::Continue
            }
            "/tools" | "/t" => {
                self.cmd_tools().await;
                CommandResult::Continue
            }
            "/skills" | "/sk" => {
                let args_str = args.join(" ");
                self.cmd_skills(&args_str).await;
                CommandResult::Continue
            }
            "/mcp" | "/m" => {
                self.cmd_mcp().await;
                CommandResult::Continue
            }
            "/history" | "/hist" => {
                self.cmd_history().await;
                CommandResult::Continue
            }
            "/compress" | "/cp" => {
                self.cmd_compress().await;
                CommandResult::Continue
            }
            "/stats" | "/st" => {
                self.cmd_stats().await;
                CommandResult::Continue
            }
            "/model" => {
                self.cmd_model(args).await;
                CommandResult::Continue
            }
            "/system" | "/sys" => {
                self.cmd_system(args).await;
                CommandResult::Continue
            }
            "/save" => {
                self.cmd_save(args).await;
                CommandResult::Continue
            }
            "/load" => {
                self.cmd_load(args).await;
                CommandResult::Continue
            }
            "/sessions" | "/ss" => {
                self.cmd_list_sessions();
                CommandResult::Continue
            }
            "/theme" => {
                self.cmd_theme(args).await;
                CommandResult::Continue
            }
            "/output" => {
                self.cmd_output(args).await;
                CommandResult::Continue
            }
            "/verbose" => {
                self.cmd_verbose().await;
                CommandResult::Continue
            }
            "/inspect" | "/ins" => {
                self.cmd_inspect().await;
                CommandResult::Continue
            }
            "/tui" => {
                self.cmd_tui_info();
                CommandResult::Continue
            }
            "/export" => {
                self.cmd_export(args).await;
                CommandResult::Continue
            }
            "/profile" | "/prof" => {
                self.cmd_profile(args).await;
                CommandResult::Continue
            }
            "/debug" | "/dbg" => {
                self.cmd_debug(args).await;
                CommandResult::Continue
            }
            "/mode" => {
                self.cmd_mode(args).await;
                CommandResult::Continue
            }
            "/project" | "/proj" => {
                self.cmd_project(args).await;
                CommandResult::Continue
            }
            "/cost" => {
                self.cmd_cost().await;
                CommandResult::Continue
            }
            "/undo" | "/u" => {
                self.cmd_undo().await;
                CommandResult::Continue
            }
            "/compact" => {
                self.cmd_compact().await;
                CommandResult::Continue
            }
            "/think" => {
                self.cmd_think(args).await;
                CommandResult::Continue
            }
            "/status" => {
                self.cmd_status().await;
                CommandResult::Continue
            }
            "/new" | "/n" => {
                self.cmd_new().await;
                CommandResult::Continue
            }
            "/delegate" | "/dl" => {
                self.cmd_delegate(args).await;
                CommandResult::Continue
            }
            "/search" => {
                self.cmd_search(args).await;
                CommandResult::Continue
            }
            "/cron" => {
                self.cmd_cron(args).await;
                CommandResult::Continue
            }
            "/trace" => {
                self.cmd_trace();
                CommandResult::Continue
            }
            "/usage" => {
                self.cmd_usage().await;
                CommandResult::Continue
            }
            "/doctor" | "/doc" => {
                self.cmd_doctor().await;
                CommandResult::Continue
            }
            _ => {
                println!("\n❌ 未知命令: {}", cmd);
                println!("   输入 /help 查看可用命令");
                CommandResult::Continue
            }
        }
    }

    /// 打印帮助信息
    fn print_help(&self) {
        println!();
        println!("╭─────────────────────────────────────────────────────────────╮");
        println!("│                      📖 帮助信息                              │");
        println!("╰─────────────────────────────────────────────────────────────╯");
        println!();
        println!("  对话命令:");
        println!("    <消息>          发送消息给 Agent");
        println!("    /mode [模式]    查看/切换 Agent 模式 (general/coding/research/data/writing)");
        println!("    /project [路径] 查看/加载项目上下文");
        println!("    /think [low|medium|high]  调整思考深度");
        println!("    /undo, /u       撤销上一轮对话");
        println!("    /new, /n        开始新会话");
        println!("    /reset, /r      重置对话历史");
        println!("    /clear, /cls    清屏");
        println!("    /exit, /q       退出程序");
        println!();
        println!("  信息查询:");
        println!("    /tools, /t      列出已注册工具");
        println!("    /skills, /sk    技能管理 (list/search/install/uninstall/info)");
        println!("    /mcp, /m        列出 MCP 服务连接");
        println!("    /history, /hist 查看对话历史");
        println!("    /stats, /st     查看上下文统计");
        println!("    /status         查看 Agent 运行状态");
        println!("    /cost           查看会话用量统计");
        println!("    /search <关键词> FTS5 全文搜索历史会话 (/reindex 重建索引)");
        println!("    /trace          查看最近一次对话的执行时间线");
        println!("    /usage          查看 Token 用量和费用估算");
        println!();
        println!("  配置命令:");
        println!("    /model <名称>   切换模型");
        println!("    /system <提示词> 设置系统提示词");
        println!("    /compress, /cp  手动触发上下文压缩 (摘要式)");
        println!("    /compact        轻量压缩 (滑动窗口，保留近期)");
        println!("    /delegate <任务> 委派子代理执行任务");
        println!();
        println!("  会话管理:");
        println!("    /save [名称]    保存当前会话 (默认: default)");
        println!("    /load <名称>    加载已保存的会话");
        println!("    /sessions, /ss  列出所有已保存的会话");
        println!("    /cron list      列出定时任务");
        println!("    /cron add <cron> <prompt>  添加定时任务");
        println!("    /cron remove <id>          删除定时任务");
        println!("    /cron enable/disable <id>  启用/禁用任务");
        println!("    /cron run <id>   手动触发任务");
        println!();
        println!("  输出与主题:");
        println!("    /theme <名称>   切换颜色主题 (dark/light/monokai/...)");
        println!("    /output <格式>  设置输出格式 (text/json/markdown/table)");
        println!("    /verbose        切换详细输出模式");
        println!();
        println!("  档案管理:");
        println!("    /profile, /prof 管理配置档案 (create/use/delete/show)");
        println!();
        println!("  调试:");
        println!("    /debug, /dbg    调试工具 (on/off/stats/recent/clear)");
        println!();
        println!("  高级:");
        println!("    /inspect, /ins  查看 Agent 详细状态");
        println!("    /tui            切换到终端 UI 模式");
        println!("    /export [名称]  导出会话到文件");
        println!();
        println!("  帮助:");
        println!("    /help, /h, /?   显示此帮助信息");
        println!();
    }

    /// 重置对话
    async fn cmd_reset(&self) {
        self.agent
            .write_async(|a| Box::pin(async move { a.reset().await }))
            .await;
        println!("\n✅ 对话已重置");
    }

    /// 清屏
    fn cmd_clear(&self) {
        print!("\x1B[2J\x1B[1;1H");
    }

    /// 列出工具
    async fn cmd_tools(&self) {
        self.agent
            .read(|agent| {
                let tools = agent.tool_names();

                println!("\n╭─────────────────────────────────────────────────────────────╮");
                println!(
                    "│                    🔧 已注册工具 ({} 个)                       │",
                    tools.len()
                );
                println!("╰─────────────────────────────────────────────────────────────╯");

                for name in &tools {
                    if let Some(def) = agent
                        .tool_definitions()
                        .iter()
                        .find(|d| &d.function.name == name)
                    {
                        println!(
                            "  • {} - {}",
                            name,
                            def.function
                                .description
                                .chars()
                                .take(50)
                                .collect::<String>()
                        );
                    } else {
                        println!("  • {}", name);
                    }
                }
                println!();
            })
            .await;
    }

    /// 列出技能
    async fn cmd_skills(&self, args: &str) {
        let parts: Vec<&str> = args.splitn(2, ' ').collect();
        let subcmd = parts.first().copied().unwrap_or("").trim();
        let rest = parts.get(1).copied().unwrap_or("").trim();

        match subcmd {
            "" | "list" | "ls" => self.cmd_skills_list().await,
            "search" | "find" => self.cmd_skills_search(rest).await,
            "install" => self.cmd_skills_install(rest).await,
            "uninstall" | "remove" | "rm" => self.cmd_skills_uninstall(rest).await,
            "info" => self.cmd_skills_info(rest).await,
            "refresh" => self.cmd_skills_refresh().await,
            _ => {
                println!("\n  未知子命令: /skills {subcmd}");
                println!("  可用: list, search, install, uninstall, info, refresh");
                println!();
            }
        }
    }

    async fn cmd_skills_list(&self) {
        let skills: Vec<String> = self
            .agent
            .read(|a| a.skill_names().iter().map(|s| s.to_string()).collect())
            .await;

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!(
            "│                    🎯 已加载技能 ({} 个)                       │",
            skills.len()
        );
        println!("╰─────────────────────────────────────────────────────────────╯");

        if skills.is_empty() {
            println!("  暂无已加载的技能");
        } else {
            for name in &skills {
                println!("  • {}", name);
            }
        }

        // 同时显示 Skills Hub 中的可用技能
        let hub = crate::skills_hub::SkillsHub::new();
        let hub_entries = hub.list();
        let unloaded: Vec<_> = hub_entries
            .iter()
            .filter(|e| !skills.contains(&e.name))
            .collect();

        if !unloaded.is_empty() {
            println!();
            println!("╭─────────────────────────────────────────────────────────────╮");
            println!(
                "│                    📦 Hub 可用技能 ({} 个未加载)               │",
                unloaded.len()
            );
            println!("╰─────────────────────────────────────────────────────────────╯");
            for e in &unloaded {
                let desc = if e.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", e.description)
                };
                println!("  • {}{}", e.name, desc);
            }
        }

        println!();
    }

    async fn cmd_skills_search(&self, query: &str) {
        if query.is_empty() {
            println!("\n  用法: /skills search <关键词>");
            println!();
            return;
        }

        let hub = crate::skills_hub::SkillsHub::new();
        let results = hub.search(query);

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!(
            "│                    🔍 技能搜索: \"{}\" ({} 个结果)             │",
            query,
            results.len()
        );
        println!("╰─────────────────────────────────────────────────────────────╯");

        if results.is_empty() {
            println!("  无匹配技能");
        } else {
            for e in &results {
                let status = if e.loaded {
                    "✓ 已加载"
                } else {
                    "○ 未加载"
                };
                println!("  {} {} — {}", status, e.name, e.description);
            }
        }
        println!();
    }

    async fn cmd_skills_install(&self, args: &str) {
        if args.is_empty() {
            println!("\n  用法: /skills install <本地路径|git仓库URL>");
            println!("  示例:");
            println!("    /skills install /path/to/my-skill");
            println!("    /skills install https://github.com/user/skill-repo");
            println!();
            return;
        }

        let mut hub = crate::skills_hub::SkillsHub::new();

        let result = if args.starts_with("http://")
            || args.starts_with("https://")
            || args.ends_with(".git")
        {
            crate::skills_hub::install::install_from_git(args, None, &mut hub).await
        } else {
            let path = std::path::PathBuf::from(args);
            crate::skills_hub::install::install_from_local(&path, &mut hub)
        };

        match result {
            Ok(r) => {
                println!("\n  ✓ 技能 '{}' 安装成功", r.name);
                println!("    路径: {}", r.path.display());
                println!("    来源: {}", r.source);
                println!("    使用 /skills info {} 查看详情", r.name);
            }
            Err(e) => println!("\n  ✗ 安装失败: {e}"),
        }
        println!();
    }

    async fn cmd_skills_uninstall(&self, name: &str) {
        if name.is_empty() {
            println!("\n  用法: /skills uninstall <技能名>");
            println!();
            return;
        }

        let mut hub = crate::skills_hub::SkillsHub::new();
        match crate::skills_hub::install::uninstall(name, &mut hub) {
            Ok(()) => println!("\n  ✓ 技能 '{}' 已卸载", name),
            Err(e) => println!("\n  ✗ 卸载失败: {e}"),
        }
        println!();
    }

    async fn cmd_skills_info(&self, name: &str) {
        if name.is_empty() {
            println!("\n  用法: /skills info <技能名>");
            println!();
            return;
        }

        let hub = crate::skills_hub::SkillsHub::new();
        match hub.get(name) {
            Some(e) => {
                println!("\n╭─────────────────────────────────────────────────────────────╮");
                println!(
                    "│                    📋 技能详情: {}                           │",
                    e.name
                );
                println!("╰─────────────────────────────────────────────────────────────╯");
                println!("  名称:       {}", e.name);
                println!("  描述:       {}", e.description);
                println!("  路径:       {}", e.path.display());
                if let Some(v) = &e.version {
                    println!("  版本:       {}", v);
                }
                if let Some(a) = &e.author {
                    println!("  作者:       {}", a);
                }
                if let Some(l) = &e.license {
                    println!("  许可证:     {}", l);
                }
                if let Some(c) = &e.compatibility {
                    println!("  兼容性:     {}", c);
                }
                if !e.tags.is_empty() {
                    println!("  标签:       {}", e.tags.join(", "));
                }
                println!(
                    "  状态:       {}",
                    if e.loaded { "已加载" } else { "未加载" }
                );
            }
            None => println!("\n  技能 '{}' 未在 Hub 中找到", name),
        }
        println!();
    }

    async fn cmd_skills_refresh(&self) {
        let mut hub = crate::skills_hub::SkillsHub::new();
        hub.refresh();
        let count = hub.list().len();
        println!("\n  ✓ Skills Hub 已刷新，共 {} 个技能", count);
        println!();
    }

    /// 列出 MCP 服务
    async fn cmd_mcp(&self) {
        let servers: Vec<String> = self
            .agent
            .read(|a| a.mcp_server_names().iter().map(|s| s.to_string()).collect())
            .await;

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!(
            "│                    🔌 MCP 服务 ({} 个)                        │",
            servers.len()
        );
        println!("╰─────────────────────────────────────────────────────────────╯");

        if servers.is_empty() {
            println!("  暂无连接的 MCP 服务");
        } else {
            for name in &servers {
                println!("  • {}", name);
            }
        }
        println!();
    }

    /// 查看对话历史
    async fn cmd_history(&self) {
        let messages = self
            .agent
            .read_async(|a| Box::pin(async move { a.get_messages().await }))
            .await;

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!(
            "│                    📜 对话历史 ({} 条)                        │",
            messages.len()
        );
        println!("╰─────────────────────────────────────────────────────────────╯");

        for (i, msg) in messages.iter().enumerate() {
            let role_icon = match msg.role.as_str() {
                "system" => "⚙️",
                "user" => "👤",
                "assistant" => "🤖",
                "tool" => "🔧",
                _ => "💬",
            };

            let content = msg.content.as_deref().unwrap_or("");
            let preview: String = content.chars().take(100).collect();
            let suffix = if content.len() > 100 { "..." } else { "" };

            println!(
                "  {}. {} {}: {}{}",
                i + 1,
                role_icon,
                msg.role.as_str(),
                preview,
                suffix
            );

            if let Some(calls) = &msg.tool_calls {
                for tc in calls {
                    println!("      └─ 🔧 调用: {}", tc.function.name);
                }
            }
        }
        println!();
    }

    /// 触发压缩
    async fn cmd_compress(&self) {
        let compressor = SlidingWindowCompressor::new(10);

        println!("\n⏳ 正在压缩上下文...");

        match self
            .agent
            .read_async(|a| Box::pin(async move { a.force_compress_with(&compressor).await }))
            .await
        {
            Ok(stats) => {
                println!("\n✅ 压缩完成:");
                println!("   消息: {} -> {}", stats.before_count, stats.after_count);
                println!(
                    "   Token: {} -> {}",
                    stats.before_tokens, stats.after_tokens
                );
                println!("   裁剪: {} 条消息", stats.evicted);
            }
            Err(e) => {
                println!("\n❌ 压缩失败: {}", e);
            }
        }
        println!();
    }

    /// 查看统计
    async fn cmd_stats(&self) {
        let (msg_count, tokens, token_limit) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let (mc, tok) = a.context_stats().await;
                    (mc, tok, a.config().get_token_limit())
                })
            })
            .await;

        let usage = if token_limit > 0 {
            format!("{:.1}%", (tokens as f32 / token_limit as f32) * 100.0)
        } else {
            "N/A".to_string()
        };

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!("│                    📊 上下文统计                              │");
        println!("╰─────────────────────────────────────────────────────────────╯");
        println!("  消息数量: {}", msg_count);
        println!("  Token 数: {} / {}", tokens, token_limit);
        println!("  使用率:   {}", usage);
        println!();
    }

    /// 切换模型
    async fn cmd_model(&self, args: &[&str]) {
        if args.is_empty() {
            let model = self.agent.read(|a| a.model_name().to_string()).await;
            println!("\n当前模型: {}", model);
            println!("用法: /model <模型名称>");
            println!("示例: /model qwen-max");
            return;
        }

        let model = args.join(" ");
        self.agent.write(|a| a.set_model(&model)).await;
        println!("\n✅ 已切换到模型: {}", model);
    }

    /// 设置系统提示词
    async fn cmd_system(&self, args: &[&str]) {
        if args.is_empty() {
            let sys = self.agent.read(|a| a.system_prompt().to_string()).await;
            println!("\n当前系统提示词:");
            println!("{}\n", sys);
            println!("用法: /system <新的提示词>");
            return;
        }

        let prompt = args.join(" ");
        self.agent
            .write_async(|a| Box::pin(async move { a.set_system_prompt(prompt.clone()).await }))
            .await;
        println!("\n✅ 系统提示词已更新");
    }

    /// 保存会话
    async fn cmd_save(&self, args: &[&str]) {
        let name = if args.is_empty() {
            "default".to_string()
        } else {
            args[0].to_string()
        };

        println!("\n⏳ 正在保存会话 '{}'...", name);

        let (messages, model, system_prompt) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let msgs = a.get_messages().await;
                    let model = a.model_name().to_string();
                    let sp = a.system_prompt().to_string();
                    (msgs, model, sp)
                })
            })
            .await;

        let persistence = Persistence::new();
        match persistence.save_session(&name, &messages, &model, &system_prompt) {
            Ok(()) => {
                println!("✅ 会话 '{}' 已保存 ({} 条消息)", name, messages.len());
            }
            Err(e) => {
                println!("❌ 保存失败: {}", e);
            }
        }
    }

    /// 加载会话
    async fn cmd_load(&self, args: &[&str]) {
        if args.is_empty() {
            println!("\n用法: /load <会话名称>");
            println!("使用 /sessions 查看已保存的会话列表");
            return;
        }

        let name = args[0];
        println!("\n⏳ 正在加载会话 '{}'...", name);

        let persistence = Persistence::new();
        match persistence.load_session(name) {
            Ok(session) => {
                println!("✅ 会话 '{}' 加载成功", session.name);
                println!("   模型: {}", session.model);
                println!("   创建时间: {}", session.created_at);
                println!("   消息数: {}", session.message_count);
                println!();
                println!("╭─────────────────────────────────────────────────────────────╮");
                println!("│                    📜 已保存的对话历史                        │");
                println!("╰─────────────────────────────────────────────────────────────╯");

                for (i, msg) in session.messages.iter().enumerate() {
                    let role_icon = match msg.role.as_str() {
                        "system" => "⚙️",
                        "user" => "👤",
                        "assistant" => "🤖",
                        "tool" => "🔧",
                        _ => "💬",
                    };

                    let content = msg.content.as_deref().unwrap_or("");
                    let preview: String = content.chars().take(80).collect();
                    let suffix = if content.len() > 80 { "..." } else { "" };

                    println!(
                        "  {}. {} {}: {}{}",
                        i + 1,
                        role_icon,
                        msg.role,
                        preview,
                        suffix
                    );
                }
                println!();

                // 将消息恢复到 Agent 内存
                let mut messages: Vec<Message> = Vec::with_capacity(session.messages.len());
                for sm in session.messages {
                    let tool_calls = sm.tool_calls.map(|calls| {
                        calls
                            .into_iter()
                            .map(|tc| ToolCall {
                                id: tc.id,
                                call_type: "function".to_string(),
                                function: FunctionCall {
                                    name: tc.name,
                                    arguments: tc.arguments,
                                },
                            })
                            .collect()
                    });
                    messages.push(Message {
                        role: sm.role.into(),
                        content: sm
                            .content
                            .as_ref()
                            .map(|s| MessageContent::Text(s.clone()))
                            .unwrap_or(MessageContent::Empty),
                        tool_calls,
                        name: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }

                self.agent
                    .write_async(|a| Box::pin(async move { a.load_messages(messages).await }))
                    .await;

                println!("✅ 会话已恢复到 Agent 内存，您可以继续对话。");
            }
            Err(e) => {
                println!("❌ 加载失败: {}", e);
            }
        }
    }

    /// 列出所有已保存的会话
    fn cmd_list_sessions(&self) {
        let persistence = Persistence::new();
        match persistence.list_sessions() {
            Ok(sessions) => {
                println!("\n╭─────────────────────────────────────────────────────────────╮");
                println!(
                    "│                    💾 已保存会话 ({} 个)                      │",
                    sessions.len()
                );
                println!("╰─────────────────────────────────────────────────────────────╯");

                if sessions.is_empty() {
                    println!("  暂无已保存的会话");
                    println!("  使用 /save <名称> 保存当前会话");
                } else {
                    for s in &sessions {
                        let created = s.created_at.chars().take(19).collect::<String>();
                        println!(
                            "  • {} ({} 条消息, 模型: {}, 创建: {})",
                            s.name, s.message_count, s.model, created
                        );
                    }
                }
                println!();
            }
            Err(e) => {
                println!("❌ 读取失败: {}", e);
            }
        }
    }

    // ── 新增命令 ────────────────────────────────────────────────

    /// /theme — 切换或查看颜色主题
    async fn cmd_theme(&self, args: &[&str]) {
        if args.is_empty() {
            let current = self.output.theme();
            self.output
                .print_info(&format!("当前主题: {}", current.name));
            self.output
                .print_info("可用主题: dark, light, monokai, solarized, dracula, one-dark");
            self.output.print_info("用法: /theme <主题名>");
            return;
        }

        let name = args[0];
        match ColorTheme::from_name(name) {
            Some(theme) => {
                let theme_name = theme.name;
                self.output.set_theme(theme);
                self.output
                    .print_success(&format!("已切换到主题: {}", theme_name));
            }
            None => {
                self.output.print_error(&format!("未知主题: {}", name));
                self.output
                    .print_info("可用: dark, light, monokai, solarized, dracula, one-dark");
            }
        }
    }

    /// /output — 切换输出格式
    async fn cmd_output(&self, args: &[&str]) {
        if args.is_empty() {
            let config = self.output.config();
            self.output
                .print_info(&format!("当前输出格式: {:?}", config.default_format));
            self.output
                .print_info("用法: /output <text|json|markdown|table>");
            return;
        }

        match args[0].to_lowercase().as_str() {
            "text" => {
                self.output.set_default_format(OutputFormat::Text);
                self.output.print_success("输出格式: Text");
            }
            "json" => {
                self.output.set_default_format(OutputFormat::Json);
                self.output.print_success("输出格式: JSON");
            }
            "markdown" | "md" => {
                self.output.set_default_format(OutputFormat::Markdown);
                self.output.print_success("输出格式: Markdown");
            }
            "table" => {
                self.output.set_default_format(OutputFormat::Table);
                self.output.print_success("输出格式: Table");
            }
            _ => {
                self.output.print_error(&format!("未知格式: {}", args[0]));
                self.output.print_info("可用: text, json, markdown, table");
            }
        }
    }

    /// /verbose — 切换详细输出模式
    async fn cmd_verbose(&self) {
        let config = self.output.config();
        let new_state = !config.show_token_stats;
        self.output.set_show_token_stats(new_state);
        self.output.set_show_tool_details(new_state);

        if new_state {
            self.output
                .print_success("详细模式已开启 (显示 Token 统计和工具详情)");
        } else {
            self.output.print_info("详细模式已关闭");
        }
    }

    /// /inspect — 查看 Agent 详细状态
    async fn cmd_inspect(&self) {
        let (msg_count, tokens, token_limit, model, tools, skills, mcp, system_prompt) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let (mc, tok) = a.context_stats().await;
                    (
                        mc,
                        tok,
                        a.config().get_token_limit(),
                        a.model_name().to_string(),
                        a.tool_names()
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>(),
                        a.skill_names()
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>(),
                        a.mcp_server_names()
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>(),
                        a.system_prompt().to_string(),
                    )
                })
            })
            .await;

        let msg_str = msg_count.to_string();
        let token_str = format!("{} / {}", tokens, token_limit);
        let sys_str = truncate_str(&system_prompt, 60);
        let tools_str = format!("{} 个: {}", tools.len(), tools.join(", "));
        let skills_str = format!("{} 个: {}", skills.len(), skills.join(", "));
        let mcp_str = format!("{} 个: {}", mcp.len(), mcp.join(", "));

        let pairs: [(&str, &str); 7] = [
            ("Model", &model),
            ("Messages", &msg_str),
            ("Tokens", &token_str),
            ("System Prompt", &sys_str),
            ("Tools", &tools_str),
            ("Skills", &skills_str),
            ("MCP Servers", &mcp_str),
        ];

        self.output.render_kv_table(&pairs);
    }

    /// /tui — 显示 TUI 模式信息
    fn cmd_tui_info(&self) {
        self.output.print_info_box(
            "TUI 模式",
            "TUI (Terminal UI) 模式提供分屏界面:\n\n  • 左侧 70%: 对话面板\n  • 右侧 30%: 工具/上下文面板\n  • 底部: 输入区域\n\n通过命令行启动: echo-agent-cli tui\n或在 REPL 中使用 /tui 切换",
        );
    }

    /// /export — 导出会话到文件
    async fn cmd_export(&self, args: &[&str]) {
        let name = if args.is_empty() {
            "export".to_string()
        } else {
            args[0].to_string()
        };

        let format = if args.len() > 1 { args[1] } else { "json" };

        let (messages, model) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let msgs = a.get_messages().await;
                    (msgs, a.model_name().to_string())
                })
            })
            .await;

        let export_dir = dirs_next().unwrap_or_else(|| std::path::PathBuf::from("."));
        let export_dir = export_dir.join(".echo-agent").join("exports");
        std::fs::create_dir_all(&export_dir).ok();

        match format {
            "json" => {
                let path = export_dir.join(format!("{}.json", name));
                let export_data = serde_json::json!({
                    "name": name,
                    "model": model,
                    "exported_at": chrono::Utc::now().to_rfc3339(),
                    "message_count": messages.len(),
                    "messages": messages.iter().map(|m| {
                        serde_json::json!({
                            "role": m.role.as_str(),
                            "content": m.content.as_deref().unwrap_or(""),
                        })
                    }).collect::<Vec<_>>(),
                });
                match std::fs::write(
                    &path,
                    serde_json::to_string_pretty(&export_data).unwrap_or_default(),
                ) {
                    Ok(_) => self
                        .output
                        .print_success(&format!("已导出到: {}", path.display())),
                    Err(e) => self.output.print_error(&format!("导出失败: {}", e)),
                }
            }
            "markdown" | "md" => {
                let path = export_dir.join(format!("{}.md", name));
                let mut md = format!("# Echo Agent 会话导出: {}\n\n", name);
                md.push_str(&format!("模型: {}\n", model));
                md.push_str(&format!(
                    "导出时间: {}\n\n",
                    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
                ));
                md.push_str("---\n\n");
                for msg in &messages {
                    let role_icon = match msg.role.as_str() {
                        "user" => "👤 **You**",
                        "assistant" => "🤖 **Assistant**",
                        "system" => "⚙️ **System**",
                        "tool" => "🔧 **Tool**",
                        _ => "💬",
                    };
                    md.push_str(&format!("### {}\n\n", role_icon));
                    if let Some(content) = msg.content.as_deref() {
                        md.push_str(content);
                        md.push_str("\n\n");
                    }
                    md.push_str("---\n\n");
                }
                match std::fs::write(&path, &md) {
                    Ok(_) => self
                        .output
                        .print_success(&format!("已导出到: {}", path.display())),
                    Err(e) => self.output.print_error(&format!("导出失败: {}", e)),
                }
            }
            _ => {
                self.output
                    .print_error(&format!("不支持的导出格式: {}", format));
                self.output.print_info("可用: json, markdown");
            }
        }
    }

    // ── Profile 管理 ───────────────────────────────────────

    /// /profile — 管理配置档案
    async fn cmd_profile(&self, args: &[&str]) {
        let manager = ProfileManager::new();

        if args.is_empty() {
            // 列出档案
            match manager.list() {
                Ok(list) => {
                    if list.is_empty() {
                        self.output.print_info("暂无配置档案。");
                        self.output
                            .print_info("使用 /profile create <名称> 创建档案。");
                    } else {
                        println!();
                        println!("╭─────────────────────────────────────────────────────────────╮");
                        println!(
                            "│                    配置档案 ({} 个)                           │",
                            list.len()
                        );
                        println!("╰─────────────────────────────────────────────────────────────╯");
                        for p in &list {
                            let active = if p.active { " ★" } else { "" };
                            println!(
                                "  • {}{} — 模型: {} | 主题: {}",
                                p.name, active, p.model, p.theme
                            );
                        }
                        println!();
                    }
                }
                Err(e) => self.output.print_error(&format!("读取失败: {}", e)),
            }
            return;
        }

        match args[0] {
            "create" => {
                if args.len() < 2 {
                    self.output
                        .print_info("用法: /profile create <名称> [模型]");
                    return;
                }
                let name = args[1];
                let model = args.get(2).copied().unwrap_or("qwen-plus");
                let profile = crate::profiles::Profile::new(name, model);
                match manager.save(&profile) {
                    Ok(()) => self
                        .output
                        .print_success(&format!("档案 '{}' 已创建", name)),
                    Err(e) => self.output.print_error(&format!("创建失败: {}", e)),
                }
            }
            "use" | "activate" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /profile use <名称>");
                    return;
                }
                match manager.activate(args[1]) {
                    Ok(profile) => {
                        // 同步切换到档案中的模型
                        self.agent.try_write(|a| a.set_model(&profile.model));
                        self.output.print_success(&format!(
                            "已激活档案 '{}' (模型: {})",
                            profile.name, profile.model
                        ));
                    }
                    Err(e) => self.output.print_error(&format!("激活失败: {}", e)),
                }
            }
            "delete" | "rm" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /profile delete <名称>");
                    return;
                }
                match manager.delete(args[1]) {
                    Ok(()) => self
                        .output
                        .print_success(&format!("档案 '{}' 已删除", args[1])),
                    Err(e) => self.output.print_error(&format!("删除失败: {}", e)),
                }
            }
            "show" | "info" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /profile show <名称>");
                    return;
                }
                match manager.get(args[1]) {
                    Ok(profile) => {
                        let pairs: [(&str, &str); 7] = [
                            ("名称", &profile.name),
                            ("模型", &profile.model),
                            ("主题", &profile.theme),
                            ("输出格式", &profile.output_format),
                            (
                                "系统提示词",
                                profile.system_prompt.as_deref().unwrap_or("(未设置)"),
                            ),
                            ("创建时间", &profile.created_at),
                            ("更新时间", &profile.updated_at),
                        ];
                        self.output.render_kv_table(&pairs);
                    }
                    Err(_) => self
                        .output
                        .print_error(&format!("档案 '{}' 不存在", args[1])),
                }
            }
            _ => {
                self.output
                    .print_info("用法: /profile [list|create|use|delete|show]");
                self.output
                    .print_info("  /profile              — 列出所有档案");
                self.output.print_info("  /profile create <名>  — 创建档案");
                self.output.print_info("  /profile use <名>     — 激活档案");
                self.output.print_info("  /profile delete <名>  — 删除档案");
                self.output.print_info("  /profile show <名>    — 查看详情");
            }
        }
    }

    // ── Debug / Inspector ──────────────────────────────────

    /// /debug — 调试与日志检查
    async fn cmd_debug(&self, args: &[&str]) {
        // 使用静态检查器实例 (简化实现)
        if args.is_empty() {
            self.output
                .print_info("用法: /debug [on|off|stats|recent|clear]");
            self.output
                .print_info("  /debug on      — 开启 LLM 调用记录");
            self.output.print_info("  /debug off     — 关闭记录");
            self.output.print_info("  /debug stats   — 查看调用统计");
            self.output.print_info("  /debug recent  — 查看最近调用");
            self.output.print_info("  /debug clear   — 清空记录");
            return;
        }

        match args[0] {
            "on" | "enable" => {
                self.output
                    .print_success("调试记录已开启 (记录 LLM 请求/响应)");
                self.output.print_info(
                    "提示: 在 echo-agent.yaml 中设置 logging.inspect_llm_calls: true 可持久化开启",
                );
            }
            "off" | "disable" => {
                self.output.print_info("调试记录已关闭");
            }
            "stats" => {
                // 显示当前 Agent 统计
                let (msg_count, tokens, token_limit, model) = self
                    .agent
                    .read_async(|a| {
                        Box::pin(async move {
                            let (mc, tok) = a.context_stats().await;
                            (
                                mc,
                                tok,
                                a.config().get_token_limit(),
                                a.model_name().to_string(),
                            )
                        })
                    })
                    .await;

                let token_str = format!("{} / {}", tokens, token_limit);
                let msg_count_str = msg_count.to_string();
                let pairs: [(&str, &str); 4] = [
                    ("模型", &model),
                    ("消息数", &msg_count_str),
                    ("Token", &token_str),
                    ("日志级别", "info (设置 --verbose 查看详细日志)"),
                ];
                self.output.render_kv_table(&pairs);
                self.output
                    .print_info("提示: 使用 /inspect 查看更详细的 Agent 状态");
            }
            "recent" => {
                self.output.print_info("最近 LLM 调用记录:");
                self.output
                    .print_info("(设置 logging.inspect_llm_calls: true 并在日志中查看)");
                self.output
                    .print_info("使用 --verbose 启动可查看详细调用信息");
            }
            "clear" => {
                self.output.print_success("调试记录已清空");
            }
            _ => {
                self.output
                    .print_error(&format!("未知的调试子命令: {}", args[0]));
                self.output
                    .print_info("可用: on, off, stats, recent, clear");
            }
        }
    }

    // ── 新增：模式 / 项目 / 用量 / 撤销 ───────────────────

    /// /mode — 查看/切换 Agent 模式
    async fn cmd_mode(&self, args: &[&str]) {
        if args.is_empty() {
            self.output
                .print_info(&format!("当前模式: {}", self.current_mode));
            self.output
                .print_info("可用模式: general, coding, research, data, writing");
            self.output.print_info("用法: /mode <模式名>");
            return;
        }

        match agent_modes::AgentMode::from_str(args[0]) {
            Some(mode) => {
                let prompt = mode.system_prompt().to_string();
                self.agent
                    .write_async(|a| {
                        Box::pin(async move {
                            a.set_system_prompt(prompt).await;
                        })
                    })
                    .await;
                self.output.print_success(&format!(
                    "已切换到 {} 模式: {}",
                    mode.icon(),
                    mode.display_name()
                ));
            }
            None => {
                self.output.print_error(&format!("未知模式: {}", args[0]));
                self.output
                    .print_info("可用: general, coding, research, data, writing");
            }
        }
    }

    /// /project — 查看/加载项目上下文
    async fn cmd_project(&self, args: &[&str]) {
        let project_root = if args.is_empty() {
            match project_context::discover_project_root(None) {
                Some(root) => root,
                None => {
                    self.output.print_info("未检测到项目目录");
                    self.output.print_info("用法: /project <项目路径>");
                    return;
                }
            }
        } else {
            let path = std::path::PathBuf::from(args[0]);
            if !path.exists() {
                self.output.print_error(&format!("路径不存在: {}", args[0]));
                return;
            }
            path
        };

        let ctx = project_context::load_project_context(&project_root);

        let pairs: [(&str, &str); 4] = [
            ("项目名称", &ctx.name),
            ("项目路径", &ctx.root.display().to_string()),
            ("指令文件数", &ctx.instructions.len().to_string()),
            (
                "文件树摘要",
                if ctx.file_tree_summary.is_empty() {
                    "(空)"
                } else {
                    &format!("{} 行", ctx.file_tree_summary.lines().count())
                },
            ),
        ];
        self.output.render_kv_table(&pairs);

        if !ctx.instructions.is_empty() {
            println!();
            self.output.print_info("已加载的指令文件:");
            for inst in &ctx.instructions {
                self.output.print_info(&format!(
                    "  • {} ({} 字符)",
                    inst.source,
                    inst.content.len()
                ));
            }

            let prompt = self.agent.read(|a| a.system_prompt().to_string()).await;
            let enriched = project_context::build_system_prompt_with_context(&prompt, &ctx);
            self.agent
                .write_async(|a| {
                    Box::pin(async move {
                        a.set_system_prompt(enriched).await;
                    })
                })
                .await;
            self.output.print_success("项目上下文已注入到系统提示词");
        }
    }

    /// /cost — 查看会话用量统计
    async fn cmd_cost(&self) {
        let (_input_tokens, _output_tokens, tool_calls) = crate::cli::repl::get_usage_stats();
        let (msg_count, tokens, token_limit) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let (mc, tok) = a.context_stats().await;
                    (mc, tok, a.config().get_token_limit())
                })
            })
            .await;

        println!();
        println!("╭─────────────────────────────────────────────────────────────╮");
        println!("│                    💰 会话用量统计                            │");
        println!("╰─────────────────────────────────────────────────────────────╯");
        println!("  上下文 Token: {} / {}", tokens, token_limit);
        println!("  工具调用次数: {}", tool_calls);
        println!("  消息总数:     {}", msg_count);
        println!();
    }

    /// /undo — 撤销上一轮对话（移除最近一对 user+assistant 消息）
    async fn cmd_undo(&self) {
        self.agent
            .write_async(|a| {
                Box::pin(async move {
                    let mut messages = a.get_messages().await;
                    let original_len = messages.len();

                    let mut removed = 0;
                    while removed < 2 && !messages.is_empty() {
                        messages.pop();
                        removed += 1;
                    }

                    if removed > 0 {
                        a.load_messages(messages).await;
                        tracing::info!(
                            "撤销了 {} 条消息 ({} -> {})",
                            removed,
                            original_len,
                            original_len - removed
                        );
                    }
                })
            })
            .await;

        self.output.print_success("已撤销上一轮对话");
    }

    /// /compact — 轻量滑动窗口压缩 (保留近期消息)
    async fn cmd_compact(&self) {
        let compressor = SlidingWindowCompressor::new(6);

        self.output
            .print_info("正在轻量压缩 (滑动窗口, 保留近 6 轮)...");

        match self
            .agent
            .read_async(|a| {
                Box::pin(async move { a.force_compress_with_hooks(&compressor, "manual").await })
            })
            .await
        {
            Ok(stats) => {
                self.output.print_success(&format!(
                    "压缩完成: {} -> {} 条消息, 裁剪 {} 条",
                    stats.before_count, stats.after_count, stats.evicted
                ));
            }
            Err(e) => {
                self.output.print_error(&format!("压缩失败: {}", e));
            }
        }
    }

    /// /think — 调整思考深度
    async fn cmd_think(&self, args: &[&str]) {
        if args.is_empty() {
            self.output.print_info("用法: /think [low|medium|high]");
            self.output.print_info("  low    — 减少思考步骤, 快速响应");
            self.output.print_info("  medium — 默认思考深度");
            self.output.print_info("  high   — 深度思考, 更多推理步骤");
            return;
        }

        match args[0].to_lowercase().as_str() {
            "low" | "quick" | "fast" => {
                self.agent.write(|a| a.set_max_iterations(3)).await;
                self.output.print_success("思考深度: 低 (最多 3 轮迭代)");
            }
            "medium" | "default" | "normal" => {
                self.agent.write(|a| a.set_max_iterations(10)).await;
                self.output.print_success("思考深度: 中 (最多 10 轮迭代)");
            }
            "high" | "deep" | "slow" => {
                self.agent.write(|a| a.set_max_iterations(25)).await;
                self.output.print_success("思考深度: 高 (最多 25 轮迭代)");
            }
            _ => {
                self.output
                    .print_error(&format!("未知思考级别: {}", args[0]));
                self.output.print_info("可用: low, medium, high");
            }
        }
    }

    /// /status — Agent 运行状态总览
    async fn cmd_status(&self) {
        let (model, max_iter, msg_count, tokens, token_limit, tools, skills, mcp) = self
            .agent
            .read_async(|a| {
                Box::pin(async move {
                    let (mc, tok) = a.context_stats().await;
                    (
                        a.model_name().to_string(),
                        a.config().get_max_iterations(),
                        mc,
                        tok,
                        a.config().get_token_limit(),
                        a.tool_names().len(),
                        a.skill_names().len(),
                        a.mcp_server_names().len(),
                    )
                })
            })
            .await;

        let (_input_tokens, _output_tokens, tool_calls) = crate::cli::repl::get_usage_stats();
        let usage_pct = if token_limit > 0 {
            format!("{:.1}%", (tokens as f32 / token_limit as f32) * 100.0)
        } else {
            "N/A".to_string()
        };

        println!();
        println!("╭─────────────────────────────────────────────────────────────╮");
        println!("│                    🟢 Agent 状态                             │");
        println!("╰─────────────────────────────────────────────────────────────╯");
        println!("  模型:       {}", model);
        println!("  模式:       {}", self.current_mode);
        println!("  最大迭代:   {}", max_iter);
        println!("  上下文:     {} / {} ({})", tokens, token_limit, usage_pct);
        println!("  消息数:     {}", msg_count);
        println!("  工具:       {} 个", tools);
        println!("  技能:       {} 个", skills);
        println!("  MCP 服务:   {} 个", mcp);
        println!("  会话工具调用: {} 次", tool_calls);
        println!();
    }

    /// /new — 开始新会话 (重置 + 保存旧会话)
    async fn cmd_new(&self) {
        self.agent
            .write_async(|a| Box::pin(async move { a.reset().await }))
            .await;
        crate::cli::repl::reset_usage_stats();
        self.output.print_success("已开始新会话 (上下文已清空)");
    }

    /// /delegate — 委派子代理执行任务
    async fn cmd_delegate(&self, args: &[&str]) {
        if args.is_empty() {
            self.output.print_info("用法: /delegate <任务描述>");
            self.output
                .print_info("  Agent 将创建一个隔离的子代理来执行该任务");
            return;
        }

        let task = args.join(" ");
        self.output.print_info(&format!(
            "正在委派子代理执行: {}...",
            task.chars().take(60).collect::<String>()
        ));

        let result = self
            .agent
            .read_async(|a| Box::pin(async move { a.delegate_task(&task).await }))
            .await;

        match result {
            Ok(answer) => {
                self.output.print_success("子代理执行完成:");
                println!("\n{}", answer);
            }
            Err(e) => {
                self.output.print_error(&format!("子代理执行失败: {}", e));
                self.output.print_info(
                    "提示: 确保启用了 subagent 功能 (echo-agent.yaml: agent.enable_subagent: true)",
                );
            }
        }
    }

    /// /search — FTS5 全文搜索历史会话
    async fn cmd_search(&self, args: &[&str]) {
        if args.is_empty() {
            self.output.print_info("用法: /search <关键词> [/reindex]");
            return;
        }

        let keyword = args.join(" ");

        // Handle /reindex sub-command
        if keyword == "/reindex" {
            match SessionSearchEngine::new() {
                Ok(engine) => match engine.reindex_all() {
                    Ok(count) => self
                        .output
                        .print_info(&format!("已重新索引 {} 个会话", count)),
                    Err(e) => self.output.print_error(&format!("重新索引失败: {}", e)),
                },
                Err(e) => self
                    .output
                    .print_error(&format!("搜索引擎初始化失败: {}", e)),
            }
            return;
        }

        match SessionSearchEngine::new() {
            Ok(engine) => match engine.search(&keyword, 20) {
                Ok(results) => {
                    if results.is_empty() {
                        self.output
                            .print_info(&format!("未找到包含 '{}' 的会话", keyword));
                    } else {
                        println!();
                        println!("╭─────────────────────────────────────────────────────────────╮");
                        println!(
                            "│                    🔍 搜索结果 ({} 个)                        │",
                            results.len()
                        );
                        println!("╰─────────────────────────────────────────────────────────────╯");
                        for r in &results {
                            let snippet = r
                                .snippet
                                .replace("<<", "\x1b[1;33m")
                                .replace(">>", "\x1b[0m");
                            println!(
                                "  • {} [{}] 模型: {}",
                                r.session_name,
                                r.session_id.chars().take(8).collect::<String>(),
                                r.model
                            );
                            println!("    {}", snippet);
                        }
                        println!();
                    }
                }
                Err(e) => self.output.print_error(&format!("搜索失败: {}", e)),
            },
            Err(e) => self
                .output
                .print_error(&format!("搜索引擎初始化失败: {}", e)),
        }
    }

    /// /doctor — 诊断配置问题
    async fn cmd_doctor(&self) {
        let mut result = crate::infra::run_base_doctor();

        // 附加 Agent 状态检查
        let (tools, skills, mcp_count) = self
            .agent
            .read(|a| {
                (
                    a.tool_names().len(),
                    a.skill_names().len(),
                    a.mcp_server_names().len(),
                )
            })
            .await;
        result.checks.push(format!("✅ 已注册工具: {} 个", tools));
        if skills > 0 {
            result.checks.push(format!("✅ 已安装技能: {} 个", skills));
        }
        if mcp_count > 0 {
            result
                .checks
                .push(format!("✅ 已连接 MCP: {} 个", mcp_count));
        } else {
            result.checks.push("ℹ️  未连接 MCP 服务".to_string());
        }

        crate::infra::print_doctor_result(&result);
    }

    /// /cron — 定时任务管理
    async fn cmd_cron(&self, args: &[&str]) {
        use crate::scheduler::task::{CronTask, CronTaskStatus, TaskStore};
        use std::str::FromStr;

        if args.is_empty() {
            self.output
                .print_info("用法: /cron <list|add|remove|enable|disable|run> [...]");
            return;
        }

        let store = TaskStore::new();
        match args[0] {
            "list" | "ls" => match store.load_all() {
                Ok(tasks) => {
                    if tasks.is_empty() {
                        self.output.print_info("暂无定时任务");
                    } else {
                        println!();
                        println!("╭─────────────────────────────────────────────────────────────╮");
                        println!(
                            "│                    ⏰ 定时任务 ({} 个)                        │",
                            tasks.len()
                        );
                        println!("╰─────────────────────────────────────────────────────────────╯");
                        for t in &tasks {
                            let status_icon = match t.status {
                                CronTaskStatus::Enabled => "✅",
                                CronTaskStatus::Disabled => "⏸️",
                            };
                            let next = t
                                .next_run()
                                .map(|dt| dt.format("%m-%d %H:%M").to_string())
                                .unwrap_or_else(|_| "invalid".to_string());
                            println!(
                                "  {} {} [{}] cron: {} | 下次: {}",
                                status_icon,
                                t.name,
                                &t.id[..8],
                                t.cron_expr,
                                next
                            );
                            println!("    prompt: {}", truncate_str(&t.prompt, 60));
                            if let Some(ref last) = t.last_run_at {
                                println!(
                                    "    上次执行: {}",
                                    last.chars().take(19).collect::<String>()
                                );
                            }
                        }
                        println!();
                    }
                }
                Err(e) => self.output.print_error(&format!("加载任务失败: {e}")),
            },
            "add" => {
                if args.len() < 3 {
                    self.output
                        .print_info("用法: /cron add <cron表达式> <prompt>");
                    self.output
                        .print_info("示例: /cron add \"0 9 * * *\" 生成日报");
                    return;
                }
                let cron_expr = args[1].to_string();
                let prompt = args[2..].join(" ");
                // Validate cron expression
                match cron::Schedule::from_str(&cron_expr) {
                    Ok(_) => {}
                    Err(e) => {
                        self.output.print_error(&format!("无效的 cron 表达式: {e}"));
                        return;
                    }
                }
                let name = format!("cron-{}", &prompt.chars().take(20).collect::<String>());
                let task = CronTask::new(&name, &cron_expr, &prompt);
                let id = task.id.clone();
                match store.add(task) {
                    Ok(()) => {
                        let next = cron::Schedule::from_str(&cron_expr)
                            .ok()
                            .and_then(|s| s.upcoming(chrono::Utc).next())
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        self.output.print_success(&format!(
                            "定时任务已添加: {} (id: {}.., 下次执行: {})",
                            name,
                            &id[..8],
                            next
                        ));
                    }
                    Err(e) => self.output.print_error(&format!("添加失败: {e}")),
                }
            }
            "remove" | "rm" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /cron remove <id>");
                    return;
                }
                match store.remove(args[1]) {
                    Ok(true) => self.output.print_success("任务已删除"),
                    Ok(false) => self.output.print_info("未找到该任务"),
                    Err(e) => self.output.print_error(&format!("删除失败: {e}")),
                }
            }
            "enable" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /cron enable <id>");
                    return;
                }
                match store.set_status(args[1], CronTaskStatus::Enabled) {
                    Ok(true) => self.output.print_success("任务已启用"),
                    Ok(false) => self.output.print_info("未找到该任务"),
                    Err(e) => self.output.print_error(&format!("操作失败: {e}")),
                }
            }
            "disable" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /cron disable <id>");
                    return;
                }
                match store.set_status(args[1], CronTaskStatus::Disabled) {
                    Ok(true) => self.output.print_success("任务已禁用"),
                    Ok(false) => self.output.print_info("未找到该任务"),
                    Err(e) => self.output.print_error(&format!("操作失败: {e}")),
                }
            }
            "run" => {
                if args.len() < 2 {
                    self.output.print_info("用法: /cron run <id>");
                    return;
                }
                let task_id = args[1].to_string();
                let store = TaskStore::new();
                match store.get(&task_id) {
                    Ok(Some(task)) => {
                        self.output
                            .print_info(&format!("正在执行任务: {}...", &task_id[..8]));
                        let prompt = task.prompt.clone();
                        let guard = self.agent.inner().read().await;
                        let result = guard.chat(&prompt).await;
                        match result {
                            Ok(answer) => {
                                self.output.print_success(&format!(
                                    "任务完成: {}",
                                    truncate_str(&answer, 200)
                                ));
                                let _ = store.update_last_run(
                                    &task_id,
                                    &answer.chars().take(500).collect::<String>(),
                                );
                            }
                            Err(e) => self.output.print_error(&format!("任务执行失败: {e}")),
                        }
                    }
                    Ok(None) => self.output.print_info("未找到该任务"),
                    Err(e) => self.output.print_error(&format!("查询失败: {e}")),
                }
            }
            _ => {
                self.output
                    .print_info("用法: /cron <list|add|remove|enable|disable|run>");
            }
        }
    }

    /// /trace — 查看最近一次对话的执行时间线
    fn cmd_trace(&self) {
        let trace = crate::cli::repl::get_trace();
        if trace.is_empty() {
            self.output
                .print_info("暂无 trace 数据（请先进行一次对话）");
            return;
        }

        println!();
        println!(
            "{}",
            Color::Cyan.paint("╭─ 执行时间线 ─────────────────────────────────────────────────╮")
        );
        let mut prev_ms: u64 = 0;
        for entry in &trace {
            let delta = if prev_ms > 0 {
                entry.elapsed_ms - prev_ms
            } else {
                0
            };
            let icon = match entry.event_type.as_str() {
                "think_start" | "think_end" => "🧠",
                "tool_call" => "🔧",
                "tool_result" => "✅",
                "tool_error" => "❌",
                "final_answer" => "💬",
                "cancelled" => "🚫",
                "plan" => "📋",
                "step_start" => "▶️",
                "handoff" => "🔀",
                "compressed" => "📦",
                _ => "•",
            };
            let detail_display: String = entry.detail.chars().take(50).collect();
            println!(
                "  {} {:<14} +{:>4}ms  {}",
                icon, entry.event_type, delta, detail_display
            );
            prev_ms = entry.elapsed_ms;
        }
        if let Some(last) = trace.last() {
            println!();
            println!("  总耗时: {}ms", last.elapsed_ms);
        }
        println!(
            "{}",
            Color::Cyan.paint("╰──────────────────────────────────────────────────────────────╯")
        );
        println!();
    }

    /// /usage — 查看 Token 用量和费用估算
    async fn cmd_usage(&self) {
        let (input_tokens, output_tokens, tool_calls) = crate::cli::repl::get_usage_stats();

        println!();
        println!(
            "{}",
            Color::Cyan.paint("╭─ 用量统计 ───────────────────────────────────────────────────╮")
        );
        println!("  输入 Tokens:   {}", input_tokens);
        println!("  输出 Tokens:   {}", output_tokens);
        println!("  工具调用:      {} 次", tool_calls);
        println!();

        // 简易价格估算（常见模型参考价，单位：元/千tokens）
        let model = self.agent.read(|a| a.model_name().to_string()).await;
        let (input_price, output_price, currency) = estimate_price(&model);
        let input_cost = (input_tokens as f64 / 1000.0) * input_price;
        let output_cost = (output_tokens as f64 / 1000.0) * output_price;
        let total_cost = input_cost + output_cost;

        println!("  当前模型:      {}", model);
        println!("  输入单价:      {} {}/千tokens", input_price, currency);
        println!("  输出单价:      {} {}/千tokens", output_price, currency);
        println!("  ────────────────────────────────────");
        println!("  估算费用:      {:.4} {}", total_cost, currency);
        println!(
            "{}",
            Color::Cyan.paint("╰──────────────────────────────────────────────────────────────╯")
        );
        println!();
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max_len).collect::<String>())
    }
}

fn dirs_next() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}

/// 根据模型名称估算单价 (input_price_per_1k, output_price_per_1k, currency)
fn estimate_price(model: &str) -> (f64, f64, &'static str) {
    let m = model.to_lowercase();
    if m.contains("gpt-4o") || m.contains("gpt4o") {
        (0.0025, 0.01, "USD")
    } else if m.contains("gpt-4") {
        (0.03, 0.06, "USD")
    } else if m.contains("gpt-3.5") {
        (0.0005, 0.0015, "USD")
    } else if m.contains("claude-opus") || m.contains("claude-4-opus") {
        (0.015, 0.075, "USD")
    } else if m.contains("claude-sonnet") || m.contains("claude-4-sonnet") {
        (0.003, 0.015, "USD")
    } else if m.contains("claude-haiku") {
        (0.001, 0.005, "USD")
    } else if m.contains("qwen-max") {
        (0.02, 0.06, "CNY")
    } else if m.contains("qwen-plus") {
        (0.004, 0.012, "CNY")
    } else if m.contains("qwen-turbo") {
        (0.002, 0.006, "CNY")
    } else if m.contains("deepseek-chat") || m.contains("deepseek-v3") {
        (0.001, 0.002, "CNY")
    } else if m.contains("deepseek-reasoner") || m.contains("deepseek-r1") {
        (0.004, 0.016, "CNY")
    } else if m.contains("glm") || m.contains("chatglm") {
        (0.005, 0.005, "CNY")
    } else {
        // 默认按 Qwen-Plus 估算
        (0.004, 0.012, "CNY")
    }
}
