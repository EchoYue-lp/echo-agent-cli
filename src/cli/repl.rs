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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use nu_ansi_term::Color;
use reedline::{Prompt, PromptHistorySearchStatus, Signal};

use crate::agent_handle::AgentHandle;
use echo_agent::prelude::*;

use super::commands::{CommandHandler, CommandResult};
use super::editor::{EditorConfig, create_enhanced_editor};
use crate::output::OutputRenderer;

static TOTAL_INPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_TOOL_CALLS: AtomicUsize = AtomicUsize::new(0);
static FILE_CHANGE_COUNT: AtomicUsize = AtomicUsize::new(0);

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
    FILE_CHANGE_COUNT.store(0, Ordering::Relaxed);
}

/// REPL 配置
pub struct ReplConfig {
    pub prompt: String,
    pub history_file: String,
    pub mode: String,
    pub project: Option<String>,
    pub task_service: Option<Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    pub scheduler_runner: Option<Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    /// Shared ReviewIntegration (from bootstrap) — enables Dreaming + reuse
    /// in session-end memory review. When `None`, review functions fall back
    /// to building a temporary instance (legacy behavior).
    pub review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: "~/.echo-agent/history.txt".to_string(),
            mode: "general".to_string(),
            project: None,
            task_service: None,
            scheduler_runner: None,
            review_integration: None,
        }
    }
}

/// 运行 REPL
pub async fn run_repl(agent: AgentHandle, config: ReplConfig) -> anyhow::Result<()> {
    let output = OutputRenderer::default();

    output.print_banner(env!("CARGO_PKG_VERSION"));

    // ── Dreaming: daily self-evolution pass (mode parity with GUI) ────
    // Spawn background Dreaming task so CLI sessions also get recall-
    // frequency-driven memory promotion/demotion (AGENTS.md: TUI/CLI must
    // be feature-equivalent with GUI). Cancelled on session exit.
    let dreaming_cancel = config.review_integration.as_ref().map(|ri| {
        let cancel = tokio_util::sync::CancellationToken::new();
        echo_agent_app_core::infra::spawn_dreaming_task(ri.clone(), cancel.clone());
        tracing::info!("Dreaming task spawned for CLI session");
        cancel
    });

    let model_name = agent.read(|a| a.model_name().to_string()).await;

    // Load project context: use explicit --project path, or auto-discover from cwd.
    let project_ctx = {
        let project_path = config.project.as_deref().unwrap_or(".");
        let explicit = config.project.is_some();
        let root = if explicit {
            Some(std::path::PathBuf::from(project_path))
        } else {
            crate::project::context::discover_project_root(Some(std::path::Path::new(".")))
        };
        root.map(|r| crate::project::context::load_project_context(&r))
    };
    let instructions_count = project_ctx
        .as_ref()
        .map(|c| c.instructions.len())
        .unwrap_or(0);
    let project_display = project_ctx
        .as_ref()
        .map(|c| c.root.to_string_lossy().to_string());

    output.print_session_info(
        &config.mode,
        &model_name,
        project_display.as_deref(),
        instructions_count,
    );

    // Build command registry with trait-based commands
    let mut registry = crate::cli::command::CommandRegistry::new();
    crate::cli::cmd_impls::coding::register_all(&mut registry);
    crate::cli::cmd_impls::diff_cmd::register_all(&mut registry);
    crate::cli::cmd_impls::git::register_all(&mut registry);
    crate::cli::cmd_impls::session::register_all(&mut registry);
    crate::cli::cmd_impls::info::register_all(&mut registry);
    crate::cli::cmd_impls::context::register_all(&mut registry);
    crate::cli::cmd_impls::advanced::register_all(&mut registry);
    crate::cli::cmd_impls::skills::register_all(&mut registry);
    crate::cli::cmd_impls::hooks::register_all(&mut registry);
    crate::cli::cmd_impls::eval::register_all(&mut registry);
    crate::cli::cmd_impls::evolution::register_all(&mut registry);
    crate::cli::cmd_impls::tasks_ext::register_all(&mut registry);
    crate::cli::cmd_impls::research::register_all(&mut registry);
    crate::cli::cmd_impls::pipelines::register_all(&mut registry);
    crate::cli::cmd_impls::pipeline::register_all(&mut registry);
    crate::cli::cmd_impls::workspace::register_all(&mut registry);
    crate::cli::cmd_impls::plugins::register_all(&mut registry);
    crate::cli::cmd_impls::cron::register_all(&mut registry);
    crate::cli::cmd_impls::all::register_all(&mut registry);

    // Create CodingLoop for coding-mode commands (C6 fix).
    let project_root = project_ctx
        .as_ref()
        .map(|c| c.root.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let coding_loop = Arc::new(tokio::sync::Mutex::new(
        crate::project::coding_loop::CodingLoop::new(&project_root),
    ));

    let cmd_handler = CommandHandler::new(agent.clone())
        .with_registry(Arc::new(registry))
        .with_coding_loop(coding_loop)
        .with_task_service_opt(config.task_service)
        .with_scheduler_opt(config.scheduler_runner);

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

    // ── Auto-memory: extract observations on session end ────────────
    run_auto_memory_on_exit(&agent).await;

    // ── Reflection: summarize session learnings ─────────────────────
    run_reflection_on_exit(&agent).await;

    // ── Memory review: staleness scoring, conflict detection, GC ────
    run_memory_review_on_exit(&agent, &config.review_integration).await;

    // ── Stop Dreaming background task ──────────────────────────────
    if let Some(cancel) = dreaming_cancel {
        cancel.cancel();
    }

    Ok(())
}

/// Run auto-memory extraction when the session ends.
///
/// Non-blocking: errors are silently ignored to avoid disrupting exit flow.
async fn run_auto_memory_on_exit(agent: &AgentHandle) {
    use echo_agent_app_core::auto_memory::{
        AutoMemoryConfig, append_to_project_memory, extract_observations,
        format_observations_for_memory, write_observations_to_memory_layer,
    };

    // Check if auto-memory is enabled (shared with /auto-memory command)
    // Use the global flag from the cmd_impls module
    let enabled =
        crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
    if !enabled {
        return;
    }

    // Extract messages from the agent context
    let messages: Vec<(String, String)> = agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                ctx.messages()
                    .iter()
                    .map(|m| {
                        (
                            m.role.as_str().to_string(),
                            m.content.as_text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
        })
        .await;

    // Need a minimum number of messages to extract meaningful observations
    if messages.len() < 2 {
        return;
    }

    let config = AutoMemoryConfig::default();
    let observations = extract_observations(&messages, &config);

    if observations.is_empty() {
        return;
    }

    let count = observations.len();
    let formatted = format_observations_for_memory(&observations);

    let typed_written = match agent.read(|a| a.memory_layer_manager().cloned()).await {
        Some(lm) => match write_observations_to_memory_layer(&observations, &lm).await {
            Ok(count) => count,
            Err(e) => {
                println!("  Auto-memory: failed to save typed memory ({})", e);
                0
            }
        },
        None => {
            tracing::debug!("auto_memory: agent has no shared layer manager, skipping typed write");
            0
        }
    };

    match append_to_project_memory(&observations) {
        Ok(()) => {
            println!(
                "  💾 Auto-memory: saved {} observation(s) to project memory, {} to typed memory.",
                count, typed_written
            );
            // Print a brief summary of what was saved
            for line in formatted.lines().take(8) {
                println!("     {}", line);
            }
            if formatted.lines().count() > 8 {
                println!("     ...");
            }
        }
        Err(e) => {
            // Silently report but don't block exit
            println!("  Auto-memory: failed to save ({})", e);
        }
    }
}

/// Run lightweight reflection when the session ends.
///
/// Generates a brief summary of session learnings and appends to
/// `.echo-agent/memory/PROJECT.md`. Non-blocking: errors are silently
/// ignored to avoid disrupting exit flow.
async fn run_reflection_on_exit(agent: &AgentHandle) {
    // Get LLM client from agent
    let llm_client = agent.read(|a| a.llm_client().cloned()).await;

    let Some(llm) = llm_client else {
        return;
    };

    // Get message count to ensure there's enough context for reflection
    let message_count = agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                ctx.messages().len()
            })
        })
        .await;

    // Need at least a few exchanges for meaningful reflection
    if message_count < 4 {
        return;
    }

    let prompt = "Reflect on the current session and summarize key learnings in 1-2 sentences.\n\
                  Focus on reusable insights. Be specific. Max 200 tokens.\n\nReflection:";

    let messages = vec![echo_agent::prelude::Message::user(prompt.to_string())];
    let options = echo_agent::prelude::SimpleChatOptions::default().with_max_tokens(300);

    let reflection = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        llm.chat_simple_with_options(messages, options),
    )
    .await
    {
        Ok(Ok(text)) => text,
        Ok(Err(_)) | Err(_) => return, // Silently skip on failure
    };

    // Write to .echo-agent/memory/PROJECT.md
    let memory_dir = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".echo-agent")
        .join("memory");
    let _ = std::fs::create_dir_all(&memory_dir);
    let memory_file = memory_dir.join("PROJECT.md");

    let entry = format!(
        "\n## [session] Reflection ({})\n{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        reflection.trim()
    );

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&memory_file)
    {
        use std::io::Write;
        let _ = file.write_all(entry.as_bytes());
        println!("  🪞 Reflection saved to {}", memory_file.display());
    }
}

/// Run memory review when the session ends.
///
/// Performs staleness scoring, conflict detection, merge, and archival
/// on typed memories. Non-blocking: errors are silently ignored to avoid
/// disrupting exit flow.
///
/// When `shared_ri` is provided, reuses the bootstrap-time ReviewIntegration
/// (same shared store + layer manager as Dreaming). Otherwise falls back to
/// building a temporary instance from the agent's store (legacy behavior).
async fn run_memory_review_on_exit(
    agent: &AgentHandle,
    shared_ri: &Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) {
    // Prefer the shared ReviewIntegration (same store Dreaming uses).
    if let Some(ri) = shared_ri {
        if let Some(review_result) = ri.on_session_end().await {
            match review_result {
                Ok(report) => {
                    let count = report.total_scanned;
                    if count > 0 {
                        println!(
                            "  📋 Memory review: {} scanned, {} stale, {} conflicts, {} merged, {} archived",
                            count,
                            report.stale_count,
                            report.conflict_groups,
                            report.merges_applied,
                            report.archives_applied
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  ⚠ Memory review failed: {e}");
                }
            }
        }
        return;
    }

    // Fallback: build a temporary ReviewIntegration from the agent's store.
    let store = agent.read(|a| a.store().cloned()).await;
    let Some(store) = store else {
        return;
    };

    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
    let review_integration = echo_agent_app_core::evolution::ReviewIntegration::new(
        echo_agent::evolution::ReviewConfig::default(),
        echo_agent_dir,
        store,
    );

    if let Some(review_result) = review_integration.on_session_end().await {
        match review_result {
            Ok(report) => {
                let count = report.total_scanned;
                if count > 0 {
                    println!(
                        "  📋 Memory review: {} scanned, {} stale, {} conflicts, {} merged, {} archived",
                        count,
                        report.stale_count,
                        report.conflict_groups,
                        report.merges_applied,
                        report.archives_applied
                    );
                }
            }
            Err(e) => {
                eprintln!("  ⚠ Memory review failed: {e}");
            }
        }
    }
}

/// 与 Agent 对话
#[allow(unused_assignments)]
async fn chat_with_agent(agent: &AgentHandle, message: &str, output: &OutputRenderer) {
    output.print_user_message(message);

    // Show spinner during connection establishment and first-token wait.
    // chat_stream() returns quickly (spawns internal task); the real wait
    // is in stream.next().await for the first SSE chunk.
    let mut spinner = output.start_spinner("Connecting to model...");

    // ReactAgent serializes execution internally via execution_mutex
    let agent_guard = agent.inner().read().await;
    match agent_guard.chat_stream(message).await {
        Ok(mut stream) => {
            spinner.set_message("Waiting for response...");
            let mut spinner_cleared = false;
            let mut first_chunk = true;
            let mut iteration_count: u32 = 0;
            let mut tool_call_count: u32 = 0;
            let start_time = std::time::Instant::now();

            // Helper: clear spinner on first meaningful event.
            macro_rules! clear_spinner {
                () => {
                    if !spinner_cleared {
                        spinner.finish_and_clear();
                        spinner_cleared = true;
                    }
                };
            }

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        // Record trace entry for significant events
                        {
                            let (_etype, _detail) = match &event {
                                AgentEvent::ThinkStart => (
                                    "think_start".into(),
                                    format!("step {}", iteration_count + 1),
                                ),
                                AgentEvent::ThinkEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                } => (
                                    "think_end".into(),
                                    format!("in={prompt_tokens} out={completion_tokens}"),
                                ),
                                AgentEvent::LlmUsage {
                                    cached_prompt_tokens,
                                    cache_creation_prompt_tokens,
                                    usage_reported,
                                    ..
                                } => (
                                    "llm_usage".into(),
                                    format!(
                                        "cached={cached_prompt_tokens} cache_write={cache_creation_prompt_tokens} usage_reported={usage_reported}"
                                    ),
                                ),
                                AgentEvent::ToolCall { name, .. } => {
                                    ("tool_call".into(), name.clone())
                                }
                                AgentEvent::ToolResult { name, .. } => {
                                    ("tool_result".into(), name.clone())
                                }
                                AgentEvent::ToolError { name, .. } => {
                                    ("tool_error".into(), name.clone())
                                }
                                AgentEvent::FinalAnswer(_) => {
                                    ("final_answer".into(), String::new())
                                }
                                AgentEvent::Cancelled => ("cancelled".into(), String::new()),
                                AgentEvent::ContextCompressed {
                                    before_tokens,
                                    after_tokens,
                                    ..
                                } => (
                                    "compressed".into(),
                                    format!("{before_tokens}->{after_tokens}"),
                                ),
                                AgentEvent::SafetyNotice { action, risk, .. } => {
                                    ("safety_notice".into(), format!("{action} — {risk}"))
                                }
                                _ => (String::new(), String::new()),
                            };
                        }
                        match event {
                            AgentEvent::ThinkStart => {
                                clear_spinner!();
                                if !first_chunk {
                                    println!();
                                }
                                iteration_count += 1;
                                let step_label =
                                    format!("  ⏳ 思考中 (步骤 {})...", iteration_count);
                                let styled = nu_ansi_term::Color::Fixed(8).paint(&step_label);
                                println!("{}", styled);
                                first_chunk = true;
                            }
                            AgentEvent::ThinkEnd {
                                prompt_tokens,
                                completion_tokens,
                            } => {
                                TOTAL_INPUT_TOKENS.fetch_add(prompt_tokens, Ordering::Relaxed);
                                TOTAL_OUTPUT_TOKENS.fetch_add(completion_tokens, Ordering::Relaxed);
                            }
                            AgentEvent::LlmUsage { .. } => {}
                            AgentEvent::Token(token) => {
                                clear_spinner!();
                                if first_chunk {
                                    output.print_assistant_prefix();
                                    first_chunk = false;
                                }
                                output.print_token(&token);
                            }
                            AgentEvent::SafetyNotice {
                                action,
                                reason,
                                risk,
                                permission,
                            } => {
                                clear_spinner!();
                                if !first_chunk {
                                    println!();
                                }
                                let icon = nu_ansi_term::Color::Yellow.paint("Safety");
                                println!("  {}  {}", icon, action);
                                println!("       Reason: {}", reason);
                                println!("       Risk: {} | Permission: {}", risk, permission);
                                first_chunk = true;
                            }
                            AgentEvent::ParameterError {
                                tool,
                                parameter,
                                expected,
                                got,
                            } => {
                                clear_spinner!();
                                if !first_chunk {
                                    println!();
                                }
                                let icon = nu_ansi_term::Color::Red.paint("ParamError");
                                println!(
                                    "  {}  {}: parameter '{}' expected {}, got {}",
                                    icon, tool, parameter, expected, got
                                );
                                first_chunk = true;
                            }
                            AgentEvent::ToolCall { name, args } => {
                                clear_spinner!();
                                tool_call_count += 1;
                                TOTAL_TOOL_CALLS.fetch_add(1, Ordering::Relaxed);
                                if !first_chunk {
                                    println!();
                                }
                                // Danger warning for destructive operations
                                if name == "shell" || name == "delete_file" || name == "git_commit"
                                {
                                    let danger = nu_ansi_term::Color::Red.paint(format!(
                                        "DANGER: {} — irreversible operation",
                                        name
                                    ));
                                    println!("  {}", danger);
                                    if name == "shell"
                                        && let Some(cmd) =
                                            args.get("command").and_then(|v| v.as_str())
                                    {
                                        println!("     Command: {}", cmd);
                                    }
                                }
                                output.print_tool_call(&name, &args);
                                first_chunk = true;
                            }
                            AgentEvent::ToolResult {
                                name,
                                output: tool_output,
                            } => {
                                // Auto-track file changes for coding loop
                                if matches!(
                                    name.as_str(),
                                    "write_file"
                                        | "edit_file"
                                        | "append_file"
                                        | "create_file"
                                        | "delete_file"
                                        | "update_file"
                                        | "move_file"
                                ) {
                                    FILE_CHANGE_COUNT.fetch_add(1, Ordering::Relaxed);
                                }
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
                                    },
                                );
                                first_chunk = true;
                            }
                            AgentEvent::FinalAnswer(_answer) => {
                                clear_spinner!();
                                if !first_chunk {
                                    println!();
                                }
                            }
                            AgentEvent::Cancelled => {
                                clear_spinner!();
                                output.print_warning("执行已取消");
                            }
                            AgentEvent::MemoryRecalled { .. } => {}
                            AgentEvent::Chart { .. } => {}
                            AgentEvent::GuardTriggered { .. } => {}
                            AgentEvent::Error { source, message } => {
                                clear_spinner!();
                                output.print_error(&format!("[{}] {}", source, message));
                            }
                            AgentEvent::ContextCompressed {
                                before_count,
                                after_count,
                                before_tokens,
                                after_tokens,
                            } => {
                                let saved = before_tokens.saturating_sub(after_tokens);
                                let styled = nu_ansi_term::Color::Fixed(8).paint(format!(
                                    "  📦 上下文自动压缩: {}→{} 条消息, {}→{} tokens (节省 {})",
                                    before_count, after_count, before_tokens, after_tokens, saved
                                ));
                                println!("{}", styled);
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        clear_spinner!();
                        output.print_error(&format!("错误: {}", e));
                        break;
                    }
                }
            }

            let elapsed = start_time.elapsed();

            // Ensure spinner is cleared even if stream produced no meaningful events
            clear_spinner!();

            // Emit ChatCompleted webhook
            {
                let (input_tokens, output_tokens, _) = get_usage_stats();
                crate::webhook::emitter::emit_global(crate::webhook::WebhookEvent::ChatCompleted {
                    model: String::new(), // model not easily available here
                    input_tokens,
                    output_tokens,
                    elapsed_ms: elapsed.as_millis() as u64,
                });
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

            // Auto-eval suggestion
            if tool_call_count > 0 {
                let hint = nu_ansi_term::Color::Fixed(8)
                    .paint("  Tip: /self-review to analyze, /test to verify, /diff to review");
                println!("{}", hint);
            }

            // Auto-save trajectory (background, non-blocking)
            {
                let agent_clone = agent.clone();
                tokio::spawn(async move {
                    use echo_agent::agent::Agent;
                    let result = agent_clone
                        .read(|a| (a.run_store.clone(), a.model_name().to_string()))
                        .await;
                    let (store, model_name) = result;
                    if let Some(ref store) = store
                        && let Ok(runs) = store.list_all(1).await
                        && let Some(summary) = runs.first()
                        && let Ok(Some(run)) = store.load(&summary.run_id).await
                        && let Ok(saver) = echo_agent::improve::TrajectorySaver::default_dir()
                    {
                        let _ = saver.save(&run, &model_name).await;
                    }
                });
            }

            // Interactive git change handling (replaces auto-commit)
            let changes = FILE_CHANGE_COUNT.swap(0, Ordering::Relaxed);
            if changes > 0 {
                let cwd = std::env::current_dir().unwrap_or_default();
                if cwd.join(".git").exists() {
                    let choice = crate::cli::git_ops::prompt_for_git_action(changes);
                    match choice {
                        'c' => {
                            if let Err(e) = crate::cli::git_ops::interactive_commit(&cwd, changes) {
                                println!("  {} {}", nu_ansi_term::Color::Red.paint("✗"), e);
                            }
                        }
                        's' => {
                            if let Err(e) = crate::cli::git_ops::interactive_stage(&cwd) {
                                println!("  {} {}", nu_ansi_term::Color::Red.paint("✗"), e);
                            }
                        }
                        _ => {} // 'n' — do nothing
                    }
                }
            }

            println!();
        }
        Err(e) => {
            spinner.finish_error(&format!("Connection failed: {}", e));
            output.print_error(&format!("对话失败: {}", e));
            crate::webhook::emitter::emit_global(crate::webhook::WebhookEvent::AgentError {
                error: e.to_string(),
            });
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

    fn render_prompt_indicator(
        &self,
        _prompt_mode: reedline::PromptEditMode,
    ) -> std::borrow::Cow<'_, str> {
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
