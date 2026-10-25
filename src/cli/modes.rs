//! 运行模式管理
//!
//! 提供 CLI 模式和 IM 通道模式的启动逻辑。
//! Web 模式已移除 — GUI 通过 Tauri IPC 通信。

use anyhow::Result;
use futures::FutureExt;

use crate::agent_handle::AgentHandle;
use crate::cli::args::{Args, JsonlApprovalPolicy, JsonlPermissionMode};
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

pub(crate) use echo_agent_app_core::api::text_commands::extension_receipt_terminal;

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
    lease.retain_execution(pool_execution);
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
    if let Some(error) = result.as_ref().err().filter(|error| error.is_observation()) {
        return Err(anyhow::Error::msg(error.to_string()));
    }
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

/// Retained channel entry delegates to the single app-core channel owner.
#[cfg(feature = "channels")]
pub use echo_agent_app_core::api::channel_runtime::ChannelsModeArgs;
#[cfg(feature = "channels")]
pub async fn run_channels_mode(args: ChannelsModeArgs) -> Result<()> {
    echo_agent_app_core::api::channel_runtime::run_channels_mode(args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
}
