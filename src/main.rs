//! Echo Agent CLI - AI Agent 命令行与 Web 服务
//!
//! 提供两种交互模式：
//! - **Web 模式**: 启动 HTTP/WebSocket 服务，提供完整的 REST API
//! - **CLI 模式**: 启动交互式命令行界面，支持 REPL 对话
//!
//! # 快速开始
//!
//! ```bash
//! # 仅启动 Web 服务（默认）
//! echo-agent-cli
//!
//! # 仅启动 CLI 交互
//! echo-agent-cli --cli
//!
//! # 同时启动 Web 服务和 CLI 交互
//! echo-agent-cli --web --cli
//!
//! # 指定端口
//! echo-agent-cli --web --port 8080
//! ```
//!
//! # 命令行选项
//!
//! | 选项 | 说明 |
//! |------|------|
//! | `--web` | 启动 Web 服务 |
//! | `--cli` | 启动命令行交互 |
//! | `--port <PORT>` | Web 服务端口 |
//! | `--host <HOST>` | Web 服务地址 |
//! | `--model <MODEL>` | 使用的模型名称 |
//! | `--no-color` | 禁用彩色输出 |
//! | `-h, --help` | 显示帮助信息 |
//! | `-V, --version` | 显示版本信息 |

use echo_agent_cli::cli;
use echo_agent_cli::config;
use echo_agent_cli::agent_handle::AgentHandle;
use echo_agent_cli::infra;

use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // 解析命令行参数
    let args = cli::Args::parse();

    // 加载 YAML 配置文件
    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

    // 处理其他子命令 (不需要创建 Agent 即可执行的命令)
    if let Some(ref cmd) = args.command {
        if !matches!(cmd, cli::Commands::Tui) {
            return cli::handle_subcommand(cmd).await;
        }
    }

    // 初始化日志（使用配置中的级别）
    infra::init_logging(&app_config.logging.level);

    // 创建 Agent + 加载 MCP 配置（统一路径，消除重复）
    let mut agent = infra::create_agent(&args, &app_config);
    infra::load_mcp_config(&mut agent, args.mcp_config.as_deref(), &app_config).await;

    // Configure auto-compression if token_limit is set
    if app_config.has_compressor() {
        app_config.apply_compressor(&agent).await;
        tracing::info!(
            token_limit = app_config.agent.token_limit,
            strategy = %app_config.agent.compress_strategy,
            window = app_config.agent.compress_window,
            "Auto context compression configured"
        );
    }

    let agent_handle = AgentHandle::new(agent);

    // Load user hooks from YAML config
    infra::load_user_hooks(&agent_handle, &app_config).await;

    // Fire SessionStart("startup") hook — after hooks are loaded so they can react
    infra::fire_startup_hook(&agent_handle).await;

    // Spawn config file watcher (fires ConfigChange hooks + reloads hooks on change)
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if let Some(config_path) = echo_agent_cli::config_watcher::resolve_config_path(args.config.as_deref()) {
        echo_agent_cli::config_watcher::spawn_config_watcher(
            config_path,
            agent_handle.clone(),
            cancel_token.clone(),
        );
    }

    // 处理 TUI 子命令
    if matches!(args.command, Some(cli::Commands::Tui)) {
        return echo_agent_cli::tui::run_tui(agent_handle.clone()).await;
    }

    // 决定运行模式
    let run_web = args.web || (!args.cli && !args.channels && !args.tui);
    let run_cli = args.cli;
    let run_channels = args.channels;
    let run_tui = args.tui;

    if run_tui {
        return echo_agent_cli::tui::run_tui(agent_handle.clone()).await;
    }

    if run_channels {
        #[cfg(feature = "channels")]
        {
            let channels_handle = tokio::spawn(cli::run_channels_mode(&app_config));

            if run_web && run_cli {
                cli::run_both_modes(agent_handle, &args, &app_config).await?;
            } else if run_cli {
                cli::run_cli_mode(agent_handle, &args, &app_config).await?;
            } else if run_web {
                cli::run_web_mode(agent_handle, &args, &app_config).await?;
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
    } else if run_web && run_cli {
        cli::run_both_modes(agent_handle, &args, &app_config).await?;
    } else if run_cli {
        // 仅 CLI 模式
        cli::run_cli_mode(agent_handle, &args, &app_config).await?;
    } else {
        // 仅 Web 模式
        cli::run_web_mode(agent_handle, &args, &app_config).await?;
    }

    Ok(())
}

// ── 单元测试 ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::prelude::*;
    use echo_agent_cli::config;

    #[test]
    fn test_create_agent_config() {
        let args = cli::Args {
            web: false,
            cli: false,
            port: 3000,
            host: "0.0.0.0".to_string(),
            model: Some("test-model".to_string()),
            system_prompt: Some("test prompt".to_string()),
            mode: "general".to_string(),
            project: None,
            mcp_config: None,
            config: None,
            no_color: false,
            channels: false,
            tui: false,
            output: "text".to_string(),
            verbose: false,
            command: None,
        };

        let app_config = config::AppConfig::default();
        let agent = infra::create_agent(&args, &app_config);
        assert_eq!(agent.model_name(), "test-model");
    }

    #[test]
    fn test_args_default() {
        let args = cli::Args::parse_from(["echo-agent-cli"]);
        assert!(!args.web);
        assert!(!args.cli);
        assert_eq!(args.port, 3000);
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_args_cli_mode() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--cli"]);
        assert!(args.cli);
        assert!(!args.web);
    }

    #[test]
    fn test_args_both_modes() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--web", "--cli"]);
        assert!(args.web);
        assert!(args.cli);
    }

    #[test]
    fn test_args_custom_port() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--port", "8080"]);
        assert_eq!(args.port, 8080);
    }
}
