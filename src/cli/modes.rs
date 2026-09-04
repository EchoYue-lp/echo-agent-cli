//! 运行模式管理
//!
//! 提供 CLI 模式和 IM 通道模式的启动逻辑。
//! Web 模式已移除 — GUI 通过 Tauri IPC 通信。

use anyhow::Result;
use futures::FutureExt;

use crate::agent_handle::AgentHandle;
use crate::cli::args::{Args, JsonlApprovalPolicy, JsonlPermissionMode};
#[cfg(feature = "channels")]
use echo_agent_app_core::api::config::EkoConfig;
use echo_agent_app_core::api::runtime::ApplicationServices;

/// Owned settlement for a concurrently running product surface that must stop
/// before CLI tears down shared foreground, pool, and plugin resources.
pub struct CompanionModeShutdown {
    name: &'static str,
    cancel: echo_agent::agent::CancellationToken,
    settlement: futures::future::Shared<
        futures::future::BoxFuture<'static, std::result::Result<(), String>>,
    >,
    cancel_on_drop: bool,
}

#[derive(Clone)]
pub struct CompanionModeObserver {
    settlement: futures::future::Shared<
        futures::future::BoxFuture<'static, std::result::Result<(), String>>,
    >,
}

impl CompanionModeShutdown {
    pub fn new(
        name: &'static str,
        cancel: echo_agent::agent::CancellationToken,
        settlement: tokio::task::JoinHandle<Result<()>>,
    ) -> Self {
        let settlement = async move {
            match settlement.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(format!("{name} settlement task failed: {error}")),
            }
        }
        .boxed()
        .shared();
        Self {
            name,
            cancel,
            settlement,
            cancel_on_drop: true,
        }
    }

    pub fn bind(mut self, services: &mut ApplicationServices) -> Result<CompanionModeObserver> {
        let cancel = self.cancel.clone();
        let settlement = self.settlement.clone();
        let observer = CompanionModeObserver {
            settlement: self.settlement.clone(),
        };
        services.track_external_owner(
            self.name,
            move || {
                cancel.cancel();
                Ok(())
            },
            settlement,
        )?;
        self.cancel_on_drop = false;
        Ok(observer)
    }
}

impl Drop for CompanionModeShutdown {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.cancel.cancel();
        }
    }
}

impl CompanionModeObserver {
    pub async fn wait(self) -> Result<()> {
        self.settlement.await.map_err(anyhow::Error::msg)
    }
}

fn repl_config_for(args: &Args) -> crate::cli::ReplConfig {
    crate::cli::ReplConfig {
        prompt: "echo".to_string(),
        history_file: echo_agent_app_core::api::data_root::user_data_path("history.txt")
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
        subagent_projection: None,
    }
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
    pub permission_mode: JsonlPermissionMode,
    pub approval_policy: JsonlApprovalPolicy,
    pub attachment_paths: Vec<std::path::PathBuf>,
}

async fn run_jsonl_extension_command(
    request: echo_agent_app_core::api::extension_commands::ExtensionCommandRequest,
    scoped_runtime: echo_agent_app_core::api::state::ScopedChatRuntime,
    lease: echo_agent_app_core::api::foreground_turn::ForegroundTurnLease,
    sink: std::sync::Arc<dyn echo_agent_app_core::api::chat_driver::ChatSink>,
    services: &ApplicationServices,
    conversation_id: &str,
    options: &JsonlRunOptions,
) -> Result<()> {
    let receipt = if options.attachment_paths.is_empty() {
        echo_agent_app_core::api::extension_commands::ExtensionCommandDispatcher::new(
            services.app_state.clone(),
        )
        .dispatch(request, Some(scoped_runtime), conversation_id.to_string())
        .await
    } else {
        echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt::failed(
            request.kind(),
            request.identity(),
            scoped_runtime.execution_scope().workspace_id().to_string(),
            "JSONL Extension management commands do not accept attachments",
        )
    };
    finish_jsonl_extension_command(lease, sink, receipt).await
}

async fn settle_jsonl_foreground(
    lease: echo_agent_app_core::api::foreground_turn::ForegroundTurnLease,
    outcome: echo_agent_app_core::api::chat_driver::TurnOutcome,
) -> Result<()> {
    lease
        .settle_after_observers(outcome)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::new)
}

async fn finish_jsonl_extension_command(
    lease: echo_agent_app_core::api::foreground_turn::ForegroundTurnLease,
    sink: std::sync::Arc<dyn echo_agent_app_core::api::chat_driver::ChatSink>,
    receipt: echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt,
) -> Result<()> {
    let outcome = extension_receipt_terminal(&receipt);
    if !sink.on_event(
        echo_agent_app_core::api::chat_driver::ChatDriverEvent::ExtensionReceipt(Box::new(receipt)),
    ) {
        settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "jsonl_output",
                    "JSONL output closed before the Extension receipt was delivered",
                ),
            ),
        )
        .await?;
        return Err(anyhow::anyhow!(
            "JSONL output closed before the Extension receipt was delivered"
        ));
    }
    let terminal_status = outcome.status().to_string();
    let delivered = sink.on_event(
        echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
            status: terminal_status,
        },
    );
    settle_jsonl_foreground(lease, outcome.clone()).await?;
    if !delivered {
        return Err(anyhow::anyhow!(
            "JSONL output closed before the terminal status was delivered"
        ));
    }
    match outcome {
        echo_agent_app_core::api::chat_driver::TurnOutcome::Completed => Ok(()),
        echo_agent_app_core::api::chat_driver::TurnOutcome::Cancelled => {
            Err(anyhow::anyhow!("Extension command was cancelled"))
        }
        echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(failure) => {
            Err(anyhow::anyhow!("{}: {}", failure.code, failure.message))
        }
    }
}

pub(crate) fn extension_receipt_terminal(
    receipt: &echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt,
) -> echo_agent_app_core::api::chat_driver::TurnOutcome {
    use echo_agent_app_core::api::extension_commands::ExtensionCommandStatus;

    match receipt.status() {
        ExtensionCommandStatus::Settled => {
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed
        }
        ExtensionCommandStatus::Committed => {
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "extension_committed",
                    receipt.meta().error.clone().unwrap_or_else(|| {
                        "Extension durable state is committed; runtime settlement is pending"
                            .to_string()
                    }),
                ),
            )
        }
        ExtensionCommandStatus::Degraded => {
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "extension_degraded",
                    receipt
                        .meta()
                        .error
                        .clone()
                        .unwrap_or_else(|| "Extension settlement is degraded".to_string()),
                ),
            )
        }
        ExtensionCommandStatus::Failed => {
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "extension_failed",
                    receipt
                        .meta()
                        .error
                        .clone()
                        .unwrap_or_else(|| "Extension command failed".to_string()),
                ),
            )
        }
    }
}

/// Run one prompt through the shared finite chat driver and print only the
/// canonical, already-journaled application envelope stream.
pub async fn run_jsonl_mode(
    _agent: AgentHandle,
    prompt: &str,
    conversation_id: String,
    services: &ApplicationServices,
    options: JsonlRunOptions,
) -> Result<()> {
    if prompt.trim().is_empty() {
        return Err(anyhow::anyhow!("--jsonl requires a non-empty prompt"));
    }
    let reflection_command = echo_agent_app_core::api::reflection::ReflectionCommand::parse(prompt)
        .map_err(anyhow::Error::new)?;

    let turn_id = uuid::Uuid::new_v4().to_string();
    let (scoped_runtime, lease) = services
        .app_state
        .begin_scoped_chat_turn_owned(
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Cli,
            &conversation_id,
            turn_id.clone(),
        )
        .await
        .map_err(anyhow::Error::from)?;
    let jsonl_renderer = std::sync::Arc::new(crate::cli::jsonl::JsonlChatSink::stdout());
    let renderer: std::sync::Arc<dyn echo_agent_app_core::api::chat_driver::ChatSink> =
        jsonl_renderer.clone();
    let sink = echo_agent_app_core::api::chat_event_log::bind_surface_chat_sink(
        echo_agent_app_core::api::chat_event_log::ChatSurface::Cli,
        renderer,
        services.app_state.storage.chat_events.clone(),
        services.app_state.storage.tool_executions.clone(),
        scoped_runtime.execution_scope().workspace_id().to_string(),
        Some(conversation_id.clone()),
        turn_id.clone(),
    );
    if reflection_command.is_some() {
        if !options.attachment_paths.is_empty() {
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "reflection_attachments",
                        "/reflect does not accept attachments",
                    ),
                ),
            )
            .await?;
            return Err(anyhow::anyhow!("/reflect does not accept attachments"));
        }
        if !sink.on_event(
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnConfiguration {
                permission_mode: options.permission_mode.as_str().to_string(),
                approval_policy: options.approval_policy.as_str().to_string(),
                attachments: Vec::new(),
            },
        ) || !sink.on_event(
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        ) {
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "jsonl_output",
                        "JSONL output closed before reflection started",
                    ),
                ),
            )
            .await?;
            return Err(anyhow::anyhow!(
                "JSONL output closed before reflection started"
            ));
        }
        let execution = match scoped_runtime.agent_for(&conversation_id).await {
            Ok(execution) => execution,
            Err(error) => {
                let detail = format!("Reflection Agent is unavailable: {error}");
                settle_jsonl_foreground(
                    lease,
                    echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "reflection_agent",
                            detail.clone(),
                        ),
                    ),
                )
                .await?;
                return Err(anyhow::Error::msg(detail));
            }
        };
        let agent = execution.agent();
        let receipt = match echo_agent_app_core::api::reflection::reflect_session(
            &scoped_runtime,
            &agent,
            Some(&conversation_id),
        )
        .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let detail = error.to_string();
                settle_jsonl_foreground(
                    lease,
                    echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "reflection_failed",
                            detail.clone(),
                        ),
                    ),
                )
                .await?;
                return Err(anyhow::Error::msg(detail));
            }
        };
        if !jsonl_renderer.write_reflection_receipt(&receipt) {
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "jsonl_output",
                        "JSONL output closed before the reflection receipt was delivered",
                    ),
                ),
            )
            .await?;
            return Err(anyhow::anyhow!(
                "JSONL output closed before the reflection receipt was delivered"
            ));
        }
        if !sink.on_event(
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        ) {
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "jsonl_output",
                        "JSONL output closed before the reflection terminal was delivered",
                    ),
                ),
            )
            .await?;
            return Err(anyhow::anyhow!(
                "JSONL output closed before the reflection terminal was delivered"
            ));
        }
        settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed,
        )
        .await?;
        return Ok(());
    }
    let identity = echo_agent_app_core::api::extension_commands::ExtensionCommandIdentity {
        request_id: turn_id.clone(),
        operation_id: uuid::Uuid::new_v4().to_string(),
    };
    let extension_command = echo_agent_app_core::api::extension_commands::parse_extension_command(
        prompt,
        identity.clone(),
    );
    if extension_command
        .as_ref()
        .is_ok_and(|request| request.is_some())
        || extension_command.is_err()
    {
        if !sink.on_event(
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnConfiguration {
                permission_mode: options.permission_mode.as_str().to_string(),
                approval_policy: options.approval_policy.as_str().to_string(),
                attachments: Vec::new(),
            },
        ) || !sink.on_event(
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        ) {
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "jsonl_output",
                        "JSONL output closed before the Extension command started",
                    ),
                ),
            )
            .await?;
            return Err(anyhow::anyhow!(
                "JSONL output closed before the Extension command started"
            ));
        }
        return match extension_command {
            Ok(Some(request)) => {
                run_jsonl_extension_command(
                    request,
                    scoped_runtime,
                    lease,
                    sink,
                    services,
                    &conversation_id,
                    &options,
                )
                .await
            }
            Err(error) => {
                let Some(kind) = error.extension else {
                    settle_jsonl_foreground(
                        lease,
                        echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "extension_identity",
                                error.to_string(),
                            ),
                        ),
                    )
                    .await?;
                    return Err(anyhow::Error::new(error));
                };
                let receipt =
                    echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt::failed(
                        kind,
                        identity,
                        scoped_runtime.execution_scope().workspace_id().to_string(),
                        error.to_string(),
                    );
                finish_jsonl_extension_command(lease, sink, receipt).await
            }
            Ok(None) => Err(anyhow::anyhow!(
                "Extension command parser returned no command after claiming the prompt"
            )),
        };
    }

    let pool_execution = scoped_runtime
        .agent_for(&conversation_id)
        .await
        .map_err(anyhow::Error::from)?;
    let agent = pool_execution.agent();
    let permission_mode = options
        .permission_mode
        .as_str()
        .parse::<echo_agent::tools::permission::PermissionMode>()
        .map_err(anyhow::Error::msg)?;
    agent
        .write(|agent| agent.set_permission_mode(permission_mode))
        .await;

    let workspace_root = scoped_runtime.execution_scope().root().to_path_buf();
    let mut attachments = Vec::with_capacity(options.attachment_paths.len());
    for path in &options.attachment_paths {
        match echo_agent_app_core::api::attachments::stage_local_attachment(
            path,
            Some(&workspace_root),
        ) {
            Ok(attachment) => attachments.push(attachment),
            Err(error) => {
                let cleanup =
                    echo_agent_app_core::api::attachments::discard_staged_attachment_refs(
                        &attachments,
                    )
                    .err()
                    .map(|cleanup| format!("; attachment cleanup failed: {cleanup}"))
                    .unwrap_or_default();
                settle_jsonl_foreground(
                    lease,
                    echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "jsonl_attachment",
                            format!("failed to stage {}: {error}{cleanup}", path.display()),
                        ),
                    ),
                )
                .await?;
                return Err(anyhow::anyhow!(
                    "failed to stage JSONL attachment {}: {error}{cleanup}",
                    path.display()
                ));
            }
        }
    }
    if !sink.on_event(
        echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnConfiguration {
            permission_mode: options.permission_mode.as_str().to_string(),
            approval_policy: options.approval_policy.as_str().to_string(),
            attachments: attachments
                .iter()
                .map(
                    |attachment| echo_agent_app_core::api::chat_driver::ChatAttachmentDescriptor {
                        name: attachment.name.clone(),
                        mime_type: attachment.mime_type.clone(),
                        source: attachment.source,
                    },
                )
                .collect(),
        },
    ) {
        let _ = echo_agent_app_core::api::attachments::discard_staged_attachment_refs(&attachments);
        settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "jsonl_output",
                    "JSONL output closed before turn configuration was delivered",
                ),
            ),
        )
        .await?;
        return Err(anyhow::anyhow!(
            "JSONL output closed before turn configuration was delivered"
        ));
    }
    if !sink.on_event(
        echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        },
    ) {
        settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "jsonl_output",
                    "JSONL output closed before the turn started",
                ),
            ),
        )
        .await?;
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
            echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                status: "failed".to_string(),
            },
        );
        settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("conversation_store", detail.clone()),
            ),
        )
        .await?;
        return Err(anyhow::Error::msg(detail));
    }

    let spill_dir = echo_agent_app_core::api::prepared_turn::resolve_user_input_spill_dir(Some(
        &workspace_root,
    ));
    let turn = match echo_agent_app_core::api::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::api::prepared_turn::UserTurnInput {
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
                echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
                    status: "failed".to_string(),
                },
            );
            settle_jsonl_foreground(
                lease,
                echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("prepared_turn", detail.clone()),
                ),
            )
            .await?;
            return Err(anyhow::Error::msg(detail));
        }
    };
    let resources = std::sync::Arc::new(echo_agent_app_core::api::chat_resources::ChatResources {
        execution_scope: scoped_runtime.execution_scope().clone(),
        workspace_io_receipt: Some(scoped_runtime.workspace_io_receipt()),
        pool: scoped_runtime.pool(),
        store: scoped_runtime.task_runtime(),
        sink: sink.clone(),
        webhook_emitter: Some(services.app_state.webhook.emitter.clone()),
        conv_id: Some(conversation_id),
        root_message_id: turn_id,
        attachments: turn.inline_attachment_refs(),
        cancel: lease.cancellation_token(),
        review_integration: scoped_runtime.review_integration(),
        memory_generation: None,
        human_loop_provider: Some(std::sync::Arc::new(
            crate::cli::jsonl::JsonlHumanLoopProvider::new(sink.clone(), options.approval_policy),
        )),
    });
    let turn_cancel = lease.cancellation_token();
    let _pool_execution = pool_execution;
    let result = await_jsonl_driver_or_cancel(
        echo_agent_app_core::api::foreground_turn::drive_foreground_chat(
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
        echo_agent_app_core::api::chat_driver::TurnOutcome::status,
    );
    if !sink.on_event(
        echo_agent_app_core::api::chat_driver::ChatDriverEvent::TurnStatus {
            status: status.to_string(),
        },
    ) {
        return Err(anyhow::anyhow!(
            "JSONL output closed before the terminal status was delivered"
        ));
    }

    match result {
        Ok(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed) => Ok(()),
        Ok(echo_agent_app_core::api::chat_driver::TurnOutcome::Cancelled) => {
            Err(anyhow::anyhow!("one-shot turn was cancelled"))
        }
        Ok(echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(failure)) => {
            Err(anyhow::anyhow!("{}: {}", failure.code, failure.message))
        }
        Err(error) => Err(anyhow::Error::msg(error)),
    }
}

/// 运行 CLI 模式
#[allow(clippy::too_many_arguments)] // startup adapter wires the shared agent, pool, stores, and UI services once
pub async fn run_cli_mode(
    agent: AgentHandle,
    args: &Args,
    review_integration: Option<
        std::sync::Arc<echo_agent_app_core::api::evolution::ReviewIntegration>,
    >,
    prompt_assembly: echo_agent_app_core::api::project::prompt::PromptAssembly,
    pool: std::sync::Arc<echo_agent_app_core::api::agent_pool::AgentPool>,
    task_runtime_store: Option<
        std::sync::Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    >,
    conversation_id: String,
    webhook_emitter: std::sync::Arc<echo_agent_app_core::api::webhook::WebhookEmitter>,
    plugin_runtime: std::sync::Arc<echo_agent_app_core::api::plugin_runtime::PluginRuntimeService>,
    services: &ApplicationServices,
    repl_hitl_session: crate::cli::ReplHumanLoopSession,
) -> Result<()> {
    let mut repl_config = repl_config_for(args);
    repl_config.task_service = services.app_state.tasks.service.clone();
    repl_config.scheduler_runner = services.app_state.scheduler.runner.clone();
    repl_config.review_integration = review_integration;
    repl_config.prompt_assembly = Some(prompt_assembly);
    repl_config.pool = Some(pool.clone());
    repl_config.task_runtime_store = task_runtime_store.clone();
    repl_config.conversation_id = conversation_id;
    repl_config.webhook_emitter = Some(webhook_emitter);
    repl_config.plugin_runtime = Some(plugin_runtime.clone());
    repl_config.subagent_projection = Some(services.subagent_projection.clone());
    repl_config.app_state = Some(services.app_state.clone());

    crate::cli::run_repl(agent, repl_config, repl_hitl_session).await
}

/// Settle every shared headless owner exactly once after all product surfaces
/// have stopped accepting work. Failures are aggregated without skipping later
/// teardown steps.
pub async fn shutdown_application_services(
    mode_result: Result<()>,
    mut services: ApplicationServices,
    session_exit_agent: Option<AgentHandle>,
) -> Result<()> {
    let app_state = services.app_state.clone();
    let receipt = services.begin_shutdown(
        echo_agent_app_core::api::runtime::ApplicationLifecycleReason::Shutdown,
        mode_result.err(),
    )?;
    let review_integration = app_state.review_integration.clone();
    let auto_memory_integration = review_integration.clone();
    let session_review_integration = review_integration.clone();
    let run_session_review = session_exit_agent.is_some();
    if let Some(agent) = session_exit_agent.as_ref() {
        crate::cli::repl::run_auto_memory_on_exit(agent, &auto_memory_integration).await;
    }
    if run_session_review {
        crate::cli::repl::run_memory_review_on_exit(&session_review_integration).await;
    }
    let receipt = services.join(receipt).await;
    receipt.into_result().map_err(anyhow::Error::new)
}

/// 运行 IM 通道模式（QQ Bot、飞书等）
///
/// Channel agent 经 `AgentPool` 全套接通(bootstrap 等价:state_store/store/compressor/
/// MemoryLayerManager/permission_service/per-sender cache_user_id+conversation_id),
/// per-sender 隔离由 framework SessionHandler 与 EKO 的三元身份哈希共同承载。
#[cfg(feature = "channels")]
pub struct ChannelsModeArgs {
    pub app_state: std::sync::Arc<echo_agent_app_core::api::state::AppState>,
    pub app_config: EkoConfig,
    pub webhook_emitter: std::sync::Arc<echo_agent_app_core::api::webhook::WebhookEmitter>,
    pub foreground_turns: echo_agent_app_core::api::foreground_turn::ForegroundTurnControl,
    pub shutdown: echo_agent::agent::CancellationToken,
}

#[cfg(feature = "channels")]
pub async fn run_channels_mode(args: ChannelsModeArgs) -> Result<()> {
    use std::sync::Arc;

    use echo_agent::channels::{
        ChannelManager, FeishuChannel, FeishuConfig, MessageHandler, QqChannel, QqConfig,
        SessionHandler,
    };

    use crate::cli::channels::{AppChannelMessageHandler, ChannelSessionCoordinator};

    let ChannelsModeArgs {
        app_state,
        app_config,
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

    // handler_factory 每 channel 产出一个 SessionHandler,其内层工厂每
    // (channel,conversation,sender) 产出 AppChannelMessageHandler；handler
    // 从 AppState 解析消息所属 workspace generation 的 exact runtime。
    let channel_coordinators = Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory_coordinators = Arc::clone(&channel_coordinators);
    let handler_factory = move |_channel_id: &str| -> Arc<dyn MessageHandler> {
        let session_config = session_config.clone();
        let app_state = app_state.clone();
        let webhook_emitter = webhook_emitter.clone();
        let foreground_turns = foreground_turns.clone();
        let session_coordinator = Arc::new(ChannelSessionCoordinator::new());
        factory_coordinators
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::clone(&session_coordinator));
        let end_coordinator = Arc::clone(&session_coordinator);
        Arc::new(
            SessionHandler::new(
                session_config,
                move |instance: &echo_agent::channels::ChannelSessionInstance| -> Box<dyn MessageHandler> {
                Box::new(AppChannelMessageHandler::new(
                    app_state.clone(),
                    webhook_emitter.clone(),
                    foreground_turns.clone(),
                    instance.clone(),
                    Arc::clone(&session_coordinator),
                ))
                },
            )
            .with_on_session_end(move |info| end_coordinator.record_session_end(info)),
        )
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
        let coordinators = channel_coordinators
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for coordinator in &coordinators {
            if let Err(error) = coordinator.begin_input_pump_shutdown() {
                failures.push(format!("input pump admission: {error}"));
            }
        }
        let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
        for coordinator in coordinators {
            if let Err(error) = coordinator.join_input_pumps().await {
                failures.push(format!("input pump join: {error}"));
            }
        }
        return finish_channel_lifecycle(failures, stop_result);
    }
    tracing::info!(started_count, "IM channels started");

    shutdown.cancelled().await;

    tracing::info!("正在关闭 IM 通道...");
    let coordinators = channel_coordinators
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for coordinator in &coordinators {
        if let Err(error) = coordinator.begin_input_pump_shutdown() {
            failures.push(format!("input pump admission: {error}"));
        }
    }
    let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
    for coordinator in coordinators {
        if let Err(error) = coordinator.join_input_pumps().await {
            failures.push(format!("input pump join: {error}"));
        }
    }
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
    use std::sync::Arc;
    #[cfg(feature = "channels")]
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn jsonl_intercepts_only_the_shared_reflection_command() -> Result<()> {
        assert!(
            echo_agent_app_core::api::reflection::ReflectionCommand::parse("/reflect")?.is_some()
        );
        assert!(
            echo_agent_app_core::api::reflection::ReflectionCommand::parse("normal prompt")?
                .is_none()
        );
        assert!(
            echo_agent_app_core::api::reflection::ReflectionCommand::parse("/reflect extra")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn committed_extension_receipt_is_pending_not_completed() -> Result<()> {
        let receipt = echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt::Browser {
            meta: echo_agent_app_core::api::extension_commands::ExtensionReceiptMeta {
                request_id: "request-1".to_string(),
                operation_id: "operation-1".to_string(),
                authority_scope: "workspace-a".to_string(),
                workspace_generation: "generation-a".to_string(),
                sender_id: None,
                sender_incarnation: None,
                status: echo_agent_app_core::api::extension_commands::ExtensionCommandStatus::Committed,
                error: None,
            },
            receipt: None,
        };

        match extension_receipt_terminal(&receipt) {
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(failure) => {
                assert_eq!(failure.code, "extension_committed");
                assert!(failure.message.contains("pending"));
                Ok(())
            }
            outcome => Err(anyhow::anyhow!(
                "committed receipt completed unexpectedly: {outcome:?}"
            )),
        }
    }

    #[tokio::test]
    async fn jsonl_foreground_settlement_propagates_durable_terminal_debt() -> Result<()> {
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "workspace-jsonl-debt",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Cli,
            "conversation-jsonl-debt",
            "turn-jsonl-debt",
        )?;
        let projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(|_| Box::pin(async { Err("jsonl durable terminal debt".to_string()) }));
        control.supervise_input_lifecycle_scoped(
            "workspace-jsonl-debt",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Cli,
            "conversation-jsonl-debt",
            "turn-jsonl-debt",
            async { Ok(()) },
            projector,
        )?;

        let error = settle_jsonl_foreground(
            lease,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed,
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("JSONL durable terminal debt was swallowed"))?;

        assert!(error.to_string().contains("jsonl durable terminal debt"));
        assert!(control.has_active_turns());
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct ResetProbe {
        calls: Arc<AtomicUsize>,
    }

    #[cfg(feature = "channels")]
    struct SenderSessionProbe {
        generation: usize,
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
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for SenderSessionProbe {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            Ok(echo_agent::channels::OutboundMessage::new(
                &message.channel_id,
                message.reply_target(),
                message.chat_type,
                self.generation.to_string(),
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
        let handler = SessionHandler::new(
            channel_session_config(30),
            move |_instance: &echo_agent::channels::ChannelSessionInstance| {
                Box::new(ResetProbe {
                    calls: Arc::clone(&factory_calls),
                }) as Box<dyn MessageHandler>
            },
        );
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
    #[tokio::test]
    async fn production_session_config_isolates_group_senders_and_reuses_each_sender() -> Result<()>
    {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler, SessionHandler};

        let created = Arc::new(AtomicUsize::new(0));
        let factory_created = Arc::clone(&created);
        let handler = SessionHandler::new(
            channel_session_config(30),
            move |_instance: &echo_agent::channels::ChannelSessionInstance| {
                let generation = factory_created
                    .fetch_add(1, Ordering::SeqCst)
                    .saturating_add(1);
                Box::new(SenderSessionProbe { generation }) as Box<dyn MessageHandler>
            },
        );
        let message = |sender_id: &str, message_id: &str| {
            InboundMessage::new(
                "test-channel",
                sender_id,
                "shared-group",
                ChatType::Group,
                "hello",
                message_id,
            )
        };

        let alice_first = handler.handle(message("alice", "m1")).await?;
        let bob = handler.handle(message("bob", "m2")).await?;
        let alice_second = handler.handle(message("alice", "m3")).await?;
        assert_eq!(alice_first.text, "1");
        assert_eq!(bob.text, "2");
        assert_eq!(alice_second.text, "1");
        assert_eq!(handler.active_sessions(), 2);
        assert_eq!(created.load(Ordering::SeqCst), 2);
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
