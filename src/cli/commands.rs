//! 命令处理模块
//!
//! 定义所有支持的斜杠命令。

use std::sync::Arc;

use echo_agent::prelude::*;

use crate::persistence::Persistence;

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
    agent: Arc<tokio::sync::Mutex<ReactAgent>>,
}

impl CommandHandler {
    pub fn new(agent: Arc<tokio::sync::Mutex<ReactAgent>>) -> Self {
        Self { agent }
    }

    /// 处理用户输入
    pub async fn handle(&self, input: &str) -> CommandResult {
        let input = input.trim();

        // 空输入
        if input.is_empty() {
            return CommandResult::Continue;
        }

        // 斜杠命令
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
                self.cmd_skills().await;
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
        println!("    /reset, /r      重置对话历史");
        println!("    /clear, /cls    清屏");
        println!("    /exit, /q       退出程序");
        println!();
        println!("  信息查询:");
        println!("    /tools, /t      列出已注册工具");
        println!("    /skills, /sk    列出已安装技能");
        println!("    /mcp, /m        列出 MCP 服务连接");
        println!("    /history, /hist 查看对话历史");
        println!("    /stats, /st     查看上下文统计");
        println!();
        println!("  配置命令:");
        println!("    /model <名称>   切换模型");
        println!("    /system <提示词> 设置系统提示词");
        println!("    /compress, /cp  手动触发上下文压缩");
        println!();
        println!("  会话管理:");
        println!("    /save [名称]    保存当前会话 (默认: default)");
        println!("    /load <名称>    加载已保存的会话");
        println!("    /sessions, /ss  列出所有已保存的会话");
        println!();
        println!("  帮助:");
        println!("    /help, /h, /?   显示此帮助信息");
        println!();
    }

    /// 重置对话
    async fn cmd_reset(&self) {
        let mut agent = self.agent.lock().await;
        agent.reset();
        println!("\n✅ 对话已重置");
    }

    /// 清屏
    fn cmd_clear(&self) {
        print!("\x1B[2J\x1B[1;1H");
    }

    /// 列出工具
    async fn cmd_tools(&self) {
        let agent = self.agent.lock().await;
        let tools = agent.tool_names();

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!("│                    🔧 已注册工具 ({} 个)                       │", tools.len());
        println!("╰─────────────────────────────────────────────────────────────╯");

        for name in &tools {
            if let Some(def) = agent.tool_definitions().iter().find(|d| &d.function.name == name) {
                println!("  • {} - {}", name, def.function.description.chars().take(50).collect::<String>());
            } else {
                println!("  • {}", name);
            }
        }
        println!();
    }

    /// 列出技能
    async fn cmd_skills(&self) {
        let agent = self.agent.lock().await;
        let skills = agent.skill_names();

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!("│                    🎯 已安装技能 ({} 个)                       │", skills.len());
        println!("╰─────────────────────────────────────────────────────────────╯");

        if skills.is_empty() {
            println!("  暂无已安装的技能");
        } else {
            for name in &skills {
                println!("  • {}", name);
            }
        }
        println!();
    }

    /// 列出 MCP 服务
    async fn cmd_mcp(&self) {
        let agent = self.agent.lock().await;
        let servers = agent.mcp_server_names();

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!("│                    🔌 MCP 服务 ({} 个)                        │", servers.len());
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
        let agent = self.agent.lock().await;
        let messages = agent.get_messages();

        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!("│                    📜 对话历史 ({} 条)                        │", messages.len());
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

            println!("  {}. {} {}: {}{}", i + 1, role_icon, msg.role, preview, suffix);

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
        let mut agent = self.agent.lock().await;
        let compressor = SlidingWindowCompressor::new(10);

        println!("\n⏳ 正在压缩上下文...");

        match agent.force_compress_with(&compressor).await {
            Ok(stats) => {
                println!("\n✅ 压缩完成:");
                println!("   消息: {} -> {}", stats.before_count, stats.after_count);
                println!("   Token: {} -> {}", stats.before_tokens, stats.after_tokens);
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
        let agent = self.agent.lock().await;
        let (msg_count, tokens) = agent.context_stats();
        let token_limit = agent.config().get_token_limit();

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
            let agent = self.agent.lock().await;
            println!("\n当前模型: {}", agent.model_name());
            println!("用法: /model <模型名称>");
            println!("示例: /model qwen-max");
            return;
        }

        let model = args.join(" ");
        let mut agent = self.agent.lock().await;
        agent.set_model(&model);
        println!("\n✅ 已切换到模型: {}", model);
    }

    /// 设置系统提示词
    async fn cmd_system(&self, args: &[&str]) {
        if args.is_empty() {
            let agent = self.agent.lock().await;
            println!("\n当前系统提示词:");
            println!("{}\n", agent.system_prompt());
            println!("用法: /system <新的提示词>");
            return;
        }

        let prompt = args.join(" ");
        let mut agent = self.agent.lock().await;
        agent.set_system_prompt(prompt.clone());
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

        let agent = self.agent.lock().await;
        let messages: Vec<_> = agent.get_messages().to_vec();
        let model = agent.model_name().to_string();
        let system_prompt = agent.system_prompt().to_string();
        drop(agent);

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

                    println!("  {}. {} {}: {}{}", i + 1, role_icon, msg.role, preview, suffix);
                }
                println!();
                println!("💡 提示: 对话历史已展示。由于 Agent API 限制，无法直接恢复到 Agent 内存中。");
                println!("         建议使用 /save 在重要节点保存，以便将来回顾。");
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
                println!("│                    💾 已保存会话 ({} 个)                      │", sessions.len());
                println!("╰─────────────────────────────────────────────────────────────╯");

                if sessions.is_empty() {
                    println!("  暂无已保存的会话");
                    println!("  使用 /save <名称> 保存当前会话");
                } else {
                    for s in &sessions {
                        let created = s.created_at.chars().take(19).collect::<String>();
                        println!("  • {} ({} 条消息, 模型: {}, 创建: {})",
                            s.name, s.message_count, s.model, created);
                    }
                }
                println!();
            }
            Err(e) => {
                println!("❌ 读取失败: {}", e);
            }
        }
    }
}
