//! REPL (Read-Eval-Print-Loop) 交互实现
//!
//! 提供交互式命令行界面，支持：
//! - 多行输入
//! - 历史记录
//! - 自动补全
//! - 流式输出显示

use std::sync::Arc;

use futures::StreamExt;
use nu_ansi_term::Color;
use reedline::{
    default_emacs_keybindings, ColumnarMenu, DefaultCompleter, Emacs,
    FileBackedHistory, Keybindings, MenuBuilder, Prompt, PromptHistorySearchStatus,
    Reedline, ReedlineEvent, ReedlineMenu, Signal,
};

use echo_agent::prelude::*;

use super::commands::{CommandHandler, CommandResult};

/// REPL 配置
pub struct ReplConfig {
    /// 提示符
    pub prompt: String,
    /// 历史文件路径
    pub history_file: String,
    /// 是否启用自动补全
    pub enable_completion: bool,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: "~/.echo-agent/history.txt".to_string(),
            enable_completion: true,
        }
    }
}

/// 运行 REPL
pub async fn run_repl(agent: Arc<tokio::sync::Mutex<ReactAgent>>, config: ReplConfig) -> anyhow::Result<()> {
    // 打印欢迎信息
    print_welcome();

    // 创建命令处理器
    let cmd_handler = CommandHandler::new(agent.clone());

    // 创建 Reedline 编辑器
    let mut line_editor = create_line_editor(&config)?;

    // 自定义提示符
    let prompt = EchoPrompt::new(&config.prompt);

    // 主循环
    loop {
        let signal = line_editor.read_line(&prompt);

        match signal {
            Ok(Signal::Success(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // 处理命令或对话
                match cmd_handler.handle(line).await {
                    CommandResult::Continue => {}
                    CommandResult::Exit => break,
                    CommandResult::Chat(message) => {
                        // 执行对话
                        chat_with_agent(&agent, &message).await;
                    }
                }
            }
            Ok(Signal::CtrlC) => {
                // Ctrl+C: 取消当前输入
                println!("\n（输入 /exit 退出）");
            }
            Ok(Signal::CtrlD) => {
                // Ctrl+D: 退出
                println!("\n👋 再见！");
                break;
            }
            Err(err) => {
                eprintln!("错误: {}", err);
            }
        }
    }

    Ok(())
}

/// 与 Agent 对话
async fn chat_with_agent(agent: &Arc<tokio::sync::Mutex<ReactAgent>>, message: &str) {
    let mut agent = agent.lock().await;

    print_user_message(message);

    // 使用流式输出
    match agent.chat_stream(message).await {
        Ok(mut stream) => {
            print!("\n");
            let mut first_chunk = true;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        match event {
                            AgentEvent::Token(token) => {
                                if first_chunk {
                                    print_assistant_prefix();
                                    first_chunk = false;
                                }
                                print!("{}", token);
                                use std::io::Write;
                                std::io::stdout().flush().ok();
                            }
                            AgentEvent::ToolCall { name, args } => {
                                if !first_chunk {
                                    println!();
                                }
                                print_tool_call(&name, &args);
                                first_chunk = true;
                            }
                            AgentEvent::ToolResult { name, output, .. } => {
                                print_tool_result(&name, &output);
                                first_chunk = true;
                            }
                            AgentEvent::FinalAnswer(answer) => {
                                if !first_chunk {
                                    println!();
                                }
                                print_final_answer(&answer);
                            }
                            AgentEvent::Cancelled => {
                                println!("\n⚠️ 执行已取消");
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        println!("\n❌ 错误: {}", e);
                        break;
                    }
                }
            }
            println!();
        }
        Err(e) => {
            println!("\n❌ 对话失败: {}", e);
        }
    }
}

/// 打印欢迎信息
fn print_welcome() {
    println!();
    println!("{}", Color::Cyan.paint("╭─────────────────────────────────────────────────────────────╮"));
    println!("{}", Color::Cyan.paint("│                                                             │"));
    println!("{}", Color::Cyan.paint("│   🤖 Echo Agent CLI - AI Agent 命令行工具                    │"));
    println!("{}", Color::Cyan.paint("│                                                             │"));
    println!("{}", Color::Cyan.paint("│   输入消息开始对话，或输入 /help 查看帮助                    │"));
    println!("{}", Color::Cyan.paint("│                                                             │"));
    println!("{}", Color::Cyan.paint("╰─────────────────────────────────────────────────────────────╯"));
    println!();
}

/// 打印用户消息
fn print_user_message(message: &str) {
    println!("\n{} {}", Color::Blue.paint("👤 You:"), message);
}

/// 打印助手前缀
fn print_assistant_prefix() {
    print!("{} ", Color::Green.paint("🤖 Assistant:"));
}

/// 打印工具调用
fn print_tool_call(name: &str, args: &serde_json::Value) {
    println!(
        "\n  {} {} {}",
        Color::Yellow.paint("🔧 调用工具:"),
        Color::Yellow.bold().paint(name),
        Color::DarkGray.paint(format!("{}", args))
    );
}

/// 打印工具结果
fn print_tool_result(name: &str, output: &str) {
    let preview: String = output.chars().take(200).collect();
    let suffix = if output.len() > 200 { "..." } else { "" };
    println!(
        "  {} {}: {}{}",
        Color::DarkGray.paint("↳"),
        Color::DarkGray.paint(name),
        Color::DarkGray.paint(preview),
        suffix
    );
}

/// 打印最终答案
fn print_final_answer(answer: &str) {
    // 已经在流式输出中打印了
    let _ = answer;
}

/// 创建 Reedline 编辑器
fn create_line_editor(config: &ReplConfig) -> anyhow::Result<Reedline> {
    // 扩展历史文件路径
    let history_path = shellexpand::tilde(&config.history_file);
    let history_path = std::path::Path::new(history_path.as_ref());

    // 创建历史目录
    if let Some(parent) = history_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // 创建历史记录
    let history = FileBackedHistory::with_file(1000, history_path.to_path_buf())?;

    // 创建补全器
    let completer = create_completer();

    // 创建菜单
    let menu = ReedlineMenu::EngineCompleter(Box::new(ColumnarMenu::default().with_name("completion_menu")));

    // 创建键绑定
    let keybindings = create_keybindings();

    // 创建编辑器
    let editor = Reedline::create()
        .with_history(Box::new(history))
        .with_completer(Box::new(completer))
        .with_menu(menu)
        .with_edit_mode(Box::new(Emacs::new(keybindings)));

    Ok(editor)
}

/// 创建补全器
fn create_completer() -> DefaultCompleter {
    let commands = vec![
        "/help", "/h", "/?",
        "/exit", "/quit", "/q",
        "/reset", "/r",
        "/clear", "/cls",
        "/tools", "/t",
        "/skills", "/sk",
        "/mcp", "/m",
        "/history", "/hist",
        "/compress", "/cp",
        "/stats", "/st",
        "/model",
        "/system", "/sys",
        "/save",
        "/load",
    ];

    DefaultCompleter::new_with_wordlen(commands.into_iter().map(String::from).collect(), 2)
}

/// 创建键绑定
fn create_keybindings() -> Keybindings {
    let mut keybindings = default_emacs_keybindings();

    // Ctrl+L: 清屏
    keybindings.add_binding(
        reedline::KeyModifiers::CONTROL,
        reedline::KeyCode::Char('l'),
        ReedlineEvent::ExecuteHostCommand("clear".to_string()),
    );

    keybindings
}

/// 自定义提示符
struct EchoPrompt {
    prompt: String,
}

impl EchoPrompt {
    fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
        }
    }
}

impl Prompt for EchoPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(format!("{} > ", Color::Green.bold().paint(&self.prompt)))
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: reedline::PromptEditMode) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("... ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: reedline::PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };

        std::borrow::Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repl_config_default() {
        let config = ReplConfig::default();
        assert_eq!(config.prompt, "echo");
        assert!(config.enable_completion);
    }

    #[test]
    fn test_echo_prompt() {
        let prompt = EchoPrompt::new("test");
        let left = prompt.render_prompt_left();
        assert!(left.contains("test"));
    }
}