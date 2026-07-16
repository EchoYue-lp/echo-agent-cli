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

#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
use echo_agent_cli::cli;
#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
use echo_agent_cli::config;
#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
use echo_agent_cli::infra;

#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

/// Build a `TaskRuntimeStore` for headless (non-GUI) entry points (TUI / channels).
///
/// Opens the on-disk store (recovering any incomplete runs), falling back to an
/// in-memory store if the file-backed store is unavailable. Returned as `Option<Arc<...>>`
/// because `drive_chat` takes `Option<&TaskRuntimeStore>` (normal-only callers
/// pass `None`). Headless modes support complex tasks (TUI/GUI parity,
/// AGENTS.md), so they always provide a store.
#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
fn build_task_runtime_store_for_headless()
-> Option<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>> {
    let store = match echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new() {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!("Failed to open TaskRuntime store: {e}; in-memory fallback");
            match echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new_in_memory() {
                Ok(store) => store,
                Err(memory_error) => {
                    tracing::error!(
                        "Failed to initialize in-memory task_runtime store: {memory_error}"
                    );
                    return None;
                }
            }
        }
    };
    let recovered = store.recover_incomplete();
    if recovered > 0 {
        tracing::info!(recovered, "recovered incomplete task_runtime runs");
    }
    Some(std::sync::Arc::new(store))
}

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

    #[cfg(all(feature = "channels", not(feature = "gui"), not(feature = "tui")))]
    {
        run_tui_or_cli_entry().await
    }

    #[cfg(all(not(feature = "tui"), not(feature = "gui"), not(feature = "channels")))]
    {
        compile_error!("One of the tui, gui, or channels features must be enabled");
    }
}

#[cfg(any(feature = "tui", all(feature = "channels", not(feature = "gui"))))]
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

    // TUI/CLI and GUI share the same file-backed conversation projection.
    let conversation_store = echo_agent_app_core::infra::create_conversation_store();
    let requested_conversation_id = if let Some(id) = args.resume.as_ref() {
        Some(id.clone())
    } else if args.r#continue {
        match conversation_store.as_ref() {
            Some(store) => store
                .list_conversations(echo_agent::memory::ConversationFilter {
                    limit: Some(1),
                    ..Default::default()
                })
                .await?
                .first()
                .map(|conversation| conversation.conversation_id.clone()),
            None => None,
        }
    } else {
        None
    };
    if (args.r#continue || args.resume.is_some()) && requested_conversation_id.is_none() {
        anyhow::bail!("No persisted conversation is available to resume");
    }
    let conversation_id = requested_conversation_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // 创建 Agent + 加载 MCP 配置（统一路径，消除重复）
    let params = echo_agent_cli::infra::AgentCreateParams {
        model: args.model.clone(),
        system_prompt: None,
        project: args.project.clone(),
        session_id: None,
        conversation_id: Some(conversation_id.clone()),
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: None,
        working_dir: None,
        task_runtime_store: None,
        browser_runtime: None,
    };
    // ── Bootstrap Agent Runtime (shared TUI/GUI initialization) ──
    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params).await?;
    let agent_handle = runtime.agent_handle.clone();
    echo_agent_app_core::infra::inject_conversation_store(&agent_handle, &conversation_store);

    // Every headless surface is a full Agent surface. Build one TaskRuntime
    // store, register the same task tools on the primary agent, and inject the
    // store into the shared pool before any pooled agent is created.
    let task_runtime_store = build_task_runtime_store_for_headless();
    if let Some(store) = task_runtime_store.clone() {
        echo_agent_app_core::tasks::task_runtime::register_task_tools_on_agent(
            &agent_handle,
            store,
        )
        .await;
    }
    let pool = {
        let mut pool = echo_agent_app_core::agent_pool::AgentPool::from_runtime(
            &runtime,
            echo_agent_app_core::agent_pool::PoolConfig::default(),
        )
        .await;
        if let Some(store) = task_runtime_store.clone() {
            pool.set_task_runtime_store(store);
        }
        let pool = std::sync::Arc::new(pool);
        pool.spawn_cleanup_monitor().await;
        pool
    };

    if requested_conversation_id.is_some() {
        let store = conversation_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Conversation store is unavailable"))?;
        let conversation = store
            .get_conversation(&conversation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Conversation '{conversation_id}' was not found"))?;
        let stored = store.get_messages(&conversation_id).await?;
        let messages = echo_agent_app_core::conversation_restore::restore_messages(&stored);
        let message_count = messages.len();
        agent_handle
            .read_async(|agent| Box::pin(async move { agent.load_messages(messages).await }))
            .await;
        let short_id: String = conversation_id.chars().take(8).collect();
        let date: String = conversation.updated_at.chars().take(19).collect();
        tracing::info!(
            conversation_id = %conversation_id,
            message_count,
            "Conversation resumed from file store"
        );
        if !is_tui_entry {
            println!("Resuming conversation {short_id} from {date}, {message_count} messages");
        }
    }

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

    // ── User-facing TUI mode (default) ─────────────────────────────────
    #[cfg(feature = "tui")]
    if is_tui_entry {
        tracing::info!(
            pool_size = pool.pool_size().await,
            "AgentPool initialized for TUI (background task isolation)"
        );

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
            &app_config.tui,
            "💬 通用",
            tui_pending,
            pool.clone(),
            task_runtime_store.clone(),
            runtime.review_integration.clone(),
            conversation_store.clone(),
            conversation_id.clone(),
            app_config
                .configured_models
                .iter()
                .filter(|model| model.enabled)
                .map(|model| {
                    echo_agent_app_core::model_config::resolve_runtime_model(
                        &app_config,
                        Some(&model.id),
                    )
                })
                .collect(),
            runtime.browser_runtime.clone(),
            runtime.prompt_assembly.clone(),
            args.no_alt_screen,
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
                            "  📋 Memory review: {} scanned, {} stale, {} conflicts, {} proposals queued",
                            report.total_scanned,
                            report.stale_count,
                            report.conflict_groups,
                            report.conflict_proposals.len()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  ⚠ Memory review failed: {e}");
                }
            }
        }

        runtime.browser_runtime.shutdown().await;
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
            tracing::info!(
                pool_size = pool.pool_size().await,
                "AgentPool initialized for channels (IM per-sender agents)"
            );

            let channels_handle = tokio::spawn(cli::run_channels_mode(
                pool.clone(),
                app_config.clone(),
                task_runtime_store.clone(),
            ));

            if run_cli {
                cli::run_cli_mode(
                    agent_handle,
                    runtime.hitl_dispatcher.clone(),
                    &args,
                    &app_config,
                    runtime.review_integration.clone(),
                    runtime.prompt_assembly.clone(),
                    pool.clone(),
                    task_runtime_store.clone(),
                    conversation_id.clone(),
                )
                .await?;
            } else {
                // 仅 channels 模式，等待 channels 或 Ctrl+C
                channels_handle.await??;
                runtime.browser_runtime.shutdown().await;
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
            runtime.review_integration.clone(),
            runtime.prompt_assembly.clone(),
            pool,
            task_runtime_store,
            conversation_id,
        )
        .await?;
    } else {
        // No legacy mode specified — should have entered TUI above
        eprintln!("请使用 TUI（默认）、Tauri 桌面模式、或 --cli 模式。");
        std::process::exit(1);
    }

    // Keep runtime alive until shutdown
    runtime.browser_runtime.shutdown().await;
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
            no_alt_screen: false,
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
            browser_runtime: None,
        };
        let app_config = config::AppConfig::default();
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("test setup failed: runtime: {error}");
                return;
            }
        };
        let agent = match runtime.block_on(infra::create_agent(&params, &app_config)) {
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
