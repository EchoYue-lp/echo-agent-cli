#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    Stderr,
    TuiFile,
    MachineReadableFile,
}

/// Load shell environment variables by spawning the user's login shell.
///
/// On macOS, GUI apps launched from Dock/Finder/Spotlight do NOT inherit
/// shell environment variables (from ~/.zshrc, ~/.bash_profile, etc.).
/// This function bridges that gap — the same approach used by VS Code,
/// JetBrains, and other macOS GUI apps.
///
/// Only sets variables that are NOT already present in the process environment,
/// so explicit env vars always take precedence.
///
/// Only imports known API key variables to avoid polluting the environment
/// with unrelated shell state.
pub fn load_shell_env(app_config: &EkoConfig) {
    #[cfg(target_os = "macos")]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

        // Spawn a login interactive shell and print its environment.
        // -l = login shell (sources ~/.zprofile, ~/.bash_profile, etc.)
        // -i = interactive (sources ~/.zshrc, ~/.bashrc, etc.)
        // -c = run command then exit
        let output = match std::process::Command::new(&shell)
            .args(["-lic", "env"])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("Failed to spawn shell for env loading: {e}");
                return;
            }
        };

        if !output.status.success() {
            tracing::warn!("Shell env command exited with status: {}", output.status);
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Import only credential names the user explicitly assigned to a
        // provider. MCP_CONFIG_PATH is application configuration, not a model
        // vendor assumption.
        let mut imported_names = app_config
            .model_providers
            .values()
            .filter_map(|provider| provider.api_key_env.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        imported_names.insert("MCP_CONFIG_PATH".to_string());

        // SAFETY: `std::env::set_var` is not thread-safe in Rust. We use a
        // `std::sync::Once` to guarantee this block runs at most once per
        // process lifetime, and it must be called early in `main()` / app
        // startup before background threads are spawned.
        static SHELL_ENV_LOADED: std::sync::Once = std::sync::Once::new();
        let mut loaded = Vec::new();
        SHELL_ENV_LOADED.call_once(|| {
            for line in stdout.lines() {
                if let Some((key, value)) = line.split_once('=')
                    && imported_names.contains(key)
                    && std::env::var(key).is_err()
                    && !value.is_empty()
                {
                    unsafe { std::env::set_var(key, value) };
                    loaded.push(key.to_string());
                }
            }
        });

        if !loaded.is_empty() {
            tracing::info!(vars = loaded.join(", "), "Loaded shell env vars (GUI mode)");
        }
    }

    // Linux and other non-macOS targets do not import login-shell variables,
    // but keep the shared startup API so callers do not need platform forks.
    // Mark the configuration as intentionally unused on those targets; this
    // has no effect on the macOS environment-loading behavior above.
    #[cfg(not(target_os = "macos"))]
    let _ = app_config;
}

pub fn init_logging_for_tui(level: &str) {
    init_logging_with_target(level, LogTarget::TuiFile);
}

/// Keep stdout and stderr free of tracing output for a machine protocol.
pub fn init_logging_for_machine_output(level: &str) {
    init_logging_with_target(level, LogTarget::MachineReadableFile);
}

pub fn init_logging(level: &str) {
    init_logging_with_target(level, LogTarget::Stderr);
}

/// 本地时区的日志时间格式化器。
///
/// tracing-subscriber 默认用 UTC（RFC3339 带 `Z` 后缀）。本格式器改用
/// `chrono::Local` 输出机器当前时区时间（如 `2026-07-09T09:50:48.876+08:00`），
/// 便于本地排查问题。chrono::Local 读取系统时区（`TZ` 环境变量或系统配置）。
#[cfg(not(feature = "telemetry"))]
struct LocalTimer;

#[cfg(not(feature = "telemetry"))]
impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        // RFC3339 + 本地时区偏移，保留毫秒精度（与默认 SystemTime 精度一致）。
        write!(
            w,
            "{}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        )
    }
}

/// 初始化日志系统（线程安全，仅执行一次）
///
/// When the `telemetry` feature is enabled, this delegates to
/// [`echo_agent::telemetry::init_telemetry`] which sets up OTLP tracing + metrics
/// configured via `OTEL_EXPORTER_OTLP_ENDPOINT` (defaults to `http://localhost:4317`).
pub fn init_logging_with_target(level: &str, target: LogTarget) {
    // `level` is consumed by the EnvFilter below when `telemetry` is off;
    // reference it here so the param is considered used under all feature combos.
    #[cfg(feature = "telemetry")]
    let _ = level;
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();

    INIT.get_or_init(|| {
        #[cfg(feature = "telemetry")]
        {
            let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4317".to_string());
            let service_name =
                std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "echo-agent-cli".to_string());

            let config = echo_agent::telemetry::TelemetryConfig {
                otlp_endpoint,
                service_name,
                enable_console: matches!(target, LogTarget::Stderr),
            };
            // Use env filter matching the requested level
            // Note: We don't set RUST_LOG env var to avoid thread-safety issues
            // Instead, we rely on tracing_subscriber's EnvFilter::new() to parse the filter
            let _ = echo_agent::telemetry::init_telemetry(config);
        }

        #[cfg(not(feature = "telemetry"))]
        {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            // Include echo_agent_app_core so the task_runtime module's traces
            // (task_execute/execute_run/drain loop) are visible by default.
            // Previously this crate was omitted, silently hiding all B1-B7
            // instrumentation unless RUST_LOG was set explicitly.
            let default_filter = format!(
                "echo_agent_cli={level},echo_agent={level},echo_agent_app_core={level},tower_http=info"
            );
            let env_filter = || {
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::filter::EnvFilter::new(&default_filter))
            };

            match target {
                LogTarget::TuiFile => {
                    if let Ok(file) = std::fs::File::create(tui_log_path()) {
                        let _ = tracing_subscriber::registry()
                            .with(env_filter())
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .with_writer(std::sync::Mutex::new(file))
                                    .with_ansi(false)
                                    .with_timer(LocalTimer),
                            )
                            .try_init();
                    }
                }
                LogTarget::MachineReadableFile => {
                    let registry = tracing_subscriber::registry().with(env_filter());
                    if let Some(file) = app_log_file() {
                        let _ = registry
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .with_writer(std::sync::Mutex::new(file))
                                    .with_ansi(false)
                                    .with_timer(LocalTimer),
                            )
                            .try_init();
                    } else {
                        let _ = registry.try_init();
                    }
                }
                LogTarget::Stderr => {
                    // Dual sink: keep the stderr console output (visible in the
                    // `cargo tauri dev` terminal) AND mirror to a rotating-ish
                    // file at ~/.eko/logs/app.log so issues can be
                    // diagnosed after the fact without re-running. Append mode
                    // so restarts don't wipe the log.
                    use tracing_subscriber::layer::SubscriberExt;
                    let registry = tracing_subscriber::registry().with(env_filter());
                    let file_layer = app_log_file().map(|file| {
                        tracing_subscriber::fmt::layer()
                            .with_writer(std::sync::Mutex::new(file))
                            .with_ansi(false)
                            .with_timer(LocalTimer)
                    });
                    if let Some(file_layer) = file_layer {
                        let _ = registry
                            .with(
                                tracing_subscriber::fmt::layer().with_timer(LocalTimer),
                            )
                            .with(file_layer)
                            .try_init();
                    } else {
                        let _ = registry
                            .with(tracing_subscriber::fmt::layer().with_timer(LocalTimer))
                            .try_init();
                    }
                }
            }
        }
    });
}

#[cfg(not(feature = "telemetry"))]
fn tui_log_path() -> std::path::PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut current = cwd.as_path();
        loop {
            let state_dir = crate::workspace::layout::WorkspaceLayout::state_dir(current);
            if state_dir.exists()
                || crate::workspace::layout::WorkspaceLayout::manifest(current).exists()
                || crate::workspace::layout::WorkspaceLayout::legacy_manifest(current).exists()
            {
                let dir = state_dir.join("logs");
                let _ = std::fs::create_dir_all(&dir);
                return dir.join("tui.log");
            }

            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
    }

    let dir = crate::data_root::user_data_path("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tui.log")
}

/// Open the shared GUI app log file for appending: `~/.eko/logs/app.log`.
///
/// Used by the Stderr log target as a second sink so that `cargo tauri dev`
/// output is also persisted to disk (the stderr stream itself is lost once the
/// terminal that launched the app is closed). Append mode keeps history across
/// restarts; rotate/truncate manually if it grows too large.
#[cfg(not(feature = "telemetry"))]
fn app_log_file() -> Option<std::fs::File> {
    let dir = crate::data_root::user_data_path("logs");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
        .ok()
}

// ── Doctor 诊断 ──────────────────────────────────────────────────
