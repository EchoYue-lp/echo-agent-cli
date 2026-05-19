//! REPL (Read-Eval-Print-Loop) 交互实现
//!
//! 提供交互式命令行界面，支持：
//! - 多行输入
//! - 历史记录
//! - 自动补全
//! - 流式输出显示
//! - 思考步骤可视化
//! - 工具调用交互式审批
//! - Token 用量追踪

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use futures::StreamExt;
use nu_ansi_term::Color;
use reedline::{
    Prompt, PromptHistorySearchStatus, Signal,
};

use echo_agent::prelude::*;
use crate::agent_handle::AgentHandle;

use crate::output::OutputRenderer;
use super::commands::{CommandHandler, CommandResult};
use super::editor::{create_enhanced_editor, EditorConfig};

static TOTAL_INPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_TOOL_CALLS: AtomicUsize = AtomicUsize::new(0);

pub fn get_usage_stats() -> (usize, usize, usize) {
    (
        TOTAL_INPUT_TOKENS.load(Ordering::Relaxed),
        TOTAL_OUTPUT_TOKENS.load(Ordering::Relaxed),
        TOTAL_TOOL_CALLS.load(Ordering::Relaxed),
    )
}

pub fn reset_usage_stats() {
    TOTAL_INPUT_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_OUTPUT_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_TOOL_CALLS.store(0, Ordering::Relaxed);
}

// ── Trace Buffer ──────────────────────────────────────────────────

/// A single trace entry captured during streaming.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub event_type: String,
    pub detail: String,
    pub elapsed_ms: u64,
}

static TRACE_BUFFER: std::sync::LazyLock<Mutex<Vec<TraceEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Record a trace entry (called during stream processing).
pub fn push_trace(entry: TraceEntry) {
    let mut buf = TRACE_BUFFER.lock().unwrap();
    // Keep only last 200 entries
    if buf.len() >= 200 {
        let excess = buf.len() - 199;
        buf.drain(0..excess);
    }
    buf.push(entry);
}

/// Clear the trace buffer at the start of a new chat.
pub fn clear_trace() {
    TRACE_BUFFER.lock().unwrap().clear();
}

/// Get a snapshot of the current trace buffer.
pub fn get_trace() -> Vec<TraceEntry> {
    TRACE_BUFFER.lock().unwrap().clone()
}

/// REPL 配置
pub struct ReplConfig {
    pub prompt: String,
    pub history_file: String,
    pub mode: String,
    pub project: Option<String>,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: "~/.echo-agent/history.txt".to_string(),
            mode: "general".to_string(),
            project: None,
        }
    }
}

/// 运行 REPL
pub async fn run_repl(agent: AgentHandle, config: ReplConfig) -> anyhow::Result<()> {
    let output = OutputRenderer::default();

    output.print_banner(env!("CARGO_PKG_VERSION"));

    let model_name = agent.read(|a| a.model_name().to_string()).await;
    let instructions_count = if config.project.is_some() {
        let ctx = crate::project::context::load_project_context(
            std::path::Path::new(config.project.as_deref().unwrap_or(".")),
        );
        ctx.instructions.len()
    } else {
        0
    };
    output.print_session_info(
        &config.mode,
        &model_name,
        config.project.as_deref(),
        instructions_count,
    );

    let cmd_handler = CommandHandler::new(agent.clone());

    let editor_config = EditorConfig {
        prompt: config.prompt.clone(),
        history_file: config.history_file.clone(),
        ..Default::default()
    };
    let mut line_editor = create_enhanced_editor(&editor_config)?;

    let prompt = EchoPrompt::new(&config.prompt);

    loop {
        let signal = line_editor.read_line(&prompt);

        match signal {
            Ok(Signal::Success(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                match cmd_handler.handle(line).await {
                    CommandResult::Continue => {}
                    CommandResult::Exit => break,
                    CommandResult::Chat(message) => {
                        chat_with_agent(&agent, &message, &output).await;
                    }
                }
            }
            Ok(Signal::CtrlC) => {
                output.print_info("（输入 /exit 退出）");
            }
            Ok(Signal::CtrlD) => {
                output.print_success("再见！");
                break;
            }
            Err(err) => {
                output.print_error(&format!("错误: {}", err));
            }
        }
    }

    Ok(())
}

/// 与 Agent 对话
async fn chat_with_agent(
    agent: &AgentHandle,
    message: &str,
    output: &OutputRenderer,
) {
    let agent = agent.inner().read().await;

    output.print_user_message(message);
    clear_trace();

    match agent.chat_stream(message).await {
        Ok(mut stream) => {
            println!();
            let mut first_chunk = true;
            let mut iteration_count: u32 = 0;
            let mut tool_call_count: u32 = 0;
            let start_time = std::time::Instant::now();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        // Record trace entry for significant events
                        {
                            let (etype, detail) = match &event {
                                AgentEvent::ThinkStart => ("think_start".into(), format!("step {}", iteration_count + 1)),
                                AgentEvent::ThinkEnd { prompt_tokens, completion_tokens } =>
                                    ("think_end".into(), format!("in={prompt_tokens} out={completion_tokens}")),
                                AgentEvent::ToolCall { name, .. } => ("tool_call".into(), name.clone()),
                                AgentEvent::ToolResult { name, .. } => ("tool_result".into(), name.clone()),
                                AgentEvent::ToolError { name, .. } => ("tool_error".into(), name.clone()),
                                AgentEvent::FinalAnswer(_) => ("final_answer".into(), String::new()),
                                AgentEvent::Cancelled => ("cancelled".into(), String::new()),
                                AgentEvent::PlanGenerated { steps } => ("plan".into(), format!("{} steps", steps.len())),
                                AgentEvent::StepStart { description, .. } => ("step_start".into(), description.chars().take(60).collect()),
                                AgentEvent::HandoffStart { from, to } => ("handoff".into(), format!("{from}->{to}")),
                                AgentEvent::ContextCompressed { before_tokens, after_tokens, .. } =>
                                    ("compressed".into(), format!("{before_tokens}->{after_tokens}")),
                                _ => (String::new(), String::new()),
                            };
                            if !etype.is_empty() {
                                push_trace(TraceEntry {
                                    event_type: etype,
                                    detail,
                                    elapsed_ms: start_time.elapsed().as_millis() as u64,
                                });
                            }
                        }
                        match event {
                            AgentEvent::ThinkStart => {
                                if !first_chunk {
                                    println!();
                                }
                                iteration_count += 1;
                                let step_label = format!(
                                    "  ⏳ 思考中 (步骤 {})...",
                                    iteration_count
                                );
                                let styled = nu_ansi_term::Color::Fixed(8).paint(&step_label);
                                println!("{}", styled);
                                first_chunk = true;
                            }
                            AgentEvent::ThinkEnd { prompt_tokens, completion_tokens } => {
                                TOTAL_INPUT_TOKENS.fetch_add(prompt_tokens, Ordering::Relaxed);
                                TOTAL_OUTPUT_TOKENS.fetch_add(completion_tokens, Ordering::Relaxed);
                            }
                            AgentEvent::Token(token) => {
                                if first_chunk {
                                    output.print_assistant_prefix();
                                    first_chunk = false;
                                }
                                output.print_token(&token);
                            }
                            AgentEvent::ToolCall { name, args } => {
                                tool_call_count += 1;
                                TOTAL_TOOL_CALLS.fetch_add(1, Ordering::Relaxed);
                                if !first_chunk {
                                    println!();
                                }
                                output.print_tool_call(&name, &args);
                                first_chunk = true;
                            }
                            AgentEvent::ToolResult { name, output: tool_output } => {
                                output.print_tool_result(&name, &tool_output, true);
                                first_chunk = true;
                            }
                            AgentEvent::ToolError { name, error } => {
                                let err_text = format!("✗ {}: {}", name, error);
                                let styled = nu_ansi_term::Color::Red.paint(&err_text);
                                println!("  {}", styled);
                                crate::webhook::emitter::emit_global(
                                    crate::webhook::WebhookEvent::ToolFailed {
                                        name: name.clone(),
                                        error: error.clone(),
                                    }
                                );
                                first_chunk = true;
                            }
                            AgentEvent::FinalAnswer(_answer) if !first_chunk => {
                                println!();
                            }
                            AgentEvent::Cancelled => {
                                output.print_warning("执行已取消");
                            }
                            AgentEvent::PlanGenerated { steps } => {
                                let plan_label = nu_ansi_term::Color::Cyan.paint("  📋 执行计划:");
                                println!("\n{}", plan_label);
                                for (i, step) in steps.iter().enumerate() {
                                    let step_text = format!("    {}. {}", i + 1, step);
                                    let styled = nu_ansi_term::Color::Fixed(12).paint(&step_text);
                                    println!("{}", styled);
                                }
                                first_chunk = true;
                            }
                            AgentEvent::StepStart { step_index: _, description } => {
                                let desc_preview: String = description.chars().take(60).collect();
                                let step_label = format!("  ▶ 执行: {}...", desc_preview);
                                let styled = nu_ansi_term::Color::Fixed(8).paint(&step_label);
                                println!("{}", styled);
                            }
                            AgentEvent::StepEnd { .. } => {}
                            AgentEvent::HandoffStart { from, to } => {
                                let handoff_label = format!("  🔀 交接: {} -> {}", from, to);
                                let styled = nu_ansi_term::Color::Yellow.paint(&handoff_label);
                                println!("\n{}", styled);
                                first_chunk = true;
                            }
                            AgentEvent::HandoffEnd { .. } => {}
                            AgentEvent::MemoryRecalled { .. } => {}
                            AgentEvent::Chart { .. } => {}
                            AgentEvent::GuardTriggered { .. } => {}
                            AgentEvent::ReflectionStart { .. } => {}
                            AgentEvent::ReflectionEnd { .. } => {}
                            AgentEvent::CritiqueGenerated { .. } => {}
                            AgentEvent::Refining { .. } => {}
                            AgentEvent::Error { .. } => {}
                            AgentEvent::ContextCompressed { before_count, after_count, before_tokens, after_tokens } => {
                                let saved = before_tokens.saturating_sub(after_tokens);
                                let styled = nu_ansi_term::Color::Fixed(8).paint(
                                    format!("  📦 上下文自动压缩: {}→{} 条消息, {}→{} tokens (节省 {})", before_count, after_count, before_tokens, after_tokens, saved)
                                );
                                println!("{}", styled);
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        output.print_error(&format!("错误: {}", e));
                        break;
                    }
                }
            }

            let elapsed = start_time.elapsed();

            // Emit ChatCompleted webhook
            {
                let (input_tokens, output_tokens, _) = get_usage_stats();
                crate::webhook::emitter::emit_global(
                    crate::webhook::WebhookEvent::ChatCompleted {
                        model: String::new(), // model not easily available here
                        input_tokens,
                        output_tokens,
                        elapsed_ms: elapsed.as_millis() as u64,
                    }
                );
            }

            let config = output.config();

            if config.show_token_stats || config.show_tool_details {
                println!();
                let duration_str = if elapsed.as_secs() >= 60 {
                    format!("{}m {}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
                } else {
                    format!("{:.1}s", elapsed.as_secs_f64())
                };
                let stats = format!("  ⏱ {:.0}  🔧 {} 工具调用", duration_str, tool_call_count);
                let styled = nu_ansi_term::Color::Fixed(8).paint(&stats);
                println!("{}", styled);
            }

            println!();
        }
        Err(e) => {
            output.print_error(&format!("对话失败: {}", e));
            crate::webhook::emitter::emit_global(
                crate::webhook::WebhookEvent::AgentError {
                    error: e.to_string(),
                }
            );
        }
    }
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
    }

    #[test]
    fn test_echo_prompt() {
        let prompt = EchoPrompt::new("test");
        let left = prompt.render_prompt_left();
        assert!(left.contains("test"));
    }
}