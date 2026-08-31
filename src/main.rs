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
use echo_agent_app_core::api::config;
#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use echo_agent_cli::cli;
#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use echo_agent_cli::infra;

#[cfg(any(feature = "tui", feature = "gui", feature = "channels"))]
use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

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
    let mcp_config_path = echo_agent_app_core::api::mcp_config_runtime::resolve_mcp_config_path(
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
    let conversation_store = echo_agent_app_core::api::infra::create_conversation_store();
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
        product_data_io: None,
        execution_scope: None,
    };
    // ── Bootstrap Agent Runtime (shared TUI/GUI initialization) ──
    let runtime = echo_agent_app_core::api::runtime::AgentRuntime::bootstrap(
        &app_config,
        params,
        mcp_config_path,
    )
    .await?;
    let agent_handle = runtime.agent_handle.clone();

    let run_jsonl = args.jsonl.is_some();
    let run_cli = args.cli;
    let run_channels = args.channels;

    // Register interactive transports before application recovery can emit an
    // attended request. Surface rendering still starts only after composition.
    #[cfg(feature = "tui")]
    let mut tui_session = if is_tui_entry {
        use echo_agent_app_core::api::hitl::TuiHumanLoopProvider;
        let provider = std::sync::Arc::new(TuiHumanLoopProvider::new());
        let pending = provider.pending_handle();
        let registration = runtime
            .hitl_dispatcher
            .register_owned("tui", provider.clone())
            .await;
        tracing::info!("HITL: TUI provider registered");
        Some((provider, pending, registration))
    } else {
        None
    };

    // CLI is the sole Reedline/stdin owner. Register its HITL transport
    // before scheduler and TaskRun recovery can emit interactive requests.
    let mut repl_hitl_session = if run_cli {
        Some(cli::ReplHumanLoopSession::register(runtime.hitl_dispatcher.clone()).await)
    } else {
        None
    };

    #[cfg_attr(not(feature = "channels"), allow(unused_mut))]
    let mut application_services =
        match echo_agent_app_core::api::runtime::ApplicationServices::compose(
            &runtime,
            args.config.as_deref(),
            conversation_store.clone(),
            echo_agent::tools::permission::PermissionMode::Default,
        )
        .await
        {
            Ok(services) => services,
            Err(error) => {
                #[cfg(feature = "tui")]
                if let Some((provider, _, registration)) = tui_session.take() {
                    provider.close_now("TUI bootstrap failed");
                    drop(registration);
                }
                let hitl_shutdown_error = match repl_hitl_session.take() {
                    Some(session) => session.shutdown("CLI bootstrap failed").await.err(),
                    None => None,
                };
                return match hitl_shutdown_error {
                    Some(hitl_error) => Err(anyhow::anyhow!(
                        "{error}; REPL HITL bootstrap shutdown failed: {hitl_error}"
                    )),
                    None => Err(error),
                };
            }
        };

    if requested_conversation_id.is_some() {
        let restore_result = async {
            let store = conversation_store
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Conversation store is unavailable"))?;
            let conversation = store
                .get_conversation(&conversation_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Conversation '{conversation_id}' was not found"))?;
            let stored = store.get_messages(&conversation_id).await?;
            let messages = echo_agent::memory::restore_messages(&stored)?;
            Ok::<_, anyhow::Error>((conversation, messages))
        }
        .await;
        let (conversation, messages) = match restore_result {
            Ok(restored) => restored,
            Err(error) => {
                #[cfg(feature = "tui")]
                if let Some((provider, _, registration)) = tui_session.take() {
                    provider.close_now("TUI conversation restore failed");
                    drop(registration);
                }
                if let Some(session) = repl_hitl_session.take() {
                    let _ = session.shutdown("CLI conversation restore failed").await;
                }
                let receipt = application_services
                    .settle(
                        echo_agent_app_core::api::runtime::ApplicationLifecycleReason::BootstrapRollback,
                        Some(error),
                    )
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
        };
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

    let pool = application_services.pool.clone();
    let task_runtime_store = application_services.app_state.tasks.runtime.clone();
    let webhook_emitter = application_services.app_state.webhook.emitter.clone();

    // ── User-facing TUI mode (default) ─────────────────────────────────
    #[cfg(feature = "tui")]
    if is_tui_entry {
        let (tui_provider, tui_pending, tui_hitl_registration) = match tui_session.take() {
            Some(session) => session,
            None => {
                let receipt = application_services
                    .settle(
                        echo_agent_app_core::api::runtime::ApplicationLifecycleReason::BootstrapRollback,
                        Some(anyhow::anyhow!("TUI HITL session owner is unavailable")),
                    )
                    .await;
                return Err(anyhow::Error::new(receipt.into_error()));
            }
        };
        tracing::info!(
            pool_size = pool.pool_size().await,
            "AgentPool initialized for TUI (background task isolation)"
        );
        let tui_result = echo_agent_cli::tui::run_tui(
            agent_handle.clone(),
            &app_config.tui,
            "💬 通用",
            tui_pending,
            tui_provider.clone(),
            webhook_emitter.clone(),
            application_services.app_state.scheduler.runner.clone(),
            conversation_store.clone(),
            conversation_id.clone(),
            app_config
                .configured_models
                .iter()
                .filter(|model| model.enabled)
                .filter_map(|model| {
                    echo_agent_app_core::api::model_config::resolve_runtime_model(
                        &app_config,
                        Some(&model.id),
                    )
                    .ok()
                })
                .collect(),
            runtime.browser_runtime.clone(),
            runtime.prompt_assembly.clone(),
            runtime.plugin_runtime.clone(),
            application_services.app_state.clone(),
            args.no_alt_screen,
        )
        .await;

        tui_provider.close_now("TUI session ended");
        drop(tui_hitl_registration);
        let shutdown_result = cli::shutdown_application_services(
            tui_result,
            application_services,
            Some(agent_handle.clone()),
        )
        .await;
        drop(runtime);
        return shutdown_result;
    }

    let mut mode_error: Option<anyhow::Error> = None;
    if run_jsonl {
        let prompt = args.jsonl.as_deref().unwrap_or_default();
        if let Err(error) = cli::run_jsonl_mode(
            agent_handle.clone(),
            prompt,
            conversation_id.clone(),
            &application_services,
            cli::JsonlRunOptions {
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
                app_state: application_services.app_state.clone(),
                app_config: app_config.clone(),
                webhook_emitter: webhook_emitter.clone(),
                foreground_turns: application_services
                    .app_state
                    .session
                    .foreground_turns
                    .clone(),
                shutdown: channels_cancel.clone(),
            }));
            let channel_observer =
                cli::CompanionModeShutdown::new("channels", channels_cancel, channels_handle)
                    .bind(&mut application_services)?;

            if run_cli {
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
                            &application_services,
                            session,
                        )
                        .await
                    }
                    None => Err(anyhow::anyhow!("CLI HITL session owner is unavailable")),
                };
                if let Err(error) = cli_result {
                    mode_error = Some(error);
                }
            } else {
                tokio::select! {
                    result = channel_observer.wait() => {
                        if let Err(error) = result {
                            mode_error = Some(error);
                        }
                    }
                    _ = echo_agent_cli::infra::shutdown_signal() => {}
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
                    &application_services,
                    session,
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
    let shutdown_result = cli::shutdown_application_services(
        mode_result,
        application_services,
        (run_cli || run_jsonl).then_some(agent_handle.clone()),
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
    use echo_agent_app_core::api::config;

    #[test]
    fn test_create_agent_config() {
        let args = cli::Args {
            web: false,
            cli: false,
            tui: false,
            no_alt_screen: false,
            jsonl: None,
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
            product_data_io: Some(
                echo_agent_app_core::api::product_data_io::ProductDataIoService::new(),
            ),
            execution_scope: None,
        };
        let mut app_config = config::EkoConfig::default();
        app_config.model.default_model_id = Some("local-test:test-model".to_string());
        app_config.model_providers.insert(
            "local-test".to_string(),
            config::ModelProviderConfig {
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        app_config.configured_models.push(config::ConfiguredModel {
            id: "local-test:test-model".to_string(),
            provider: "local-test".to_string(),
            model: "test-model".to_string(),
            ..Default::default()
        });
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
            "--jsonl-permission",
            "full-auto",
            "--jsonl-approval",
            "auto-approve",
            "--jsonl-attachment",
            "/tmp/context.txt",
        ]);
        assert_eq!(args.jsonl.as_deref(), Some("inspect the project"));
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
