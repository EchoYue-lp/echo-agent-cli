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
        review_integration: None,
    }
}

/// 运行 CLI 模式
pub async fn run_cli_mode(
    agent: AgentHandle,
    hitl_dispatcher: std::sync::Arc<crate::state::HitlDispatcher>,
    args: &Args,
    app_config: &AppConfig,
    task_store: std::sync::Arc<dyn echo_agent::memory::Store>,
    review_integration: Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
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
    repl_config.review_integration = review_integration;

    crate::cli::run_repl(agent, repl_config).await
}

/// 运行 IM 通道模式（QQ Bot、飞书等）
///
/// Channel agent 经 `AgentPool` 全套接通(bootstrap 等价:state_store/store/compressor/
/// MemoryLayerManager/permission_service/per-sender cache_user_id+conversation_id),
/// per-sender 隔离由 pool key `channel:{channel_id}:{sender_id}` 承载。
#[cfg(feature = "channels")]
pub async fn run_channels_mode(
    pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    app_config: AppConfig,
    task_runtime_store: Option<
        std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
) -> Result<()> {
    use std::sync::Arc;

    use echo_agent::channels::{
        ChannelManager, FeishuChannel, FeishuConfig, MessageHandler, QqChannel, QqConfig,
        SessionConfig, SessionHandler,
    };

    use crate::cli::channels::AppChannelMessageHandler;

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
                app_config.channels.feishu.webhook_bind.clone(),
                app_config.channels.feishu.webhook_path.clone(),
                app_config
                    .channels
                    .feishu
                    .webhook_verification_token
                    .clone(),
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

    // pool create_agent 用 app_config 默认 model(system_prompt/agent_name 来自
    // app_config),无需在此解析 runtime_model 或裸建 agent —— bootstrap 全套已由
    // pool 注入。handler_factory 每 channel 产出一个 SessionHandler,其内层工厂
    // 每 (channel,sender) 产出 AppChannelMessageHandler(持 pool clone)。
    let handler_factory = move |_channel_id: &str| -> Arc<dyn MessageHandler> {
        let session_config = session_config.clone();
        let pool = pool.clone();
        let store = task_runtime_store.clone();
        Arc::new(SessionHandler::new(
            session_config,
            move || -> Box<dyn MessageHandler> {
                Box::new(AppChannelMessageHandler::new(pool.clone(), store.clone()))
            },
        ))
    };

    tracing::info!("启动 {} 个 IM 通道...", manager.len());
    let start_results = manager.start_all(handler_factory).await;
    let failures: Vec<_> = start_results.iter().filter(|r| r.is_err()).collect();
    if !failures.is_empty() {
        tracing::warn!(
            "{} 个通道启动失败（共 {} 个）",
            failures.len(),
            start_results.len()
        );
    }
    tracing::info!("所有 IM 通道已启动");

    crate::infra::shutdown_signal().await;

    tracing::info!("正在关闭 IM 通道...");
    manager.stop_all().await?;
    tracing::info!("所有 IM 通道已关闭");

    Ok(())
}
