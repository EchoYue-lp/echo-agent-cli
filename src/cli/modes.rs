//! 运行模式管理
//!
//! 提供 Web 模式、CLI 模式、双模式和 IM 通道模式的启动逻辑。

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

use echo_agent::agent::CancellationToken;

use crate::agent_handle::AgentHandle;
use crate::cli::args::Args;
use crate::cli::router::build_router;
use crate::config::AppConfig;
use crate::state;

/// Shared web infrastructure setup, used by both `run_web_mode` and `run_both_modes`.
struct WebInfra {
    app: axum::Router,
    addr: String,
    cancel_token: CancellationToken,
}

async fn setup_web_infrastructure(
    agent: &AgentHandle,
    args: &Args,
    app_config: &AppConfig,
) -> Result<WebInfra> {
    let conversation_store = crate::infra::create_conversation_store();
    crate::infra::inject_conversation_store(agent, &conversation_store);

    if !app_config.webhooks.endpoints.is_empty() {
        let webhook_eps: Vec<crate::webhook::emitter::WebhookEndpoint> = app_config
            .webhooks
            .endpoints
            .iter()
            .map(|e| crate::webhook::emitter::WebhookEndpoint {
                url: e.url.clone(),
                events: e.events.clone(),
                secret: e.secret.clone(),
            })
            .collect();
        crate::webhook::emitter::init_global(webhook_eps);
        tracing::info!("Webhook emitter initialized with {} endpoints", app_config.webhooks.endpoints.len());
    }

    let state = Arc::new({
        let mut s = state::AppState::from_shared(
            agent.clone(),
            conversation_store,
            app_config.clone(),
        );
        s.start_scheduler();
        s
    });

    let cancel_token = CancellationToken::new();
    crate::infra::spawn_mcp_health_check(state.clone(), cancel_token.clone());

    if let Err(e) = crate::metrics::init_metrics() {
        tracing::warn!("Failed to initialize metrics: {}", e);
    }

    crate::ws::handler::cleanup_stale_uploads().await;

    let app = build_router(state.clone()).await;
    let host = if args.host != "0.0.0.0" { &args.host } else { &app_config.server.host };
    let port = if args.port != 3000 { args.port } else { app_config.server.port };
    let addr = format!("{}:{}", host, port);

    Ok(WebInfra { app, addr, cancel_token })
}

/// 运行 Web 模式
pub async fn run_web_mode(
    agent: AgentHandle,
    args: &Args,
    app_config: &AppConfig,
) -> Result<()> {
    let infra = setup_web_infrastructure(&agent, args, app_config).await?;
    let listener = tokio::net::TcpListener::bind(&infra.addr).await?;

    crate::infra::print_web_startup_info(&infra.addr);

    axum::serve(listener, infra.app)
        .with_graceful_shutdown(crate::infra::shutdown_signal())
        .await?;

    Ok(())
}

fn repl_config_for(args: &Args) -> crate::cli::ReplConfig {
    crate::cli::ReplConfig {
        prompt: "echo".to_string(),
        history_file: "~/.echo-agent/history.txt".to_string(),
        mode: args.mode.clone(),
        project: args.project.clone(),
    }
}

/// 运行 CLI 模式
pub async fn run_cli_mode(agent: AgentHandle, args: &Args, _app_config: &AppConfig) -> Result<()> {
    crate::cli::run_repl(agent, repl_config_for(args)).await
}

/// 同时运行 Web 和 CLI 模式
pub async fn run_both_modes(agent: AgentHandle, args: &Args, app_config: &AppConfig) -> Result<()> {
    let infra = setup_web_infrastructure(&agent, args, app_config).await?;
    let listener = tokio::net::TcpListener::bind(&infra.addr).await?;

    crate::infra::print_both_startup_info(&infra.addr);

    let web_shutdown = infra.cancel_token.clone();
    let web_handle = tokio::spawn(async move {
        axum::serve(listener, infra.app)
            .with_graceful_shutdown(async move { web_shutdown.cancelled().await })
            .await
    });
    let web_abort = web_handle.abort_handle();

    crate::cli::run_repl(agent, repl_config_for(args)).await?;

    // CLI 退出后，通知 Web 服务和后台任务优雅关闭
    tracing::info!("CLI 已退出，正在关闭 Web 服务...");
    infra.cancel_token.cancel();

    // 等待 Web 服务优雅关闭（最多 30 秒）
    match tokio::time::timeout(Duration::from_secs(30), web_handle).await {
        Ok(Ok(Ok(()))) => tracing::info!("Web 服务已优雅关闭"),
        Ok(Ok(Err(e))) => tracing::error!("Web 服务异常退出: {e}"),
        Ok(Err(join_err)) => tracing::error!("Web 服务任务异常: {join_err}"),
        Err(_) => {
            tracing::warn!("Web 服务未在 30 秒内关闭，强制终止");
            web_abort.abort();
        }
    }

    Ok(())
}

/// 运行 IM 通道模式（QQ Bot、飞书等）
#[cfg(feature = "channels")]
pub async fn run_channels_mode(app_config: &AppConfig) -> Result<()> {
    use echo_agent::channels::{
        AgentChannelHandler, ChannelManager, FeishuChannel, FeishuConfig, MessageHandler,
        QqChannel, QqConfig, SessionConfig, SessionHandler,
    };

    let mut manager = ChannelManager::new();

    // 注册 QQ Bot
    if app_config.channels.qq.enabled {
        let config = QqConfig {
            app_id: app_config.channels.qq.app_id.clone(),
            client_secret: app_config.channels.qq.client_secret.clone(),
        };
        match QqChannel::new(config) {
            Ok(ch) => {
                manager.register(Box::new(ch));
                tracing::info!("已注册 QQ Bot 通道");
            }
            Err(e) => tracing::warn!("QQ Bot 注册失败: {e}"),
        }
    } else {
        tracing::info!("QQ Bot 通道已禁用（channels.qq.enabled = false）");
    }

    // 注册飞书
    if app_config.channels.feishu.enabled {
        let config = match app_config.channels.feishu.mode.as_str() {
            "webhook" => FeishuConfig::new_webhook(
                app_config.channels.feishu.app_id.clone(),
                app_config.channels.feishu.app_secret.clone(),
            ),
            _ => FeishuConfig::new_long_poll(
                app_config.channels.feishu.app_id.clone(),
                app_config.channels.feishu.app_secret.clone(),
            ),
        };
        match FeishuChannel::new(config) {
            Ok(ch) => {
                manager.register(Box::new(ch));
                tracing::info!("已注册飞书通道（{}模式）", app_config.channels.feishu.mode);
            }
            Err(e) => tracing::warn!("飞书注册失败: {e}"),
        }
    } else {
        tracing::info!("飞书通道已禁用（channels.feishu.enabled = false）");
    }

    if manager.is_empty() {
        tracing::error!("没有可用的 IM 通道，请在 echo-agent.yaml 中启用并配置 channels");
        return Ok(());
    }

    let session_config =
        SessionConfig::default().with_timeout_minutes(app_config.channels.session.timeout_minutes);

    let model = app_config.model.name.clone();
    let system_prompt = app_config.agent.system_prompt.clone();
    let agent_name = app_config.agent.name.clone();
    let handler_factory = move |_channel_id: &str| -> Arc<dyn MessageHandler> {
        let model = model.clone();
        let system_prompt = system_prompt.clone();
        let agent_name = agent_name.clone();
        let session_config = session_config.clone();
        Arc::new(SessionHandler::new(
            session_config,
            move || -> Box<dyn MessageHandler> {
                Box::new(AgentChannelHandler::standard(
                    &model,
                    &agent_name,
                    &system_prompt,
                ))
            },
        ))
    };

    tracing::info!("启动 {} 个 IM 通道...", manager.len());
    manager.start_all(handler_factory).await?;
    tracing::info!("所有 IM 通道已启动");

    crate::infra::shutdown_signal().await;

    tracing::info!("正在关闭 IM 通道...");
    manager.stop_all().await?;
    tracing::info!("所有 IM 通道已关闭");

    Ok(())
}
