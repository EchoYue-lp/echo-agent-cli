//! 子命令处理函数
//!
//! 处理 Run、Profiles、Sessions、Completions 子命令。

use anyhow::Result;
use std::io::Read;
use futures::StreamExt;

use crate::cli::args::{Commands, ProfileAction, SessionAction};
use crate::config;
use crate::{profiles, sessions, shell};
use echo_agent::prelude::*;

/// 处理子命令分发
pub async fn handle_subcommand(cmd: &Commands) -> Result<()> {
    match cmd {
        Commands::Run {
            message,
            pipe,
            model,
            output,
        } => {
            let app_config = config::load_config(None);
            let model = model.as_deref().unwrap_or(&app_config.model.name);
            handle_run_command(message, *pipe, model, output).await?;
        }
        Commands::Profiles { action } => {
            handle_profile_action(action).await?;
        }
        Commands::Sessions { action } => {
            handle_session_action(action).await?;
        }
        Commands::Completions { shell, all } => {
            handle_completions_command(shell, *all)?;
        }
        Commands::Tui => {
            unreachable!("Tui subcommand should be handled before handle_subcommand");
        }
        Commands::Onboard => {
            super::onboard::run_onboard()?;
        }
        Commands::Doctor => {
            handle_doctor_command()?;
        }
    }
    Ok(())
}

/// 处理一次性对话 (run 子命令)
pub async fn handle_run_command(
    message: &[String],
    pipe: bool,
    model: &str,
    output: &str,
) -> Result<()> {
    let input = if pipe {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim().to_string()
    } else if message.is_empty() {
        eprintln!("错误: 请提供消息，或使用 --pipe 从 stdin 读取");
        return Ok(());
    } else {
        message.join(" ")
    };

    if input.is_empty() {
        return Ok(());
    }

    let agent =
        ReactAgent::new(AgentConfig::standard(model, "echo-agent", "你是一个智能助手"));

    match output {
        "json" => {
            match agent.chat(&input).await {
                Ok(response) => {
                    let json = serde_json::json!({
                        "model": model,
                        "input": input,
                        "response": response,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                Err(e) => {
                    let json = serde_json::json!({
                        "error": e.to_string(),
                    });
                    eprintln!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        _ => {
            match agent.chat_stream(&input).await {
                Ok(mut stream) => {
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(AgentEvent::Token(token)) => print!("{}", token),
                            Ok(AgentEvent::ToolCall { name, .. }) => {
                                eprintln!("\n🔧 调用工具: {}", name);
                            }
                            Ok(AgentEvent::ToolResult { name, output: tool_output, .. }) => {
                                eprintln!("\n✓ {}: {}", name, tool_output.chars().take(100).collect::<String>());
                            }
                            Ok(AgentEvent::FinalAnswer(_)) => {}
                            Ok(AgentEvent::Cancelled) => {
                                eprintln!("\n⚠ 执行已取消");
                            }
                            Err(e) => {
                                eprintln!("\n错误: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                    println!();
                }
                Err(e) => eprintln!("错误: {}", e),
            }
        }
    }

    Ok(())
}

/// 处理档案管理子命令
pub async fn handle_profile_action(action: &ProfileAction) -> Result<()> {
    let manager = profiles::ProfileManager::new();

    match action {
        ProfileAction::List => {
            let list = manager.list()?;
            if list.is_empty() {
                println!("暂无配置档案。使用 echo-agent-cli profiles create <名称> 创建。");
            } else {
                println!("{:<20} {:<20} {:<10} {:<8} 更新时间", "名称", "模型", "主题", "激活");
                println!("{}", "─".repeat(80));
                for p in &list {
                    let active = if p.active { "★" } else { "" };
                    let updated: String = p.updated_at.chars().take(19).collect();
                    println!(
                        "{:<20} {:<20} {:<10} {:<8} {}",
                        p.name, p.model, p.theme, active, updated
                    );
                }
            }
        }
        ProfileAction::Show { name } => match manager.get(name) {
            Ok(profile) => {
                println!("档案: {}", profile.name);
                println!("  模型:       {}", profile.model);
                println!("  主题:       {}", profile.theme);
                println!("  输出格式:   {}", profile.output_format);
                println!("  最大迭代:   {}", profile.max_iterations);
                if let Some(ref sp) = profile.system_prompt {
                    println!("  系统提示词: {}", sp);
                }
                println!("  创建时间:   {}", profile.created_at);
                println!("  更新时间:   {}", profile.updated_at);
            }
            Err(_) => println!("档案 '{}' 不存在", name),
        },
        ProfileAction::Create {
            name,
            model,
            system_prompt,
        } => {
            let model = model.as_deref().unwrap_or("qwen-plus");
            let mut profile = profiles::Profile::new(name, model);
            if let Some(sp) = system_prompt {
                profile.system_prompt = Some(sp.clone());
            }
            manager.save(&profile)?;
            println!("档案 '{}' 已创建", name);
        }
        ProfileAction::Update {
            name,
            model,
            system_prompt,
            theme,
        } => {
            if let Ok(mut profile) = manager.get(name) {
                if let Some(m) = model {
                    profile.model = m.clone();
                }
                if let Some(sp) = system_prompt {
                    profile.system_prompt = Some(sp.clone());
                }
                if let Some(t) = theme {
                    profile.theme = t.clone();
                }
                manager.save(&profile)?;
                println!("档案 '{}' 已更新", name);
            } else {
                println!("档案 '{}' 不存在", name);
            }
        }
        ProfileAction::Use { name } => match manager.activate(name) {
            Ok(profile) => println!("已激活档案 '{}' (模型: {})", profile.name, profile.model),
            Err(_) => println!("档案 '{}' 不存在", name),
        },
        ProfileAction::Delete { name } => {
            manager.delete(name)?;
            println!("档案 '{}' 已删除", name);
        }
    }
    Ok(())
}

/// 处理会话管理子命令
pub async fn handle_session_action(action: &SessionAction) -> Result<()> {
    let mut manager = sessions::SessionManager::new();

    match action {
        SessionAction::List => {
            let list = manager.list()?;
            if list.is_empty() {
                println!("暂无会话记录。");
            } else {
                println!("{:<36} {:<24} {:<12} {:<8} 更新时间", "ID", "名称", "模型", "消息");
                println!("{}", "─".repeat(100));
                for s in &list {
                    let updated: String = s.updated_at.chars().take(19).collect();
                    let name = truncate_str_max(&s.name, 22);
                    println!(
                        "{:<36} {:<24} {:<12} {:<8} {}",
                        s.id, name, s.model, s.message_count, updated
                    );
                }
            }
        }
        SessionAction::Show { id } => match manager.load(id) {
            Ok(session) => {
                println!("会话: {}", session.name);
                println!("  ID:        {}", session.id);
                println!("  模型:      {}", session.model);
                println!("  消息数:    {}", session.message_count);
                if let Some(ref branch) = session.branch {
                    println!("  分支:      {}", branch);
                }
                if let Some(ref parent) = session.parent_id {
                    println!("  父会话:    {}", parent);
                }
                println!("  创建时间:  {}", session.created_at);
                println!("  更新时间:  {}", session.updated_at);
                println!("\n─ 消息 ─");
                for (i, msg) in session.messages.iter().enumerate() {
                    let role_icon = match msg.role.as_str() {
                        "user" => "👤",
                        "assistant" => "🤖",
                        "system" => "⚙️",
                        "tool" => "🔧",
                        _ => "💬",
                    };
                    let preview: String = msg
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect();
                    println!("  {}. {} {}: {}", i + 1, role_icon, msg.role, preview);
                }
            }
            Err(_) => println!("会话 '{}' 不存在", id),
        },
        SessionAction::Branch {
            parent_id,
            branch_name,
        } => match manager.branch(parent_id, branch_name) {
            Ok(branch) => println!("分支 '{}' 已创建 (ID: {})", branch_name, branch.id),
            Err(e) => println!("创建分支失败: {}", e),
        },
        SessionAction::Diff { id_a, id_b } => match manager.diff(id_a, id_b) {
            Ok(diff) => {
                println!("差异: +{} 行, -{} 行", diff.added, diff.removed);
                for hunk in &diff.hunks {
                    for line in &hunk.lines {
                        match line {
                            sessions::DiffLine::Context(s) => println!("  {}", s),
                            sessions::DiffLine::Added(s) => println!("+ {}", s),
                            sessions::DiffLine::Removed(s) => println!("- {}", s),
                        }
                    }
                }
            }
            Err(e) => println!("对比失败: {}", e),
        },
        SessionAction::Export { id, format, output } => match manager.load(id) {
            Ok(session) => {
                let ext = match format.as_str() {
                    "json" => "json",
                    "markdown" | "md" => "md",
                    "html" => "html",
                    _ => {
                        println!("不支持的格式: {}", format);
                        return Ok(());
                    }
                };
                let path = output
                    .clone()
                    .unwrap_or_else(|| format!("session-{}.{}", &session.id[..8], ext));
                let path = std::path::Path::new(&path);
                match format.as_str() {
                    "json" => manager.export_json(id, path)?,
                    "markdown" | "md" => manager.export_markdown(id, path)?,
                    "html" => manager.export_html(id, path)?,
                    _ => unreachable!(),
                }
                println!("已导出到: {}", path.display());
            }
            Err(e) => println!("导出失败: {}", e),
        },
        SessionAction::Delete { id } => {
            manager.delete(id)?;
            println!("会话 '{}' 已删除", id);
        }
    }
    Ok(())
}

/// 处理补全生成子命令
pub fn handle_completions_command(shell_type: &str, all: bool) -> Result<()> {
    if all {
        let dir = dirs_home_path().join(".echo-agent").join("completions");
        shell::generate_all(&dir)?;
        println!("所有 Shell 补全脚本已生成到: {}", dir.display());
        return Ok(());
    }

    match shell::ShellType::from_str(shell_type) {
        Some(st) => {
            let script = shell::generate_completion(st);
            println!("{}", script);
            eprintln!();
            shell::print_install_hint(st);
        }
        None => {
            eprintln!("未知 Shell 类型: {}", shell_type);
            eprintln!("可用: bash, zsh, fish, elvish, powershell");
        }
    }
    Ok(())
}

/// 处理 Doctor 诊断子命令
pub fn handle_doctor_command() -> Result<()> {
    let result = crate::infra::run_base_doctor();
    crate::infra::print_doctor_result(&result);
    Ok(())
}

/// 获取用户 home 目录路径
fn dirs_home_path() -> std::path::PathBuf {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// 截断字符串到指定字符数
fn truncate_str_max(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max - 3).collect::<String>())
    }
}
