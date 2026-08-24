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

#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use echo_agent_app_core::config;
#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use echo_agent_cli::cli;
#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use echo_agent_cli::infra;

#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

/// Build a `TaskRuntimeStore` for headless (non-GUI) entry points (TUI / channels).
///
/// Headless modes support complex tasks (TUI/GUI parity), so root authority
/// and cross-process lease failures abort bootstrap instead of silently
/// switching persistence semantics.
#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
fn build_task_runtime_store_for_headless()
-> anyhow::Result<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>> {
    Ok(std::sync::Arc::new(
        echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new()?,
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    echo_agent_cli::configure_data_root()?;

    // Tauri CLI builds the package-name binary (`echo-agent-cli`) and then
    // bundles/renames it. In a GUI-only build, route this binary to the
    // desktop runtime unless the caller explicitly selected the canonical
    // non-interactive JSONL surface.
    #[cfg(all(feature = "gui", not(feature = "tui")))]
    {
        if process_args_request_jsonl(std::env::args_os()) {
            return run_tui_or_cli_entry().await;
        }
        return echo_agent_cli::tauri::desktop::run_desktop_entry().await;
    }

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

#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
async fn run_tui_or_cli_entry() -> anyhow::Result<()> {
    // 解析命令行参数
    let args = cli::Args::parse();

    // 加载 YAML 配置文件
    let mut app_config = config::load_config(args.config.as_deref());
    let configured_mcp_path = app_config.mcp.config_path.clone();
    // Resolve MCP before the generic environment overlay copies
    // MCP_CONFIG_PATH into EkoConfig. This preserves CLI > YAML > env.
    let mcp_config_path = echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(
        args.mcp_config.as_deref(),
        &app_config,
    );
    config::apply_env_overrides(&mut app_config);
    // Keep EkoConfig as the file-backed configuration; the resolved runtime
    // source above owns environment and CLI overrides.
    app_config.mcp.config_path = configured_mcp_path;

    // --verbose 覆盖日志级别为 debug
    if args.verbose {
        app_config.logging.level = "debug".to_string();
    }
    let webhook_emitter = std::sync::Arc::new(
        echo_agent_app_core::webhook::WebhookEmitter::from_config(&app_config),
    );

    let is_tui_entry =
        args.tui || (!args.web && !args.cli && !args.channels && args.jsonl.is_none());

    // 初始化日志。默认用户入口是 TUI，日志必须写入文件，避免污染全屏界面。
    #[cfg(feature = "tui")]
    if is_tui_entry {
        infra::init_logging_for_tui(&app_config.logging.level);
    } else if args.jsonl.is_some() {
        infra::init_logging_for_machine_output(&app_config.logging.level);
    } else {
        infra::init_logging(&app_config.logging.level);
    }

    #[cfg(not(feature = "tui"))]
    {
        if args.jsonl.is_some() {
            infra::init_logging_for_machine_output(&app_config.logging.level);
        } else {
            infra::init_logging(&app_config.logging.level);
        }
    }

    if args.web {
        anyhow::bail!(
            "Web 模式已移除。请使用 Tauri 桌面模式（cargo tauri dev）或 CLI 模式（--cli）。"
        );
    }

    #[cfg(not(feature = "tui"))]
    if is_tui_entry {
        anyhow::bail!("TUI 模式需要 tui feature。请使用: cargo build --features tui");
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
        command_cell_runtime: None,
        execution_scope: None,
    };
    // ── Bootstrap Agent Runtime (shared TUI/GUI initialization) ──
    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params, mcp_config_path)
            .await?;
    let agent_handle = runtime.agent_handle.clone();
    echo_agent_app_core::infra::inject_conversation_store(&agent_handle, &conversation_store);

    // Every headless surface is a full Agent surface. Build one TaskRuntime
    // store, register the same task tools on the primary agent, and inject the
    // store into the shared pool before any pooled agent is created.
    let task_runtime_store = build_task_runtime_store_for_headless()?;
    echo_agent_app_core::tasks::task_runtime::register_task_tools_on_agent(
        &agent_handle,
        task_runtime_store.clone(),
    )
    .await;
    let task_runtime_store = Some(task_runtime_store);
    let pool = {
        let pool = echo_agent_app_core::agent_pool::AgentPool::from_runtime(
            &runtime,
            echo_agent_app_core::agent_pool::PoolConfig::default(),
            task_runtime_store.clone(),
        )
        .await?;
        if let Some(store) = task_runtime_store.clone() {
            echo_agent_app_core::tasks::task_runtime::bind_task_execute_to_pool(
                &agent_handle,
                store,
                &pool,
            )
            .await;
        }
        pool
    };
    let foreground_turns = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();

    if requested_conversation_id.is_some() {
        let store = conversation_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Conversation store is unavailable"))?;
        let conversation = store
            .get_conversation(&conversation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Conversation '{conversation_id}' was not found"))?;
        let stored = store.get_messages(&conversation_id).await?;
        let messages = echo_agent::memory::restore_messages(&stored)?;
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
        if !is_tui_entry && args.jsonl.is_none() {
            println!("Resuming conversation {short_id} from {date}, {message_count} messages");
        }
    }

    // Spawn config watcher (reloads hooks + webhook endpoints on change).
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let config_path = echo_agent_cli::config_watcher::resolve_config_path(args.config.as_deref());
    let config_watcher = std::sync::Arc::new(echo_agent_cli::config_watcher::spawn_config_watcher(
        config_path,
        agent_handle.clone(),
        Some(webhook_emitter.clone()),
        cancel_token.clone(),
    ));

    // ── User-facing TUI mode (default) ─────────────────────────────────
    #[cfg(feature = "tui")]
    if is_tui_entry {
        tracing::info!(
            pool_size = pool.pool_size().await,
            "AgentPool initialized for TUI (background task isolation)"
        );

        // TUI owns its provider for the full-screen session. The REPL provider
        // is registered only by CLI startup, so no provider swap is needed.
        use echo_agent_app_core::hitl::TuiHumanLoopProvider;
        let tui_provider = std::sync::Arc::new(TuiHumanLoopProvider::new());
        let tui_pending = tui_provider.pending_handle();
        let tui_hitl_registration = runtime
            .hitl_dispatcher
            .register_owned("tui", tui_provider.clone())
            .await;
        tracing::info!("HITL: TUI provider registered");
        let tui_services = cli::start_headless_services(
            agent_handle.clone(),
            runtime.hitl_dispatcher.clone(),
            &app_config,
            cli::HeadlessServiceResources {
                model_consumers: runtime.model_consumers.clone(),
                active_model_id: runtime
                    .active_runtime_model
                    .as_ref()
                    .map(|model| model.id.clone())
                    .unwrap_or_default(),
                pool: pool.clone(),
                task_runtime_store: task_runtime_store.clone(),
                webhook_emitter: webhook_emitter.clone(),
                conversation_store: conversation_store.clone(),
                runtime_state_store: runtime.state_store.clone(),
                review_integration: runtime.review_integration.clone(),
                mcp_config_runtime: runtime.mcp_config_runtime.clone(),
                plugin_runtime: runtime.plugin_runtime.clone(),
                config_watcher: config_watcher.clone(),
                foreground_turns: foreground_turns.clone(),
                command_cell_runtime: runtime.command_cell_runtime.clone(),
                browser_runtime: runtime.browser_runtime.clone(),
            },
        )
        .await;
        let tui_services = match tui_services {
            Ok(services) => services,
            Err(error) => {
                tui_provider.close_now("TUI bootstrap failed");
                drop(tui_hitl_registration);
                let error = infra::settle_service_bootstrap_failure(
                    anyhow::anyhow!(error),
                    task_runtime_store.as_ref(),
                    Some(&pool),
                    &runtime.plugin_runtime,
                    &config_watcher,
                    &runtime.mcp_config_runtime,
                    &runtime.browser_runtime,
                )
                .await;
                cancel_token.cancel();
                return Err(error);
            }
        };
        let tui_scheduler = tui_services.scheduler_runner.clone();
        let tui_app_state = tui_services.app_state.clone();
        if let Some(scheduler) = tui_scheduler.as_ref()
            && let Err(error) = runtime
                .plugin_runtime
                .bind_scheduler(scheduler.clone())
                .await
        {
            tracing::warn!(%error, "failed to bind plugin monitors to TUI scheduler");
        }

        let tui_dreaming_owner = runtime.review_integration.as_ref().map(|integration| {
            let cancel = tokio_util::sync::CancellationToken::new();
            let settlement = echo_agent_app_core::infra::spawn_dreaming_task(
                integration.clone(),
                agent_handle.clone(),
                Some(pool.clone()),
                cancel.clone(),
            );
            tracing::info!("Dreaming task spawned for TUI session");
            cli::HeadlessDreamingOwner::new(cancel, settlement)
        });

        let tui_result = echo_agent_cli::tui::run_tui(
            agent_handle.clone(),
            &app_config.tui,
            "💬 通用",
            tui_pending,
            tui_provider.clone(),
            webhook_emitter.clone(),
            tui_scheduler,
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
            runtime.plugin_runtime.clone(),
            tui_app_state.clone(),
            args.no_alt_screen,
        )
        .await;

        tui_provider.close_now("TUI session ended");
        drop(tui_hitl_registration);
        let shutdown_result = cli::shutdown_headless_services(
            tui_result,
            tui_services,
            tui_dreaming_owner,
            Some(agent_handle.clone()),
            runtime.plugin_runtime.clone(),
            config_watcher.clone(),
            runtime.mcp_config_runtime.clone(),
            runtime.browser_runtime.clone(),
            cancel_token.clone(),
        )
        .await;
        drop(runtime);
        return shutdown_result;
    }

    // ── Hidden legacy/internal modes ───────────────────────────────────
    let run_jsonl = args.jsonl.is_some();
    let run_cli = args.cli;
    let run_channels = args.channels;

    // CLI is the sole Reedline/stdin owner. Register its HITL transport
    // before scheduler and TaskRun recovery can emit interactive requests.
    let mut repl_hitl_session = if run_cli {
        Some(cli::ReplHumanLoopSession::register(runtime.hitl_dispatcher.clone()).await)
    } else {
        None
    };

    // CLI-only, channel-only, and combined mode share one application service
    // bootstrap. Surface composition below only owns input/output lifetimes.
    let headless_services = match cli::start_headless_services(
        agent_handle.clone(),
        runtime.hitl_dispatcher.clone(),
        &app_config,
        cli::HeadlessServiceResources {
            model_consumers: runtime.model_consumers.clone(),
            active_model_id: runtime
                .active_runtime_model
                .as_ref()
                .map(|model| model.id.clone())
                .unwrap_or_default(),
            pool: pool.clone(),
            task_runtime_store: task_runtime_store.clone(),
            webhook_emitter: webhook_emitter.clone(),
            conversation_store: conversation_store.clone(),
            runtime_state_store: runtime.state_store.clone(),
            review_integration: runtime.review_integration.clone(),
            mcp_config_runtime: runtime.mcp_config_runtime.clone(),
            plugin_runtime: runtime.plugin_runtime.clone(),
            config_watcher: config_watcher.clone(),
            foreground_turns: foreground_turns.clone(),
            command_cell_runtime: runtime.command_cell_runtime.clone(),
            browser_runtime: runtime.browser_runtime.clone(),
        },
    )
    .await
    {
        Ok(services) => services,
        Err(error) => {
            let hitl_shutdown_error = match repl_hitl_session.take() {
                Some(session) => session.shutdown("CLI bootstrap failed").await.err(),
                None => None,
            };
            let error = infra::settle_service_bootstrap_failure(
                anyhow::anyhow!(error),
                task_runtime_store.as_ref(),
                Some(&pool),
                &runtime.plugin_runtime,
                &config_watcher,
                &runtime.mcp_config_runtime,
                &runtime.browser_runtime,
            )
            .await;
            cancel_token.cancel();
            return match hitl_shutdown_error {
                Some(hitl_error) => Err(anyhow::anyhow!(
                    "{error}; REPL HITL bootstrap shutdown failed: {hitl_error}"
                )),
                None => Err(error),
            };
        }
    };
    if let Some(scheduler) = headless_services.scheduler_runner.as_ref()
        && let Err(error) = runtime
            .plugin_runtime
            .bind_scheduler(scheduler.clone())
            .await
    {
        tracing::warn!(%error, "failed to bind plugin monitors to headless scheduler");
    }

    let dreaming_owner = runtime.review_integration.as_ref().map(|integration| {
        let cancel = tokio_util::sync::CancellationToken::new();
        let settlement = echo_agent_app_core::infra::spawn_dreaming_task(
            integration.clone(),
            agent_handle.clone(),
            Some(pool.clone()),
            cancel.clone(),
        );
        cli::HeadlessDreamingOwner::new(cancel, settlement)
    });

    let mut mode_error: Option<anyhow::Error> = None;
    if run_jsonl {
        let prompt = args.jsonl.as_deref().unwrap_or_default();
        if let Err(error) = cli::run_jsonl_mode(
            agent_handle.clone(),
            prompt,
            conversation_id.clone(),
            &headless_services,
            cli::JsonlRunOptions {
                interaction_mode: args.jsonl_mode,
                permission_mode: args.jsonl_permission,
                approval_policy: args.jsonl_approval,
                attachment_paths: args.jsonl_attachment.clone(),
            },
        )
        .await
        {
            mode_error = Some(error);
        }
    } else if run_channels {
        #[cfg(feature = "channels")]
        {
            tracing::info!(
                pool_size = pool.pool_size().await,
                "AgentPool initialized for channels (IM per-sender agents)"
            );
            let channels_cancel = echo_agent::agent::CancellationToken::new();
            let channels_handle = tokio::spawn(cli::run_channels_mode(cli::ChannelsModeArgs {
                app_state: headless_services.app_state.clone(),
                pool: pool.clone(),
                app_config: app_config.clone(),
                task_runtime_store: task_runtime_store.clone(),
                review_integration: runtime.review_integration.clone(),
                webhook_emitter: webhook_emitter.clone(),
                foreground_turns: foreground_turns.clone(),
                shutdown: channels_cancel.clone(),
            }));

            if run_cli {
                let companion_shutdown =
                    cli::CompanionModeShutdown::new("channels", channels_cancel, channels_handle);
                let cli_result = match repl_hitl_session.take() {
                    Some(session) => {
                        cli::run_cli_mode(
                            agent_handle.clone(),
                            &args,
                            runtime.review_integration.clone(),
                            runtime.prompt_assembly.clone(),
                            pool.clone(),
                            task_runtime_store.clone(),
                            conversation_id.clone(),
                            webhook_emitter.clone(),
                            runtime.plugin_runtime.clone(),
                            &headless_services,
                            session,
                            Some(companion_shutdown),
                        )
                        .await
                    }
                    None => Err(anyhow::anyhow!("CLI HITL session owner is unavailable")),
                };
                if let Err(error) = cli_result {
                    mode_error = Some(error);
                }
            } else {
                match channels_handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => mode_error = Some(error),
                    Err(error) => {
                        mode_error = Some(anyhow::anyhow!(
                            "channel lifecycle owner failed to join: {error}"
                        ));
                    }
                }
            }
        }
        #[cfg(not(feature = "channels"))]
        {
            mode_error = Some(anyhow::anyhow!(
                "--channels 需要启用 channels feature: cargo build --features channels"
            ));
        }
    } else if run_cli {
        // 仅 CLI 模式
        let cli_result = match repl_hitl_session.take() {
            Some(session) => {
                cli::run_cli_mode(
                    agent_handle.clone(),
                    &args,
                    runtime.review_integration.clone(),
                    runtime.prompt_assembly.clone(),
                    pool.clone(),
                    task_runtime_store.clone(),
                    conversation_id.clone(),
                    webhook_emitter.clone(),
                    runtime.plugin_runtime.clone(),
                    &headless_services,
                    session,
                    None,
                )
                .await
            }
            None => Err(anyhow::anyhow!("CLI HITL session owner is unavailable")),
        };
        if let Err(error) = cli_result {
            mode_error = Some(error);
        }
    } else {
        // No legacy mode specified — should have entered TUI above
        mode_error = Some(anyhow::anyhow!(
            "请使用 TUI（默认）、Tauri 桌面模式、或 --cli 模式。"
        ));
    }

    if let Some(session) = repl_hitl_session.take()
        && let Err(error) = session.shutdown("CLI mode did not start").await
    {
        mode_error = Some(match mode_error {
            Some(previous) => anyhow::anyhow!("{previous}; REPL HITL shutdown: {error}"),
            None => anyhow::anyhow!("REPL HITL shutdown: {error}"),
        });
    }
    let mode_result = mode_error.map_or(Ok(()), Err);
    let shutdown_result = cli::shutdown_headless_services(
        mode_result,
        headless_services,
        dreaming_owner,
        (run_cli || run_jsonl).then_some(agent_handle.clone()),
        runtime.plugin_runtime.clone(),
        config_watcher.clone(),
        runtime.mcp_config_runtime.clone(),
        runtime.browser_runtime.clone(),
        cancel_token.clone(),
    )
    .await;
    drop(runtime);
    shutdown_result
}

#[cfg(all(feature = "gui", not(feature = "tui")))]
fn process_args_request_jsonl(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    args.into_iter().skip(1).any(|argument| {
        argument
            .to_str()
            .is_some_and(|argument| argument == "--jsonl" || argument.starts_with("--jsonl="))
    })
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
    use echo_agent_app_core::config;

    #[test]
    fn test_create_agent_config() {
        let args = cli::Args {
            web: false,
            cli: false,
            tui: false,
            no_alt_screen: false,
            jsonl: None,
            jsonl_mode: cli::args::JsonlInteractionMode::Auto,
            jsonl_permission: cli::args::JsonlPermissionMode::Default,
            jsonl_approval: cli::args::JsonlApprovalPolicy::Reject,
            jsonl_attachment: Vec::new(),
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
            command_cell_runtime: None,
            execution_scope: None,
        };
        let mut app_config = config::EkoConfig::default();
        app_config.model.provider = "local-test".to_string();
        app_config.model.name = "test-model".to_string();
        app_config.model.base_url = Some("http://127.0.0.1:11434/v1/chat/completions".to_string());
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
        assert!(args.jsonl.is_none());
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
    fn test_args_accept_jsonl_one_shot_prompt() {
        let args = cli::Args::parse_from([
            "echo-agent-cli",
            "--jsonl",
            "inspect the project",
            "--jsonl-mode",
            "task",
            "--jsonl-permission",
            "full-auto",
            "--jsonl-approval",
            "auto-approve",
            "--jsonl-attachment",
            "/tmp/context.txt",
        ]);
        assert_eq!(args.jsonl.as_deref(), Some("inspect the project"));
        assert_eq!(args.jsonl_mode, cli::args::JsonlInteractionMode::Task);
        assert_eq!(
            args.jsonl_permission,
            cli::args::JsonlPermissionMode::FullAuto
        );
        assert_eq!(
            args.jsonl_approval,
            cli::args::JsonlApprovalPolicy::AutoApprove
        );
        assert_eq!(
            args.jsonl_attachment,
            vec![std::path::PathBuf::from("/tmp/context.txt")]
        );
        assert!(!args.cli);
        assert!(!args.channels);
    }

    #[test]
    fn test_args_custom_port_for_internal_web() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--port", "8080"]);
        assert_eq!(args.port, 8080);
    }

    #[test]
    fn test_args_accepts_explicit_mcp_config_override() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--mcp-config", "/tmp/eko-mcp.json"]);
        assert_eq!(args.mcp_config.as_deref(), Some("/tmp/eko-mcp.json"));
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

#[cfg(all(test, feature = "gui", not(feature = "tui")))]
mod gui_entry_tests {
    use super::process_args_request_jsonl;
    use std::ffi::OsString;

    #[test]
    fn gui_binary_preserves_explicit_jsonl_machine_entry() {
        assert!(process_args_request_jsonl([
            OsString::from("echo-agent-cli"),
            OsString::from("--jsonl"),
            OsString::from("inspect the project"),
        ]));
        assert!(process_args_request_jsonl([
            OsString::from("echo-agent-cli"),
            OsString::from("--jsonl=inspect the project"),
        ]));
        assert!(!process_args_request_jsonl([
            OsString::from("echo-agent-cli"),
            OsString::from("-psn_0_12345"),
        ]));
    }
}
