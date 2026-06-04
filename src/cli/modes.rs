//! 运行模式管理
//!
//! 提供 CLI 模式和 IM 通道模式的启动逻辑。
//! Web 模式已移除 — GUI 通过 Tauri IPC 通信。

use anyhow::Result;

use crate::agent_handle::AgentHandle;
use crate::cli::args::Args;
use crate::config::AppConfig;

fn repl_config_for(args: &Args) -> crate::cli::ReplConfig {
    crate::cli::ReplConfig {
        prompt: "echo".to_string(),
        history_file: "~/.echo-agent/history.txt".to_string(),
        mode: "general".to_string(),
        project: args.project.clone(),
        task_service: None,
        scheduler_runner: None,
    }
}

/// 运行 CLI 模式
pub async fn run_cli_mode(
    agent: AgentHandle,
    hitl_dispatcher: std::sync::Arc<crate::state::HitlDispatcher>,
    args: &Args,
    app_config: &AppConfig,
    task_store: std::sync::Arc<dyn echo_agent::memory::Store>,
) -> Result<()> {
    // Start BackgroundTaskService for CLI mode
    let (task_service, scheduler_runner) = {
        use crate::state::AppState;
        let mut state = AppState::from_shared(
            agent.clone(),
            hitl_dispatcher.clone(),
            None,
            app_config.clone(),
        );
        state.start_task_service(task_store.clone()).await;
        state.start_scheduler_with_store(Some(task_store));
        (state.tasks.service.clone(), state.scheduler.runner.clone())
    };

    let mut repl_config = repl_config_for(args);
    repl_config.task_service = task_service;
    repl_config.scheduler_runner = scheduler_runner;

    crate::cli::run_repl(agent, repl_config).await
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
