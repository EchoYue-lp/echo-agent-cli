//! 运行模式管理
//!
//! 提供 CLI 模式和 IM 通道模式的启动逻辑。
//! Web 模式已移除 — GUI 通过 Tauri IPC 通信。

use anyhow::Result;

use crate::agent_handle::AgentHandle;
use crate::cli::args::Args;
use echo_agent::config::AppConfig;

type CliShutdownFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>;

struct CliShutdownStep<'a> {
    name: &'static str,
    future: CliShutdownFuture<'a>,
}

/// Owned settlement for a concurrently running product surface that must stop
/// before CLI tears down shared foreground, pool, and plugin resources.
pub struct CompanionModeShutdown {
    name: &'static str,
    cancel: echo_agent::agent::CancellationToken,
    settlement: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl CompanionModeShutdown {
    pub fn new(
        name: &'static str,
        cancel: echo_agent::agent::CancellationToken,
        settlement: tokio::task::JoinHandle<Result<()>>,
    ) -> Self {
        Self {
            name,
            cancel,
            settlement: Some(settlement),
        }
    }

    async fn shutdown(mut self) -> Result<()> {
        self.cancel.cancel();
        let settlement = self
            .settlement
            .take()
            .ok_or_else(|| anyhow::anyhow!("{} settlement handle is unavailable", self.name))?;
        settlement
            .await
            .map_err(|error| anyhow::anyhow!("{} settlement task failed: {error}", self.name))?
    }
}

impl Drop for CompanionModeShutdown {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn drain_cli_shutdown(
    repl_result: Result<()>,
    steps: Vec<CliShutdownStep<'_>>,
) -> Result<()> {
    let mut failures = repl_result
        .err()
        .map(|error| format!("REPL: {error}"))
        .into_iter()
        .collect::<Vec<_>>();

    for step in steps {
        if let Err(error) = step.future.await {
            tracing::warn!(step = step.name, %error, "CLI shutdown step failed");
            failures.push(format!("{}: {error}", step.name));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("CLI mode failed: {}", failures.join("; ")))
    }
}

fn repl_config_for(args: &Args) -> crate::cli::ReplConfig {
    crate::cli::ReplConfig {
        prompt: "echo".to_string(),
        history_file: echo_agent::paths::user_data_path("history.txt")
            .to_string_lossy()
            .into_owned(),
        mode: "general".to_string(),
        project: args.project.clone(),
        task_service: None,
        scheduler_runner: None,
        plugin_runtime: None,
        review_integration: None,
        prompt_assembly: None,
        pool: None,
        task_runtime_store: None,
        conversation_id: String::new(),
        webhook_emitter: None,
        app_state: None,
    }
}

pub struct HeadlessServiceResources {
    pub model_consumers: echo_agent_app_core::infra::AgentModelConsumers,
    pub active_model_id: String,
    pub pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    pub task_runtime_store:
        Option<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    pub webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    pub conversation_store: Option<std::sync::Arc<dyn echo_agent::memory::ConversationStore>>,
    pub review_integration:
        Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    pub mcp_config_runtime:
        std::sync::Arc<echo_agent_app_core::mcp_config_runtime::McpConfigRuntime>,
    pub plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    pub config_watcher: std::sync::Arc<echo_agent_app_core::config_watcher::ConfigWatcherHandle>,
    pub foreground_turns: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
}

pub async fn start_headless_services(
    agent: AgentHandle,
    hitl_dispatcher: std::sync::Arc<crate::state::HitlDispatcher>,
    app_config: &AppConfig,
    resources: HeadlessServiceResources,
) -> Result<(
    Option<std::sync::Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    Option<std::sync::Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    std::sync::Arc<echo_agent_app_core::state::AppState>,
)> {
    let scheduler_store: std::sync::Arc<dyn echo_agent::memory::Store> = {
        let file_path =
            echo_agent_app_core::persistence::Persistence::base_dir().join("scheduler_store");
        match echo_agent::memory::FileStore::new(&file_path) {
            Ok(store) => std::sync::Arc::new(store),
            Err(error) => {
                tracing::warn!(%error, "failed to create scheduler store; using in-memory");
                std::sync::Arc::new(echo_agent::memory::InMemoryStore::new())
            }
        }
    };
    use crate::state::AppState;
    let mut state = AppState::from_shared(
        agent,
        Some(resources.model_consumers),
        hitl_dispatcher,
        resources.conversation_store,
        app_config.clone(),
        resources.mcp_config_runtime,
    )
    .with_active_model_id(resources.active_model_id)
    .with_review_integration(resources.review_integration)
    .with_plugin_runtime(Some(resources.plugin_runtime))
    .with_config_watcher(Some(resources.config_watcher))
    .with_foreground_turns(resources.foreground_turns);
    state.webhook.emitter = resources.webhook_emitter;
    state.connection.pool = Some(resources.pool);
    state.tasks.runtime = resources.task_runtime_store;
    state
        .start_scheduler_and_task_service(Some(scheduler_store))
        .await?;
    let task_service = state.tasks.service.clone();
    let scheduler = state.scheduler.runner.clone();
    Ok((task_service, scheduler, std::sync::Arc::new(state)))
}

/// 运行 CLI 模式
#[allow(clippy::too_many_arguments)] // startup adapter wires the shared agent, pool, stores, and UI services once
pub async fn run_cli_mode(
    agent: AgentHandle,
    model_consumers: echo_agent_app_core::infra::AgentModelConsumers,
    active_model_id: String,
    hitl_dispatcher: std::sync::Arc<crate::state::HitlDispatcher>,
    args: &Args,
    app_config: &AppConfig,
    review_integration: Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    prompt_assembly: echo_agent_app_core::project::prompt::PromptAssembly,
    pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    task_runtime_store: Option<
        std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
    conversation_id: String,
    webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    mcp_config_runtime: std::sync::Arc<echo_agent_app_core::mcp_config_runtime::McpConfigRuntime>,
    config_watcher: std::sync::Arc<echo_agent_app_core::config_watcher::ConfigWatcherHandle>,
    conversation_store: Option<std::sync::Arc<dyn echo_agent::memory::ConversationStore>>,
    foreground_turns: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    companion_shutdown: Option<CompanionModeShutdown>,
) -> Result<()> {
    let mut companion_shutdown = companion_shutdown;
    let service_result = start_headless_services(
        agent.clone(),
        hitl_dispatcher,
        app_config,
        HeadlessServiceResources {
            model_consumers,
            active_model_id,
            pool: pool.clone(),
            task_runtime_store: task_runtime_store.clone(),
            webhook_emitter: webhook_emitter.clone(),
            conversation_store,
            review_integration: review_integration.clone(),
            mcp_config_runtime,
            plugin_runtime: plugin_runtime.clone(),
            config_watcher,
            foreground_turns: foreground_turns.clone(),
        },
    )
    .await;
    let (task_service, scheduler_runner, app_state) = match service_result {
        Ok(services) => services,
        Err(error) => {
            let mut steps = Vec::new();
            if let Some(companion) = companion_shutdown.take() {
                steps.push(CliShutdownStep {
                    name: companion.name,
                    future: Box::pin(companion.shutdown()),
                });
            }
            steps.extend([
                CliShutdownStep {
                    name: "foreground turns",
                    future: Box::pin(async {
                        foreground_turns
                            .shutdown()
                            .await
                            .map_err(|shutdown_error| anyhow::anyhow!(shutdown_error))
                    }),
                },
                CliShutdownStep {
                    name: "memory review",
                    future: Box::pin(async {
                        if let Some(integration) = review_integration.as_ref() {
                            integration
                                .shutdown_background_reviews()
                                .await
                                .map_err(anyhow::Error::msg)?;
                        }
                        Ok(())
                    }),
                },
                CliShutdownStep {
                    name: "TaskRun drivers",
                    future: Box::pin(async {
                        if let Some(store) = task_runtime_store.as_ref() {
                            store
                                .shutdown_run_drivers()
                                .await
                                .map_err(|shutdown_error| anyhow::anyhow!(shutdown_error))?;
                        }
                        Ok(())
                    }),
                },
                CliShutdownStep {
                    name: "agent pool",
                    future: Box::pin(async { pool.shutdown().await.map_err(anyhow::Error::msg) }),
                },
                CliShutdownStep {
                    name: "plugin runtime",
                    future: Box::pin(async { plugin_runtime.shutdown().await }),
                },
            ]);
            return drain_cli_shutdown(Err(error), steps).await;
        }
    };
    if let Some(scheduler) = scheduler_runner.as_ref()
        && let Err(error) = plugin_runtime.bind_scheduler(scheduler.clone()).await
    {
        tracing::warn!(%error, "failed to bind plugin monitors to CLI scheduler");
    }

    let dreaming_task = review_integration.as_ref().map(|integration| {
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = echo_agent_app_core::infra::spawn_dreaming_task(
            integration.clone(),
            agent.clone(),
            Some(pool.clone()),
            cancel.clone(),
        );
        tracing::info!("Dreaming task spawned for CLI session");
        (cancel, task)
    });

    let mut repl_config = repl_config_for(args);
    repl_config.task_service = task_service;
    repl_config.scheduler_runner = scheduler_runner;
    repl_config.review_integration = review_integration.clone();
    repl_config.prompt_assembly = Some(prompt_assembly);
    repl_config.pool = Some(pool.clone());
    repl_config.task_runtime_store = task_runtime_store.clone();
    repl_config.conversation_id = conversation_id;
    repl_config.webhook_emitter = Some(webhook_emitter);
    repl_config.plugin_runtime = Some(plugin_runtime.clone());
    repl_config.app_state = Some(app_state.clone());

    let auto_memory_agent = agent.clone();
    let auto_memory_integration = review_integration.clone();
    let session_review_integration = review_integration.clone();
    let background_review_integration = review_integration.clone();
    let repl_result = crate::cli::run_repl(agent, repl_config).await;
    let mut steps = Vec::new();
    if let Some(companion) = companion_shutdown.take() {
        steps.push(CliShutdownStep {
            name: companion.name,
            future: Box::pin(companion.shutdown()),
        });
    }
    steps.extend([
        CliShutdownStep {
            name: "foreground turns",
            future: Box::pin(async {
                app_state
                    .session
                    .foreground_turns
                    .shutdown()
                    .await
                    .map_err(|error| anyhow::anyhow!(error))
            }),
        },
        CliShutdownStep {
            name: "model mutations",
            future: Box::pin(async {
                app_state
                    .shutdown_model_mutations()
                    .await
                    .map_err(anyhow::Error::from)
            }),
        },
        CliShutdownStep {
            name: "Dreaming",
            future: Box::pin(async move {
                if let Some((cancel, task)) = dreaming_task {
                    cancel.cancel();
                    task.await
                        .map_err(|error| anyhow::anyhow!("Dreaming task failed: {error}"))?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "auto-memory",
            future: Box::pin(async move {
                crate::cli::repl::run_auto_memory_on_exit(
                    &auto_memory_agent,
                    &auto_memory_integration,
                )
                .await;
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "memory review",
            future: Box::pin(async move {
                crate::cli::repl::run_memory_review_on_exit(&session_review_integration).await;
                if let Some(integration) = background_review_integration.as_ref() {
                    integration
                        .shutdown_background_reviews()
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "workspace transition",
            future: Box::pin(async { app_state.shutdown_workspace_transition().await }),
        },
        CliShutdownStep {
            name: "scheduler",
            future: Box::pin(async {
                app_state
                    .shutdown_scheduler()
                    .await
                    .map_err(anyhow::Error::from)
            }),
        },
        CliShutdownStep {
            name: "TaskRun drivers",
            future: Box::pin(async {
                if let Some(store) = app_state.tasks.runtime.as_ref() {
                    store
                        .shutdown_run_drivers()
                        .await
                        .map_err(|error| anyhow::anyhow!(error))?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "agent pool",
            future: Box::pin(async {
                if let Some(pool) = app_state.connection.pool.as_ref() {
                    pool.shutdown().await.map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "plugin runtime",
            future: Box::pin(async { plugin_runtime.shutdown().await }),
        },
    ]);
    drain_cli_shutdown(repl_result, steps).await
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
    review_integration: Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    foreground_turns: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    shutdown: echo_agent::agent::CancellationToken,
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
                manager.register(Box::new(ch))?;
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
                manager.register(Box::new(ch))?;
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
        let review_integration = review_integration.clone();
        let webhook_emitter = webhook_emitter.clone();
        let foreground_turns = foreground_turns.clone();
        Arc::new(SessionHandler::new(
            session_config,
            move || -> Box<dyn MessageHandler> {
                Box::new(AppChannelMessageHandler::new(
                    pool.clone(),
                    store.clone(),
                    review_integration.clone(),
                    webhook_emitter.clone(),
                    foreground_turns.clone(),
                ))
            },
        ))
    };

    tracing::info!("启动 {} 个 IM 通道...", manager.len());
    let start_results = manager.start_all(handler_factory).await;
    let failures: Vec<_> = start_results
        .iter()
        .filter(|result| result.result.is_err())
        .collect();
    if !failures.is_empty() {
        tracing::warn!(
            failed_channels = %failures
                .iter()
                .map(|failure| failure.channel_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            "{} 个通道启动失败（共 {} 个）",
            failures.len(),
            start_results.len()
        );
    }
    tracing::info!("所有 IM 通道已启动");

    tokio::select! {
        _ = crate::infra::shutdown_signal() => {}
        _ = shutdown.cancelled() => {}
    }

    tracing::info!("正在关闭 IM 通道...");
    manager.stop_all().await?;
    tracing::info!("所有 IM 通道已关闭");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn recorded_shutdown_step(
        calls: Arc<Mutex<Vec<&'static str>>>,
        name: &'static str,
        fail: bool,
    ) -> CliShutdownStep<'static> {
        CliShutdownStep {
            name,
            future: Box::pin(async move {
                calls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("shutdown call log is unavailable"))?
                    .push(name);
                if fail {
                    Err(anyhow::anyhow!("{name} injected failure"))
                } else {
                    Ok(())
                }
            }),
        }
    }

    #[tokio::test]
    async fn earlier_shutdown_failures_do_not_skip_later_owners() -> Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let steps = vec![
            recorded_shutdown_step(Arc::clone(&calls), "foreground", true),
            recorded_shutdown_step(Arc::clone(&calls), "model", false),
            recorded_shutdown_step(Arc::clone(&calls), "Dreaming", false),
            recorded_shutdown_step(Arc::clone(&calls), "memory", false),
            recorded_shutdown_step(Arc::clone(&calls), "workspace", true),
            recorded_shutdown_step(Arc::clone(&calls), "scheduler", false),
            recorded_shutdown_step(Arc::clone(&calls), "drivers", false),
            recorded_shutdown_step(Arc::clone(&calls), "pool", false),
            recorded_shutdown_step(Arc::clone(&calls), "plugin", false),
        ];

        let error = drain_cli_shutdown(Ok(()), steps)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("injected shutdown failures were not reported"))?;
        let observed = calls
            .lock()
            .map_err(|_| anyhow::anyhow!("shutdown call log is unavailable"))?
            .clone();

        assert_eq!(
            observed,
            [
                "foreground",
                "model",
                "Dreaming",
                "memory",
                "workspace",
                "scheduler",
                "drivers",
                "pool",
                "plugin"
            ]
        );
        assert!(error.to_string().contains("foreground injected failure"));
        assert!(error.to_string().contains("workspace injected failure"));
        Ok(())
    }
}
