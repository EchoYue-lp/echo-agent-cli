//! EKO — Tauri desktop runtime.
//!
//! Shared by the dedicated `echo-agent-tauri` binary and by the package-name
//! binary when Tauri CLI builds the app with `--no-default-features --features gui`.

use crate::{cli, config_watcher, infra, state::AppState};
use clap::Parser;
use echo_agent::config;
use std::ffi::OsString;
use std::sync::Arc;

/// Crash log path — written to when the app panics before Tauri starts.
/// This is the only way to debug silent crashes on macOS (no terminal attached).
fn crash_log_path() -> std::path::PathBuf {
    echo_agent::paths::user_data_path("crash.log")
}

/// Install a panic hook that writes the panic message to a crash log file.
/// Without this, macOS .app launches that panic produce no visible output.
fn install_panic_hook() {
    let log_path = crash_log_path();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Also call the default hook (writes to stderr, which is lost in .app)
        default_hook(info);

        // Write to crash log so the user can inspect it
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let message = format!(
            "EKO crashed at {}\n\
             Location: {:?}\n\n\
             {}\n\n\
             Please report this issue or check your configuration.\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            info.location(),
            info
        );
        let _ = std::fs::write(&log_path, &message);

        // On macOS, also try to show a native dialog. P1-7: do NOT interpolate
        // the panic payload (`info`) or the log path into the AppleScript
        // string — a panic message can contain arbitrary text (including
        // user-controlled filenames) and `replace('"', "\\\"")` is not a
        // complete AppleScript escape (backslashes, braces, `}`/`{` all break
        // out). The dialog now uses a fixed string and only points the user at
        // the crash log file; full details live in the log written above, not
        // in the dialog.
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            let _ = Command::new("osascript")
                .arg("-e")
                .arg(
                    "display dialog \"EKO crashed during startup.\\n\\n\
                     Details have been written to the crash log.\\n\\n\
                     Run from Terminal to see full output:\\n\
                     /Applications/EKO.app/Contents/MacOS/echo-agent-cli\" \
                     with title \"EKO\" buttons {\"OK\"} default button \"OK\"",
                )
                .output();
        }
    }));
}

/// Run the Tauri desktop app and report startup failures to a crash log.
pub async fn run_desktop_entry() -> anyhow::Result<()> {
    // 统一全局根目录为 ~/.eko。必须在 crash_log_path()/init_logging 等任何路径
    // 解析之前调用(dedicated echo-agent-tauri bin 直接进这里)。
    let _ = echo_agent::paths::set_user_data_dir_name(".eko");
    // 插件数据同步落到 ~/.eko/plugins(默认 ~/.echo-agent/plugins,审计 P0-3)。
    let _ = echo_agent::plugin::set_plugin_data_base_dir_name(".eko");

    install_panic_hook();

    // Log startup to crash log (overwrite previous crash)
    {
        let log_path = crash_log_path();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &log_path,
            format!(
                "EKO starting at {}\n\
                 If you see this file, the app is running but may have failed to display.\n\
                 Try running from Terminal:\n\
                 /Applications/EKO.app/Contents/MacOS/echo-agent-cli\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            ),
        );
    }

    if let Err(e) = run_desktop().await {
        let log_path = crash_log_path();
        let message = format!(
            "EKO failed to start at {}\n\nError: {:?}\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            e
        );
        let _ = std::fs::write(&log_path, &message);
        eprintln!("{}", message);

        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            let _ = Command::new("osascript")
                .arg("-e")
                .arg(format!(
                    "display dialog \"EKO failed to start.\\n\\n\
                     Error: {}\\n\\n\
                     Crash log: {}\" \
                     with title \"EKO\" buttons {{\"OK\"}} default button \"OK\"",
                    format!("{:?}", e).replace('"', "\\\""),
                    log_path.display()
                ))
                .output();
        }
    }

    Ok(())
}

async fn run_desktop() -> anyhow::Result<()> {
    // Load project-local .env file (standard dotenvy behavior)
    dotenvy::dotenv().ok();

    // On macOS, GUI apps launched from Dock/Finder don't inherit shell env vars.
    // Spawn the user's login shell to pick up API keys from ~/.zshrc etc.
    infra::load_shell_env();

    // The desktop binary accepts the same model/project/config overrides as
    // TUI/CLI, including the canonical `--mcp-config` source.
    let args = parse_desktop_args(std::env::args_os())?;
    let mut app_config = config::load_config(args.config.as_deref());
    let configured_mcp_path = app_config.mcp.config_path.clone();
    // Resolve MCP before the generic environment overlay copies
    // MCP_CONFIG_PATH into AppConfig. This preserves CLI > YAML > env.
    let mcp_config_path = echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(
        args.mcp_config.as_deref(),
        &app_config,
    );
    config::apply_env_overrides(&mut app_config);
    // Keep AppConfig as the file-backed configuration; the resolved runtime
    // source above owns environment and CLI overrides.
    app_config.mcp.config_path = configured_mcp_path;
    let webhook_emitter = Arc::new(echo_agent_app_core::webhook::WebhookEmitter::from_config(
        &app_config,
    ));

    infra::init_logging(&app_config.logging.level);

    // ── Bootstrap Agent Runtime (shared TUI/GUI initialization) ──
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
        // The TaskRuntimeStore doesn't exist yet at primary-agent build time
        // (AppState creates it later). Tools are registered post-hoc via
        // `register_task_tools_on_agent` once AppState is built.
        task_runtime_store: None,
        browser_runtime: None,
    };

    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params, mcp_config_path)
            .await?;
    let agent_handle = runtime.agent_handle.clone();

    // ── Config watcher ──
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let config_path = config_watcher::resolve_config_path(args.config.as_deref());
    let config_save_path = config_watcher::resolve_config_save_path(args.config.as_deref());
    config_watcher::spawn_config_watcher(
        config_path,
        agent_handle.clone(),
        Some(webhook_emitter.clone()),
        cancel_token.clone(),
    );

    // Cron definitions are independent of TaskRun lifecycle state.
    let scheduler_store: Arc<dyn echo_agent::memory::Store> = {
        let file_path =
            echo_agent_app_core::persistence::Persistence::base_dir().join("scheduler_store");
        match echo_agent::memory::FileStore::new(&file_path) {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(echo_agent::memory::InMemoryStore::new()),
        }
    };

    // ── Build application state ──
    let conversation_store = infra::create_conversation_store();
    infra::inject_conversation_store(&agent_handle, &conversation_store);

    let mut state_inner = AppState::from_shared(
        agent_handle.clone(),
        runtime.hitl_dispatcher.clone(),
        conversation_store,
        app_config.clone(),
        runtime.mcp_config_runtime.clone(),
    )
    .with_config_path(config_save_path)
    .with_review_integration(runtime.review_integration.clone())
    .with_prompt_assembly(runtime.prompt_assembly.clone())
    .with_plugin_runtime(Some(runtime.plugin_runtime.clone()));
    state_inner.webhook.emitter = webhook_emitter;

    // Build task tools before the pool extracts the shared ToolManager, and
    // pass the store into pool construction before its background agent exists.
    if let Some(task_store) = state_inner.tasks.runtime.clone() {
        echo_agent_app_core::tasks::task_runtime::register_task_tools_on_agent(
            &agent_handle,
            task_store,
        )
        .await;
    }

    // ── Initialize agent pool for multi-conversation parallel execution ──
    // init_pool() also starts the cleanup monitor automatically.
    let pool = runtime
        .init_pool(
            echo_agent_app_core::agent_pool::PoolConfig::default(),
            state_inner.tasks.runtime.clone(),
        )
        .await;
    if let Some(task_store) = state_inner.tasks.runtime.clone() {
        echo_agent_app_core::tasks::task_runtime::bind_task_execute_to_pool(
            &agent_handle,
            task_store,
            &pool,
        )
        .await;
    }
    tracing::info!(
        pool_size = pool.pool_size().await,
        "AgentPool initialized for GUI (cleanup monitor started)"
    );

    state_inner.set_pool(pool);

    state_inner.start_task_service().await;
    state_inner
        .start_scheduler_with_store(Some(scheduler_store))
        .await?;
    if let Some(scheduler) = state_inner.scheduler.runner.as_ref()
        && let Err(error) = runtime
            .plugin_runtime
            .bind_scheduler(scheduler.clone())
            .await
    {
        tracing::warn!(%error, "failed to bind plugin monitors to GUI scheduler");
    }
    let state = Arc::new(state_inner);

    infra::spawn_mcp_health_check(state.clone(), cancel_token.clone());

    // (stage4 F1) Dreaming runs once after boot and then daily in every mode.
    if let Some(ri) = state.review_integration.clone() {
        infra::spawn_dreaming_task(
            ri,
            agent_handle.clone(),
            state.connection.pool.clone(),
            cancel_token.clone(),
        );
    }

    // ── Launch Tauri window ──
    let terminal_manager = Arc::new(crate::tauri::terminal::TerminalManager::new());
    let bridge_supervisor = Arc::new(crate::tauri::state::TauriBridgeSupervisor::new());
    let tauri_result = crate::tauri::build_tauri_app(
        state.clone(),
        runtime.browser_runtime.clone(),
        terminal_manager.clone(),
        bridge_supervisor.clone(),
    )
    .run(tauri::generate_context!());

    // Tauri window closed → cancel background tasks
    cancel_token.cancel();
    runtime.mcp_config_runtime.shutdown().await;
    bridge_supervisor.shutdown().await;
    terminal_manager.close_all().await;
    if let Err(error) = state.shutdown_scheduler().await {
        tracing::warn!(%error, "failed to shut down GUI scheduler");
    }
    if let Some(store) = state.tasks.runtime.as_ref()
        && let Err(error) = store.shutdown_hook_events().await
    {
        tracing::warn!(%error, "failed to shut down task hook dispatcher");
    }
    runtime.browser_runtime.shutdown().await;
    tauri_result.map_err(|e| anyhow::anyhow!("error while running Tauri application: {e}"))?;

    Ok(())
}

fn parse_desktop_args(args: impl IntoIterator<Item = OsString>) -> Result<cli::Args, clap::Error> {
    let args = args
        .into_iter()
        // Finder may add a process-serial-number argument when launching a
        // macOS application bundle. It is platform metadata, not an EKO flag.
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"));
    cli::Args::try_parse_from(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_args_preserve_explicit_mcp_override() -> anyhow::Result<()> {
        let args = parse_desktop_args(
            ["echo-agent-tauri", "--mcp-config", "/tmp/eko-mcp.json"]
                .into_iter()
                .map(OsString::from),
        )?;
        assert_eq!(args.mcp_config.as_deref(), Some("/tmp/eko-mcp.json"));
        Ok(())
    }

    #[test]
    fn desktop_args_ignore_macos_finder_process_serial_number() -> anyhow::Result<()> {
        let args = parse_desktop_args(
            [
                "echo-agent-tauri",
                "-psn_0_12345",
                "--mcp-config",
                "/tmp/eko-mcp.json",
            ]
            .into_iter()
            .map(OsString::from),
        )?;
        assert_eq!(args.mcp_config.as_deref(), Some("/tmp/eko-mcp.json"));
        Ok(())
    }
}
