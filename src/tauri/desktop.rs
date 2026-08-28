//! EKO — Tauri desktop runtime.
//!
//! Shared by the dedicated `echo-agent-tauri` binary and by the package-name
//! binary when Tauri CLI builds the app with `--no-default-features --features gui`.

use crate::{cli, infra};
use clap::Parser;
use echo_agent_app_core::config;
use std::ffi::OsString;
use std::sync::Arc;

/// Crash log path — written to when the app panics before Tauri starts.
/// This is the only way to debug silent crashes on macOS (no terminal attached).
fn crash_log_path() -> std::path::PathBuf {
    echo_agent_app_core::data_root::user_data_path("crash.log")
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
                "EKO failed to start at {}\n\nError: {:?}\n",
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
                        "display dialog \"EKO failed to start.\\n\\n\
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
    let mcp_config_path = echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(
        args.mcp_config.as_deref(),
        &app_config,
    );
    config::apply_env_overrides(&mut app_config);
    // Keep EkoConfig as the file-backed configuration; the resolved runtime
    // source above owns environment and CLI overrides.
    app_config.mcp.config_path = configured_mcp_path;
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
        command_cell_runtime: None,
        product_data_io: None,
        execution_scope: None,
    };

    let runtime =
        echo_agent_app_core::runtime::AgentRuntime::bootstrap(&app_config, params, mcp_config_path)
            .await?;
    let conversation_store = infra::create_conversation_store();
    let mut services = echo_agent_app_core::runtime::ApplicationServices::compose(
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
    let tauri_result = crate::tauri::build_tauri_app(
        state.clone(),
        runtime.browser_runtime.clone(),
        bridge_supervisor.clone(),
    )
    .run(tauri::generate_context!());

    let primary_error = tauri_result
        .err()
        .map(|error| anyhow::anyhow!("error while running Tauri application: {error}"));
    let receipt = services
        .settle(
            echo_agent_app_core::runtime::ApplicationLifecycleReason::Shutdown,
            primary_error,
        )
        .await;
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
