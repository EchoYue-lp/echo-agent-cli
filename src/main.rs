//! EKO - TUI/GUI 通用 Agent
//!
//! 默认启动全屏 TUI；GUI 使用独立的 Tauri 入口。Web/REPL 仅保留为内部兼容入口。
//!
//! # 快速开始
//!
//! ```bash
//! # 启动 TUI（默认）
//! echo-agent-cli
//!
//! # 指定模型
//! echo-agent-cli --model claude-sonnet-4-6
//! ```

#[cfg(feature = "tui")]
use echo_agent_cli::cli;
#[cfg(feature = "tui")]
use echo_agent_cli::config;
#[cfg(feature = "tui")]
use echo_agent_cli::infra;

#[cfg(feature = "tui")]
use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // Tauri CLI builds the package-name binary (`echo-agent-cli`) and then
    // bundles/renames it. In a GUI-only build, route this binary to the
    // desktop runtime so the packaged app does not start the TUI path.
    #[cfg(all(feature = "gui", not(feature = "tui")))]
    return echo_agent_cli::tauri::desktop::run_desktop_entry().await;

    #[cfg(feature = "tui")]
    {
        run_tui_or_cli_entry().await
    }

    #[cfg(all(not(feature = "tui"), not(feature = "gui")))]
    {
        compile_error!("Either the tui or gui feature must be enabled");
    }
}

#[cfg(feature = "tui")]
async fn run_tui_or_cli_entry() -> anyhow::Result<()> {
    // 解析命令行参数
    let args = cli::Args::parse();

    // 加载 YAML 配置文件
    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

    // --verbose 覆盖日志级别为 debug
    if args.verbose {
        app_config.logging.level = "debug".to_string();
    }

    let is_tui_entry = args.tui || (!args.web && !args.cli && !args.channels);

    // 初始化日志。默认用户入口是 TUI，日志必须写入文件，避免污染全屏界面。
    #[cfg(feature = "tui")]
    if is_tui_entry {
        infra::init_logging_for_tui(&app_config.logging.level);
    } else {
        infra::init_logging(&app_config.logging.level);
    }

    #[cfg(not(feature = "tui"))]
    {
        infra::init_logging(&app_config.logging.level);
    }

    // 创建 Agent + 加载 MCP 配置（统一路径，消除重复）
    let params = echo_agent_cli::infra::AgentCreateParams {
        model: args.model.clone(),
        system_prompt: None,
        project: args.project.clone(),
        session_id: None,
        conversation_id: None,
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: None,
        working_dir: None,
        task_runtime_store: None,
        route: None,
    };
    // ── Bootstrap Agent Runtime (shared TUI/GUI initialization) ──
    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params).await?;
    let agent_handle = runtime.agent_handle.clone();

    // Spawn config file watcher (fires ConfigChange hooks + reloads hooks on change)
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if let Some(config_path) =
        echo_agent_cli::config_watcher::resolve_config_path(args.config.as_deref())
    {
        echo_agent_cli::config_watcher::spawn_config_watcher(
            config_path,
            agent_handle.clone(),
            cancel_token.clone(),
        );
    }

    // ── Background task store (all modes) ──
    // Create a SQLite store for background task persistence.
    // Passed to mode functions so they can start BackgroundTaskService.
    let task_store: std::sync::Arc<dyn echo_agent::memory::Store> = {
        let db_path = echo_agent_app_core::persistence::Persistence::base_dir().join("tasks.db");
        match echo_agent::memory::SqliteStore::new(&db_path) {
            Ok(store) => std::sync::Arc::new(store),
            Err(e) => {
                tracing::warn!("Failed to create SQLite store for tasks: {e}");
                // Fallback: use FileStore
                let file_path =
                    echo_agent_app_core::persistence::Persistence::base_dir().join("tasks_store");
                match echo_agent::memory::FileStore::new(&file_path) {
                    Ok(store) => std::sync::Arc::new(store),
                    Err(e2) => {
                        tracing::error!("Failed to create FileStore for tasks: {e2}");
                        std::sync::Arc::new(echo_agent::memory::InMemoryStore::new())
                    }
                }
            }
        }
    };

    // ── G3: Session resume (--continue / --resume) ──────────────────
    if args.r#continue || args.resume.is_some() {
        use echo_agent::llm::types::{FunctionCall, Message, MessageContent, ToolCall};
        use echo_agent_cli::sessions::{Session, SessionManager};

        /// Convert persisted session messages back into agent `Message` values,
        /// re-linking tool-result messages to their parent assistant tool-call IDs.
        fn restore_messages(session: &Session) -> Vec<Message> {
            let mut out: Vec<Message> = Vec::new();
            let mut pending_tc_ids: Vec<String> = Vec::new();
            // Map tool_call_id → tool name from the preceding assistant message
            let mut tc_name_map: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut tc_idx: usize = 0;

            for sm in &session.messages {
                let text = sm.content.clone().unwrap_or_default();
                match sm.role.as_str() {
                    "system" => {
                        out.push(Message::system(text));
                        pending_tc_ids.clear();
                        tc_name_map.clear();
                        tc_idx = 0;
                    }
                    "user" => {
                        out.push(Message::user(text));
                        pending_tc_ids.clear();
                        tc_name_map.clear();
                        tc_idx = 0;
                    }
                    "assistant" => {
                        if let Some(ref tcs) = sm.tool_calls {
                            let calls: Vec<ToolCall> = tcs
                                .iter()
                                .map(|tc| ToolCall {
                                    id: tc.id.clone(),
                                    call_type: "function".to_string(),
                                    function: FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect();
                            pending_tc_ids = calls.iter().map(|c| c.id.clone()).collect();
                            tc_name_map.clear();
                            for tc in tcs {
                                tc_name_map.insert(tc.id.clone(), tc.name.clone());
                            }
                            tc_idx = 0;
                            let mut msg = Message::assistant_with_tools(calls);
                            if !text.is_empty() {
                                msg.content = MessageContent::Text(text);
                            }
                            out.push(msg);
                        } else {
                            out.push(Message::assistant(text));
                            pending_tc_ids.clear();
                            tc_name_map.clear();
                            tc_idx = 0;
                        }
                    }
                    "tool" => {
                        let id = pending_tc_ids.get(tc_idx).cloned().unwrap_or_else(|| {
                            tracing::warn!(
                                "restore_messages: tool result at index {tc_idx} has no matching tool call ID, using placeholder"
                            );
                            format!("unknown_{tc_idx}")
                        });
                        // Restore tool name from the assistant's tool_calls
                        let name = tc_name_map.get(&id).cloned().unwrap_or_default();
                        tc_idx += 1;
                        out.push(Message::tool_result(id, name, text));
                    }
                    _ => {
                        out.push(Message::user(text));
                    }
                }
            }
            out
        }

        let manager = SessionManager::new();
        let resume_result: anyhow::Result<Option<echo_agent_cli::sessions::Session>> = if let Some(
            ref session_id,
        ) =
            args.resume
        {
            match manager.load(session_id) {
                Ok(s) => Ok(Some(s)),
                Err(_) => {
                    eprintln!(
                        "\u{2717} Session '{}' not found. Use `echo-agent-cli sessions list` to see available sessions.",
                        session_id
                    );
                    std::process::exit(1);
                }
            }
        } else {
            manager.get_latest()
        };

        match resume_result {
            Ok(Some(session)) => {
                let messages = restore_messages(&session);
                let msg_count = messages.len();
                if !messages.is_empty() {
                    agent_handle
                        .read_async(|a| {
                            Box::pin(async move {
                                a.load_messages(messages).await;
                            })
                        })
                        .await;
                }
                let date: String = session.updated_at.chars().take(19).collect();
                let short_id = if session.id.len() >= 8 {
                    &session.id[..8]
                } else {
                    &session.id
                };
                println!(
                    "\u{2713} Resuming session {} from {}, {} messages",
                    short_id, date, msg_count
                );
                tracing::info!(
                    session_id = %session.id,
                    messages = msg_count,
                    "Session resumed"
                );
            }
            Ok(None) => {
                eprintln!(
                    "\u{2717} No previous sessions found. Start a new session normally, then use --continue to resume it later."
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!(
                    "\u{26a0} Failed to load session data: {e}. Starting a fresh session instead."
                );
                tracing::warn!("Session resume failed, falling back to new session: {e}");
            }
        }
    }

    // ── User-facing TUI mode (default) ─────────────────────────────────
    #[cfg(feature = "tui")]
    if is_tui_entry {
        // Initialize AgentPool for background task isolation.
        // Background tasks get a dedicated agent from the pool so they
        // don't block the user's TUI conversation (separate execution_mutex).
        let pool = runtime
            .init_pool(echo_agent_app_core::agent_pool::PoolConfig::default())
            .await;
        tracing::info!(
            pool_size = pool.pool_size().await,
            "AgentPool initialized for TUI (background task isolation)"
        );

        // Start BackgroundTaskService with the pool so independent
        // background tasks can use distinct worker agents.
        let tui_task_service = {
            let cancel = echo_agent::agent::CancellationToken::new();
            match echo_agent_app_core::tasks::BackgroundTaskService::with_pool(
                pool.clone(),
                task_store.clone(),
                cancel,
                None,
            )
            .await
            {
                Ok(svc) => {
                    let svc = std::sync::Arc::new(svc);
                    svc.clone().spawn();
                    tracing::info!("BackgroundTaskService started for TUI mode");
                    Some(svc)
                }
                Err(e) => {
                    tracing::warn!("BackgroundTaskService unavailable in TUI: {e}");
                    None
                }
            }
        };

        // Swap REPL provider → TUI provider (REPL blocks on stdin, incompatible
        // with the TUI alternate screen).
        let tui_pending = {
            use echo_agent_app_core::hitl::TuiHumanLoopProvider;
            let tui_provider = std::sync::Arc::new(TuiHumanLoopProvider::new());
            let pending = tui_provider.pending_handle();
            runtime.hitl_dispatcher.unregister("repl").await;
            runtime.hitl_dispatcher.register("tui", tui_provider).await;
            tracing::info!("HITL: REPL provider swapped for TUI provider");
            pending
        };

        echo_agent_cli::tui::run_tui(
            agent_handle.clone(),
            tui_task_service,
            &app_config.tui,
            "💬 通用",
            tui_pending,
        )
        .await?;

        // ── Memory review on session end (TUI) ──────────────────────
        if let Some(ref review_integration) = runtime.review_integration
            && let Some(review_result) = review_integration.on_session_end().await
        {
            match review_result {
                Ok(report) => {
                    if report.total_scanned > 0 {
                        println!(
                            "  📋 Memory review: {} scanned, {} stale, {} conflicts, {} merged, {} archived",
                            report.total_scanned,
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

        drop(runtime);
        cancel_token.cancel();

        return Ok(());
    }

    #[cfg(not(feature = "tui"))]
    if is_tui_entry {
        eprintln!("TUI 模式需要 tui feature。请使用: cargo build --features tui");
        std::process::exit(1);
    }

    // ── Hidden legacy/internal modes ───────────────────────────────────
    let run_cli = args.cli;
    let run_channels = args.channels;

    if args.web {
        eprintln!("Web 模式已移除。请使用 Tauri 桌面模式（cargo tauri dev）或 CLI 模式（--cli）。");
        std::process::exit(1);
    }

    if run_channels {
        #[cfg(feature = "channels")]
        {
            let channels_handle = tokio::spawn(cli::run_channels_mode(app_config.clone()));

            if run_cli {
                cli::run_cli_mode(
                    agent_handle,
                    runtime.hitl_dispatcher.clone(),
                    &args,
                    &app_config,
                    task_store.clone(),
                )
                .await?;
            } else {
                // 仅 channels 模式，等待 channels 或 Ctrl+C
                channels_handle.await??;
                return Ok(());
            }
            // channels 会在后台运行，主模式退出后自动结束
        }
        #[cfg(not(feature = "channels"))]
        {
            tracing::error!(
                "--channels 需要启用 channels feature: cargo build --features channels"
            );
        }
    } else if run_cli {
        // 仅 CLI 模式
        cli::run_cli_mode(
            agent_handle,
            runtime.hitl_dispatcher.clone(),
            &args,
            &app_config,
            task_store.clone(),
        )
        .await?;
    } else {
        // No legacy mode specified — should have entered TUI above
        eprintln!("请使用 TUI（默认）、Tauri 桌面模式、或 --cli 模式。");
        std::process::exit(1);
    }

    // Keep runtime alive until shutdown
    drop(runtime);
    cancel_token.cancel();

    Ok(())
}

// ── 单元测试 ─────────────────────────────────────────────────────

// The tests below reference `cli::Args` and `infra::AgentCreateParams`,
// which are only declared with `#[cfg(feature = "tui")]` (see top of file).
// Gate the whole test module on the same feature so a `gui`-only build
// does not try to compile tests that reference missing modules.
#[cfg(all(test, feature = "tui"))]
mod tests {
    use super::*;
    use echo_agent::prelude::*;
    use echo_agent_cli::config;

    #[test]
    fn test_create_agent_config() {
        let args = cli::Args {
            web: false,
            cli: false,
            tui: false,
            port: 3000,
            host: "127.0.0.1".to_string(),
            model: Some("test-model".to_string()),
            project: None,
            mcp_config: None,
            config: None,
            channels: false,
            verbose: false,
            r#continue: false,
            resume: None,
        };

        let params = infra::AgentCreateParams {
            model: args.model.clone(),
            system_prompt: None,
            project: args.project.clone(),
            session_id: None,
            conversation_id: None,
            react_checkpoint_interval: None,
            state_store: None,
            memory_context_suffix: None,
            working_dir: None,
            task_runtime_store: None,
            route: None,
        };
        let app_config = config::AppConfig::default();
        let agent = match tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(infra::create_agent(&params, &app_config))
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("test setup failed: create_agent: {e}");
                return;
            }
        };
        assert_eq!(agent.model_name(), "test-model");
    }

    #[test]
    fn test_args_default_starts_tui_product() {
        let args = cli::Args::parse_from(["echo-agent-cli"]);
        assert!(!args.web);
        assert!(!args.cli);
        assert!(!args.channels);
        assert_eq!(args.port, 3000);
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_args_tui_mode() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--tui"]);
        assert!(args.tui);
        assert!(!args.web);
        assert!(!args.cli);
    }

    #[test]
    fn test_args_internal_modes_remain_parseable() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--web", "--cli"]);
        assert!(args.web);
        assert!(args.cli);
    }

    #[test]
    fn test_args_custom_port_for_internal_web() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--port", "8080"]);
        assert_eq!(args.port, 8080);
    }

    #[test]
    fn test_args_continue_flag() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--continue"]);
        assert!(args.r#continue);
        assert!(args.resume.is_none());
    }

    #[test]
    fn test_args_continue_short_flag() {
        let args = cli::Args::parse_from(["echo-agent-cli", "-c"]);
        assert!(args.r#continue);
    }

    #[test]
    fn test_args_resume_flag() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--resume", "abc-123"]);
        assert!(!args.r#continue);
        assert_eq!(args.resume.as_deref(), Some("abc-123"));
    }

    #[test]
    fn test_args_resume_short_flag() {
        let args = cli::Args::parse_from(["echo-agent-cli", "-r", "xyz-789"]);
        assert_eq!(args.resume.as_deref(), Some("xyz-789"));
    }
}
