//! EchoCoWork — Tauri desktop runtime.
//!
//! Shared by the dedicated `echo-agent-tauri` binary and by the package-name
//! binary when Tauri CLI builds the app with `--no-default-features --features gui`.

use crate::{cli, config, config_watcher, infra, state::AppState};
use clap::Parser;
use std::sync::Arc;

/// Crash log path — written to when the app panics before Tauri starts.
/// This is the only way to debug silent crashes on macOS (no terminal attached).
fn crash_log_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home)
        .join(".echo-agent")
        .join("crash.log")
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
            "EchoCoWork crashed at {}\n\
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
                    "display dialog \"EchoCoWork crashed during startup.\\n\\n\
                     Details have been written to the crash log.\\n\\n\
                     Run from Terminal to see full output:\\n\
                     /Applications/EchoCoWork.app/Contents/MacOS/echo-agent-cli\" \
                     with title \"EchoCoWork\" buttons {\"OK\"} default button \"OK\"",
                )
                .output();
        }
    }));
}

/// Run the Tauri desktop app and report startup failures to a crash log.
pub async fn run_desktop_entry() -> anyhow::Result<()> {
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
                "EchoCoWork starting at {}\n\
                 If you see this file, the app is running but may have failed to display.\n\
                 Try running from Terminal:\n\
                 /Applications/EchoCoWork.app/Contents/MacOS/echo-agent-cli\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            ),
        );
    }

    if let Err(e) = run_desktop().await {
        let log_path = crash_log_path();
        let message = format!(
            "EchoCoWork failed to start at {}\n\nError: {:?}\n",
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
                    "display dialog \"EchoCoWork failed to start.\\n\\n\
                     Error: {}\\n\\n\
                     Crash log: {}\" \
                     with title \"EchoCoWork\" buttons {{\"OK\"}} default button \"OK\"",
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

    let args = cli::Args::parse_from(["echo-agent-tauri"]);
    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

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
    };

    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params).await?;
    let agent_handle = runtime.agent_handle.clone();

    // ── Config watcher ──
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if let Some(config_path) = config_watcher::resolve_config_path(args.config.as_deref()) {
        config_watcher::spawn_config_watcher(
            config_path,
            agent_handle.clone(),
            cancel_token.clone(),
        );
    }

    // ── Task store ──
    let task_store: Arc<dyn echo_agent::memory::Store> = {
        let db_path = echo_agent_app_core::persistence::Persistence::base_dir().join("tasks.db");
        match echo_agent::memory::SqliteStore::new(&db_path) {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(echo_agent::memory::InMemoryStore::new()),
        }
    };

    // ── Build application state ──
    let conversation_store = infra::create_conversation_store();
    infra::inject_conversation_store(&agent_handle, &conversation_store);

    // ── Initialize agent pool for multi-conversation parallel execution ──
    // init_pool() also starts the cleanup monitor automatically.
    let pool = runtime
        .init_pool(echo_agent_app_core::agent_pool::PoolConfig::default())
        .await;
    tracing::info!(
        pool_size = pool.pool_size().await,
        "AgentPool initialized for GUI (cleanup monitor started)"
    );

    let mut state_inner = AppState::from_shared(
        agent_handle.clone(),
        runtime.hitl_dispatcher.clone(),
        conversation_store,
        app_config.clone(),
    )
    .with_review_integration(runtime.review_integration.clone());
    state_inner.set_pool(pool);

    // Wire task hook bridge so YAML hooks see task lifecycle events
    let task_hooks = runtime
        .task_hook_bridge
        .clone()
        .map(|b| b as Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>);
    state_inner
        .start_task_service_with_hooks(task_store.clone(), task_hooks)
        .await;
    state_inner.start_scheduler_with_store(Some(task_store));
    let state = Arc::new(state_inner);

    infra::spawn_mcp_health_check(state.clone(), cancel_token.clone());

    // ── Launch Tauri window ──
    crate::tauri::build_tauri_app(state.clone())
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");

    // Tauri window closed → cancel background tasks
    cancel_token.cancel();

    Ok(())
}
