//! EKO — Tauri desktop runtime.
//!
//! Shared by the dedicated `echo-agent-tauri` binary and by the package-name
//! binary when Tauri CLI builds the app with `--no-default-features --features gui`.

use crate::{cli, infra};
use clap::Parser;
use echo_agent_app_core::api::config;
use std::ffi::OsString;
use std::sync::Arc;

/// Crash log path — written to when the app panics before Tauri starts.
/// This is the only way to debug silent crashes on macOS (no terminal attached).
fn crash_log_path() -> std::path::PathBuf {
    echo_agent_app_core::api::data_root::user_data_path("crash.log")
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

/// Run the Tauri desktop app and report lifecycle failures to a crash log.
pub async fn run_desktop_entry() -> anyhow::Result<()> {
    // Must run before crash_log_path(), logging, or any Store resolves a path.
    crate::configure_data_root()?;

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

    match run_desktop().await {
        Ok(()) => Ok(()),
        Err(error) => {
            let log_path = crash_log_path();
            let message = format!(
                "EKO failed at {}\n\nError: {:?}\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                error
            );
            let _ = std::fs::write(&log_path, &message);
            eprintln!("{}", message);

            #[cfg(target_os = "macos")]
            {
                use std::process::Command;
                let _ = Command::new("osascript")
                    .arg("-e")
                    .arg(format!(
                        "display dialog \"EKO failed.\\n\\n\
                         Error: {}\\n\\n\
                         Crash log: {}\" \
                         with title \"EKO\" buttons {{\"OK\"}} default button \"OK\"",
                        format!("{:?}", error).replace('"', "\\\""),
                        log_path.display()
                    ))
                    .output();
            }
            Err(error)
        }
    }
}

async fn run_desktop() -> anyhow::Result<()> {
    // Load project-local .env file (standard dotenvy behavior)
    dotenvy::dotenv().ok();

    // The desktop binary accepts the same model/project/config overrides as
    // TUI/CLI, including the canonical `--mcp-config` source.
    let args = parse_desktop_args(std::env::args_os())?;
    let mut app_config = config::load_config(args.config.as_deref());
    // On macOS, GUI apps launched from Dock/Finder don't inherit shell env
    // vars. Import only names explicitly declared by configured providers.
    infra::load_shell_env(&app_config);
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
    infra::init_logging(&app_config.logging.level);

    let tauri_context: tauri::Context<tauri::Wry> = tauri::generate_context!();

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
        command_cell_runtime: None,
        product_data_io: None,
        execution_scope: None,
    };

    let runtime = echo_agent_app_core::api::runtime::AgentRuntime::bootstrap(
        &app_config,
        params,
        mcp_config_path,
    )
    .await?;
    let conversation_store = infra::create_conversation_store();
    let mut services = echo_agent_app_core::api::runtime::ApplicationServices::compose(
        &runtime,
        args.config.as_deref(),
        conversation_store,
        echo_agent::tools::permission::PermissionMode::Default,
    )
    .await?;
    let state = services.app_state.clone();
    tracing::info!(
        pool_size = services.pool.pool_size().await,
        "AgentPool initialized for GUI"
    );

    // ── Launch Tauri window ──
    let bridge_supervisor = Arc::new(crate::tauri::state::TauriBridgeSupervisor::new());
    let bridge_begin = bridge_supervisor.clone();
    let bridge_join = bridge_supervisor.clone();
    services.track_external_owner(
        "Tauri event bridges",
        move || {
            bridge_begin.begin_shutdown();
            Ok(())
        },
        async move { bridge_join.join().await },
    )?;
    let builder = crate::tauri::build_tauri_app(
        state,
        runtime.browser_runtime.clone(),
        services.subagent_projection.clone(),
        bridge_supervisor,
    );
    run_desktop_with_settlement(
        move || {
            let app = builder.build(tauri_context)?;
            // App::run exits the process, bypassing our application owner.
            // Returning the loop keeps Tokio alive until every accepted service
            // settles. Repeated exit requests still produce only one return.
            Ok(app.run_return(|_, _| {}))
        },
        move |primary_error| {
            services.settle(
                echo_agent_app_core::api::runtime::ApplicationLifecycleReason::Shutdown,
                primary_error,
            )
        },
    )
    .await
}

async fn run_desktop_with_settlement<Run, Settle, Settlement>(
    run: Run,
    settle: Settle,
) -> anyhow::Result<()>
where
    Run: FnOnce() -> anyhow::Result<i32>,
    Settle: FnOnce(Option<anyhow::Error>) -> Settlement,
    Settlement: std::future::Future<Output = echo_agent_app_core::api::runtime::ApplicationLifecycleReceipt>,
{
    let primary_error = match run() {
        Ok(0) => None,
        Ok(code) => Some(anyhow::anyhow!(
            "Tauri application exited with status {code}"
        )),
        Err(error) => Some(anyhow::anyhow!(
            "error while running Tauri application: {error}"
        )),
    };
    // The same boundary handles builder failures and normal loop completion;
    // neither path may return before the existing service owner joins.
    let receipt = settle(primary_error).await;
    receipt.into_result().map_err(anyhow::Error::new)
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
    use echo_agent_app_core::api::runtime::{
        ApplicationLifecycleError, ApplicationLifecycleOwner, ApplicationLifecycleReason,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

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

    #[tokio::test]
    async fn desktop_returning_loop_settles_once_after_repeated_exit_requests() -> anyhow::Result<()>
    {
        let cancel = CancellationToken::new();
        let mut owner = ApplicationLifecycleOwner::new(cancel.clone());
        let exit_requests = Arc::new(AtomicUsize::new(0));
        let begins = Arc::new(AtomicUsize::new(0));
        let joins = Arc::new(AtomicUsize::new(0));
        let begin_count = begins.clone();
        let join_count = joins.clone();
        owner.track_external_owner(
            "desktop test service",
            move || {
                begin_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            async move {
                join_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        let requested = exit_requests.clone();

        run_desktop_with_settlement(
            move || {
                // Model several requests observed by a returning event loop.
                // Its one return, rather than each request, triggers settlement.
                for _ in 0..3 {
                    requested.fetch_add(1, Ordering::SeqCst);
                }
                Ok(0)
            },
            |error| owner.settle(ApplicationLifecycleReason::Shutdown, error),
        )
        .await?;

        assert_eq!(exit_requests.load(Ordering::SeqCst), 3);
        assert_eq!(begins.load(Ordering::SeqCst), 1);
        assert_eq!(joins.load(Ordering::SeqCst), 1);
        assert!(cancel.is_cancelled());
        Ok(())
    }

    #[tokio::test]
    async fn desktop_exit_waits_for_cancelled_service_to_finish() -> anyhow::Result<()> {
        let mut owner = ApplicationLifecycleOwner::new(CancellationToken::new());
        let bridges = Arc::new(crate::tauri::state::TauriBridgeSupervisor::new());
        let reservation = bridges.reserve()?;
        let cancel = bridges.cancellation_token();
        let (cancel_seen_tx, cancel_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let joined = Arc::new(AtomicUsize::new(0));
        let joined_count = joined.clone();
        reservation.track(tokio::spawn(async move {
            cancel.cancelled().await;
            let _ = cancel_seen_tx.send(());
            let _ = release_rx.await;
            joined_count.fetch_add(1, Ordering::SeqCst);
        }));
        let bridge_begin = bridges.clone();
        owner.track_external_owner(
            "Tauri event bridges",
            move || {
                bridge_begin.begin_shutdown();
                Ok(())
            },
            async move { bridges.join().await },
        );
        let exit = tokio::spawn(run_desktop_with_settlement(
            || Ok(0),
            |error| owner.settle(ApplicationLifecycleReason::Shutdown, error),
        ));

        timeout(TEST_TIMEOUT, cancel_seen_rx).await??;
        let still_waiting = !exit.is_finished();
        release_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("desktop service ended before release"))?;
        timeout(TEST_TIMEOUT, exit).await???;

        assert!(still_waiting);
        assert_eq!(joined.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn desktop_builder_failure_still_cancels_and_joins_services() -> anyhow::Result<()> {
        let cancel = CancellationToken::new();
        let mut owner = ApplicationLifecycleOwner::new(cancel.clone());
        let joins = Arc::new(AtomicUsize::new(0));
        let joined_count = joins.clone();
        owner.track_external_owner("desktop service", || Ok(()), async move {
            joined_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let error = run_desktop_with_settlement(
            || Err(anyhow::anyhow!("desktop builder failed")),
            |error| owner.settle(ApplicationLifecycleReason::Shutdown, error),
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("desktop builder failure was reported as success"))?;
        let error = error
            .downcast_ref::<ApplicationLifecycleError>()
            .ok_or_else(|| anyhow::anyhow!("desktop error lost its lifecycle receipt"))?;

        assert!(cancel.is_cancelled());
        assert_eq!(joins.load(Ordering::SeqCst), 1);
        assert!(error.receipt.failures.is_empty());
        assert!(
            error
                .receipt
                .primary_error
                .as_deref()
                .is_some_and(|message| message.contains("desktop builder failed"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn desktop_cleanup_failure_is_reported_after_later_services_join() -> anyhow::Result<()> {
        let mut owner = ApplicationLifecycleOwner::new(CancellationToken::new());
        owner.track_external_owner(
            "failed desktop service",
            || Err("desktop admission failed".to_string()),
            async { Err("desktop join failed".to_string()) },
        );
        let joins = Arc::new(AtomicUsize::new(0));
        let joined_count = joins.clone();
        owner.track_external_owner("later desktop service", || Ok(()), async move {
            joined_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let error = run_desktop_with_settlement(
            || Ok(0),
            |error| owner.settle(ApplicationLifecycleReason::Shutdown, error),
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("desktop cleanup failure was reported as success"))?;
        let error = error
            .downcast_ref::<ApplicationLifecycleError>()
            .ok_or_else(|| anyhow::anyhow!("desktop error lost its lifecycle receipt"))?;

        assert_eq!(joins.load(Ordering::SeqCst), 1);
        assert!(error.receipt.primary_error.is_none());
        assert_eq!(error.receipt.failures.len(), 2);
        assert!(error.receipt.failures.iter().all(|failure| {
            failure.owner == "failed desktop service" && failure.error.contains("desktop")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn desktop_nonzero_exit_is_not_reported_as_success() -> anyhow::Result<()> {
        let cancel = CancellationToken::new();
        let owner = ApplicationLifecycleOwner::new(cancel.clone());
        let error = run_desktop_with_settlement(
            || Ok(17),
            |error| owner.settle(ApplicationLifecycleReason::Shutdown, error),
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("nonzero desktop exit was reported as success"))?;
        let error = error
            .downcast_ref::<ApplicationLifecycleError>()
            .ok_or_else(|| anyhow::anyhow!("desktop error lost its lifecycle receipt"))?;

        assert!(cancel.is_cancelled());
        assert!(error.receipt.failures.is_empty());
        assert_eq!(
            error.receipt.primary_error.as_deref(),
            Some("Tauri application exited with status 17")
        );
        Ok(())
    }
}
