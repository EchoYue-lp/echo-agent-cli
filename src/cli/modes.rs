//! 运行模式管理
//!
//! 提供 CLI 模式和 IM 通道模式的启动逻辑。
//! Web 模式已移除 — GUI 通过 Tauri IPC 通信。

use anyhow::Result;

use crate::agent_handle::AgentHandle;
use crate::cli::args::{Args, JsonlApprovalPolicy, JsonlInteractionMode, JsonlPermissionMode};
use echo_agent_app_core::config::EkoConfig;

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
        if let Some(settlement) = self.settlement.take() {
            settlement.abort();
        }
    }
}

pub struct HeadlessDreamingOwner {
    cancel: tokio_util::sync::CancellationToken,
    settlement: Option<tokio::task::JoinHandle<()>>,
}

impl HeadlessDreamingOwner {
    pub fn new(
        cancel: tokio_util::sync::CancellationToken,
        settlement: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            cancel,
            settlement: Some(settlement),
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.cancel.cancel();
        let settlement = self
            .settlement
            .take()
            .ok_or_else(|| anyhow::anyhow!("Dreaming settlement handle is unavailable"))?;
        settlement
            .await
            .map_err(|error| anyhow::anyhow!("Dreaming settlement task failed: {error}"))
    }
}

impl Drop for HeadlessDreamingOwner {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(settlement) = self.settlement.take() {
            settlement.abort();
        }
    }
}

async fn drain_cli_shutdown(
    mode_result: Result<()>,
    steps: Vec<CliShutdownStep<'_>>,
) -> Result<()> {
    let mut failures = mode_result
        .err()
        .map(|error| format!("surface: {error}"))
        .into_iter()
        .collect::<Vec<_>>();

    for step in steps {
        if let Err(error) = step.future.await {
            tracing::warn!(step = step.name, %error, "application shutdown step failed");
            failures.push(format!("{}: {error}", step.name));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "application mode failed: {}",
            failures.join("; ")
        ))
    }
}

fn repl_config_for(args: &Args) -> crate::cli::ReplConfig {
    crate::cli::ReplConfig {
        prompt: "echo".to_string(),
        history_file: echo_agent_app_core::data_root::user_data_path("history.txt")
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
    pub runtime_state_store: Option<std::sync::Arc<dyn echo_agent::state::RuntimeStateStore>>,
    pub review_integration:
        Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    pub mcp_config_runtime:
        std::sync::Arc<echo_agent_app_core::mcp_config_runtime::McpConfigRuntime>,
    pub plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    pub config_watcher: std::sync::Arc<echo_agent_app_core::config_watcher::ConfigWatcherHandle>,
    pub foreground_turns: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    pub command_cell_runtime: std::sync::Arc<
        echo_agent_app_core::tasks::task_runtime::command_cells::CommandCellRuntimeService,
    >,
    pub browser_runtime: std::sync::Arc<echo_agent_app_core::browser::BrowserRuntime>,
}

/// Composition receipt for the single headless bootstrap shared by CLI and
/// channel surfaces. It contains no independent lifecycle state; shutdown is
/// delegated to the existing AppState subsystem owners.
pub struct HeadlessServices {
    pub task_service: Option<std::sync::Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    pub scheduler_runner: Option<std::sync::Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    pub app_state: std::sync::Arc<echo_agent_app_core::state::AppState>,
}

async fn await_jsonl_driver_or_cancel<Driver, Signal, Output>(
    driver: Driver,
    cancel: echo_agent::agent::CancellationToken,
    signal: Signal,
) -> Output
where
    Driver: std::future::Future<Output = Output>,
    Signal: std::future::Future<Output = ()>,
{
    tokio::pin!(driver);
    tokio::pin!(signal);
    tokio::select! {
        result = &mut driver => result,
        () = &mut signal => {
            cancel.cancel();
            driver.await
        }
    }
}

#[derive(Debug, Clone)]
pub struct JsonlRunOptions {
    pub interaction_mode: JsonlInteractionMode,
    pub permission_mode: JsonlPermissionMode,
    pub approval_policy: JsonlApprovalPolicy,
    pub attachment_paths: Vec<std::path::PathBuf>,
}

/// Run one prompt through the shared finite chat driver and print only the
/// canonical, already-journaled application envelope stream.
pub async fn run_jsonl_mode(
    _agent: AgentHandle,
    prompt: &str,
    conversation_id: String,
    services: &HeadlessServices,
    options: JsonlRunOptions,
) -> Result<()> {
    if prompt.trim().is_empty() {
        return Err(anyhow::anyhow!("--jsonl requires a non-empty prompt"));
    }

    let turn_id = uuid::Uuid::new_v4().to_string();
    let (scoped_runtime, lease) = services
        .app_state
        .begin_scoped_chat_turn_owned(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &conversation_id,
            turn_id.clone(),
        )
        .await
        .map_err(anyhow::Error::from)?;
    let pool_execution = scoped_runtime
        .agent_for(&conversation_id)
        .await
        .map_err(anyhow::Error::from)?;
    let agent = pool_execution.agent();
    let renderer: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
        std::sync::Arc::new(crate::cli::jsonl::JsonlChatSink::stdout());
    let sink = echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
        echo_agent_app_core::chat_event_log::ChatSurface::Cli,
        renderer,
        services.app_state.storage.chat_events.clone(),
        services.app_state.storage.tool_executions.clone(),
        scoped_runtime.execution_scope().workspace_id().to_string(),
        Some(conversation_id.clone()),
        turn_id.clone(),
    );
    let permission_mode =
        echo_agent_app_core::permission::parse_permission_mode(options.permission_mode.as_str())
            .map_err(anyhow::Error::msg)?;
    agent
        .write(|agent| agent.set_permission_mode(permission_mode))
        .await;

    let workspace_root = scoped_runtime.execution_scope().root().to_path_buf();
    let mut attachments = Vec::with_capacity(options.attachment_paths.len());
    for path in &options.attachment_paths {
        match echo_agent_app_core::attachments::stage_local_attachment(path, Some(&workspace_root))
        {
            Ok(attachment) => attachments.push(attachment),
            Err(error) => {
                let cleanup =
                    echo_agent_app_core::attachments::discard_staged_attachment_refs(&attachments)
                        .err()
                        .map(|cleanup| format!("; attachment cleanup failed: {cleanup}"))
                        .unwrap_or_default();
                lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "jsonl_attachment",
                        format!("failed to stage {}: {error}{cleanup}", path.display()),
                    ),
                ));
                return Err(anyhow::anyhow!(
                    "failed to stage JSONL attachment {}: {error}{cleanup}",
                    path.display()
                ));
            }
        }
    }
    if !sink.on_event(
        echo_agent_app_core::chat_driver::ChatDriverEvent::TurnConfiguration {
            interaction_mode: options.interaction_mode.runtime().as_str().to_string(),
            permission_mode: options.permission_mode.as_str().to_string(),
            approval_policy: options.approval_policy.as_str().to_string(),
            attachments: attachments
                .iter()
                .map(
                    |attachment| echo_agent_app_core::chat_driver::ChatAttachmentDescriptor {
                        name: attachment.name.clone(),
                        mime_type: attachment.mime_type.clone(),
                        source: attachment.source,
                    },
                )
                .collect(),
        },
    ) {
        let _ = echo_agent_app_core::attachments::discard_staged_attachment_refs(&attachments);
        lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message(
                "jsonl_output",
                "JSONL output closed before turn configuration was delivered",
            ),
        ));
        return Err(anyhow::anyhow!(
            "JSONL output closed before turn configuration was delivered"
        ));
    }
    if !sink.on_event(
        echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        },
    ) {
        lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message(
                "jsonl_output",
                "JSONL output closed before the turn started",
            ),
        ));
        return Err(anyhow::anyhow!(
            "JSONL output closed before the turn started"
        ));
    }

    let title: String = prompt.trim().chars().take(80).collect();
    if let Err(error) = scoped_runtime
        .ensure_conversation(echo_agent::memory::NewConversation {
            conversation_id: conversation_id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some(title),
        })
        .await
    {
        let detail = format!("failed to persist JSONL conversation metadata: {error}");
        let _ = sink.on_event(
            echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
                status: "failed".to_string(),
            },
        );
        lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message("conversation_store", detail.clone()),
        ));
        return Err(anyhow::Error::msg(detail));
    }

    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(&workspace_root));
    let turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: prompt,
            attachments: &attachments,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&turn_id),
        },
    ) {
        Ok(turn) => turn,
        Err(error) => {
            let detail = format!("failed to prepare JSONL user turn: {error}");
            let _ = sink.on_event(
                echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
                    status: "failed".to_string(),
                },
            );
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("prepared_turn", detail.clone()),
            ));
            return Err(anyhow::Error::msg(detail));
        }
    };
    let resources = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        execution_scope: scoped_runtime.execution_scope().clone(),
        pool: scoped_runtime.pool(),
        store: scoped_runtime.task_runtime(),
        sink: sink.clone(),
        webhook_emitter: Some(services.app_state.webhook.emitter.clone()),
        conv_id: Some(conversation_id),
        root_message_id: turn_id,
        attachments: turn.inline_attachment_refs(),
        cancel: lease.cancellation_token(),
        interaction_mode: options.interaction_mode.runtime(),
        review_integration: scoped_runtime.review_integration(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(std::sync::Arc::new(
            crate::cli::jsonl::JsonlHumanLoopProvider::new(sink.clone(), options.approval_policy),
        )),
    });
    let turn_cancel = lease.cancellation_token();
    let _pool_execution = pool_execution;
    let result = await_jsonl_driver_or_cancel(
        echo_agent_app_core::foreground_turn::drive_foreground_chat(
            lease, &agent, &turn, resources,
        ),
        turn_cancel,
        async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::warn!(%error, "JSONL Ctrl+C listener is unavailable");
                std::future::pending::<()>().await;
            }
        },
    )
    .await;
    let status = result.as_ref().map_or(
        "failed",
        echo_agent_app_core::chat_driver::TurnOutcome::status,
    );
    if !sink.on_event(
        echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
            status: status.to_string(),
        },
    ) {
        return Err(anyhow::anyhow!(
            "JSONL output closed before the terminal status was delivered"
        ));
    }

    match result {
        Ok(echo_agent_app_core::chat_driver::TurnOutcome::Completed) => Ok(()),
        Ok(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled) => {
            Err(anyhow::anyhow!("one-shot turn was cancelled"))
        }
        Ok(echo_agent_app_core::chat_driver::TurnOutcome::Failed(failure)) => {
            Err(anyhow::anyhow!("{}: {}", failure.code, failure.message))
        }
        Err(error) => Err(anyhow::Error::msg(error)),
    }
}

pub async fn start_headless_services(
    agent: AgentHandle,
    hitl_dispatcher: std::sync::Arc<crate::state::HitlDispatcher>,
    app_config: &EkoConfig,
    resources: HeadlessServiceResources,
) -> Result<HeadlessServices> {
    let scheduler_store: std::sync::Arc<dyn echo_agent::memory::Store> = {
        let file_path = echo_agent_app_core::data_root::user_data_path("scheduler_store");
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
        resources.runtime_state_store,
        app_config.clone(),
        resources.mcp_config_runtime,
    )?
    .with_active_model_id(resources.active_model_id)
    .with_review_integration(resources.review_integration)
    .with_plugin_runtime(Some(resources.plugin_runtime))
    .with_config_watcher(Some(resources.config_watcher))
    .with_foreground_turns(resources.foreground_turns);
    state.webhook.emitter = resources.webhook_emitter;
    state.connection.pool = Some(resources.pool);
    state.tasks.runtime = resources.task_runtime_store;
    state = state
        .with_command_cell_runtime(resources.command_cell_runtime)
        .with_workspace_delete_hook(resources.browser_runtime);
    match state.recover_committed_conversation_deletions().await {
        Ok(receipts) if !receipts.is_empty() => tracing::info!(
            count = receipts.len(),
            "Recovered committed conversation deletion finalizers"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            %error,
            "Some committed conversation deletion finalizers remain pending"
        ),
    }
    state
        .start_scheduler_and_task_service(Some(scheduler_store))
        .await?;
    let task_service = state.tasks.service.clone();
    let scheduler = state.scheduler.runner.clone();
    let app_state = std::sync::Arc::new(state);
    if let Err(error) = app_state.recover_agent_deliveries().await {
        tracing::warn!(%error, "failed to resume durable Agent deliveries during headless startup");
    }
    Ok(HeadlessServices {
        task_service,
        scheduler_runner: scheduler,
        app_state,
    })
}

/// 运行 CLI 模式
#[allow(clippy::too_many_arguments)] // startup adapter wires the shared agent, pool, stores, and UI services once
pub async fn run_cli_mode(
    agent: AgentHandle,
    args: &Args,
    review_integration: Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    prompt_assembly: echo_agent_app_core::project::prompt::PromptAssembly,
    pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    task_runtime_store: Option<
        std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
    conversation_id: String,
    webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    services: &HeadlessServices,
    repl_hitl_session: crate::cli::ReplHumanLoopSession,
    companion_shutdown: Option<CompanionModeShutdown>,
) -> Result<()> {
    let mut companion_shutdown = companion_shutdown;
    let mut repl_config = repl_config_for(args);
    repl_config.task_service = services.task_service.clone();
    repl_config.scheduler_runner = services.scheduler_runner.clone();
    repl_config.review_integration = review_integration;
    repl_config.prompt_assembly = Some(prompt_assembly);
    repl_config.pool = Some(pool.clone());
    repl_config.task_runtime_store = task_runtime_store.clone();
    repl_config.conversation_id = conversation_id;
    repl_config.webhook_emitter = Some(webhook_emitter);
    repl_config.plugin_runtime = Some(plugin_runtime.clone());
    repl_config.app_state = Some(services.app_state.clone());

    let repl_result = crate::cli::run_repl(agent, repl_config, repl_hitl_session).await;
    let mut steps = Vec::new();
    if let Some(companion) = companion_shutdown.take() {
        steps.push(CliShutdownStep {
            name: companion.name,
            future: Box::pin(companion.shutdown()),
        });
    }
    drain_cli_shutdown(repl_result, steps).await
}

/// Settle every shared headless owner exactly once after all product surfaces
/// have stopped accepting work. Failures are aggregated without skipping later
/// teardown steps.
#[allow(clippy::too_many_arguments)]
pub async fn shutdown_headless_services(
    mode_result: Result<()>,
    services: HeadlessServices,
    dreaming_owner: Option<HeadlessDreamingOwner>,
    session_exit_agent: Option<AgentHandle>,
    plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    config_watcher: std::sync::Arc<echo_agent_app_core::config_watcher::ConfigWatcherHandle>,
    mcp_config_runtime: std::sync::Arc<echo_agent_app_core::mcp_config_runtime::McpConfigRuntime>,
    browser_runtime: std::sync::Arc<echo_agent_app_core::browser::BrowserRuntime>,
    root_cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let app_state = services.app_state;
    let review_integration = app_state.review_integration.clone();
    let auto_memory_integration = review_integration.clone();
    let session_review_integration = review_integration.clone();
    let background_review_integration = review_integration;
    let run_session_review = session_exit_agent.is_some();
    let steps = vec![
        CliShutdownStep {
            name: "Agent deliveries",
            future: Box::pin(async {
                app_state
                    .shutdown_agent_deliveries()
                    .await
                    .map_err(anyhow::Error::from)
            }),
        },
        CliShutdownStep {
            name: "foreground turns",
            future: Box::pin(async {
                app_state
                    .session
                    .foreground_turns
                    .shutdown()
                    .await
                    .map_err(anyhow::Error::from)
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
                if let Some(owner) = dreaming_owner {
                    owner.shutdown().await?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "auto-memory",
            future: Box::pin(async move {
                if let Some(agent) = session_exit_agent.as_ref() {
                    crate::cli::repl::run_auto_memory_on_exit(agent, &auto_memory_integration)
                        .await;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "memory review",
            future: Box::pin(async move {
                if run_session_review {
                    crate::cli::repl::run_memory_review_on_exit(&session_review_integration).await;
                }
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
                        .map_err(anyhow::Error::msg)?;
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
            name: "command cells",
            future: Box::pin(async {
                app_state
                    .shutdown_command_cells()
                    .await
                    .map_err(anyhow::Error::msg)
            }),
        },
        CliShutdownStep {
            name: "plugin runtime",
            future: Box::pin(async { plugin_runtime.shutdown().await }),
        },
        CliShutdownStep {
            name: "config watcher",
            future: Box::pin(async { config_watcher.shutdown().await }),
        },
        CliShutdownStep {
            name: "MCP runtime",
            future: Box::pin(async {
                mcp_config_runtime.shutdown().await;
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "browser runtime",
            future: Box::pin(async {
                browser_runtime.shutdown().await;
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "task hook dispatcher",
            future: Box::pin(async {
                if let Some(store) = app_state.tasks.runtime.as_ref() {
                    store
                        .shutdown_hook_events()
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
        CliShutdownStep {
            name: "root cancellation",
            future: Box::pin(async {
                root_cancel.cancel();
                Ok(())
            }),
        },
    ];
    drain_cli_shutdown(mode_result, steps).await
}

/// 运行 IM 通道模式（QQ Bot、飞书等）
///
/// Channel agent 经 `AgentPool` 全套接通(bootstrap 等价:state_store/store/compressor/
/// MemoryLayerManager/permission_service/per-sender cache_user_id+conversation_id),
/// per-sender 隔离由 pool key `channel:{channel_id}:{sender_id}` 承载。
#[cfg(feature = "channels")]
pub struct ChannelsModeArgs {
    pub app_state: std::sync::Arc<echo_agent_app_core::state::AppState>,
    pub pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    pub app_config: EkoConfig,
    pub task_runtime_store:
        Option<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    pub review_integration:
        Option<std::sync::Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    pub webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    pub foreground_turns: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    pub shutdown: echo_agent::agent::CancellationToken,
}

#[cfg(feature = "channels")]
pub async fn run_channels_mode(args: ChannelsModeArgs) -> Result<()> {
    use std::sync::Arc;

    use echo_agent::channels::{
        ChannelManager, FeishuChannel, FeishuConfig, MessageHandler, QqChannel, QqConfig,
        SessionHandler,
    };

    use crate::cli::channels::AppChannelMessageHandler;

    let ChannelsModeArgs {
        app_state,
        pool,
        app_config,
        task_runtime_store,
        review_integration,
        webhook_emitter,
        foreground_turns,
        shutdown,
    } = args;

    let mut manager = ChannelManager::new();
    let mut failures = Vec::new();

    // 注册 QQ Bot
    if app_config.channels.qq.enabled {
        let config = QqConfig {
            app_id: app_config.channels.qq.app_id.clone(),
            client_secret: app_config.channels.qq.client_secret.clone(),
        };
        match QqChannel::new(config) {
            Ok(ch) => {
                if let Err(error) = manager.register(Box::new(ch)) {
                    failures.push(format!("qqbot registration: {error}"));
                } else {
                    tracing::info!("已注册 QQ Bot 通道");
                }
            }
            Err(error) => failures.push(format!("qqbot configuration: {error}")),
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
                if let Err(error) = manager.register(Box::new(ch)) {
                    failures.push(format!("feishu registration: {error}"));
                } else {
                    tracing::info!("已注册飞书通道（{}模式）", app_config.channels.feishu.mode);
                }
            }
            Err(error) => failures.push(format!("feishu configuration: {error}")),
        }
    } else {
        tracing::info!("飞书通道已禁用（channels.feishu.enabled = false）");
    }

    if manager.is_empty() {
        failures.push("no configured IM channel could be started".to_string());
        return finish_channel_lifecycle(failures, Ok(()));
    }

    // Framework reset aliases would replace SessionHandler generations before
    // EKO's exact foreground/pool owner can settle them. Disable those aliases
    // at composition; `/reset` is handled by AppChannelMessageHandler.
    let session_config = channel_session_config(app_config.channels.session.timeout_minutes);

    // pool create_agent 用 app_config 默认 model(system_prompt/agent_name 来自
    // app_config),无需在此解析 runtime_model 或裸建 agent —— bootstrap 全套已由
    // pool 注入。handler_factory 每 channel 产出一个 SessionHandler,其内层工厂
    // 每 (channel,sender) 产出 AppChannelMessageHandler(持 pool clone)。
    let handler_factory = move |_channel_id: &str| -> Arc<dyn MessageHandler> {
        let session_config = session_config.clone();
        let app_state = app_state.clone();
        let pool = pool.clone();
        let store = task_runtime_store.clone();
        let review_integration = review_integration.clone();
        let webhook_emitter = webhook_emitter.clone();
        let foreground_turns = foreground_turns.clone();
        Arc::new(SessionHandler::new(
            session_config,
            move || -> Box<dyn MessageHandler> {
                Box::new(AppChannelMessageHandler::new(
                    app_state.clone(),
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
    let start_failures = start_results
        .iter()
        .filter_map(|result| {
            result
                .result
                .as_ref()
                .err()
                .map(|error| format!("{}: {error}", result.channel_id))
        })
        .collect::<Vec<_>>();
    let started_count = start_results.len().saturating_sub(start_failures.len());
    if !start_failures.is_empty() {
        tracing::warn!(
            failed_channels = %start_failures
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(","),
            "{} 个通道启动失败（共 {} 个）",
            start_failures.len(),
            start_results.len()
        );
    }
    failures.extend(start_failures);
    if started_count == 0 {
        let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
        return finish_channel_lifecycle(failures, stop_result);
    }
    tracing::info!(started_count, "IM channels started");

    tokio::select! {
        _ = crate::infra::shutdown_signal() => {}
        _ = shutdown.cancelled() => {}
    }

    tracing::info!("正在关闭 IM 通道...");
    let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
    finish_channel_lifecycle(failures, stop_result)
}

#[cfg(feature = "channels")]
fn finish_channel_lifecycle(start_failures: Vec<String>, stop_result: Result<()>) -> Result<()> {
    let mut failures = start_failures;
    if let Err(error) = stop_result {
        failures.push(format!("shutdown: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "channel lifecycle failed: {}",
            failures.join("; ")
        ))
    }
}

#[cfg(feature = "channels")]
fn channel_session_config(timeout_minutes: u64) -> echo_agent::channels::SessionConfig {
    echo_agent::channels::SessionConfig::default()
        .with_timeout_minutes(timeout_minutes)
        .with_reset_keywords(Vec::new())
        .with_command_prefix(None)
        .with_reset_commands(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "channels")]
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    #[tokio::test]
    async fn headless_dreaming_owner_cancels_and_joins_its_task() -> Result<()> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_cancel = cancel.clone();
        let task_observed = Arc::clone(&observed);
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            task_observed.store(true, std::sync::atomic::Ordering::Release);
        });

        HeadlessDreamingOwner::new(cancel, task).shutdown().await?;
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct ResetProbe {
        calls: Arc<AtomicUsize>,
    }

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for ResetProbe {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(echo_agent::channels::OutboundMessage::new(
                message.channel_id,
                message.chat_id,
                message.chat_type,
                "app-owned-reset",
            ))
        }

        async fn reply(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn production_session_config_forwards_reset_to_app_handler() -> Result<()> {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler, SessionHandler};
        use futures::StreamExt;

        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::clone(&calls);
        let handler = SessionHandler::new(channel_session_config(30), move || {
            Box::new(ResetProbe {
                calls: Arc::clone(&factory_calls),
            }) as Box<dyn MessageHandler>
        });
        let mut stream = handler
            .handle_stream(InboundMessage::new(
                "test-channel",
                "sender",
                "conversation",
                ChatType::Direct,
                "/reset",
                "message",
            ))
            .await?;
        let response = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("session handler returned an empty reset stream"))??;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.text, "app-owned-reset");
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct LifecycleProbeChannel {
        id: &'static str,
        fail_start: bool,
        fail_stop: bool,
        stops: Arc<AtomicUsize>,
    }

    #[cfg(feature = "channels")]
    fn channel_test_error(message: impl Into<String>) -> echo_agent::error::ReactError {
        echo_agent::error::ReactError::Channel(Box::new(echo_agent::error::ChannelError::Other(
            message.into(),
        )))
    }

    #[cfg(feature = "channels")]
    static LIFECYCLE_PROBE_CAPABILITIES: echo_agent::channels::ChannelCapabilities =
        echo_agent::channels::ChannelCapabilities {
            chat_types: &[echo_agent::channels::ChatType::Direct],
            supports_media: false,
            supports_threads: false,
        };

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::ChannelPlugin for LifecycleProbeChannel {
        fn id(&self) -> &str {
            self.id
        }

        fn capabilities(&self) -> &echo_agent::channels::ChannelCapabilities {
            &LIFECYCLE_PROBE_CAPABILITIES
        }

        async fn start(
            &mut self,
            _handler: Arc<dyn echo_agent::channels::MessageHandler>,
        ) -> echo_agent::error::Result<()> {
            if self.fail_start {
                Err(channel_test_error(format!("{} start failure", self.id)))
            } else {
                Ok(())
            }
        }

        async fn stop(&mut self) -> echo_agent::error::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.fail_stop {
                Err(channel_test_error(format!("{} stop failure", self.id)))
            } else {
                Ok(())
            }
        }

        async fn send(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_channel_manager_drains_and_aggregates_start_and_stop_failures() -> Result<()> {
        use echo_agent::channels::{ChannelManager, MessageHandler};

        let start_stops = Arc::new(AtomicUsize::new(0));
        let stop_stops = Arc::new(AtomicUsize::new(0));
        let mut manager = ChannelManager::new();
        manager.register(Box::new(LifecycleProbeChannel {
            id: "start-fails",
            fail_start: true,
            fail_stop: false,
            stops: Arc::clone(&start_stops),
        }))?;
        manager.register(Box::new(LifecycleProbeChannel {
            id: "stop-fails",
            fail_start: false,
            fail_stop: true,
            stops: Arc::clone(&stop_stops),
        }))?;

        let starts = manager
            .start_all(|_| {
                Arc::new(ResetProbe {
                    calls: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn MessageHandler>
            })
            .await;
        let start_failures = starts
            .into_iter()
            .filter_map(|result| {
                result
                    .result
                    .err()
                    .map(|error| format!("{}: {error}", result.channel_id))
            })
            .collect::<Vec<_>>();
        let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
        let error = finish_channel_lifecycle(start_failures, stop_result)
            .err()
            .ok_or_else(|| anyhow::anyhow!("channel lifecycle failures were not reported"))?;

        assert_eq!(start_stops.load(Ordering::SeqCst), 1);
        assert_eq!(stop_stops.load(Ordering::SeqCst), 1);
        assert!(error.to_string().contains("start-fails"));
        assert!(error.to_string().contains("stop-fails"));
        Ok(())
    }
}
