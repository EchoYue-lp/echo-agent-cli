/// 全局应用状态
///
/// 按功能域拆分为子状态，通过 `Arc<AppState>` 共享。
pub struct AppState {
    /// 连接管理（Agent 句柄）
    pub connection: ConnectionState,
    /// 配置（应用 / Web / 安全 / 沙箱 / 权限）
    pub config: ConfigState,
    /// 会话状态（工具 / 取消 / 限速）
    pub session: SessionState,
    /// 插件（MCP）
    pub plugins: PluginState,
    /// 持久化存储
    pub storage: StorageState,
    /// 历史记录（审计 / 工作流）
    pub history: HistoryState,
    /// 调度器（定时任务）
    pub scheduler: SchedulerState,
    /// 后台任务系统
    pub tasks: TaskState,
    /// Webhook 事件回调
    pub webhook: WebhookState,
    /// Run diagnostics product projection state.
    pub observability: ObservabilityState,
    /// 工作区管理
    pub workspace: WorkspaceState,
    /// Skills Hub（本地技能市场）
    pub skills_hub: Arc<RwLock<crate::skills_hub::SkillsHub>>,
    /// Sole product authority for extension mutations across every surface.
    pub extension_control: Arc<crate::extension_control::ExtensionControlService>,
    /// Sole EKO authority for direct-user per-tool visibility choices.
    pub tool_control: Arc<crate::tool_control::ToolControlService>,
    /// Shared memory review integration for GUI/IPC paths that write real memory.
    pub review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    /// Process-level shared plugin runtime (P0-4). `None` until bootstrap
    /// completes the primary agent; populated via
    /// [`Self::with_plugin_runtime`].
    pub(crate) plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    /// Sole acknowledged hook/config watcher lifecycle handle.
    pub config_watcher: Option<Arc<crate::config_watcher::ConfigWatcherHandle>>,
    /// Application-owned command-cell runtime shared by every Agent surface.
    pub command_cell_runtime:
        Option<Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>>,
    /// Interactive terminal authority shared by GUI, TUI, CLI, and channels.
    pub terminal: Arc<crate::terminal::TerminalService>,
    /// Shared direct Browser authority for GUI, TUI, CLI and channels.
    pub browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    /// Durable cross-workspace conversation inbox authority.
    pub agent_router: Arc<crate::agent_router::AgentRouter>,
    /// Delivery wake callback shared by global and workspace Agent control
    /// tools after the AppState Arc has been published.
    agent_control_wake: Arc<std::sync::OnceLock<crate::agent_control::DeliveryWake>>,
    /// Application operations (spawn/resume/handoff) for the agent control
    /// plane. Set once on the first Arc-backed registration; the shared
    /// ToolManager then exposes them to every pooled conversation Agent.
    agent_control_ops: Arc<
        std::sync::OnceLock<Arc<dyn crate::agent_control::AgentControlAppOps>>,
    >,
    /// Owned lifetime for asynchronous inbox consumers.
    pub agent_deliveries: Arc<crate::agent_router::AgentDeliverySupervisor>,
    /// Product projections that must be retired before workspace metadata can
    /// be released and the same identity recreated.
    workspace_delete_hook: Option<Arc<dyn WorkspaceDeleteHook>>,
}

impl AppState {
    /// Construct the stateless conversation-input adapter over the currently
    /// bound ChatEventLog authority. Tests and workspace transitions may
    /// replace that authority, so the service is intentionally not cached.
    pub fn conversation_inputs(&self) -> crate::conversation_input::ConversationInputService {
        crate::conversation_input::ConversationInputService::new(self.storage.chat_events.clone())
    }

    pub fn workspace_transition_in_progress(&self) -> bool {
        self.workspace
            .transitioning
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// 从共享的 Agent 和 HITL Dispatcher 创建状态（用于双模式）
    #[allow(clippy::too_many_arguments)]
    pub fn from_shared(
        agent: AgentHandle,
        model_consumers: Option<crate::infra::AgentModelConsumers>,
        hitl_dispatcher: Arc<crate::hitl::HitlDispatcher>,
        conversation_store: Option<Arc<dyn ConversationStore>>,
        runtime_state_store: Option<Arc<dyn RuntimeStateStore>>,
        app_config: crate::config::EkoConfig,
        mcp_config_runtime: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> anyhow::Result<Self> {
        let config = agent
            .try_write(|guard| WebConfig {
                model: guard.model_name().to_string(),
                system_prompt: guard.system_prompt().to_string(),
                token_limit: 8000,
                ..Default::default()
            })
            .unwrap_or_default();
        let initial_tool_output_artifacts = agent
            .try_write(|guard| guard.tool_output_artifacts())
            .flatten()
            .unwrap_or_else(|| crate::infra::tool_output_artifact_config(None));

        let active_model_id = app_config
            .model
            .default_model_id
            .as_deref()
            .and_then(|id| {
                app_config
                    .configured_models
                    .iter()
                    .find(|model| model.id == id && model.enabled)
            })
            .or_else(|| {
                app_config
                    .configured_models
                    .iter()
                    .find(|model| model.enabled)
            })
            .map(|model| model.id.clone())
            .unwrap_or_default();
        let webhook_emitter = Arc::new(crate::webhook::WebhookEmitter::from_config(&app_config));
        let global_conversation = ConversationStorageBinding {
            store: conversation_store,
            runtime_state: runtime_state_store,
            deletions: Arc::new(
                crate::conversation_deletion::ConversationDeletionService::at_default_root_with_product_data_io(
                    product_data_io.clone(),
                ),
            ),
        };
        let conversation_binding = Arc::new(RwLock::new(global_conversation.clone()));

        Ok(Self {
            connection: ConnectionState {
                agent,
                model_consumers,
                hitl_dispatcher,
                pool: None,
                conversation_binding: conversation_binding.clone(),
            },
            config: ConfigState {
                app_config: RwLock::new(app_config),
                active_model_id: RwLock::new(active_model_id),
                config_path: crate::config_watcher::resolve_config_save_path(None),
                web_config: RwLock::new(config),
                sandbox_config: RwLock::new(SandboxConfigData::default()),
                permission_mode: RwLock::new(
                    echo_agent::tools::permission::PermissionMode::Default,
                ),
                permission_rules: RwLock::new(Vec::new()),
                model_mutations: Mutex::new(ModelMutationOwnerState::default()),
                model_mutation_admission_open: std::sync::atomic::AtomicBool::new(true),
            },
            session: SessionState {
                analysis_runs: Arc::new(crate::product_data_io::AnalysisRunSupervisor::default()),
                product_data_io: product_data_io.clone(),
                foreground_turns: crate::foreground_turn::ForegroundTurnControl::default(),
            },
            plugins: PluginState {
                mcp_config: mcp_config_runtime,
                mcp_health: RwLock::new(HashMap::new()),
            },
            storage: StorageState {
                conversation: conversation_binding,
                conversation_archive: Arc::new(
                    crate::conversation_archive::ConversationArchiveStore::at_default_path()
                        .map_err(anyhow::Error::msg)?,
                ),
                tool_executions: {
                    let root = crate::tool_execution::ToolExecutionRepository::default_root();
                    let repository =
                        crate::tool_execution::ToolExecutionRepository::open(root.clone())
                            .or_else(|error| {
                                tracing::warn!(
                                    path = %root.display(),
                                    %error,
                                    "Failed to open tool execution repository; using temporary storage"
                                );
                                let fallback = std::env::temp_dir().join("eko-tool-executions");
                                crate::tool_execution::ToolExecutionRepository::open(fallback.clone())
                                    .map_err(|fallback_error| {
                                        tracing::warn!(
                                            path = %fallback.display(),
                                            error = %fallback_error,
                                            "Failed to open fallback tool execution repository"
                                        );
                                        fallback
                                    })
                            })
                            .unwrap_or_else(|fallback| {
                                crate::tool_execution::ToolExecutionRepository::without_initialization(
                                    fallback,
                                )
                            });
                    repository.register_artifact_config(initial_tool_output_artifacts);
                    Arc::new(repository)
                },
                chat_events: Arc::new(crate::chat_event_log::ChatEventLog::at_default_root()),
            },
            history: HistoryState {
                audit_logs: RwLock::new(Vec::new()),
                workflows: Arc::new(crate::workflow_service::WorkflowService::at_default_path()),
                structured_extraction: Arc::new(
                    crate::structured_extraction::StructuredExtractionService,
                ),
            },
            scheduler: SchedulerState {
                runner: None,
                cancel_token: echo_agent::agent::CancellationToken::new(),
                handle: Mutex::new(None),
            },
            tasks: TaskState {
                service: None,
                cancel_token: CancellationToken::new(),
                runtime: Some(Arc::new(
                    crate::tasks::task_runtime::TaskRuntimeStore::new()?,
                )),
            },
            webhook: WebhookState {
                emitter: webhook_emitter,
            },
            observability: ObservabilityState {
                prompt_assembly: RwLock::new(None),
            },
            workspace: WorkspaceState {
                current: RwLock::new(None),
                runtimes: Arc::new(
                    crate::workspace::runtime::WorkspaceRuntimeRegistry::new_with_product_data_io(
                        product_data_io,
                    ),
                ),
                global_conversation,
                transition: Arc::new(RwLock::new(())),
                transitioning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                #[cfg(test)]
                transition_test_barrier: std::sync::Mutex::new(None),
                settlement: Mutex::new(None),
                last_transition: RwLock::new(None),
                global_execution_root: std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                registry: Arc::new(WorkspaceRegistry::new().unwrap_or_else(|e| {
                    tracing::warn!("Failed to init workspace registry: {e}");
                    let fallback_dir = std::env::temp_dir().join("echo-workspaces");
                    WorkspaceRegistry::with_base_dir(fallback_dir.clone()).unwrap_or_else(
                        |fallback_error| {
                            tracing::warn!(
                                error = %fallback_error,
                                path = %fallback_dir.display(),
                                "Failed to create fallback workspace directory; registry writes may fail"
                            );
                            WorkspaceRegistry::without_initialization(fallback_dir)
                        },
                    )
                })),
            },
            skills_hub: Arc::new(RwLock::new(crate::skills_hub::SkillsHub::new())),
            extension_control: Arc::new(
                crate::extension_control::ExtensionControlService::default(),
            ),
            tool_control: Arc::new(crate::tool_control::ToolControlService::default()),
            review_integration: None,
            plugin_runtime: None,
            config_watcher: None,
            command_cell_runtime: None,
            terminal: crate::terminal::TerminalService::new(),
            browser_runtime: None,
            agent_router: crate::agent_router::AgentRouter::at_default_root(),
            agent_control_wake: Arc::new(std::sync::OnceLock::new()),
            agent_control_ops: Arc::new(std::sync::OnceLock::new()),
            agent_deliveries: Arc::new(crate::agent_router::AgentDeliverySupervisor::default()),
            workspace_delete_hook: None,
        })
    }

    /// Record the non-persistent model generation selected during bootstrap.
    pub fn with_active_model_id(mut self, active_model_id: impl Into<String>) -> Self {
        *self.config.active_model_id.get_mut() = active_model_id.into();
        self
    }

    /// Bind config persistence to the source selected during bootstrap.
    pub fn with_config_path(mut self, path: std::path::PathBuf) -> Self {
        self.config.config_path = path;
        self
    }

    /// Persist one complete config snapshot to the immutable bootstrap source.
    fn save_app_config(
        &self,
        config: &crate::config::EkoConfig,
    ) -> std::result::Result<(), String> {
        crate::config::save_config_file(&self.config.config_path, config)
    }

    /// Upsert one configured model through the sole application-owned config
    /// mutation settlement path.
    pub async fn upsert_configured_model_owned(
        self: &Arc<Self>,
        mutation: ConfiguredModelMutation,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::UpsertModel(mutation))
            .await
    }

    /// Upsert one provider and refresh the active generation when it uses the
    /// edited provider.
    pub async fn upsert_model_provider_owned(
        self: &Arc<Self>,
        mutation: ModelProviderMutation,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::UpsertProvider(mutation))
            .await
    }

    /// Resolve an id or unambiguous model selector, persist it as the default,
    /// and publish the exact prepared client to primary and pooled agents.
    pub async fn set_default_model_owned(
        self: &Arc<Self>,
        selector: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::SetDefault(selector.into()))
            .await
    }

    /// Delete a configured model. Deleting the active default is accepted only
    /// when another enabled model has passed the real client preflight.
    pub async fn delete_configured_model_owned(
        self: &Arc<Self>,
        model_id: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::DeleteModel(model_id.into()))
            .await
    }

    /// Delete a provider after all of its models have been removed.
    pub async fn delete_model_provider_owned(
        self: &Arc<Self>,
        provider_id: impl Into<String>,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        self.run_owned_model_mutation(ModelMutationRequest::DeleteProvider(provider_id.into()))
            .await
    }

    /// Serialize a broader EkoConfig edit with model mutations so a stale
    /// whole-config snapshot cannot overwrite an accepted model publication.
    /// When model runtime fields change, the active model is preflighted and
    /// republished within the same owned settlement.
    pub async fn update_app_config_owned<Update>(
        self: &Arc<Self>,
        reapply_active_model: bool,
        update: Update,
    ) -> Result<crate::config::EkoConfig, ModelMutationError>
    where
        Update: FnOnce(&mut crate::config::EkoConfig) -> Result<(), String> + Send + 'static,
    {
        self.run_owned_model_mutation(ModelMutationRequest::UpdateConfig {
            update: Box::new(update),
            reapply_active_model,
        })
        .await
        .map(|receipt| receipt.config)
    }

    async fn run_owned_model_mutation(
        self: &Arc<Self>,
        request: ModelMutationRequest,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        if !self
            .config
            .model_mutation_admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ModelMutationError::ShuttingDown);
        }
        let mut owner = self.config.model_mutations.lock().await;
        if !self
            .config
            .model_mutation_admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ModelMutationError::ShuttingDown);
        }
        if let ModelMutationOwnerLifecycle::Closed(_) = &owner.lifecycle {
            return Err(ModelMutationError::ShuttingDown);
        }
        let previous = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Running(previous) => Some(previous.clone()),
            _ => None,
        };
        if let Some(previous) = previous {
            let previous = previous.await.map(Some);
            owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(previous.clone()));
            previous?;
        }
        if let ModelMutationOwnerLifecycle::Settled(result) = &owner.lifecycle {
            result.as_ref().clone()?;
        }

        let state = Arc::clone(self);
        #[cfg(test)]
        let abort_for_test = matches!(&request, ModelMutationRequest::AbortSettlementForTest);
        let task = tokio::spawn(async move {
            #[cfg(test)]
            if matches!(&request, ModelMutationRequest::AbortSettlementForTest) {
                return std::future::pending::<Result<ModelMutationReceipt, ModelMutationError>>()
                    .await;
            }
            state.apply_model_mutation_inner(request).await
        });
        #[cfg(test)]
        if abort_for_test {
            task.abort();
        }
        let settlement = async move {
            task.await
                .map_err(|error| ModelMutationError::Settlement(error.to_string()))?
        }
        .boxed()
        .shared();
        owner.lifecycle = ModelMutationOwnerLifecycle::Running(settlement.clone());
        let result = settlement.await;
        owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(result.clone().map(Some)));
        result
    }

    async fn apply_model_mutation_inner(
        &self,
        request: ModelMutationRequest,
    ) -> Result<ModelMutationReceipt, ModelMutationError> {
        let current = self.config.app_config.read().await.clone();
        let active_model_id = self.config.active_model_id.read().await.clone();
        let mutation = prepare_model_mutation(&current, &active_model_id, request)?;
        let next_active_runtime = if mutation.deactivated {
            None
        } else if mutation.activated {
            Some(mutation.runtime.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its runtime candidate".to_string(),
                )
            })?)
        } else {
            resolve_active_model_runtime(&mutation.config, &active_model_id)?
        };
        let pool_session_config = match next_active_runtime.as_ref() {
            Some(runtime) => {
                crate::model_config::session_config_for_runtime(&mutation.config, runtime)
                    .map_err(ModelMutationError::Publication)?
            }
            None => mutation.config.clone(),
        };
        let _workspace_generation = self.workspace.transition.write().await;
        let mut model_pools = self.connection.pool.iter().cloned().collect::<Vec<_>>();
        model_pools.extend(
            self.workspace
                .runtimes
                .loaded_execution_runtimes()
                .await
                .into_iter()
                .map(|(_, runtime)| runtime.pool()),
        );
        // The process-global pool owns the primary Agent's model consumers in
        // production. Its prepared pool publication already retains the
        // primary Agent write guard, so preparing the same Agent again here
        // would wait forever on that guard. Lightweight test pools may omit
        // those consumers; retain the direct publication fallback for them.
        let primary_owned_by_global_pool = match self.connection.pool.as_ref() {
            Some(pool) => pool.owns_primary_model_consumers().await,
            None => false,
        };
        let _foreground = if mutation.activated || mutation.deactivated {
            Some(
                self.session
                    .foreground_turns
                    .suspend_admission_if_idle()
                    .map_err(|error| ModelMutationError::Publication(error.to_string()))?,
            )
        } else {
            None
        };
        let (runtime, prepared) = if mutation.activated {
            let runtime = mutation.runtime.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its runtime candidate".to_string(),
                )
            })?;
            let prepared = mutation.prepared.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "active model mutation lost its prepared client".to_string(),
                )
            })?;
            (Some(runtime), Some(prepared))
        } else {
            (None, None)
        };
        let mut pool_publications = Vec::new();
        if let (Some(runtime), Some(prepared)) = (runtime.as_ref(), prepared.as_ref()) {
            for pool in &model_pools {
                pool_publications.push(
                    pool.prepare_model_publication(
                        pool_session_config.clone(),
                        runtime.clone(),
                        prepared.clone(),
                    )
                    .await
                    .map_err(ModelMutationError::Publication)?,
                );
            }
        }
        let pool_deactivation = if mutation.deactivated {
            let mut deactivations = Vec::new();
            for pool in &model_pools {
                deactivations.push(
                    pool.prepare_model_deactivation(pool_session_config.clone())
                        .await
                        .map_err(ModelMutationError::Publication)?,
                );
            }
            deactivations
        } else {
            Vec::new()
        };
        let primary_publication = if primary_owned_by_global_pool {
            None
        } else {
            match (runtime.as_ref(), prepared.as_ref()) {
                (Some(runtime), Some(prepared)) => {
                    let consumers = self.connection.model_consumers.clone().ok_or_else(|| {
                        ModelMutationError::Publication(
                            "primary model consumers are unavailable".to_string(),
                        )
                    })?;
                    Some(
                        crate::infra::prepare_agent_model_publication(
                            &self.connection.agent,
                            consumers,
                            runtime,
                            prepared,
                            crate::infra::effective_token_limit(&mutation.config, Some(runtime)),
                        )
                        .await
                        .map_err(ModelMutationError::Publication)?,
                    )
                }
                _ => None,
            }
        };
        let primary_deactivation = if mutation.deactivated && !primary_owned_by_global_pool {
            let consumers = self.connection.model_consumers.clone().ok_or_else(|| {
                ModelMutationError::Publication(
                    "primary model consumers are unavailable".to_string(),
                )
            })?;
            Some(
                crate::infra::prepare_agent_model_deactivation(&self.connection.agent, consumers)
                    .await,
            )
        } else {
            None
        };

        self.save_app_config(&mutation.config)
            .map_err(ModelMutationError::Persistence)?;
        *self.config.app_config.write().await = mutation.config.clone();

        if let Some(publication) = primary_publication {
            publication.commit().await;
        } else if let Some(deactivation) = primary_deactivation {
            deactivation.commit().await;
        }

        for publication in pool_publications {
            publication.commit().await;
        }
        for deactivation in pool_deactivation {
            deactivation.commit().await;
        }
        if !mutation.activated && !mutation.deactivated {
            for pool in model_pools {
                pool.update_app_config(pool_session_config.clone()).await;
            }
        }

        if let Some(runtime) = runtime.as_ref() {
            *self.config.active_model_id.write().await = runtime.id.clone();
            tracing::info!(
                model_id = %runtime.id,
                provider = %runtime.provider,
                model = %runtime.model,
                "active model mutation fully settled"
            );
        } else if mutation.deactivated {
            self.config.active_model_id.write().await.clear();
            tracing::info!("active model removed; agent requires model configuration");
        }
        Ok(ModelMutationReceipt {
            config: mutation.config,
            model_id: mutation.model_id,
            runtime: mutation.runtime,
            activated: mutation.activated,
            deleted: mutation.deleted,
        })
    }

    /// Close model mutation admission and await an accepted settlement whose
    /// caller was dropped before application shutdown.
    pub async fn shutdown_model_mutations(&self) -> Result<(), ModelMutationError> {
        let mut owner = self.config.model_mutations.lock().await;
        if let ModelMutationOwnerLifecycle::Closed(result) = &owner.lifecycle {
            return result.clone();
        }
        let settlement = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Running(settlement) => Some(settlement.clone()),
            _ => None,
        };
        if let Some(settlement) = settlement {
            let result = settlement.await.map(Some);
            owner.lifecycle = ModelMutationOwnerLifecycle::Settled(Box::new(result));
        }
        let result = match &owner.lifecycle {
            ModelMutationOwnerLifecycle::Settled(result) => result.as_ref().clone().map(|_| ()),
            ModelMutationOwnerLifecycle::Closed(result) => result.clone(),
            ModelMutationOwnerLifecycle::Running(_) => Err(ModelMutationError::Settlement(
                "model mutation owner did not reach a terminal state".to_string(),
            )),
        };
        owner.lifecycle = ModelMutationOwnerLifecycle::Closed(result.clone());
        result
    }

    /// Attach the shared review integration created during runtime bootstrap.
    pub fn with_review_integration(
        mut self,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    ) -> Self {
        self.review_integration = review_integration;
        self
    }

    /// Attach the prompt-module report captured during runtime bootstrap.
    pub fn with_prompt_assembly(
        mut self,
        prompt_assembly: crate::project::prompt::PromptAssembly,
    ) -> Self {
        *self.observability.prompt_assembly.get_mut() = Some(prompt_assembly);
        self
    }

    /// Attach the shared plugin runtime (P0-4).
    ///
    /// Built once bootstrap has created the primary agent (the service derives
    /// its `project_root` from the agent's `working_dir`). Call before wrapping
    /// in `Arc`.
    pub fn with_plugin_runtime(
        mut self,
        plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    ) -> Self {
        self.plugin_runtime = plugin_runtime;
        self
    }

    pub fn with_browser_runtime(
        mut self,
        browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    ) -> Self {
        self.browser_runtime = browser_runtime;
        self
    }

    pub fn with_extension_control(
        mut self,
        extension_control: Arc<crate::extension_control::ExtensionControlService>,
    ) -> Self {
        self.extension_control = extension_control;
        self
    }

    pub fn with_config_watcher(
        mut self,
        config_watcher: Option<Arc<crate::config_watcher::ConfigWatcherHandle>>,
    ) -> Self {
        self.config_watcher = config_watcher;
        self
    }

    pub fn with_command_cell_runtime(
        mut self,
        runtime: Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
    ) -> Self {
        self.storage.chat_events = runtime.chat_events();
        if let Some(store) = self.tasks.runtime.as_ref() {
            runtime.bind_store(store);
        }
        runtime.bind_foreground_turns(self.session.foreground_turns.clone());
        self.command_cell_runtime = Some(runtime);
        self
    }

    pub fn with_workspace_delete_hook(mut self, hook: Arc<dyn WorkspaceDeleteHook>) -> Self {
        self.workspace_delete_hook = Some(hook);
        self
    }

    pub async fn shutdown_command_cells(&self) -> Result<(), String> {
        match self.command_cell_runtime.as_ref() {
            Some(runtime) => runtime.shutdown().await,
            None => Ok(()),
        }
    }

    pub fn with_agent_router(
        mut self,
        agent_router: Arc<crate::agent_router::AgentRouter>,
    ) -> Self {
        self.agent_router = agent_router;
        self
    }

    /// Share one foreground admission authority across concurrently active
    /// headless surfaces such as CLI and channels.
    pub fn with_foreground_turns(
        mut self,
        foreground_turns: crate::foreground_turn::ForegroundTurnControl,
    ) -> Self {
        self.session.foreground_turns = foreground_turns;
        self
    }

    /// Return the conversation store from the currently published workspace binding.
    pub async fn conversation_store(&self) -> Option<Arc<dyn ConversationStore>> {
        self.storage.conversation.read().await.store.clone()
    }

    /// Return archived conversation ids for one workspace projection.
    pub fn archived_conversation_ids(&self, workspace_id: &str) -> Result<Vec<String>, String> {
        self.storage.conversation_archive.archived_ids(workspace_id)
    }

    /// Update one conversation's EKO visibility projection.
    pub fn set_conversation_archived(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        archived: bool,
    ) -> Result<(), String> {
        self.storage
            .conversation_archive
            .set_archived(workspace_id, conversation_id, archived)
    }

    /// Create a conversation under the same identity lock used by aggregate deletion.
    pub async fn create_conversation_owned(
        &self,
        conversation: NewConversation,
    ) -> std::result::Result<Conversation, crate::conversation_deletion::ConversationDeletionError>
    {
        let _workspace = self.workspace.transition.read().await;
        let runtime = self.current_chat_runtime_inner().await.map_err(|error| {
            crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                error.to_string(),
            )
        })?;
        let store = runtime
            .conversation_store()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        runtime
            .deletions
            .create_conversation(
                store.as_ref(),
                conversation,
                Some(runtime.workspace_io_receipt()),
            )
            .await
    }

    /// Ensure a conversation under the same identity lock used by aggregate deletion.
    pub async fn ensure_conversation_owned(
        &self,
        conversation: NewConversation,
    ) -> std::result::Result<Conversation, crate::conversation_deletion::ConversationDeletionError>
    {
        let _workspace = self.workspace.transition.read().await;
        let runtime = self.current_chat_runtime_inner().await.map_err(|error| {
            crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                error.to_string(),
            )
        })?;
        let store = runtime
            .conversation_store()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        runtime
            .deletions
            .ensure_conversation(
                store.as_ref(),
                conversation,
                Some(runtime.workspace_io_receipt()),
            )
            .await
    }

    /// Begin a real user turn through the durable conversation admission boundary.
    pub async fn begin_conversation_turn_owned(
        &self,
        surface: crate::foreground_turn::ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: impl Into<String>,
    ) -> std::result::Result<
        crate::foreground_turn::ForegroundTurnLease,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let _workspace = self.workspace.transition.read().await;
        let runtime = self.current_chat_runtime_inner().await.map_err(|error| {
            crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                error.to_string(),
            )
        })?;
        runtime
            .deletions
            .begin_foreground_turn_scoped(
                &self.session.foreground_turns,
                runtime.execution_scope().workspace_id(),
                surface,
                conversation_id,
                turn_id,
                Some(runtime.workspace_io_receipt()),
            )
            .await
    }

    /// Delete every application-owned projection before retiring transcript authority.
    pub async fn delete_conversation_owned(
        &self,
        conversation_id: &str,
    ) -> std::result::Result<
        crate::conversation_deletion::ConversationDeletionReceipt,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let _workspace = self.workspace.transition.read().await;
        let runtime = self.current_chat_runtime_inner().await.map_err(|error| {
            crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                error.to_string(),
            )
        })?;
        self.delete_conversation_with_runtime(&runtime, conversation_id)
            .await
    }

    /// Delete one exact workspace conversation without consulting UI focus.
    pub async fn delete_conversation_scoped(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> std::result::Result<
        crate::conversation_deletion::ConversationDeletionReceipt,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let runtime = self
            .chat_runtime_for_scope(workspace_id)
            .await
            .map_err(|error| {
                crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                    error.to_string(),
                )
            })?;
        self.delete_conversation_with_runtime(&runtime, conversation_id)
            .await
    }

    async fn delete_conversation_with_runtime(
        &self,
        runtime: &ScopedChatRuntime,
        conversation_id: &str,
    ) -> std::result::Result<
        crate::conversation_deletion::ConversationDeletionReceipt,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let artifact_config = runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts())
            .await;
        runtime
            .deletions
            .delete(
                runtime.execution_scope().workspace_id(),
                conversation_id,
                runtime.conversation_store(),
                runtime.pool(),
                runtime.task_runtime(),
                self.storage.tool_executions.clone(),
                self.storage.chat_events.clone(),
                runtime.runtime_state_store(),
                &self.session.foreground_turns,
                Arc::clone(&self.agent_router),
                Arc::clone(&self.agent_deliveries),
                artifact_config,
                runtime.workspace_io_receipt(),
            )
            .await
    }

    /// Resume finalizer cleanup that crossed the transcript commit boundary.
    pub async fn recover_committed_conversation_deletions(
        &self,
    ) -> std::result::Result<
        Vec<crate::conversation_deletion::ConversationDeletionReceipt>,
        crate::conversation_deletion::ConversationDeletionError,
    > {
        let _workspace = self.workspace.transition.read().await;
        let runtime = self.current_chat_runtime_inner().await.map_err(|error| {
            crate::conversation_deletion::ConversationDeletionError::ConversationStore(
                error.to_string(),
            )
        })?;
        let store = runtime
            .conversation_store()
            .ok_or(crate::conversation_deletion::ConversationDeletionError::StoreUnavailable)?;
        let artifact_config = runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts())
            .await;
        runtime
            .deletions
            .recover_committed_deletions(
                crate::conversation_deletion::ConversationDeletionRecoveryContext {
                    workspace_id: runtime.execution_scope().workspace_id().to_string(),
                    conversation_store: store,
                    runtime_state: runtime.runtime_state_store(),
                    agent_pool: runtime.pool(),
                    task_runtime: runtime.task_runtime(),
                    tool_executions: self.storage.tool_executions.clone(),
                    chat_events: self.storage.chat_events.clone(),
                    foreground_turns: self.session.foreground_turns.clone(),
                    artifact_config,
                    workspace_io_receipt: runtime.workspace_io_receipt(),
                    agent_router: Arc::clone(&self.agent_router),
                    agent_deliveries: Arc::clone(&self.agent_deliveries),
                },
            )
            .await
    }

    /// Set the agent pool for multi-conversation parallel execution.
    ///
    /// Call this **before** wrapping in `Arc`.
    pub fn set_pool(&mut self, pool: Arc<crate::agent_pool::AgentPool>) {
        if let Some(store) = self.tasks.runtime.as_ref() {
            self.attach_task_execution_target_resolver(store, &pool);
        }
        self.tool_control = pool.tool_control();
        self.connection.pool = Some(pool);
    }

    fn attach_task_execution_target_resolver(
        &self,
        store: &Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
        seed_pool: &Arc<crate::agent_pool::AgentPool>,
    ) {
        let resolver: Arc<dyn crate::tasks::task_runtime::TaskExecutionTargetResolver> =
            Arc::new(WorkspaceTaskExecutionTargetResolver {
                workspace_registry: Arc::clone(&self.workspace.registry),
                runtimes: Arc::clone(&self.workspace.runtimes),
                seed_pool: Arc::downgrade(seed_pool),
                agent_router: Arc::clone(&self.agent_router),
            });
        store.attach_execution_target_resolver(resolver);
    }

    /// 启动定时任务调度器（仅在 Web 或双模式下调用）
    ///
    /// Call this **before** wrapping in `Arc`.
    pub async fn start_scheduler(&mut self) -> echo_agent::error::Result<()> {
        self.start_scheduler_with_store(None).await
    }

    /// 启动定时任务调度器，可选 Store 后端
    pub async fn start_scheduler_with_store(
        &mut self,
        backend: Option<Arc<dyn echo_agent::memory::Store>>,
    ) -> echo_agent::error::Result<()> {
        if self.scheduler.runner.is_some() {
            return Ok(());
        }
        let store = match backend {
            Some(b) => crate::scheduler::CronTaskStore::with_store(b).await?,
            None => crate::scheduler::CronTaskStore::new(),
        };
        // Phase C: pass the agent pool so each cron run acquires its OWN
        // per-run agent (worktree working_dir binding is per-run, fixing the
        // latent override bug where overlapping cron runs clobbered the shared
        // agent's working_dir). Falls back to the shared primary agent when no
        // pool is configured.
        let runner = crate::scheduler::new_scheduler_runner(
            store,
            self.scheduler.cancel_token.clone(),
            self.connection.agent.clone(),
            self.tasks.runtime.clone(),
            self.connection.pool.clone(),
            // Share the AppState's webhook emitter so cron runs emit
            // CronTaskCompleted on the same endpoint set as chat. `emit`
            // cheaply no-ops when no endpoints are registered.
            Some(self.webhook.emitter.clone()),
            self.review_integration.clone(),
        )
        .await?;
        let runner = Arc::new(runner);
        let handle = runner.clone().spawn();
        *self.scheduler.handle.get_mut() = Some(handle);
        self.scheduler.runner = Some(runner);
        tracing::info!("Scheduler runner started");
        Ok(())
    }

    /// Cancel the scheduler loop and await any in-flight cron fire.
    ///
    /// Repeated calls are harmless. The framework handle is process-scoped and
    /// workspace host execution remains independently owned.
    pub async fn shutdown_scheduler(&self) -> echo_agent::error::Result<()> {
        self.scheduler.shutdown().await
    }

    /// Start the fallible scheduler before admitting background TaskRun
    /// recovery, then start the pool monitor only after both owners exist.
    pub async fn start_scheduler_and_task_service(
        &mut self,
        backend: Option<Arc<dyn echo_agent::memory::Store>>,
    ) -> echo_agent::error::Result<()> {
        self.start_scheduler_with_store(backend).await?;
        let report = self.reconcile_task_runs_at_boot().await;
        tracing::info!(
            recovered = report.recovered,
            resumed = report.resumed,
            blocked = report.blocked,
            failed_scopes = report.failed_scopes.len(),
            "TaskRun boot reconciliation settled"
        );
        for failure in report.failed_scopes {
            tracing::warn!(%failure, "TaskRun boot scope remains unreconciled");
        }
        self.start_task_service().await;
        if let Some(pool) = self.connection.pool.as_ref() {
            pool.spawn_cleanup_monitor().await;
        }
        Ok(())
    }

    async fn reconcile_task_runs_at_boot(&self) -> TaskRunBootReport {
        let mut report = TaskRunBootReport::default();
        let chat_events = Arc::clone(&self.storage.chat_events);
        match self
            .session
            .product_data_io
            .run("recover ordinary Chat command cells", move || {
                chat_events.recover_orphan_command_cells()
            })
            .await
        {
            Ok(Ok(recovered)) if recovered > 0 => {
                tracing::info!(recovered, "ordinary Chat command cells recovered at boot");
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => report
                .failed_scopes
                .push(format!("ordinary Chat command cells: {error}")),
            Err(error) => report.failed_scopes.push(format!(
                "ordinary Chat command-cell recovery owner: {error}"
            )),
        }

        let chat_events = Arc::clone(&self.storage.chat_events);
        match self
            .session
            .product_data_io
            .run("reconcile conversation inputs at boot", move || {
                chat_events.reconcile_conversation_inputs_at_boot()
            })
            .await
        {
            Ok(Ok(recovered)) if recovered > 0 => {
                tracing::info!(recovered, "conversation inputs reconciled at boot");
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => report
                .failed_scopes
                .push(format!("conversation inputs: {error}")),
            Err(error) => report
                .failed_scopes
                .push(format!("conversation input recovery owner: {error}")),
        }

        let global_runtime = self.global_chat_runtime();
        let global_artifact_config = global_runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts())
            .await;
        if let Some(conversation_store) = global_runtime.conversation_store()
            && let Err(error) = global_runtime
                .deletions
                .recover_committed_deletions(
                    crate::conversation_deletion::ConversationDeletionRecoveryContext {
                        workspace_id: "global".to_string(),
                        conversation_store,
                        runtime_state: global_runtime.runtime_state_store(),
                        agent_pool: global_runtime.pool(),
                        task_runtime: global_runtime.task_runtime(),
                        tool_executions: self.storage.tool_executions.clone(),
                        chat_events: self.storage.chat_events.clone(),
                        foreground_turns: self.session.foreground_turns.clone(),
                        artifact_config: global_artifact_config,
                        workspace_io_receipt: global_runtime.workspace_io_receipt(),
                        agent_router: Arc::clone(&self.agent_router),
                        agent_deliveries: Arc::clone(&self.agent_deliveries),
                    },
                )
                .await
        {
            report
                .failed_scopes
                .push(format!("global conversation deletions: {error}"));
        }
        if let Some(store) = self.tasks.runtime.clone() {
            self.reconcile_task_run_scope("global", store, &mut report)
                .await;
        }
        let registry = Arc::clone(&self.workspace.registry);
        let workspaces = match self
            .session
            .product_data_io
            .run("list workspaces for boot recovery", move || registry.list())
            .await
        {
            Ok(Ok(workspaces)) => workspaces,
            Ok(Err(error)) => {
                report
                    .failed_scopes
                    .push(format!("workspace registry: {error}"));
                return report;
            }
            Err(error) => {
                report
                    .failed_scopes
                    .push(format!("workspace registry owner: {error}"));
                return report;
            }
        };
        for workspace in workspaces {
            let workspace_id = workspace.id.to_string();
            let (host, control_lease) =
                match self.workspace.runtimes.get_or_open_control(workspace).await {
                    Ok(binding) => binding,
                    Err(error) => {
                        report
                            .failed_scopes
                            .push(format!("workspace {workspace_id}: {error}"));
                        continue;
                    }
                };
            let workspace_receipt = ScopedWorkspaceIoReceipt {
                _lifetime: ScopedRuntimeLifetime::Workspace {
                    _lease: control_lease,
                },
                identity: host.workspace_io_identity(),
            };
            let store = match host.task_runtime().await {
                Ok(store) => store,
                Err(error) => {
                    report
                        .failed_scopes
                        .push(format!("workspace {workspace_id}: {error}"));
                    continue;
                }
            };
            if let Err(error) = host
                .resources()
                .deletion_service()
                .recover_committed_deletions(
                    crate::conversation_deletion::ConversationDeletionRecoveryContext {
                        workspace_id: workspace_id.clone(),
                        conversation_store: host.resources().conversation_store(),
                        runtime_state: Some(host.resources().runtime_state_store()),
                        agent_pool: None,
                        task_runtime: Some(store.clone()),
                        tool_executions: self.storage.tool_executions.clone(),
                        chat_events: self.storage.chat_events.clone(),
                        foreground_turns: self.session.foreground_turns.clone(),
                        artifact_config: Some(crate::infra::tool_output_artifact_config(Some(
                            host.root(),
                        ))),
                        workspace_io_receipt: workspace_receipt,
                        agent_router: Arc::clone(&self.agent_router),
                        agent_deliveries: Arc::clone(&self.agent_deliveries),
                    },
                )
                .await
            {
                report.failed_scopes.push(format!(
                    "workspace {workspace_id} conversation deletions: {error}"
                ));
            }
            self.reconcile_task_run_scope(&workspace_id, store, &mut report)
                .await;
        }
        report
    }

    async fn reconcile_task_run_scope(
        &self,
        workspace_id: &str,
        store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
        report: &mut TaskRunBootReport,
    ) {
        let reconciler = crate::tasks::task_runtime::TaskRunBootReconciler::for_store(&store);
        match reconciler.recover_once().await {
            Ok(recovered) => report.recovered = report.recovered.saturating_add(recovered),
            Err(error) => {
                report
                    .failed_scopes
                    .push(format!("{workspace_id}: {error}"));
                return;
            }
        }
        let candidates = match reconciler.paused_candidates().await {
            Ok(candidates) => candidates,
            Err(error) => {
                report
                    .failed_scopes
                    .push(format!("{workspace_id}: {error}"));
                return;
            }
        };
        for run in candidates {
            // BackgroundTaskService is the sole launcher owner for global
            // background runs. AppState owns ordinary continuation recovery.
            if workspace_id == "global" && run.conversation_id.starts_with("background:") {
                continue;
            }
            if run.attended_mode == crate::tasks::task_runtime::AttendedMode::Attended {
                report.blocked = report.blocked.saturating_add(1);
                continue;
            }
            match reconciler.decision(&run.run_id, true, false).await {
                Ok(crate::tasks::task_runtime::BootAutoResumeDecision::Blocked(_)) => {
                    report.blocked = report.blocked.saturating_add(1);
                    continue;
                }
                Ok(crate::tasks::task_runtime::BootAutoResumeDecision::Ready { .. }) => {}
                Err(error) => {
                    report
                        .failed_scopes
                        .push(format!("{workspace_id}/{} admission: {error}", run.run_id));
                    continue;
                }
            }
            let runtime = match self.chat_runtime_for_scope_locked(workspace_id).await {
                Ok(runtime) => runtime,
                Err(error) => {
                    report
                        .failed_scopes
                        .push(format!("{workspace_id}/{} runtime: {error}", run.run_id));
                    continue;
                }
            };
            let sink = crate::chat_event_log::bind_boot_recovery_chat_sink(
                self.storage.chat_events.clone(),
                self.storage.tool_executions.clone(),
                workspace_id.to_string(),
                run.conversation_id.clone(),
                run.root_message_id.clone(),
            );
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: runtime.execution_scope().clone(),
                workspace_io_receipt: Some(runtime.workspace_io_receipt()),
                pool: runtime.pool(),
                store: Some(store.clone()),
                sink,
                webhook_emitter: Some(self.webhook.emitter.clone()),
                conv_id: Some(run.conversation_id.clone()),
                root_message_id: run.root_message_id.clone(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                review_integration: runtime.review_integration(),
                memory_generation: None,
                human_loop_provider: None,
            });
            crate::tasks::task_runtime::continuation::register_launcher(
                &store,
                &run.run_id,
                runtime.primary_agent(),
                resources,
                run.root_message_id.clone(),
                None,
            );
            match reconciler
                .resume(&run.run_id, true, false, &self.tasks.cancel_token)
                .await
            {
                Ok(crate::tasks::task_runtime::TaskRunBootOutcome::Resumed(_)) => {
                    match crate::tasks::task_runtime::continuation::request_continue(
                        &store,
                        &run.run_id,
                        crate::tasks::task_runtime::RunTurnOrigin::Recovery,
                    ) {
                        crate::tasks::task_runtime::continuation::ContinueRequestOutcome::Running(_) => {
                            report.resumed = report.resumed.saturating_add(1);
                        }
                        outcome => {
                            crate::tasks::task_runtime::continuation::clear_launcher(
                                &store,
                                &run.run_id,
                            );
                            report.failed_scopes.push(format!(
                                "{workspace_id}/{} launcher: {outcome:?}",
                                run.run_id
                            ));
                        }
                    }
                }
                Ok(crate::tasks::task_runtime::TaskRunBootOutcome::Blocked(_)) => {
                    crate::tasks::task_runtime::continuation::clear_launcher(
                        &store,
                        &run.run_id,
                    );
                    report.blocked = report.blocked.saturating_add(1);
                }
                Ok(crate::tasks::task_runtime::TaskRunBootOutcome::Cancelled) => {
                    crate::tasks::task_runtime::continuation::clear_launcher(
                        &store,
                        &run.run_id,
                    );
                    return;
                }
                Err(error) => {
                    crate::tasks::task_runtime::continuation::clear_launcher(
                        &store,
                        &run.run_id,
                    );
                    report.failed_scopes.push(format!(
                        "{workspace_id}/{} resume: {error}",
                        run.run_id
                    ));
                }
            }
        }
    }

    /// 启动后台任务服务（所有模式都应调用）
    ///
    /// When an agent pool is active, background tasks run on a dedicated
    /// pool agent instead of the primary agent, enabling parallel execution
    /// with foreground conversations.
    ///
    /// Call this **before** wrapping in `Arc`.
    pub async fn start_task_service(&mut self) {
        if self.tasks.service.is_some() {
            return;
        }

        let service_result = if let Some(ref pool) = self.connection.pool {
            crate::tasks::BackgroundTaskService::with_pool(
                pool.clone(),
                self.tasks.cancel_token.clone(),
                self.tasks.runtime.clone(),
            )
            .await
        } else {
            crate::tasks::BackgroundTaskService::new(
                self.connection.agent.clone(),
                self.tasks.cancel_token.clone(),
                self.tasks.runtime.clone(),
            )
            .await
        };

        match service_result {
            Ok(service) => {
                let service =
                    Arc::new(service.with_review_integration(self.review_integration.clone()));
                service.clone().spawn();
                self.tasks.service = Some(service);
                tracing::info!("BackgroundTaskService started");
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "BackgroundTaskService failed to initialize — background tasks will be unavailable"
                );
            }
        }
    }

    /// 获取工具列表信息
    pub async fn get_tool_infos(
        &self,
        handle: &AgentHandle,
    ) -> std::result::Result<Vec<crate::types::ToolInfo>, crate::tool_control::ToolControlError>
    {
        let policy_disabled = self.tool_control.snapshot()?.disabled_tools;
        let (definitions, mut disabled) = handle
            .read(|agent| {
                let runtime = echo_agent::agent::AgentRunSnapshot::from_agent(agent);
                (
                    agent.tool_definitions(),
                    runtime.tools.disabled_tools.clone(),
                )
            })
            .await;
        disabled.extend(policy_disabled);

        Ok(definitions
            .into_iter()
            .map(|def| crate::types::ToolInfo {
                enabled: !disabled.contains(&def.function.name),
                name: def.function.name,
                description: def.function.description,
                parameters: def.function.parameters,
                source: crate::types::ToolSource::Builtin,
            })
            .collect())
    }

    /// Apply one direct-user tool visibility choice to every current pool.
    /// This intentionally does not consult automated-agent permission mode.
    pub async fn set_tool_enabled(
        &self,
        handle: &AgentHandle,
        name: &str,
        enabled: bool,
    ) -> std::result::Result<
        crate::tool_control::ToolControlReceipt,
        crate::tool_control::ToolControlError,
    > {
        let name = name.trim();
        let exists = handle
            .read(|agent| agent.tool_names().iter().any(|tool| tool == name))
            .await;
        if !exists {
            return Err(crate::tool_control::ToolControlError::NotRegistered {
                name: name.to_string(),
            });
        }

        let mutation = self.tool_control.set_enabled(name, enabled)?;
        match self.connection.pool.as_ref() {
            Some(pool) => pool.publish_tool_control_generation().await?,
            None => {
                let disabled = self.tool_control.snapshot()?.disabled_option();
                self.connection
                    .primary_agent()
                    .write(|agent| {
                        agent.set_disabled_tools(disabled.clone());
                        crate::subagent_prompt::refresh_primary_system_prompt(
                            agent,
                            &disabled.unwrap_or_default(),
                        );
                    })
                    .await;
            }
        }
        for (_, runtime) in self.workspace.runtimes.loaded_execution_runtimes().await {
            runtime.pool().publish_tool_control_generation().await?;
        }
        let effective_enabled = self
            .get_tool_infos(handle)
            .await?
            .into_iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.enabled)
            .ok_or_else(|| crate::tool_control::ToolControlError::NotRegistered {
                name: name.to_string(),
            })?;
        Ok(crate::tool_control::ToolControlReceipt {
            success: true,
            name: mutation.name,
            policy_enabled: mutation.policy_enabled,
            effective_enabled,
            changed: mutation.changed,
            revision: mutation.revision,
        })
    }

    /// 运行一次 MCP 健康检查，更新 `mcp_health` 状态
    pub async fn run_mcp_health_check(&self) {
        if let Err(error) = self
            .extension_control
            .refresh_current_mcp_health(self)
            .await
        {
            tracing::warn!(%error, "MCP health check skipped during extension transition");
        }
    }

    /// 添加审计日志条目（FIFO 淘汰，防止内存无限增长）
    pub async fn add_audit_entry(&self, entry: AuditLogEntry) {
        let mut logs = self.history.audit_logs.write().await;
        logs.push(entry);
        // Trim oldest entries if over the limit
        if logs.len() > max_audit_log_entries() {
            let excess = logs.len() - max_audit_log_entries();
            logs.drain(0..excess);
        }
    }

    /// 获取审计日志的只读快照
    pub async fn get_audit_logs(&self) -> Vec<AuditLogEntry> {
        self.history.audit_logs.read().await.clone()
    }

    /// 获取审计日志分页
    pub async fn get_audit_logs_paged(&self, offset: usize, limit: usize) -> Vec<AuditLogEntry> {
        let logs = self.history.audit_logs.read().await;
        logs.iter().skip(offset).take(limit).cloned().collect()
    }

    /// 获取审计日志总数
    pub async fn audit_log_count(&self) -> usize {
        self.history.audit_logs.read().await.len()
    }

    /// 清空审计日志，返回清除的条目数
    pub async fn clear_audit_entries(&self) -> usize {
        let mut logs = self.history.audit_logs.write().await;
        let count = logs.len();
        logs.clear();
        count
    }

    // ── 工作区管理 ──

    /// Create workspace metadata under the same lifecycle write admission used
    /// by switch and delete. A control lookup can never observe a half-created
    /// registry generation.
    pub async fn create_workspace_owned(
        self: &Arc<Self>,
        name: &str,
        kind: crate::workspace::WorkspaceKind,
        root: Option<std::path::PathBuf>,
    ) -> anyhow::Result<(Workspace, bool)> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::Create {
                name: name.to_string(),
                kind,
                root,
            })
            .await?
        {
            WorkspaceSettlementOutcome::Created(workspace, created) => Ok((workspace, created)),
            _ => anyhow::bail!("workspace create settlement returned an unexpected outcome"),
        }
    }

    async fn create_workspace_inner(
        &self,
        name: String,
        kind: crate::workspace::WorkspaceKind,
        root: Option<std::path::PathBuf>,
    ) -> anyhow::Result<(Workspace, bool)> {
        let _lifecycle = self.workspace.transition.write().await;
        let registry = Arc::clone(&self.workspace.registry);
        tokio::task::spawn_blocking(move || {
            let workspace_id = crate::workspace::WorkspaceId::from_name(&name);
            let requested_root = root.clone().unwrap_or_else(|| registry.default_root(&name));
            if let Ok(existing) = registry.open(&workspace_id) {
                let same_root = match (existing.root.canonicalize(), requested_root.canonicalize())
                {
                    (Ok(existing), Ok(requested)) => existing == requested,
                    _ => existing.root == requested_root,
                };
                if root.is_none() || same_root {
                    return Ok((existing, false));
                }
                anyhow::bail!(
                    "Workspace '{}' already exists at a different path: {}",
                    workspace_id,
                    existing.root.display()
                );
            }
            let workspace = match root {
                Some(root) => registry
                    .create_at(&name, kind, root)
                    .map_err(anyhow::Error::msg),
                None => registry.create(&name, kind).map_err(anyhow::Error::msg),
            }?;
            Ok((workspace, true))
        })
        .await
        .map_err(|error| anyhow::anyhow!("workspace create task failed: {error}"))?
    }

    pub async fn link_workspace_project_owned(
        self: &Arc<Self>,
        workspace_id: &crate::workspace::WorkspaceId,
        project_root: std::path::PathBuf,
    ) -> anyhow::Result<Workspace> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::LinkProject {
                workspace_id: Some(workspace_id.clone()),
                project_root,
            })
            .await?
        {
            WorkspaceSettlementOutcome::Linked(workspace) => Ok(workspace),
            _ => anyhow::bail!("workspace link settlement returned an unexpected outcome"),
        }
    }

    pub async fn link_current_workspace_project_owned(
        self: &Arc<Self>,
        project_root: std::path::PathBuf,
    ) -> anyhow::Result<Workspace> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::LinkProject {
                workspace_id: None,
                project_root,
            })
            .await?
        {
            WorkspaceSettlementOutcome::Linked(workspace) => Ok(workspace),
            _ => anyhow::bail!("workspace link settlement returned an unexpected outcome"),
        }
    }

    async fn link_workspace_project_inner(
        &self,
        workspace_id: Option<crate::workspace::WorkspaceId>,
        project_root: std::path::PathBuf,
    ) -> anyhow::Result<Workspace> {
        let _lifecycle = self.workspace.transition.write().await;
        let workspace_id = match workspace_id {
            Some(workspace_id) => workspace_id,
            None => self
                .workspace
                .current
                .read()
                .await
                .as_ref()
                .map(|host| host.id().clone())
                .ok_or_else(|| anyhow::anyhow!("No active workspace"))?,
        };
        // Project relink changes the product-data generation and tool root.
        // It cannot commit while any execution or control still owns the old
        // incarnation, otherwise exact wait/cancel and file CAS become stale.
        self.ensure_workspace_idle_for_delete_inner(&workspace_id)
            .await?;
        let registry = Arc::clone(&self.workspace.registry);
        let link_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            registry.link_project(&link_workspace_id, project_root)
        })
        .await
        .map_err(|error| anyhow::anyhow!("workspace link task failed: {error}"))??;
        if let Some(host) = self.workspace.runtimes.loaded_host(&workspace_id).await {
            host.refresh_workspace(workspace.clone()).await?;
            if let (Some(watcher), Some(execution)) =
                (self.config_watcher.as_ref(), host.execution_if_loaded())
            {
                match watcher
                    .register_workspace(
                        crate::config_watcher::ConfigWatcherWorkspaceIdentity::new(
                            workspace.id.to_string(),
                            workspace.opaque_product_data_generation(),
                        ),
                        workspace.root.clone(),
                        execution.primary_agent(),
                        execution.plugin_runtime(),
                    )
                    .await
                {
                    Ok(receipt) if receipt.errors.is_empty() => {}
                    Ok(receipt) => tracing::warn!(
                        workspace = %workspace.id,
                        errors = %receipt.errors.join("; "),
                        "Workspace relink committed with degraded config watcher settlement"
                    ),
                    Err(error) => tracing::warn!(
                        workspace = %workspace.id,
                        %error,
                        "Workspace relink committed before config watcher registration settled"
                    ),
                }
            }
        }
        Ok(workspace)
    }

    /// 获取当前活跃工作区（None 表示使用全局默认路径）。
    pub async fn current_workspace(&self) -> Option<Workspace> {
        let current = self.workspace.current.read().await.clone();
        match current {
            Some(host) => Some(host.workspace().await),
            None => None,
        }
    }

    /// Snapshot the immutable execution identity/root for a new turn.
    /// Existing turns retain their own snapshot across later focus changes.
    pub async fn current_execution_scope(&self) -> crate::workspace::WorkspaceExecutionScope {
        let current = self.workspace.current.read().await.clone();
        match current {
            Some(host) => host.execution_scope(),
            None => crate::workspace::WorkspaceExecutionScope::global(
                self.workspace.global_execution_root.clone(),
            ),
        }
    }

    /// Reject workspace deletion while any application-owned execution is
    /// still attached to the target host.
    pub async fn ensure_workspace_idle_for_delete(
        &self,
        workspace_id: &crate::workspace::WorkspaceId,
    ) -> anyhow::Result<()> {
        let _lifecycle = self.workspace.transition.read().await;
        self.ensure_workspace_idle_for_delete_inner(workspace_id)
            .await
    }

    async fn ensure_workspace_idle_for_delete_inner(
        &self,
        workspace_id: &crate::workspace::WorkspaceId,
    ) -> anyhow::Result<()> {
        let foreground = self
            .session
            .foreground_turns
            .snapshots_for_workspace(workspace_id.as_str())
            .map_err(anyhow::Error::msg)?;
        if !foreground.is_empty() {
            anyhow::bail!(
                "workspace '{}' has {} active foreground turn(s)",
                workspace_id,
                foreground.len()
            );
        }
        if self.agent_deliveries.has_active_workspace(workspace_id) {
            anyhow::bail!("workspace '{}' has active Agent delivery", workspace_id);
        }
        let activities = self.workspace.runtimes.activity_snapshot().await?;
        if let Some(activity) = activities
            .into_iter()
            .find(|activity| &activity.workspace_id == workspace_id)
            && !activity.is_idle()
        {
            anyhow::bail!(
                "workspace '{}' is busy (pool executions: {}, run drivers: {}, driver receipts: {}, controls: {})",
                workspace_id,
                activity.active_pool_executions,
                activity.active_run_drivers,
                activity.active_run_driver_receipts,
                activity.active_controls
            );
        }
        Ok(())
    }

    /// Sole workspace deletion transaction. Router and delivery admission close
    /// before active delivery drain; only then does deletion take lifecycle
    /// write admission and recheck the complete idle proof.
    pub async fn delete_workspace_owned(
        self: &Arc<Self>,
        workspace_id: &crate::workspace::WorkspaceId,
    ) -> anyhow::Result<()> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::Delete(
                workspace_id.clone(),
            ))
            .await?
        {
            WorkspaceSettlementOutcome::Deleted => Ok(()),
            _ => anyhow::bail!("workspace delete settlement returned an unexpected outcome"),
        }
    }

    async fn delete_workspace_inner(
        &self,
        workspace_id: &crate::workspace::WorkspaceId,
    ) -> anyhow::Result<()> {
        let watcher_identity = if self.config_watcher.is_some() {
            let workspace = self.workspace.registry.inspect(workspace_id)?;
            Some(crate::config_watcher::ConfigWatcherWorkspaceIdentity::new(
                workspace.id.to_string(),
                workspace.opaque_product_data_generation(),
            ))
        } else {
            None
        };
        let router_retirement = self
            .agent_router
            .begin_workspace_retirement(workspace_id.clone())?;
        let _delivery_retirement = self
            .agent_deliveries
            .retire_workspace(workspace_id.clone())
            .await?;
        let _lifecycle = self.workspace.transition.write().await;
        self.ensure_workspace_idle_for_delete_inner(workspace_id)
            .await?;
        router_retirement.purge().await?;
        let current = self.workspace.current.read().await.clone();
        if current
            .as_ref()
            .is_some_and(|host| host.id() == workspace_id)
        {
            self.exit_workspace_inner_locked().await?;
        }
        // The watcher acknowledgement is a drain boundary for earlier file
        // events. Remove the exact generation before specialist teardown so a
        // delayed event cannot race host shutdown or same-id recreation.
        if let (Some(watcher), Some(identity)) = (self.config_watcher.as_ref(), watcher_identity)
            && let Err(error) = watcher.unregister_workspace(identity).await
        {
            tracing::warn!(workspace = %workspace_id, %error, "Failed to unregister deleted workspace from config watcher");
        }
        self.workspace
            .runtimes
            .shutdown_and_evict_if_idle(workspace_id)
            .await?;
        if let Some(hook) = self.workspace_delete_hook.as_ref() {
            hook.remove_workspace(workspace_id.as_str()).await?;
        }
        let chat_events = Arc::clone(&self.storage.chat_events);
        let tool_executions = Arc::clone(&self.storage.tool_executions);
        let registry = Arc::clone(&self.workspace.registry);
        let workspace_id = workspace_id.clone();
        tokio::task::spawn_blocking(move || {
            chat_events.remove_workspace(workspace_id.as_str())?;
            tool_executions.remove_workspace(workspace_id.as_str())?;
            registry.delete(&workspace_id).map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|error| anyhow::anyhow!("workspace deletion task failed: {error}"))??;
        Ok(())
    }

    pub async fn apply_permission_mode_to_agents(
        &self,
        mode: echo_agent::tools::permission::PermissionMode,
    ) {
        match self.connection.pool.as_ref() {
            Some(pool) => pool.apply_permission_mode(mode).await,
            None => {
                self.connection
                    .primary_agent()
                    .write(|agent| agent.set_permission_mode(mode))
                    .await;
            }
        }
        for (_, runtime) in self.workspace.runtimes.loaded_execution_runtimes().await {
            runtime.pool().apply_permission_mode(mode).await;
        }
    }

    pub async fn apply_system_prompt_to_agents(&self, system_prompt: String) {
        match self.connection.pool.as_ref() {
            Some(pool) => pool.apply_system_prompt(system_prompt.clone()).await,
            None => {
                let prompt = system_prompt.clone();
                self.connection
                    .primary_agent()
                    .write_async(|agent| {
                        Box::pin(async move {
                            agent.set_system_prompt(prompt).await;
                            let disabled_tools = agent.disabled_tool_names();
                            crate::subagent_prompt::refresh_primary_system_prompt(
                                agent,
                                &disabled_tools,
                            );
                        })
                    })
                    .await;
            }
        }
        for (_, runtime) in self.workspace.runtimes.loaded_execution_runtimes().await {
            runtime
                .pool()
                .apply_system_prompt(system_prompt.clone())
                .await;
        }
    }

    /// Publish the ReAct loop ceiling (EKO `0` = unlimited sentinel) to the
    /// process primary/pool and every loaded workspace pool, so a saved
    /// `max_iterations` actually reaches the agents the user is talking to.
    pub async fn apply_max_iterations_to_agents(&self, eko_value: usize) {
        match self.connection.pool.as_ref() {
            Some(pool) => pool.apply_max_iterations(eko_value).await,
            None => {
                let resolved = crate::infra::resolved_max_iterations(eko_value);
                self.connection
                    .primary_agent()
                    .write(|agent| {
                        if agent.config().get_max_iterations() != resolved
                            && let Err(error) = agent.set_max_iterations(resolved)
                        {
                            tracing::warn!(%error, "rejected max_iterations publication");
                        }
                    })
                    .await;
            }
        }
        for (_, runtime) in self.workspace.runtimes.loaded_execution_runtimes().await {
            runtime.pool().apply_max_iterations(eko_value).await;
        }
    }

    /// Discover persisted conversation addresses from the existing workspace
    /// registry and per-workspace ConversationStores.
    pub async fn discover_agent_endpoints(
        &self,
    ) -> Result<Vec<crate::agent_router::AgentEndpoint>, AgentMessageSendError> {
        let workspaces = self
            .workspace
            .registry
            .list()
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?;
        let mut endpoints = Vec::new();
        for workspace in workspaces {
            let control = match self
                .conversation_control_for_scope(workspace.id.as_str())
                .await
            {
                Ok(control) => control,
                Err(error) => {
                    tracing::warn!(workspace = %workspace.id, %error, "Agent endpoint discovery skipped an unavailable workspace");
                    continue;
                }
            };
            let conversations = match control.store.list_conversations(Default::default()).await {
                Ok(conversations) => conversations,
                Err(error) => {
                    tracing::warn!(workspace = %workspace.id, %error, "Agent endpoint discovery skipped an unreadable conversation store");
                    continue;
                }
            };
            endpoints.extend(conversations.into_iter().map(|conversation| {
                crate::agent_router::AgentEndpoint {
                    address: crate::agent_router::AgentAddress::new(
                        workspace.id.clone(),
                        conversation.conversation_id,
                    ),
                    workspace_name: workspace.name.clone(),
                    conversation_title: conversation.title,
                    updated_at: conversation.updated_at,
                }
            }));
        }
        endpoints.sort_by(|left, right| {
            left.workspace_name
                .cmp(&right.workspace_name)
                .then_with(|| {
                    left.address
                        .conversation_id
                        .cmp(&right.address.conversation_id)
                })
        });
        Ok(endpoints)
    }

    /// Resolve a persisted conversation in the focused workspace into an
    /// optional Agent source address. Surfaces may still send one-way messages
    /// before their current conversation has been persisted.
    pub async fn current_agent_address(
        &self,
        conversation_id: Option<&str>,
    ) -> Result<Option<crate::agent_router::AgentAddress>, AgentMessageSendError> {
        let Some(conversation_id) = conversation_id.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        let Some((workspace_id, control)) = self
            .current_conversation_control()
            .await
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?
        else {
            return Ok(None);
        };
        let address = crate::agent_router::AgentAddress::new(workspace_id, conversation_id);
        let conversation = control
            .store
            .get_conversation(&address.conversation_id)
            .await
            .map_err(|error| AgentMessageSendError::Conversation(error.to_string()))?;
        Ok(conversation.map(|_| address))
    }

    /// Read the durable delivery projection for one persisted Agent endpoint.
    /// The router remains the only inbox owner; product surfaces render this
    /// projection without reading or folding inbox files themselves.
    pub async fn agent_delivery_records(
        &self,
        target: &crate::agent_router::AgentAddress,
    ) -> Result<Vec<crate::agent_router::AgentDeliveryRecord>, AgentMessageSendError> {
        self.validate_agent_address(target).await?;
        self.agent_router.records(target).await.map_err(Into::into)
    }

    pub async fn list_agent_groups(
        &self,
    ) -> Result<Vec<crate::agent_router::AgentGroup>, AgentMessageSendError> {
        self.agent_router.list_groups().await.map_err(Into::into)
    }

    pub async fn create_agent_group(
        &self,
        name: impl Into<String>,
        leader: crate::agent_router::AgentAddress,
        members: Vec<crate::agent_router::AgentGroupMember>,
    ) -> Result<crate::agent_router::AgentGroup, AgentMessageSendError> {
        self.validate_agent_group_addresses(&leader, &members)
            .await?;
        self.agent_router
            .create_group(name, leader, members)
            .await
            .map_err(Into::into)
    }

    pub async fn update_agent_group(
        &self,
        group_id: impl Into<String>,
        name: impl Into<String>,
        leader: crate::agent_router::AgentAddress,
        members: Vec<crate::agent_router::AgentGroupMember>,
    ) -> Result<crate::agent_router::AgentGroup, AgentMessageSendError> {
        self.validate_agent_group_addresses(&leader, &members)
            .await?;
        self.agent_router
            .update_group(group_id, name, leader, members)
            .await
            .map_err(Into::into)
    }

    pub async fn delete_agent_group(&self, group_id: &str) -> Result<bool, AgentMessageSendError> {
        self.agent_router
            .delete_group(group_id)
            .await
            .map_err(Into::into)
    }

    async fn validate_agent_group_addresses(
        &self,
        leader: &crate::agent_router::AgentAddress,
        members: &[crate::agent_router::AgentGroupMember],
    ) -> Result<(), AgentMessageSendError> {
        self.validate_agent_address(leader).await?;
        for member in members {
            self.validate_agent_address(&member.address).await?;
        }
        Ok(())
    }

    /// Validate both endpoints, then durably queue the message before any
    /// target wake or Agent execution occurs. The shared AgentRouter facade
    /// persists through its framework-backed DeliveryLedger; AppState remains
    /// the EKO owner for endpoint validation and wake scheduling.
    pub async fn send_agent_message_owned(
        self: &Arc<Self>,
        message: crate::agent_router::AgentMessage,
    ) -> Result<crate::agent_router::AgentDeliveryReceipt, AgentMessageSendError> {
        if let Some(source) = message.from.as_ref() {
            self.validate_agent_address(source).await?;
        }
        self.validate_agent_address(&message.to).await?;
        let target = message.to.clone();
        let receipt = self.agent_router.enqueue(message).await?;
        self.kick_agent_delivery(target)?;
        Ok(receipt)
    }

    /// Register the model Agent collaboration controls against this AppState's
    /// shared router, workspace registry, and delivery supervisor. The
    /// ToolManager is shared with pooled agents, so binding after pool setup
    /// updates every surface without constructing a second authority.
    pub async fn register_agent_control_tools(self: &Arc<Self>) {
        let Some(task_runtime) = self.tasks.runtime.clone() else {
            tracing::warn!("Agent control tools require a TaskRuntimeStore");
            return;
        };
        self.register_agent_control_tools_for(&self.connection.primary_agent(), task_runtime)
            .await;
    }

    async fn register_agent_control_tools_for(
        self: &Arc<Self>,
        agent: &crate::agent_handle::AgentHandle,
        task_runtime: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    ) {
        let weak_state = Arc::downgrade(self);
        let wake: crate::agent_control::DeliveryWake = Arc::new(move |target| {
            let Some(state) = weak_state.upgrade() else {
                return Err("AppState is no longer available".to_string());
            };
            state
                .kick_agent_delivery(target)
                .map_err(|error| error.to_string())
        });
        let _ = self.agent_control_wake.set(Arc::clone(&wake));
        let _ = self
            .agent_control_ops
            .set(Arc::new(AppStateAgentControlOps::new(self))
                as Arc<dyn crate::agent_control::AgentControlAppOps>);
        self.register_agent_control_tools_with_wake(
            agent,
            task_runtime,
            wake,
            self.workspace.global_conversation.store.clone(),
        )
        .await;
    }

    async fn register_agent_control_tools_with_wake(
        &self,
        agent: &crate::agent_handle::AgentHandle,
        task_runtime: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
        wake: crate::agent_control::DeliveryWake,
        conversation_store: Option<Arc<dyn ConversationStore>>,
    ) {
        let workspace_id = task_runtime.active_workspace_id();
        let service = crate::agent_control::AgentControlService::new(
            Arc::clone(&self.agent_router),
            task_runtime,
            Arc::clone(&self.workspace.registry),
        )
        .with_delivery_wake(wake);
        let service = match conversation_store {
            Some(store) => service.with_conversation_store(store, workspace_id),
            None => service,
        };
        let service = match self.agent_control_ops.get() {
            Some(ops) => service.with_app_ops(Arc::clone(ops)),
            None => service,
        };
        let service = Arc::new(service);
        crate::agent_control::register_agent_control_tools_on_agent(agent, service).await;
    }

    async fn validate_agent_address(
        &self,
        address: &crate::agent_router::AgentAddress,
    ) -> Result<(), AgentMessageSendError> {
        let control = self
            .conversation_control_for_scope(address.workspace_id.as_str())
            .await
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?;
        let conversation = control
            .store
            .get_conversation(&address.conversation_id)
            .await
            .map_err(|error| AgentMessageSendError::Conversation(error.to_string()))?;
        if conversation.is_none() {
            return Err(AgentMessageSendError::ConversationNotFound {
                workspace_id: address.workspace_id.to_string(),
                conversation_id: address.conversation_id.clone(),
            });
        }
        Ok(())
    }

    async fn chat_runtime_for_agent(
        &self,
        address: &crate::agent_router::AgentAddress,
    ) -> Result<ScopedChatRuntime, AgentMessageSendError> {
        self.chat_runtime_for_scope(address.workspace_id.as_str())
            .await
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))
    }

    /// Resolve one immutable execution runtime by explicit workspace identity.
    ///
    /// `current_workspace` is deliberately not consulted. Once a command has
    /// accepted a workspace identity, later UI focus changes cannot retarget it.
    pub async fn chat_runtime_for_scope(
        &self,
        workspace_id: &str,
    ) -> anyhow::Result<ScopedChatRuntime> {
        // Lock order: lifecycle admission -> metadata registry -> runtime
        // registry/host lifecycle. Workspace deletion takes the write side and
        // therefore cannot evict or remove metadata between lookup and pin.
        let _lifecycle = self.workspace.transition.read().await;
        self.chat_runtime_for_scope_locked(workspace_id).await
    }

    async fn conversation_control_for_scope(
        &self,
        workspace_id: &str,
    ) -> anyhow::Result<ScopedConversationControl> {
        let _lifecycle = self.workspace.transition.read().await;
        if workspace_id == "global" {
            let binding = &self.workspace.global_conversation;
            let store = binding
                .store
                .clone()
                .ok_or_else(|| anyhow::anyhow!("global conversation store is unavailable"))?;
            return Ok(ScopedConversationControl {
                _lifetime: ScopedRuntimeLifetime::Global,
                store,
            });
        }

        let workspace = self
            .workspace
            .registry
            .list()?
            .into_iter()
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .ok_or_else(|| anyhow::anyhow!("workspace '{workspace_id}' is not registered"))?;
        let (host, control_lease) = self
            .workspace
            .runtimes
            .get_or_open_control(workspace)
            .await?;
        Ok(ScopedConversationControl {
            _lifetime: ScopedRuntimeLifetime::Workspace {
                _lease: control_lease,
            },
            store: host.resources().conversation_store(),
        })
    }

    async fn current_conversation_control(
        &self,
    ) -> anyhow::Result<Option<(crate::workspace::WorkspaceId, ScopedConversationControl)>> {
        let _lifecycle = self.workspace.transition.read().await;
        let Some(host) = self.workspace.current.read().await.clone() else {
            return Ok(None);
        };
        let control_lease = self
            .workspace
            .runtimes
            .acquire_control_for_host(&host)
            .await?;
        Ok(Some((
            host.id().clone(),
            ScopedConversationControl {
                _lifetime: ScopedRuntimeLifetime::Workspace {
                    _lease: control_lease,
                },
                store: host.resources().conversation_store(),
            },
        )))
    }

    pub async fn workspace_control_for_scope(
        &self,
        workspace_id: &str,
    ) -> anyhow::Result<ScopedWorkspaceControl> {
        let _lifecycle = self.workspace.transition.read().await;
        if workspace_id == "global" {
            return Ok(ScopedWorkspaceControl {
                runtime: self.global_chat_runtime(),
                workspace: None,
            });
        }
        let registry = Arc::clone(&self.workspace.registry);
        let id = crate::workspace::WorkspaceId::from_raw(workspace_id.to_string());
        let workspace = self
            .session
            .product_data_io
            .run("inspect workspace control scope", move || {
                registry.inspect(&id).map_err(anyhow::Error::msg)
            })
            .await
            .map_err(anyhow::Error::msg)??;
        let runtime = self
            .chat_runtime_for_workspace_locked(workspace.clone())
            .await?;
        Ok(ScopedWorkspaceControl {
            runtime,
            workspace: Some(workspace),
        })
    }

    /// Resolve one exact product-data authority without running synchronous
    /// registry I/O on a Tokio runtime thread.
    pub async fn product_data_for_scope(
        &self,
        workspace_id: &str,
        workspace_generation: &str,
    ) -> anyhow::Result<crate::product_data_io::ScopedProductData> {
        let _lifecycle = self.workspace.transition.read().await;
        let control = if workspace_id == "global" {
            ScopedWorkspaceControl {
                runtime: self.global_chat_runtime(),
                workspace: None,
            }
        } else {
            let registry = Arc::clone(&self.workspace.registry);
            let id = crate::workspace::WorkspaceId::from_raw(workspace_id.to_string());
            let workspace = self
                .session
                .product_data_io
                .run("resolve product-data workspace", move || {
                    registry.inspect(&id).map_err(anyhow::Error::msg)
                })
                .await
                .map_err(anyhow::Error::msg)??;
            validate_workspace_product_data_generation(Some(&workspace), workspace_generation)
                .map_err(anyhow::Error::msg)?;
            let runtime = self
                .chat_runtime_for_workspace_locked(workspace.clone())
                .await?;
            ScopedWorkspaceControl {
                runtime,
                workspace: Some(workspace),
            }
        };
        if workspace_id == "global" {
            control
                .validate_generation(workspace_generation)
                .map_err(anyhow::Error::msg)?;
        }
        Ok(crate::product_data_io::ScopedProductData::new(
            control,
            Arc::clone(&self.session.analysis_runs),
            self.session.product_data_io.clone(),
        ))
    }

    /// Capture the currently focused product-data authority for an interactive
    /// CLI/TUI command. The returned value carries explicit immutable scope;
    /// callers never infer a root from a long-lived Agent.
    pub async fn current_product_data(
        &self,
    ) -> Result<crate::product_data_io::ScopedProductData, ScopedControlError> {
        if self
            .workspace
            .transitioning
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedControlError::WorkspaceTransition);
        }
        let _lifecycle = self.workspace.transition.read().await;
        if self
            .workspace
            .transitioning
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedControlError::WorkspaceTransition);
        }
        let control = match self.workspace.current.read().await.clone() {
            Some(host) => {
                let workspace = host.workspace().await;
                let runtime = self
                    .chat_runtime_for_workspace_locked(workspace.clone())
                    .await
                    .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
                ScopedWorkspaceControl {
                    runtime,
                    workspace: Some(workspace),
                }
            }
            None => ScopedWorkspaceControl {
                runtime: self.global_chat_runtime(),
                workspace: None,
            },
        };
        Ok(crate::product_data_io::ScopedProductData::new(
            control,
            Arc::clone(&self.session.analysis_runs),
            self.session.product_data_io.clone(),
        ))
    }

    pub async fn product_data_for_runtime(
        &self,
        runtime: &ScopedChatRuntime,
    ) -> anyhow::Result<crate::product_data_io::ScopedProductData> {
        let _lifecycle = self.workspace.transition.read().await;
        let workspace_id = runtime.execution_scope().workspace_id();
        let workspace = if workspace_id == "global" {
            None
        } else {
            let registry = Arc::clone(&self.workspace.registry);
            let id = crate::workspace::WorkspaceId::from_raw(workspace_id.to_string());
            Some(
                self.session
                    .product_data_io
                    .run("resolve runtime product-data workspace", move || {
                        registry.inspect(&id).map_err(anyhow::Error::msg)
                    })
                    .await
                    .map_err(anyhow::Error::msg)??,
            )
        };
        let control = ScopedWorkspaceControl {
            runtime: runtime.clone(),
            workspace,
        };
        Ok(crate::product_data_io::ScopedProductData::new(
            control,
            Arc::clone(&self.session.analysis_runs),
            self.session.product_data_io.clone(),
        ))
    }

    async fn chat_runtime_for_scope_locked(
        &self,
        workspace_id: &str,
    ) -> anyhow::Result<ScopedChatRuntime> {
        if workspace_id == "global" {
            return Ok(self.global_chat_runtime());
        }

        let workspace = self
            .workspace
            .registry
            .list()?
            .into_iter()
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .ok_or_else(|| anyhow::anyhow!("workspace '{workspace_id}' is not registered"))?;
        self.chat_runtime_for_workspace_locked(workspace).await
    }

    fn global_chat_runtime(&self) -> ScopedChatRuntime {
        let binding = &self.workspace.global_conversation;
        ScopedChatRuntime {
            _lifetime: ScopedRuntimeLifetime::Global,
            execution_scope: crate::workspace::WorkspaceExecutionScope::global(
                self.workspace.global_execution_root.clone(),
            ),
            workspace_io_identity: crate::workspace::WorkspaceIoIdentity::global(
                self.workspace.global_execution_root.clone(),
            ),
            primary_agent: self.connection.primary_agent(),
            pool: self.connection.pool.clone(),
            task_runtime: self.tasks.runtime.clone(),
            review_integration: self.review_integration.clone(),
            conversation_store: binding.store.clone(),
            runtime_state_store: binding.runtime_state.clone(),
            deletions: binding.deletions.clone(),
        }
    }

    async fn chat_runtime_for_workspace_locked(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<ScopedChatRuntime> {
        let (host, control_lease) = self
            .workspace
            .runtimes
            .get_or_open_control(workspace)
            .await?;
        let seed_pool = self.connection.pool.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Workspace execution requires the application AgentPool to be initialized"
            )
        })?;
        let execution = host.get_or_open_execution(seed_pool).await?;
        let task_runtime = execution.task_runtime();
        if let Some(wake) = self.agent_control_wake.get() {
            self.register_agent_control_tools_with_wake(
                &execution.primary_agent(),
                task_runtime.clone(),
                Arc::clone(wake),
                Some(host.resources().conversation_store()),
            )
            .await;
        }
        self.attach_task_execution_target_resolver(&task_runtime, seed_pool);
        Ok(ScopedChatRuntime {
            _lifetime: ScopedRuntimeLifetime::Workspace {
                _lease: control_lease,
            },
            execution_scope: host.execution_scope(),
            workspace_io_identity: host.workspace_io_identity(),
            primary_agent: execution.primary_agent(),
            pool: Some(execution.pool()),
            task_runtime: Some(task_runtime),
            review_integration: Some(execution.review_integration()),
            conversation_store: Some(host.resources().conversation_store()),
            runtime_state_store: Some(host.resources().runtime_state_store()),
            deletions: host.resources().deletion_service(),
        })
    }

    fn kick_agent_delivery(
        self: &Arc<Self>,
        target: crate::agent_router::AgentAddress,
    ) -> Result<(), AgentMessageSendError> {
        let state = Arc::clone(self);
        let supervisor = Arc::clone(&self.agent_deliveries);
        let operation_target = target.clone();
        let delivery_cancel = supervisor.cancellation_token();
        let recovery_state = Arc::downgrade(self);
        let recover: Arc<dyn Fn(crate::agent_router::AgentAddress) + Send + Sync> = Arc::new(
            move |target: crate::agent_router::AgentAddress| {
                if let Some(state) = recovery_state.upgrade()
                    && let Err(error) = state.kick_agent_delivery(target.clone())
                {
                    tracing::error!(conversation = %target.conversation_id, %error, "Agent delivery recovery wake failed");
                }
            },
        );
        supervisor.supervise(target, recover, move |cycle| async move {
            loop {
                state
                    .drain_agent_target(&operation_target, delivery_cancel.clone())
                    .await;
                match cycle.complete() {
                    Ok(true) => continue,
                    Ok(false) => return,
                    Err(error) => {
                        tracing::error!(
                            target = %operation_target.conversation_id,
                            %error,
                            "Agent delivery supervisor failed to settle target cycle"
                        );
                        return;
                    }
                }
            }
        })?;
        Ok(())
    }

    async fn drain_agent_target(
        self: &Arc<Self>,
        target: &crate::agent_router::AgentAddress,
        shutdown: CancellationToken,
    ) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            let pending = match self.agent_router.pending(target).await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::error!(
                        workspace = %target.workspace_id,
                        conversation = %target.conversation_id,
                        %error,
                        "Agent inbox replay failed"
                    );
                    return;
                }
            };
            if pending.is_empty() {
                return;
            }
            if let Some(next_attempt_at) = match self.agent_router.next_attempt_at(target).await {
                Ok(deadline) => deadline,
                Err(error) => {
                    tracing::error!(%error, "Agent inbox retry deadline could not be read");
                    return;
                }
            } {
                let delay = next_attempt_at
                    .signed_duration_since(chrono::Utc::now())
                    .to_std()
                    .unwrap_or(std::time::Duration::ZERO);
                if !delay.is_zero() {
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(delay) => {}
                    }
                    continue;
                }
            }
            let active = match self
                .session
                .foreground_turns
                .snapshots_for_conversation_scoped(
                    target.workspace_id.as_str(),
                    &target.conversation_id,
                ) {
                Ok(active) => active,
                Err(error) => {
                    tracing::error!(%error, "Agent delivery could not inspect target activity");
                    return;
                }
            };
            match self
                .reconcile_agent_delivery_in_flight(target, &active, &shutdown)
                .await
            {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "in-flight Agent delivery reconciliation failed");
                    return;
                }
            }
            let delivered = if active.is_empty() {
                self.deliver_agent_message_cold(target, &shutdown).await
            } else {
                self.deliver_agent_message_live(target, &active, &shutdown)
                    .await
            };
            match delivered {
                Ok(true) => {}
                Ok(false) => {
                    // A typed steer rejection records Deferred with a bounded
                    // retry deadline. Re-enter the loop so that deadline, not
                    // the whole foreground turn settlement, controls the next
                    // attempt. Paths that did not create a deferred receipt
                    // still wait for activity to change and cannot busy-loop.
                    match self.agent_router.next_attempt_at(target).await {
                        Ok(Some(_)) => continue,
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(%error, "Agent delivery retry deadline could not be read");
                            return;
                        }
                    }
                    let next_active = self
                        .session
                        .foreground_turns
                        .snapshots_for_conversation_scoped(
                            target.workspace_id.as_str(),
                            &target.conversation_id,
                        )
                        .unwrap_or_default();
                    if let Some(snapshot) = next_active.first()
                        && let Ok(waiter) = self.session.foreground_turns.settlement_waiter_scoped(
                            target.workspace_id.as_str(),
                            snapshot.surface,
                            &target.conversation_id,
                            &snapshot.root_turn_id,
                        )
                    {
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = waiter.wait() => {}
                        }
                    } else {
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = tokio::time::sleep(AGENT_DELIVERY_RETRY_DELAY) => {}
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        workspace = %target.workspace_id,
                        conversation = %target.conversation_id,
                        %error,
                        "Agent inbox delivery paused"
                    );
                    return;
                }
            }
        }
    }

    async fn reconcile_agent_delivery_in_flight(
        self: &Arc<Self>,
        target: &crate::agent_router::AgentAddress,
        active: &[crate::foreground_turn::ForegroundTurnSnapshot],
        shutdown: &CancellationToken,
    ) -> Result<bool, AgentMessageSendError> {
        let Some(in_flight) = self.agent_router.in_flight_claim(target).await? else {
            return Ok(false);
        };
        if !in_flight.effect_started {
            self.agent_router
                .turn_settled(
                    &in_flight.claim,
                    Some(in_flight.turn_id.clone()),
                    crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                    false,
                    agent_delivery_unknown_reason("effect started state was not durably observed"),
                    false,
                    None,
                )
                .await?;
            return Ok(true);
        }
        let exact = active
            .iter()
            .find(|snapshot| snapshot.active_turn_id == in_flight.turn_id);
        let Some(snapshot) = exact else {
            self.agent_router
                .turn_settled(
                    &in_flight.claim,
                    Some(in_flight.turn_id),
                    crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                    in_flight.phase == crate::agent_router::AgentDeliveryPhase::Drained,
                    agent_delivery_unknown_reason(
                        "Agent delivery side effect started before owner loss; automatic replay is blocked",
                    ),
                    false,
                    None,
                )
                .await?;
            return Ok(true);
        };
        let waiter = self
            .session
            .foreground_turns
            .settlement_waiter_scoped(
                target.workspace_id.as_str(),
                snapshot.surface,
                &target.conversation_id,
                &snapshot.root_turn_id,
            )
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?;
        let Some(settlement) = wait_for_live_delivery_or_shutdown(shutdown, waiter.wait()).await
        else {
            return Ok(true);
        };
        let settlement =
            settlement.map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?;
        match settlement.outcome {
            crate::chat_driver::TurnOutcome::Completed
                if !in_flight.claim.payload.expects_reply() =>
            {
                self.agent_router
                    .turn_settled(
                        &in_flight.claim,
                        Some(in_flight.turn_id),
                        crate::agent_router::AgentDeliveryOutcome::Completed,
                        true,
                        None,
                        false,
                        None,
                    )
                    .await?;
            }
            crate::chat_driver::TurnOutcome::Completed => {
                self.agent_router
                    .turn_settled(
                        &in_flight.claim,
                        Some(in_flight.turn_id),
                        crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                        true,
                        agent_delivery_unknown_reason(
                            "turn completed after owner loss without a delivery-owned reply terminal",
                        ),
                        false,
                        None,
                    )
                    .await?;
            }
            crate::chat_driver::TurnOutcome::Cancelled => {
                self.agent_router
                    .turn_settled(
                        &in_flight.claim,
                        Some(in_flight.turn_id),
                        crate::agent_router::AgentDeliveryOutcome::Cancelled,
                        true,
                        Some("target turn was cancelled after injection".to_string()),
                        false,
                        None,
                    )
                    .await?;
            }
            crate::chat_driver::TurnOutcome::Failed(failure) => {
                self.agent_router
                    .turn_settled(
                        &in_flight.claim,
                        Some(in_flight.turn_id),
                        crate::agent_router::AgentDeliveryOutcome::Failed,
                        true,
                        Some(format!("{}: {}", failure.code, failure.message)),
                        false,
                        None,
                    )
                    .await?;
            }
        }
        Ok(true)
    }

    async fn deliver_agent_message_live(
        self: &Arc<Self>,
        target: &crate::agent_router::AgentAddress,
        active: &[crate::foreground_turn::ForegroundTurnSnapshot],
        shutdown: &CancellationToken,
    ) -> Result<bool, AgentMessageSendError> {
        let runtime = self.chat_runtime_for_agent(target).await?;
        let pool = runtime.pool().ok_or_else(|| {
            AgentMessageSendError::Workspace(
                "target workspace AgentPool is not available".to_string(),
            )
        })?;
        let Some(execution) = pool
            .lease_existing(&target.conversation_id)
            .await
            .map_err(|error| AgentMessageSendError::Workspace(error.to_string()))?
        else {
            return Ok(false);
        };
        let Some(claim) = self.agent_router.claim_next(target).await? else {
            return Ok(true);
        };
        if claim.payload.expects_reply() {
            self.agent_router
                .defer(
                    &claim,
                    "reply-bearing Agent delivery waits for a delivery-owned cold turn",
                )
                .await?;
            return Ok(false);
        }
        let agent = execution.agent();
        let instruction = render_agent_delivery_instruction(&claim.payload);
        let Some(snapshot) = exact_live_delivery_candidate(active) else {
            self.agent_router
                .defer(
                    &claim,
                    "foreground projection has no unique candidate for the Agent active turn",
                )
                .await?;
            return Ok(false);
        };
        let waiter = match self.session.foreground_turns.settlement_waiter_scoped(
            target.workspace_id.as_str(),
            snapshot.surface,
            &target.conversation_id,
            &snapshot.root_turn_id,
        ) {
            Ok(waiter) => waiter,
            Err(_) => {
                self.agent_router
                    .defer(
                        &claim,
                        "exact foreground candidate settled before steer admission",
                    )
                    .await?;
                return Ok(false);
            }
        };
        self.agent_router
            .begin_injection(&claim, snapshot.active_turn_id.clone())
            .await?;
        match agent
            .steer_input_tracked(
                Some(&snapshot.active_turn_id),
                echo_agent::llm::types::Message::user(instruction.clone()),
            )
            .await
        {
            Ok(mut receipt) => {
                let turn_id = receipt.turn_id().to_string();
                if claim.payload.origin == crate::agent_router::AgentMessageOrigin::User
                    && let Err(error) = self
                        .record_user_steer_for_active_turn(
                            target.workspace_id.as_str(),
                            &target.conversation_id,
                            &snapshot.active_turn_id,
                            claim.payload.text(),
                        )
                        .await
                {
                    tracing::debug!(%error, "user steer constraint was not bound to a TaskRun");
                }
                self.agent_router
                    .mailbox_accepted(&claim, turn_id.clone())
                    .await?;
                let drained = tokio::select! {
                    _ = shutdown.cancelled() => {
                        let reason = agent_delivery_unknown_reason("delivery shutdown while waiting for model-context drain");
                        self.agent_router
                            .turn_settled(
                                &claim,
                                Some(turn_id.clone()),
                                crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                                false,
                                reason,
                                false,
                                None,
                            )
                            .await?;
                        return Ok(true);
                    }
                    state = receipt.wait_for_drained() => state,
                };
                if !drained.was_drained() {
                    self.agent_router
                        .turn_settled(
                            &claim,
                            Some(turn_id.clone()),
                            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                            false,
                            agent_delivery_unknown_reason(
                                "live Agent delivery mailbox did not confirm consumption before the target turn settled",
                            ),
                            false,
                            None,
                        )
                        .await?;
                    return Ok(true);
                }
                self.agent_router.drained(&claim, turn_id.clone()).await?;
                let Some(settlement) =
                    wait_for_live_delivery_or_shutdown(shutdown, waiter.wait()).await
                else {
                    self.agent_router
                        .turn_settled(
                            &claim,
                            Some(turn_id.clone()),
                            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                            true,
                            agent_delivery_unknown_reason(
                                "delivery shutdown while waiting for target turn settlement",
                            ),
                            false,
                            None,
                        )
                        .await?;
                    return Ok(true);
                };
                let settlement =
                    settlement.map_err(|error| AgentMessageSendError::Workspace(error.to_string()));
                let (outcome, reason) = match settlement {
                    Ok(settlement) => (
                        agent_delivery_outcome(&settlement.outcome),
                        agent_delivery_reason(&settlement.outcome),
                    ),
                    Err(error) => (
                        crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                        agent_delivery_unknown_reason(error.to_string()),
                    ),
                };
                self.agent_router
                    .turn_settled(&claim, None, outcome, true, reason, false, None)
                    .await?;
                Ok(true)
            }
            Err(error) if is_explicit_live_steer_rejection(&error) => {
                self.agent_router.defer(&claim, error.to_string()).await?;
                Ok(false)
            }
            Err(error) => {
                self.agent_router
                    .turn_settled(
                        &claim,
                        None,
                        crate::agent_router::AgentDeliveryOutcome::Failed,
                        false,
                        Some(error.to_string()),
                        false,
                        None,
                    )
                    .await?;
                Ok(true)
            }
        }
    }

    /// Record one accepted user-authored steer against the exact foreground
    /// TaskRun binding. The foreground owner is the identity authority for
    /// ordinary, resumed, and internal continuation turns.
    pub async fn record_user_steer_for_active_turn(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        active_turn_id: &str,
        text: &str,
    ) -> Result<bool, String> {
        if text.trim().is_empty() {
            return Ok(false);
        }
        let snapshots = self
            .session
            .foreground_turns
            .snapshots_for_conversation_scoped(workspace_id, conversation_id)
            .map_err(|error| error.to_string())?;
        let run_id = snapshots
            .iter()
            .find(|snapshot| snapshot.active_turn_id == active_turn_id)
            .and_then(|snapshot| snapshot.run_id.clone());
        let Some(run_id) = run_id else {
            return Ok(false);
        };
        let runtime = self
            .chat_runtime_for_scope(workspace_id)
            .await
            .map_err(|error| error.to_string())?;
        let Some(store) = runtime.task_runtime() else {
            return Ok(false);
        };
        let turn_id = active_turn_id.to_string();
        let text = text.to_string();
        crate::tasks::task_runtime::TaskRuntimeOperation::new(store)
            .run_store("record user steer constraint", move |store| {
                store.record_run_steer(&run_id, &turn_id, &text)
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    async fn deliver_agent_message_cold(
        self: &Arc<Self>,
        target: &crate::agent_router::AgentAddress,
        shutdown: &CancellationToken,
    ) -> Result<bool, AgentMessageSendError> {
        let runtime = self.chat_runtime_for_agent(target).await?;
        let Some(claim) = self.agent_router.claim_next(target).await? else {
            return Ok(true);
        };
        let root_turn_id = claim.payload.delivery_turn_id();
        let lease = match runtime
            .begin_turn(
                &self.session.foreground_turns,
                crate::foreground_turn::ForegroundTurnSurface::Agent,
                &target.conversation_id,
                root_turn_id.clone(),
            )
            .await
        {
            Ok(lease) => lease,
            Err(crate::conversation_deletion::ConversationDeletionError::Foreground(
                crate::foreground_turn::ForegroundTurnError::Busy { .. },
            )) => {
                self.agent_router
                    .defer(
                        &claim,
                        "target conversation became busy before cold delivery",
                    )
                    .await?;
                return Ok(false);
            }
            Err(error) => return Err(AgentMessageSendError::Conversation(error.to_string())),
        };
        if shutdown.is_cancelled() {
            let _settled = self
                .agent_router
                .turn_settled(
                    &claim,
                    Some(root_turn_id.clone()),
                    crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                    false,
                    agent_delivery_unknown_reason("shutdown before cold delivery effect admission"),
                    false,
                    None,
                )
                .await?;
            drop(lease);
            return Ok(true);
        }
        let instruction = render_agent_delivery_instruction(&claim.payload);
        let execution = match runtime.agent_for(&target.conversation_id).await {
            Ok(execution) => execution,
            Err(error) => {
                let detail = format!("AgentPool admission failed: {error}");
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id.clone()),
                        crate::agent_router::AgentDeliveryOutcome::Failed,
                        false,
                        Some(detail),
                        claim.attempt < MAX_AGENT_DELIVERY_ATTEMPTS,
                        None,
                    )
                    .await?;
                return Ok(true);
            }
        };
        let spill_dir = crate::prepared_turn::resolve_user_input_spill_dir(Some(
            runtime.execution_scope().root(),
        ));
        let mut turn = match crate::prepared_turn::PreparedUserTurn::build(
            crate::prepared_turn::UserTurnInput {
                text: &instruction,
                attachments: &[],
                spill_dir: &spill_dir,
                conversation_id: Some(&target.conversation_id),
                turn_id: Some(&root_turn_id),
            },
        ) {
            Ok(turn) => turn,
            Err(error) => {
                let detail = format!("Agent message preparation failed: {error}");
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id.clone()),
                        crate::agent_router::AgentDeliveryOutcome::Failed,
                        false,
                        Some(detail),
                        false,
                        None,
                    )
                    .await?;
                return Ok(true);
            }
        };
        if claim.payload.origin != crate::agent_router::AgentMessageOrigin::User
            || matches!(
                &claim.payload.payload,
                crate::agent_router::AgentMessagePayload::Reply { .. }
            )
        {
            turn.authorship = crate::prepared_turn::InstructionAuthorship::Runtime;
        }
        let capture = Arc::new(AgentDeliveryCaptureSink::default());
        let sink: Arc<dyn crate::chat_driver::ChatSink> = capture.clone();
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: runtime.execution_scope().clone(),
            workspace_io_receipt: Some(runtime.workspace_io_receipt()),
            pool: runtime.pool(),
            store: runtime.task_runtime(),
            sink,
            webhook_emitter: Some(self.webhook.emitter.clone()),
            conv_id: Some(target.conversation_id.clone()),
            root_message_id: root_turn_id.clone(),
            attachments: turn.inline_attachment_refs(),
            cancel: lease.cancellation_token(),
            review_integration: runtime.review_integration(),
            memory_generation: None,
            human_loop_provider: Some(Arc::new(crate::hitl::HitlDispatcher::new())),
        });
        let agent = execution.agent();
        let turn_cancel = lease.cancellation_token();
        self.agent_router
            .begin_injection(&claim, root_turn_id.clone())
            .await?;
        let (observation_tx, observation_rx) = tokio::sync::oneshot::channel();
        let observation_tx = Arc::new(Mutex::new(Some(observation_tx)));
        let router = Arc::clone(&self.agent_router);
        let observed_claim = claim.clone();
        let observed_turn_id = root_turn_id.clone();
        let input_observer: crate::chat_driver::InputReceiptObserver = Arc::new(
            move |mut receipt| {
                let observation_tx = Arc::clone(&observation_tx);
                let router = Arc::clone(&router);
                let claim = observed_claim.clone();
                let expected_turn_id = observed_turn_id.clone();
                Box::pin(async move {
                    let observation = if receipt.turn_id() != expected_turn_id {
                        Err(format!(
                            "cold Agent delivery receipt turn mismatch: expected {expected_turn_id}, got {}",
                            receipt.turn_id()
                        ))
                    } else {
                        let accepted = receipt.wait_for_accepted().await;
                        let drained_state = match accepted {
                            echo_agent::runtime::TurnInputState::Accepted => {
                                receipt.wait_for_drained().await
                            }
                            state => state,
                        };
                        match drained_state {
                            echo_agent::runtime::TurnInputState::Drained
                            | echo_agent::runtime::TurnInputState::TurnSettled {
                                drained: true,
                                ..
                            } => {
                                router
                                    .mailbox_accepted(&claim, expected_turn_id.clone())
                                    .await
                                    .map_err(|error| error.to_string())?;
                                router
                                    .drained(&claim, expected_turn_id.clone())
                                    .await
                                    .map(|_| true)
                                    .map_err(|error| error.to_string())
                            }
                            echo_agent::runtime::TurnInputState::Pending
                            | echo_agent::runtime::TurnInputState::Accepted
                            | echo_agent::runtime::TurnInputState::TurnSettled {
                                drained: false,
                                ..
                            } => Ok(false),
                        }
                    };
                    if let Some(sender) = observation_tx.lock().await.take() {
                        let _ = sender.send(observation);
                    }
                    Ok(())
                })
            },
        );
        let driver = crate::foreground_turn::drive_foreground_chat_with_input_observer(
            lease,
            &agent,
            &turn,
            resources,
            input_observer,
        );
        tokio::pin!(driver);
        let outcome = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                turn_cancel.cancel();
                driver.await
            }
            outcome = &mut driver => outcome,
        };
        let input_drained = observation_rx.await.unwrap_or_else(|_| {
            Err("cold Agent delivery input observer ended without a receipt".to_string())
        });
        drop(execution);
        let input_drained = match input_drained {
            Ok(drained) => drained,
            Err(error) => {
                let outcome_detail = cold_delivery_outcome_detail(&outcome);
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id.clone()),
                        crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                        false,
                        agent_delivery_unknown_reason(format!(
                            "cold Agent delivery receipt projection failed: {error}; {outcome_detail}"
                        )),
                        false,
                        None,
                    )
                    .await?;
                return Ok(true);
            }
        };
        if !input_drained {
            let outcome_detail = cold_delivery_outcome_detail(&outcome);
            self.agent_router
                .turn_settled(
                    &claim,
                    Some(root_turn_id.clone()),
                    match &outcome {
                        Ok(crate::chat_driver::TurnOutcome::Cancelled) => {
                            crate::agent_router::AgentDeliveryOutcome::Cancelled
                        }
                        Ok(crate::chat_driver::TurnOutcome::Failed(_)) => {
                            crate::agent_router::AgentDeliveryOutcome::Failed
                        }
                        Ok(crate::chat_driver::TurnOutcome::Completed) | Err(_) => {
                            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown
                        }
                    },
                    false,
                    agent_delivery_unknown_reason(format!(
                        "cold Agent delivery turn settled before its input reached model context; {outcome_detail}"
                    )),
                    false,
                    None,
                )
                .await?;
            return Ok(true);
        }
        match outcome {
            Ok(crate::chat_driver::TurnOutcome::Completed) => {
                let reply_message_id = self
                    .queue_agent_delivery_reply(&claim.payload, capture.final_answer())
                    .await;
                if claim.payload.expects_reply() && reply_message_id.is_none() {
                    self.agent_router
                        .turn_settled(
                            &claim,
                            Some(root_turn_id.clone()),
                            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                            true,
                            agent_delivery_unknown_reason(
                                "Agent delivery completed without a durable correlated reply",
                            ),
                            false,
                            None,
                        )
                        .await?;
                } else {
                    self.agent_router
                        .turn_settled(
                            &claim,
                            Some(root_turn_id.clone()),
                            crate::agent_router::AgentDeliveryOutcome::Completed,
                            true,
                            None,
                            false,
                            reply_message_id,
                        )
                        .await?;
                }
            }
            Ok(crate::chat_driver::TurnOutcome::Failed(failure)) => {
                let detail = format!("{}: {}", failure.code, failure.message);
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id.clone()),
                        crate::agent_router::AgentDeliveryOutcome::Failed,
                        true,
                        Some(detail),
                        false,
                        None,
                    )
                    .await?;
            }
            Ok(crate::chat_driver::TurnOutcome::Cancelled) => {
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id.clone()),
                        crate::agent_router::AgentDeliveryOutcome::Cancelled,
                        true,
                        Some("Agent delivery turn was cancelled".to_string()),
                        false,
                        None,
                    )
                    .await?;
            }
            Err(error) => {
                self.agent_router
                    .turn_settled(
                        &claim,
                        Some(root_turn_id),
                        crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
                        true,
                        agent_delivery_unknown_reason(error),
                        false,
                        None,
                    )
                    .await?;
            }
        }
        Ok(true)
    }

    async fn queue_agent_delivery_reply(
        self: &Arc<Self>,
        message: &crate::agent_router::AgentMessage,
        answer: Option<String>,
    ) -> Option<String> {
        if !message.expects_reply() {
            return None;
        }
        let (Some(source), Some(answer)) = (message.from.clone(), answer) else {
            return None;
        };
        let correlation_id = message
            .correlation_id
            .clone()
            .unwrap_or_else(|| message.message_id.clone());
        let reply = crate::agent_router::AgentMessage::agent_reply(
            message.to.clone(),
            source.clone(),
            answer,
            correlation_id,
            message.message_id.clone(),
        );
        let reply_message_id = reply.message_id.clone();
        match self.agent_router.enqueue(reply).await {
            Ok(_) => {
                if let Err(error) = self.kick_agent_delivery(source) {
                    tracing::warn!(%error, "Agent reply was queued but could not be scheduled");
                }
                Some(reply_message_id)
            }
            Err(error) => {
                tracing::error!(%error, "Agent reply could not be queued");
                None
            }
        }
    }

    pub async fn shutdown_agent_deliveries(&self) -> Result<(), AgentMessageSendError> {
        self.agent_deliveries.shutdown().await.map_err(Into::into)
    }

    pub fn begin_analysis_run_shutdown(&self) {
        self.session.analysis_runs.begin_shutdown();
    }

    pub async fn join_analysis_run_shutdown(
        &self,
    ) -> Vec<crate::product_data_io::AnalysisCancelReceipt> {
        self.session.analysis_runs.join_shutdown().await
    }

    /// Phase-one application shutdown broadcast. This method never joins: it
    /// closes durable delivery admission and cancels process-scoped producers so
    /// the lifecycle owner can safely await dependent subsystem receipts later.
    pub fn broadcast_application_shutdown(&self) -> Result<(), AgentMessageSendError> {
        let mut failures = Vec::new();
        self.config
            .model_mutation_admission_open
            .store(false, std::sync::atomic::Ordering::Release);
        self.scheduler.cancel_token.cancel();
        self.tasks.cancel_token.cancel();
        self.begin_analysis_run_shutdown();
        if let Err(error) = self.session.product_data_io.begin_shutdown() {
            failures.push(format!("product-data I/O: {error}"));
        }
        if let Err(error) = self.session.foreground_turns.begin_shutdown() {
            failures.push(format!("foreground turns: {error}"));
        }
        if let Some(store) = self.tasks.runtime.as_ref()
            && let Err(error) = store.begin_run_driver_shutdown()
        {
            failures.push(format!("TaskRun drivers: {error}"));
        }
        if let Some(store) = self.tasks.runtime.as_ref()
            && let Err(error) = store.begin_operation_shutdown()
        {
            failures.push(format!("TaskRuntime operations: {error}"));
        }
        if let Err(error) = self
            .workspace
            .runtimes
            .begin_task_runtime_operation_shutdown()
        {
            failures.push(format!("workspace TaskRuntime operations: {error}"));
        }
        if let Some(pool) = self.connection.pool.as_ref() {
            pool.begin_shutdown();
        }
        if let Some(integration) = self.review_integration.as_ref() {
            integration.begin_background_review_shutdown();
        }
        if let Some(runtime) = self.command_cell_runtime.as_ref()
            && let Err(error) = runtime.begin_shutdown()
        {
            failures.push(format!("command cells: {error}"));
        }
        if let Err(error) = self.close_agent_delivery_admission() {
            failures.push(format!("Agent deliveries: {error}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AgentMessageSendError::Workspace(failures.join("; ")))
        }
    }

    /// First phase of process shutdown: stop accepting Agent deliveries and
    /// broadcast cancellation before any subsystem join is awaited.
    pub fn close_agent_delivery_admission(&self) -> Result<(), AgentMessageSendError> {
        self.agent_deliveries
            .close_admission_and_cancel()
            .map_err(Into::into)
    }

    /// Resume every durable inbox that was accepted or left in-flight before
    /// the previous process exited. Call once after the application pool is
    /// installed and before user-facing surfaces start accepting input.
    pub async fn recover_agent_deliveries(
        self: &Arc<Self>,
    ) -> Result<usize, AgentMessageSendError> {
        let endpoints = self.discover_agent_endpoints().await?;
        let mut resumed = 0usize;
        for endpoint in endpoints {
            match self.agent_router.pending(&endpoint.address).await {
                Ok(pending) if !pending.is_empty() => {
                    if let Err(error) = self.kick_agent_delivery(endpoint.address.clone()) {
                        tracing::warn!(workspace = %endpoint.address.workspace_id, conversation = %endpoint.address.conversation_id, %error, "Agent delivery recovery could not schedule one endpoint");
                        continue;
                    }
                    resumed = resumed.saturating_add(1);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(workspace = %endpoint.address.workspace_id, conversation = %endpoint.address.conversation_id, %error, "Agent delivery recovery skipped one corrupt inbox");
                }
            }
        }
        Ok(resumed)
    }

    pub(crate) async fn replace_mcp_config_owned(
        self: &Arc<Self>,
        targets: &ExtensionRuntimeTargets,
        candidate: echo_agent::mcp::McpConfigFile,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        self.plugins
            .mcp_config
            .replace_and_reconcile(targets.mcp_reconcile_targets().await, candidate)
            .await
    }

    pub(crate) async fn upsert_mcp_server_owned(
        self: &Arc<Self>,
        targets: &ExtensionRuntimeTargets,
        name: String,
        entry: echo_agent::mcp::McpServerEntry,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        self.plugins
            .mcp_config
            .upsert_and_reconcile(targets.mcp_reconcile_targets().await, name, entry)
            .await
    }

    pub(crate) async fn set_mcp_server_enabled_owned(
        self: &Arc<Self>,
        targets: &ExtensionRuntimeTargets,
        name: &str,
        enabled: bool,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        self.plugins
            .mcp_config
            .set_enabled_and_reconcile(targets.mcp_reconcile_targets().await, name, enabled)
            .await
    }

    pub(crate) async fn remove_mcp_server_owned(
        self: &Arc<Self>,
        targets: &ExtensionRuntimeTargets,
        name: &str,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        self.plugins
            .mcp_config
            .remove_and_reconcile(targets.mcp_reconcile_targets().await, name)
            .await
    }

    /// Capture all execution authorities for the currently focused workspace.
    pub async fn current_chat_runtime(&self) -> anyhow::Result<ScopedChatRuntime> {
        let _lifecycle = self.workspace.transition.read().await;
        self.current_chat_runtime_inner().await
    }

    /// Pin the exact currently published runtime for one control operation.
    ///
    /// Unlike chat admission, a control command is not queued across a
    /// workspace transition. Returning a typed transition error keeps a command
    /// issued against workspace A from silently landing in workspace B.
    pub async fn current_control_runtime(&self) -> Result<ScopedChatRuntime, ScopedControlError> {
        if self
            .workspace
            .transitioning
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedControlError::WorkspaceTransition);
        }
        let _lifecycle = self.workspace.transition.read().await;
        if self
            .workspace
            .transitioning
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedControlError::WorkspaceTransition);
        }
        self.current_chat_runtime_inner()
            .await
            .map_err(|error| ScopedControlError::Runtime(error.to_string()))
    }

    /// Capture the exact runtime and extension authorities for the focused host.
    pub async fn current_extension_control(
        &self,
    ) -> Result<ScopedExtensionControl, ScopedControlError> {
        let _lifecycle = self
            .workspace
            .transition
            .try_read()
            .map_err(|_| ScopedControlError::WorkspaceTransition)?;
        let current = self.workspace.current.read().await.clone();
        match current {
            Some(host) => {
                let seed_pool = self.connection.pool.as_ref().ok_or_else(|| {
                    ScopedControlError::Runtime(
                        "workspace extension control requires an AgentPool".to_string(),
                    )
                })?;
                let control_lease = self
                    .workspace
                    .runtimes
                    .acquire_control_for_host(&host)
                    .await
                    .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
                let execution = host
                    .get_or_open_execution(seed_pool)
                    .await
                    .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
                let workspace = host.workspace().await;
                let project_root = workspace
                    .project_root
                    .clone()
                    .unwrap_or_else(|| workspace.root.clone());
                let resources = host.resources();
                let runtime = ScopedChatRuntime {
                    _lifetime: ScopedRuntimeLifetime::Workspace {
                        _lease: control_lease,
                    },
                    execution_scope: host.execution_scope(),
                    workspace_io_identity: host.workspace_io_identity(),
                    primary_agent: execution.primary_agent(),
                    pool: Some(execution.pool()),
                    task_runtime: Some(execution.task_runtime()),
                    review_integration: Some(execution.review_integration()),
                    conversation_store: Some(resources.conversation_store()),
                    runtime_state_store: Some(resources.runtime_state_store()),
                    deletions: resources.deletion_service(),
                };
                let plugin_runtime = execution.plugin_runtime().ok_or_else(|| {
                    ScopedControlError::Runtime(
                        "workspace plugin runtime is not initialized".to_string(),
                    )
                })?;
                Ok(ScopedExtensionControl {
                    runtime,
                    plugin_runtime,
                    project_root,
                })
            }
            None => {
                let runtime = self
                    .current_chat_runtime_inner()
                    .await
                    .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
                let plugin_runtime = self.plugin_runtime.clone().ok_or_else(|| {
                    ScopedControlError::Runtime(
                        "plugin runtime service is not initialized".to_string(),
                    )
                })?;
                Ok(ScopedExtensionControl {
                    project_root: runtime.execution_scope().root().to_path_buf(),
                    runtime,
                    plugin_runtime,
                })
            }
        }
    }

    /// Resolve Extension control from a surface-captured runtime generation.
    ///
    /// Channel and background surfaces must not re-read mutable workspace
    /// focus after their turn has already pinned an exact runtime. Pool
    /// identity distinguishes deletion/recreation ABA even when the workspace
    /// id is reused.
    pub async fn extension_control_for_runtime(
        &self,
        runtime: &ScopedChatRuntime,
    ) -> Result<ScopedExtensionControl, ScopedControlError> {
        let _lifecycle = self
            .workspace
            .transition
            .try_read()
            .map_err(|_| ScopedControlError::WorkspaceTransition)?;
        let workspace_id = runtime.execution_scope().workspace_id();
        let runtime_pool = runtime.pool().ok_or_else(|| {
            ScopedControlError::Runtime(format!(
                "extension runtime '{workspace_id}' has no AgentPool"
            ))
        })?;

        let plugin_runtime = if workspace_id == "global" {
            let current_pool = self.connection.pool.as_ref().ok_or_else(|| {
                ScopedControlError::Runtime("global AgentPool is not initialized".to_string())
            })?;
            if !Arc::ptr_eq(current_pool, &runtime_pool) {
                return Err(ScopedControlError::Runtime(
                    "global extension runtime generation was replaced".to_string(),
                ));
            }
            self.plugin_runtime.clone().ok_or_else(|| {
                ScopedControlError::Runtime(
                    "global plugin runtime service is not initialized".to_string(),
                )
            })?
        } else {
            let controls = self
                .workspace
                .runtimes
                .loaded_execution_controls()
                .await
                .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
            let mut matched = None;
            for (candidate_id, _workspace_generation, execution, _lease) in controls {
                if candidate_id.as_str() != workspace_id {
                    continue;
                }
                let candidate_pool = execution.pool();
                if !Arc::ptr_eq(&candidate_pool, &runtime_pool) {
                    return Err(ScopedControlError::Runtime(format!(
                        "workspace '{workspace_id}' extension runtime generation was replaced"
                    )));
                }
                matched = execution.plugin_runtime();
                break;
            }
            matched.ok_or_else(|| {
                ScopedControlError::Runtime(format!(
                    "workspace '{workspace_id}' plugin runtime is not registered"
                ))
            })?
        };
        let project_root = plugin_runtime.project_root().await;
        Ok(ScopedExtensionControl {
            runtime: runtime.clone(),
            plugin_runtime,
            project_root,
        })
    }

    /// Pin the global seed plus every loaded workspace extension generation.
    /// The global pool is first so future workspace forks inherit a committed
    /// policy even when no workspace host is currently loaded.
    pub async fn extension_runtime_targets(
        &self,
    ) -> Result<ExtensionRuntimeTargets, ScopedControlError> {
        let transition = Arc::clone(&self.workspace.transition)
            .try_read_owned()
            .map_err(|_| ScopedControlError::WorkspaceTransition)?;
        let global_runtime = self.plugin_runtime.clone().ok_or_else(|| {
            ScopedControlError::Runtime("plugin runtime service is not initialized".to_string())
        })?;
        let global_pool = self.connection.pool.clone().ok_or_else(|| {
            ScopedControlError::Runtime("global AgentPool is not initialized".to_string())
        })?;
        let mut targets = vec![ExtensionRuntimeTarget {
            scope: "global".to_string(),
            workspace_generation: "global".to_string(),
            prepared_generation_identity: global_runtime.prepared_generation_identity().await,
            _lifetime: ScopedRuntimeLifetime::Global,
            primary_agent: self.connection.primary_agent(),
            pool: global_pool,
            plugin_runtime: global_runtime,
        }];
        let controls = self
            .workspace
            .runtimes
            .loaded_execution_controls()
            .await
            .map_err(|error| ScopedControlError::Runtime(error.to_string()))?;
        for (workspace_id, workspace_generation, execution, lease) in controls {
            if workspace_generation.is_empty() {
                return Err(ScopedControlError::Runtime(format!(
                    "workspace '{workspace_id}' extension generation is invalid"
                )));
            }
            let plugin_runtime = execution.plugin_runtime().ok_or_else(|| {
                ScopedControlError::Runtime(format!(
                    "workspace '{workspace_id}' plugin runtime is not initialized"
                ))
            })?;
            targets.push(ExtensionRuntimeTarget {
                scope: workspace_id.to_string(),
                workspace_generation,
                prepared_generation_identity: plugin_runtime.prepared_generation_identity().await,
                _lifetime: ScopedRuntimeLifetime::Workspace { _lease: lease },
                primary_agent: execution.primary_agent(),
                pool: execution.pool(),
                plugin_runtime,
            });
        }
        Ok(ExtensionRuntimeTargets {
            _transition: transition,
            targets,
        })
    }

    /// Run one structured extraction against the pooled Agent resolved by the
    /// explicit workspace and conversation address supplied by the surface.
    pub async fn extract_structured_for_scope(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        surface: crate::foreground_turn::ForegroundTurnSurface,
        request: crate::structured_extraction::StructuredExtractionRequest,
    ) -> Result<
        crate::structured_extraction::StructuredExtractionOutcome,
        crate::structured_extraction::StructuredExtractionError,
    > {
        use crate::structured_extraction::StructuredExtractionError;

        let runtime = self
            .chat_runtime_for_scope(workspace_id)
            .await
            .map_err(|error| StructuredExtractionError::Runtime(error.to_string()))?;
        let turn_id = format!("extract:{}", uuid::Uuid::new_v4());
        let foreground = runtime
            .begin_turn(
                &self.session.foreground_turns,
                surface,
                conversation_id,
                turn_id,
            )
            .await
            .map_err(|error| StructuredExtractionError::Admission(error.to_string()))?;
        let execution = match runtime.agent_for(conversation_id).await {
            Ok(execution) => execution,
            Err(error) => {
                let error = StructuredExtractionError::AgentPool(error.to_string());
                let settlement = foreground
                    .settle_after_observers(crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(error.code(), error.to_string()),
                    ))
                    .await;
                if let Err(settlement_error) = settlement {
                    return Err(StructuredExtractionError::Admission(format!(
                        "{error}; foreground settlement failed: {settlement_error}"
                    )));
                }
                return Err(error);
            }
        };
        let result = self
            .history
            .structured_extraction
            .extract(&execution.agent(), request)
            .await;
        drop(execution);
        let outcome = match &result {
            Ok(_) => crate::chat_driver::TurnOutcome::Completed,
            Err(error) => crate::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(error.code(), error.to_string()),
            ),
        };
        match foreground.settle_after_observers(outcome).await {
            Ok(_) => result,
            Err(settlement_error) => Err(StructuredExtractionError::Admission(format!(
                "structured extraction foreground settlement failed: {settlement_error}"
            ))),
        }
    }

    /// Parse and execute the shared `/extract` contract for terminal and
    /// channel surfaces while preserving the same typed app-core outcomes.
    pub async fn execute_structured_extraction_command_for_scope(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        surface: crate::foreground_turn::ForegroundTurnSurface,
        command: &str,
    ) -> Result<String, crate::structured_extraction::StructuredExtractionError> {
        use crate::structured_extraction::{
            PreparedStructuredExtractionCommand, StructuredExtractionError,
        };

        let prepared = self.history.structured_extraction.parse_command(command)?;
        let value = match prepared {
            PreparedStructuredExtractionCommand::Examples => {
                serde_json::to_value(self.history.structured_extraction.examples())
            }
            PreparedStructuredExtractionCommand::Validate(schema) => {
                serde_json::to_value(self.history.structured_extraction.validate_schema(&schema))
            }
            PreparedStructuredExtractionCommand::Extract(request) => serde_json::to_value(
                self.extract_structured_for_scope(workspace_id, conversation_id, surface, request)
                    .await?,
            ),
        }
        .map_err(|error| StructuredExtractionError::Serialization(error.to_string()))?;
        serde_json::to_string_pretty(&value)
            .map_err(|error| StructuredExtractionError::Serialization(error.to_string()))
    }

    async fn current_chat_runtime_inner(&self) -> anyhow::Result<ScopedChatRuntime> {
        let current = self.workspace.current.read().await.clone();
        match current {
            Some(host) => {
                let control_lease = self
                    .workspace
                    .runtimes
                    .acquire_control_for_host(&host)
                    .await?;
                let seed_pool = self.connection.pool.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Workspace execution requires the application AgentPool to be initialized"
                    )
                })?;
                let execution = host.get_or_open_execution(seed_pool).await?;
                let task_runtime = execution.task_runtime();
                self.attach_task_execution_target_resolver(&task_runtime, seed_pool);
                Ok(ScopedChatRuntime {
                    _lifetime: ScopedRuntimeLifetime::Workspace {
                        _lease: control_lease,
                    },
                    execution_scope: host.execution_scope(),
                    workspace_io_identity: host.workspace_io_identity(),
                    primary_agent: execution.primary_agent(),
                    pool: Some(execution.pool()),
                    task_runtime: Some(task_runtime),
                    review_integration: Some(execution.review_integration()),
                    conversation_store: Some(host.resources().conversation_store()),
                    runtime_state_store: Some(host.resources().runtime_state_store()),
                    deletions: host.resources().deletion_service(),
                })
            }
            None => {
                let binding = self.storage.conversation.read().await;
                Ok(ScopedChatRuntime {
                    _lifetime: ScopedRuntimeLifetime::Global,
                    execution_scope: crate::workspace::WorkspaceExecutionScope::global(
                        self.workspace.global_execution_root.clone(),
                    ),
                    workspace_io_identity: crate::workspace::WorkspaceIoIdentity::global(
                        self.workspace.global_execution_root.clone(),
                    ),
                    primary_agent: self.connection.primary_agent(),
                    pool: self.connection.pool.clone(),
                    task_runtime: self.tasks.runtime.clone(),
                    review_integration: self.review_integration.clone(),
                    conversation_store: binding.store.clone(),
                    runtime_state_store: binding.runtime_state.clone(),
                    deletions: binding.deletions.clone(),
                })
            }
        }
    }

    /// Atomically capture the focused runtime and admit one foreground turn.
    pub async fn begin_scoped_chat_turn_owned(
        &self,
        surface: crate::foreground_turn::ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: impl Into<String>,
    ) -> Result<
        (
            ScopedChatRuntime,
            crate::foreground_turn::ForegroundTurnLease,
        ),
        ScopedChatTurnError,
    > {
        let _lifecycle = self.workspace.transition.read().await;
        let runtime = self
            .current_chat_runtime_inner()
            .await
            .map_err(|error| ScopedChatTurnError::Runtime(error.to_string()))?;
        let lease = runtime
            .begin_turn(
                &self.session.foreground_turns,
                surface,
                conversation_id,
                turn_id,
            )
            .await?;
        Ok((runtime, lease))
    }

    /// 切换到指定工作区。
    ///
    /// 这会重新初始化 persistence 和 session manager 以使用工作区路径。
    #[cfg(test)]
    pub(crate) async fn switch_workspace(
        self: &Arc<Self>,
        workspace: Workspace,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::Switch(workspace))
            .await?
        {
            WorkspaceSettlementOutcome::Transition(receipt) => Ok(receipt),
            _ => anyhow::bail!("workspace switch settlement returned an unexpected outcome"),
        }
    }

    pub async fn switch_workspace_registered(
        self: &Arc<Self>,
        workspace_id: crate::workspace::WorkspaceId,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::SwitchRegistered(
                workspace_id,
            ))
            .await?
        {
            WorkspaceSettlementOutcome::Transition(receipt) => Ok(receipt),
            _ => anyhow::bail!("workspace switch settlement returned an unexpected outcome"),
        }
    }

    pub async fn exit_workspace(self: &Arc<Self>) -> anyhow::Result<WorkspaceTransitionReceipt> {
        match self
            .run_owned_workspace_transition(WorkspaceTransitionRequest::Exit)
            .await?
        {
            WorkspaceSettlementOutcome::Transition(receipt) => Ok(receipt),
            _ => anyhow::bail!("workspace exit settlement returned an unexpected outcome"),
        }
    }

    #[cfg(test)]
    fn park_next_workspace_transition(
        &self,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ),
        String,
    > {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut barrier = self
            .workspace
            .transition_test_barrier
            .lock()
            .map_err(|_| "workspace transition test barrier lock is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("workspace transition test barrier is already installed".to_string());
        }
        *barrier = Some(WorkspaceTransitionTestBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        Ok((entered_rx, release_tx))
    }

    #[cfg(test)]
    pub(crate) fn park_next_workspace_control_acquire_for_test(
        &self,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ),
        String,
    > {
        self.workspace.runtimes.park_next_control_acquire()
    }

    #[cfg(test)]
    pub(crate) async fn evict_workspace_runtime_if_idle_for_test(
        &self,
        workspace_id: &crate::workspace::WorkspaceId,
    ) -> anyhow::Result<bool> {
        self.workspace
            .runtimes
            .shutdown_and_evict_if_idle(workspace_id)
            .await
    }

    async fn run_owned_workspace_transition(
        self: &Arc<Self>,
        request: WorkspaceTransitionRequest,
    ) -> anyhow::Result<WorkspaceSettlementOutcome> {
        let mut settlement = self.workspace.settlement.lock().await;
        if let Some(previous) = settlement.as_mut() {
            if let Err(error) = await_workspace_settlement(previous).await {
                tracing::warn!(
                    %error,
                    "previous detached workspace transition settled with an error"
                );
            }
            settlement.take();
        }

        let marker = self.begin_workspace_transition_marker()?;
        let state = Arc::clone(self);
        *settlement = Some(tokio::spawn(async move {
            let _marker = marker;
            match request {
                WorkspaceTransitionRequest::Create { name, kind, root } => state
                    .create_workspace_inner(name, kind, root)
                    .await
                    .map(|(workspace, created)| {
                        WorkspaceSettlementOutcome::Created(workspace, created)
                    }),
                #[cfg(test)]
                WorkspaceTransitionRequest::Switch(workspace) => {
                    let receipt = state.switch_workspace_inner(workspace).await?;
                    Ok(WorkspaceSettlementOutcome::Transition(
                        state
                            .reconcile_extension_skills_after_workspace_load(receipt)
                            .await,
                    ))
                }
                WorkspaceTransitionRequest::SwitchRegistered(workspace_id) => {
                    let receipt = state
                        .switch_registered_workspace_inner(workspace_id)
                        .await?;
                    Ok(WorkspaceSettlementOutcome::Transition(
                        state
                            .reconcile_extension_skills_after_workspace_load(receipt)
                            .await,
                    ))
                }
                WorkspaceTransitionRequest::Exit => state
                    .exit_workspace_inner()
                    .await
                    .map(WorkspaceSettlementOutcome::Transition),
                WorkspaceTransitionRequest::Delete(workspace_id) => state
                    .delete_workspace_inner(&workspace_id)
                    .await
                    .map(|()| WorkspaceSettlementOutcome::Deleted),
                WorkspaceTransitionRequest::LinkProject {
                    workspace_id,
                    project_root,
                } => state
                    .link_workspace_project_inner(workspace_id, project_root)
                    .await
                    .map(WorkspaceSettlementOutcome::Linked),
            }
        }));
        let result = match settlement.as_mut() {
            Some(handle) => await_workspace_settlement(handle).await,
            None => Err(anyhow::anyhow!(
                "workspace settlement owner lost the accepted transition"
            )),
        };
        settlement.take();
        result
    }

    async fn reconcile_extension_skills_after_workspace_load(
        self: &Arc<Self>,
        mut receipt: WorkspaceTransitionReceipt,
    ) -> WorkspaceTransitionReceipt {
        let repair = self
            .extension_control
            .reconcile_enabled_skills_on_load(self)
            .await;
        let error = match repair {
            Ok(skill_receipt)
                if skill_receipt.status
                    == crate::extension_control::SkillSettlementStatus::Settled =>
            {
                None
            }
            Ok(skill_receipt)
                if skill_receipt.target_receipts.iter().all(|target| {
                    target.error.as_deref().is_some_and(|error| {
                        error.contains("plugin runtime service is not initialized")
                    })
                }) =>
            {
                None
            }
            Ok(skill_receipt) => Some(format!(
                "skill runtime reconciliation remains {:?}: {}",
                skill_receipt.status,
                skill_receipt
                    .target_receipts
                    .iter()
                    .filter_map(|target| target
                        .error
                        .as_ref()
                        .map(|error| format!("{}={error}", target.target)))
                    .collect::<Vec<_>>()
                    .join("; "),
            )),
            Err(error)
                if error
                    .to_string()
                    .contains("plugin runtime service is not initialized") =>
            {
                // Lightweight state fixtures and the earliest bootstrap phase may not have
                // attached the specialist runtime yet. Startup performs the same repair after
                // binding that owner; do not misclassify this ordering gap as a workspace fault.
                None
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = error {
            receipt.status = WorkspaceTransitionStatus::Degraded;
            receipt
                .degraded_subsystems
                .push(WorkspaceSubsystemTransition {
                    subsystem: "extension_skill_repair".to_string(),
                    target_root: receipt.target_root.clone(),
                    stale_roots: Vec::new(),
                    error,
                });
        }
        *self.workspace.last_transition.write().await = Some(receipt.clone());
        receipt
    }

    fn begin_workspace_transition_marker(&self) -> anyhow::Result<WorkspaceTransitionMarker> {
        if self
            .workspace
            .transitioning
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            anyhow::bail!("workspace transition owner is already active");
        }
        Ok(WorkspaceTransitionMarker {
            transitioning: Arc::clone(&self.workspace.transitioning),
        })
    }

    /// Await the detached workspace settlement without tearing down its runtime.
    ///
    /// Application shutdown uses this boundary before joining ProductData flows
    /// so an accepted transition settles while its workspace specialists remain
    /// available.
    pub(crate) async fn join_workspace_transition(&self) -> anyhow::Result<()> {
        let mut settlement = self.workspace.settlement.lock().await;
        let result = match settlement.as_mut() {
            Some(handle) => await_workspace_settlement(handle).await.map(|_| ()),
            None => Ok(()),
        };
        settlement.take();
        result
    }

    /// Tear down workspace specialist runtimes after ProductData settlement.
    pub(crate) async fn shutdown_workspace_runtimes(&self) -> anyhow::Result<()> {
        for activity in self.workspace.runtimes.activity_snapshot().await? {
            tracing::debug!(
                workspace = %activity.workspace_id,
                execution_loaded = activity.execution_loaded,
                active_pool_executions = activity.active_pool_executions,
                active_run_drivers = activity.active_run_drivers,
                active_run_driver_receipts = activity.active_run_driver_receipts,
                active_task_runtime_operations = activity.active_task_runtime_operations,
                active_controls = activity.active_controls,
                idle = activity.is_idle(),
                "workspace runtime activity before shutdown"
            );
        }
        self.workspace.runtimes.shutdown().await
    }

    /// Await a detached workspace settlement and then tear down its runtime.
    /// Callers coordinating ProductData must use the two explicit phase methods
    /// so accepted extension settlement runs before specialist teardown.
    pub async fn shutdown_workspace_transition(&self) -> anyhow::Result<()> {
        let transition_result = self.join_workspace_transition().await;
        let runtime_result = self.shutdown_workspace_runtimes().await;
        match (transition_result, runtime_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(transition), Err(runtime)) => Err(anyhow::anyhow!(
                "workspace transition: {transition}; workspace runtimes: {runtime}"
            )),
        }
    }

    #[cfg(test)]
    async fn switch_workspace_inner(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let _transition = self.workspace.transition.write().await;
        #[cfg(test)]
        {
            let barrier = self
                .workspace
                .transition_test_barrier
                .lock()
                .map_err(|_| anyhow::anyhow!("workspace transition test barrier is poisoned"))?
                .take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                barrier.release.await.map_err(|_| {
                    anyhow::anyhow!("workspace transition test barrier release was dropped")
                })?;
            }
        }
        self.switch_workspace_inner_locked(workspace).await
    }

    async fn switch_registered_workspace_inner(
        &self,
        workspace_id: crate::workspace::WorkspaceId,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let _transition = self.workspace.transition.write().await;
        let workspace = self
            .workspace
            .registry
            .open(&workspace_id)
            .map_err(anyhow::Error::msg)?;
        self.switch_workspace_inner_locked(workspace).await
    }

    async fn switch_workspace_inner_locked(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let previous_workspace_id = self
            .workspace
            .current
            .read()
            .await
            .as_ref()
            .map(|host| host.id().to_string());
        let host = self.workspace.runtimes.get_or_open(workspace).await?;
        let execution = match self.connection.pool.as_ref() {
            Some(seed_pool) => Some(host.get_or_open_execution(seed_pool).await?),
            None => None,
        };
        if let (Some(seed_pool), Some(execution)) =
            (self.connection.pool.as_ref(), execution.as_ref())
        {
            self.attach_task_execution_target_resolver(&execution.task_runtime(), seed_pool);
        }
        if let Some(execution) = execution.as_ref() {
            let conversation_id = uuid::Uuid::new_v4().to_string();
            execution
                .primary_agent()
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.reset().await;
                        agent.set_conversation_id(conversation_id);
                    })
                })
                .await;
        }

        let workspace = host.workspace().await;
        let resources = host.resources();
        {
            let mut binding = self.storage.conversation.write().await;
            *binding = ConversationStorageBinding {
                store: Some(resources.conversation_store()),
                runtime_state: Some(resources.runtime_state_store()),
                deletions: resources.deletion_service(),
            };
        }
        self.storage.tool_executions.register_artifact_config(
            crate::infra::tool_output_artifact_config(Some(&workspace.root)),
        );
        *self.workspace.current.write().await = Some(host);

        let mut degraded_subsystems = Vec::new();
        if let (Some(watcher), Some(execution)) = (self.config_watcher.as_ref(), execution.as_ref())
        {
            match watcher
                .register_workspace(
                    crate::config_watcher::ConfigWatcherWorkspaceIdentity::new(
                        workspace.id.to_string(),
                        workspace.opaque_product_data_generation(),
                    ),
                    workspace.root.clone(),
                    execution.primary_agent(),
                    execution.plugin_runtime(),
                )
                .await
            {
                Ok(registration) if registration.errors.is_empty() => {}
                Ok(registration) => degraded_subsystems.push(WorkspaceSubsystemTransition {
                    subsystem: "config_watcher".to_string(),
                    target_root: registration.registered_root,
                    stale_roots: Vec::new(),
                    error: registration.errors.join("; "),
                }),
                Err(error) => degraded_subsystems.push(WorkspaceSubsystemTransition {
                    subsystem: "config_watcher".to_string(),
                    target_root: workspace.root.clone(),
                    stale_roots: Vec::new(),
                    error: error.to_string(),
                }),
            }
        }
        let receipt = WorkspaceTransitionReceipt::committed(
            previous_workspace_id,
            Some(workspace.id.to_string()),
            workspace.root.clone(),
            degraded_subsystems,
        );
        *self.workspace.last_transition.write().await = Some(receipt.clone());
        tracing::info!(
            workspace = %workspace.id,
            root = %workspace.root.display(),
            "Focused workspace runtime host"
        );
        Ok(receipt)
    }

    /// Exit workspace focus without mutating any loaded execution host.
    async fn exit_workspace_inner(&self) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let _transition = self.workspace.transition.write().await;
        self.exit_workspace_inner_locked().await
    }

    async fn exit_workspace_inner_locked(&self) -> anyhow::Result<WorkspaceTransitionReceipt> {
        let global_execution_root = self
            .workspace
            .global_execution_root
            .canonicalize()
            .map_err(|error| {
                anyhow::anyhow!("Failed to resolve the global working directory: {error}")
            })?;
        let previous_workspace_id = self
            .workspace
            .current
            .read()
            .await
            .as_ref()
            .map(|host| host.id().to_string());
        let conversation_id = uuid::Uuid::new_v4().to_string();
        self.connection
            .primary_agent()
            .write_async(|agent| {
                Box::pin(async move {
                    agent.reset().await;
                    agent.set_conversation_id(conversation_id);
                })
            })
            .await;

        *self.storage.conversation.write().await = self.workspace.global_conversation.clone();
        *self.workspace.current.write().await = None;

        let receipt = WorkspaceTransitionReceipt::committed(
            previous_workspace_id,
            None,
            global_execution_root,
            Vec::new(),
        );
        *self.workspace.last_transition.write().await = Some(receipt.clone());
        tracing::info!("Exited workspace focus; loaded hosts remain available");
        Ok(receipt)
    }
}

async fn wait_for_live_delivery_or_shutdown<F>(
    shutdown: &CancellationToken,
    settlement: F,
) -> Option<F::Output>
where
    F: std::future::Future,
{
    tokio::pin!(settlement);
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => None,
        settlement = &mut settlement => Some(settlement),
    }
}

fn prepare_model_mutation(
    current: &crate::config::EkoConfig,
    active_model_id: &str,
    request: ModelMutationRequest,
) -> Result<PreparedModelMutation, ModelMutationError> {
    match request {
        ModelMutationRequest::UpsertModel(mutation) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let previous_default = current.model.default_model_id.clone();
            let model_id =
                crate::model_config::upsert_configured_model(&mut config, mutation.model)
                    .map_err(ModelMutationError::Validation)?;
            let became_first_default = previous_default.is_none()
                && config.model.default_model_id.as_deref() == Some(model_id.as_str());
            let updates_persisted_default = mutation.set_default
                || previous_default.as_deref() == Some(model_id.as_str())
                || became_first_default;
            if updates_persisted_default {
                crate::model_config::set_default_model(&mut config, &model_id)
                    .map_err(ModelMutationError::Validation)?;
            }
            let updates_active_model = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.id == model_id);
            let activates_upserted_model =
                mutation.set_default || updates_active_model || became_first_default;
            let runtime = crate::model_config::resolve_runtime_model(
                &config,
                Some(&model_id),
            )
            .map_err(|error| ModelMutationError::Validation(error.to_string()))?;
            let prepared = crate::infra::prepare_runtime_llm(&runtime)
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id,
                runtime: Some(runtime),
                prepared: Some(prepared),
                activated: activates_upserted_model,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::UpsertProvider(mutation) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let mut provider = mutation.provider;
            if mutation.preserve_auth_token && provider.auth_token.is_none() {
                provider.auth_token = current
                    .model_providers
                    .get(&mutation.id)
                    .and_then(|current| current.auth_token.clone());
            }
            let provider_id =
                crate::model_config::upsert_model_provider(&mut config, &mutation.id, provider)
                    .map_err(ModelMutationError::Validation)?;
            let activated = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.provider == provider_id);
            let runtime = if activated {
                let active_id = active_before
                    .as_ref()
                    .map(|runtime| runtime.id.as_str())
                    .unwrap_or_default();
                resolve_active_model_runtime(&config, active_id)?
            } else {
                None
            };
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: provider_id,
                runtime,
                prepared,
                activated,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::SetDefault(selector) => {
            let selected =
                crate::model_config::resolve_runtime_model(current, Some(&selector))
                    .map_err(|error| ModelMutationError::Validation(error.to_string()))?;
            let mut config = current.clone();
            let runtime = crate::model_config::set_default_model(&mut config, &selected.id)
                .map_err(ModelMutationError::Validation)?;
            let prepared = crate::infra::prepare_runtime_llm(&runtime)
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: runtime.id.clone(),
                runtime: Some(runtime),
                prepared: Some(prepared),
                activated: true,
                deactivated: false,
                deleted: false,
            })
        }
        ModelMutationRequest::DeleteModel(model_id) => {
            let mut config = current.clone();
            match crate::model_config::delete_configured_model(&mut config, &model_id)
                .map_err(ModelMutationError::Validation)?
            {
                crate::model_config::DeleteConfiguredModelOutcome::RemovedNonDefault => {
                    if active_model_id == model_id {
                        let runtime = resolve_active_model_runtime(&config, active_model_id)?
                            .ok_or_else(|| {
                                ModelMutationError::Validation(
                                    "Deleted active model has no enabled successor".to_string(),
                                )
                            })?;
                        let prepared = crate::infra::prepare_runtime_llm(&runtime)
                            .map_err(ModelMutationError::Validation)?;
                        Ok(PreparedModelMutation {
                            config,
                            model_id,
                            runtime: Some(runtime),
                            prepared: Some(prepared),
                            activated: true,
                            deactivated: false,
                            deleted: true,
                        })
                    } else {
                        Ok(PreparedModelMutation {
                            config,
                            model_id,
                            runtime: None,
                            prepared: None,
                            activated: false,
                            deactivated: false,
                            deleted: true,
                        })
                    }
                }
                crate::model_config::DeleteConfiguredModelOutcome::ActivatedSuccessor(runtime) => {
                    let prepared = crate::infra::prepare_runtime_llm(&runtime)
                        .map_err(ModelMutationError::Validation)?;
                    Ok(PreparedModelMutation {
                        config,
                        model_id,
                        runtime: Some(*runtime),
                        prepared: Some(prepared),
                        activated: true,
                        deactivated: false,
                        deleted: true,
                    })
                }
                crate::model_config::DeleteConfiguredModelOutcome::Deactivated => {
                    Ok(PreparedModelMutation {
                        config,
                        model_id,
                        runtime: None,
                        prepared: None,
                        activated: false,
                        deactivated: true,
                        deleted: true,
                    })
                }
            }
        }
        ModelMutationRequest::DeleteProvider(provider_id) => {
            let mut config = current.clone();
            let active_before = resolve_active_model_runtime(current, active_model_id)?;
            let removes_active_model = active_before
                .as_ref()
                .is_some_and(|runtime| runtime.provider == provider_id);
            crate::model_config::delete_model_provider(&mut config, &provider_id)
                .map_err(ModelMutationError::Validation)?;
            let runtime = if removes_active_model {
                resolve_active_model_runtime(&config, active_model_id)?
            } else {
                None
            };
            if removes_active_model && runtime.is_none() {
                crate::model_config::clear_selected_model(&mut config);
            }
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            Ok(PreparedModelMutation {
                config,
                model_id: provider_id,
                activated: runtime.is_some(),
                deactivated: removes_active_model && runtime.is_none(),
                runtime,
                prepared,
                deleted: true,
            })
        }
        ModelMutationRequest::UpdateConfig {
            update,
            reapply_active_model,
        } => {
            let mut config = current.clone();
            update(&mut config).map_err(ModelMutationError::Validation)?;
            let runtime = if reapply_active_model {
                resolve_active_model_runtime(&config, active_model_id)?
            } else {
                None
            };
            let prepared = runtime
                .as_ref()
                .map(crate::infra::prepare_runtime_llm)
                .transpose()
                .map_err(ModelMutationError::Validation)?;
            let model_id = runtime
                .as_ref()
                .map(|runtime| runtime.id.clone())
                .or_else(|| config.model.default_model_id.clone())
                .unwrap_or_default();
            Ok(PreparedModelMutation {
                config,
                model_id,
                runtime,
                prepared,
                activated: reapply_active_model,
                deactivated: false,
                deleted: false,
            })
        }
        #[cfg(test)]
        ModelMutationRequest::AbortSettlementForTest => Err(ModelMutationError::Settlement(
            "test-only aborted settlement reached mutation preparation".to_string(),
        )),
    }
}

fn resolve_active_model_runtime(
    config: &crate::config::EkoConfig,
    active_model_id: &str,
) -> Result<Option<crate::model_config::ModelRuntimeConfig>, ModelMutationError> {
    if !config.configured_models.iter().any(|model| model.enabled) {
        return Ok(None);
    }
    let active_is_available = config
        .configured_models
        .iter()
        .any(|model| model.id == active_model_id && model.enabled);
    let selector = if active_is_available || config.configured_models.is_empty() {
        Some(active_model_id)
    } else {
        config.model.default_model_id.as_deref()
    };
    crate::model_config::resolve_runtime_model(config, selector)
        .map(Some)
        .map_err(|error| ModelMutationError::Validation(error.to_string()))
}

// ── Agent control application operations (spawn / resume / handoff) ──
// Implements `AgentControlAppOps` for the six-tool plane's newer siblings.
// Kept inside app_state.rs because these operations open workspace hosts and
// drive pooled chat runtimes — AppState-private machinery that the thin
// AgentControlService must not own (ADR 0016 layering).

struct AppStateAgentControlOps {
    state: std::sync::Weak<AppState>,
}

impl AppStateAgentControlOps {
    fn new(state: &Arc<AppState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }

    /// Resolve the conversation store for a workspace id: the current
    /// workspace uses its live runtime; other ids open the registered host.
    async fn conversation_store_for(
        state: &Arc<AppState>,
        workspace_id: &str,
    ) -> Result<(Arc<dyn echo_agent::memory::ConversationStore>, bool), crate::agent_control::AgentControlError>
    {
        let current = state
            .current_chat_runtime_inner()
            .await
            .map_err(|error| {
                crate::agent_control::AgentControlError::Runtime(error.to_string())
            })?;
        let current_id = current.execution_scope().workspace_id();
        if workspace_id == current_id {
            let store = current.conversation_store().ok_or_else(|| {
                crate::agent_control::AgentControlError::Runtime(
                    "current workspace has no conversation store".to_string(),
                )
            })?;
            return Ok((store, true));
        }
        // Cross-workspace: resolve the registered workspace and open its host.
        let workspace = state
            .workspace
            .registry
            .list()
            .map_err(|error| crate::agent_control::AgentControlError::Invalid(error.to_string()))?
            .into_iter()
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .ok_or_else(|| {
                crate::agent_control::AgentControlError::Invalid(format!(
                    "workspace '{workspace_id}' is not registered"
                ))
            })?;
        let host = state
            .workspace
            .runtimes
            .get_or_open(workspace)
            .await
            .map_err(|error| {
                crate::agent_control::AgentControlError::Runtime(error.to_string())
            })?;
        let store = host.resources().conversation_store();
        Ok((store, false))
    }
}

impl crate::agent_control::AgentControlAppOps for AppStateAgentControlOps {
    fn spawn_conversation(
        &self,
        request: crate::agent_control::AgentSpawnRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<serde_json::Value, crate::agent_control::AgentControlError>,
    > {
        let weak = self.state.clone();
        Box::pin(async move {
            let state = weak
                .upgrade()
                .ok_or_else(|| {
                    crate::agent_control::AgentControlError::Runtime(
                        "app state unavailable".to_string(),
                    )
                })?;
            let workspace_id = match request.workspace_id.as_deref() {
                Some(id) if !id.trim().is_empty() => id.to_string(),
                _ => state
                    .current_chat_runtime_inner()
                    .await
                    .map_err(|error| {
                        crate::agent_control::AgentControlError::Runtime(error.to_string())
                    })?
                    .execution_scope()
                    .workspace_id()
                    .to_string(),
            };
            let (store, _) =
                Self::conversation_store_for(&state, &workspace_id).await?;
            let conversation_id = format!("spawn-{}", uuid::Uuid::new_v4().as_simple());
            let title = request
                .title
                .clone()
                .unwrap_or_else(|| request.goal.chars().take(80).collect());
            let conversation = store
                .create_conversation(echo_agent::memory::NewConversation {
                    conversation_id: conversation_id.clone(),
                    user_id: "eko".to_string(),
                    agent_type: None,
                    title: Some(title),
                })
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?;
            let address = crate::agent_router::AgentAddress::new(
                crate::workspace::WorkspaceId::from_raw(workspace_id.clone()),
                conversation.conversation_id.clone(),
            );
            let mut started = false;
            if request.start {
                let text = request
                    .first_message
                    .clone()
                    .unwrap_or_else(|| request.goal.clone());
                let message = crate::agent_router::AgentMessage::agent_text(None, address.clone(), text);
                state
                    .send_agent_message_owned(message)
                    .await
                    .map_err(|error| {
                        crate::agent_control::AgentControlError::Runtime(error.to_string())
                    })?;
                started = true;
            }
            Ok(serde_json::json!({
                "workspace_id": workspace_id,
                "conversation_id": conversation.conversation_id,
                "started": started,
            }))
        })
    }

    fn resume_target(
        &self,
        request: crate::agent_control::AgentResumeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<serde_json::Value, crate::agent_control::AgentControlError>,
    > {
        let weak = self.state.clone();
        Box::pin(async move {
            let state = weak
                .upgrade()
                .ok_or_else(|| {
                    crate::agent_control::AgentControlError::Runtime(
                        "app state unavailable".to_string(),
                    )
                })?;
            let address = crate::agent_router::AgentAddress::new(
                crate::workspace::WorkspaceId::from_raw(request.workspace_id.clone()),
                request.conversation_id.clone(),
            );
            match request.resume_policy {
                crate::agent_control::AgentResumePolicy::Followup => {
                    let text = request.text.unwrap_or_else(|| {
                        "请继续之前的工作:汇报当前状态并接着推进。".to_string()
                    });
                    let message =
                        crate::agent_router::AgentMessage::agent_text(None, address.clone(), text);
                    state
                        .send_agent_message_owned(message)
                        .await
                        .map_err(|error| {
                            crate::agent_control::AgentControlError::Runtime(error.to_string())
                        })?;
                    Ok(serde_json::json!({
                        "workspace_id": request.workspace_id,
                        "conversation_id": request.conversation_id,
                        "policy": "followup",
                        "queued": true,
                    }))
                }
                crate::agent_control::AgentResumePolicy::TaskRun => {
                    let run_id = request.run_id.clone().ok_or_else(|| {
                        crate::agent_control::AgentControlError::Invalid(
                            "task_run resume requires run_id".to_string(),
                        )
                    })?;
                    // Resolve the workspace-scoped task runtime and pooled
                    // conversation agent, then relaunch the paused run.
                    let runtime = state
                        .current_chat_runtime_inner()
                        .await
                        .map_err(|error| {
                            crate::agent_control::AgentControlError::Runtime(error.to_string())
                        })?;
                    if runtime.execution_scope().workspace_id()
                        != request.workspace_id.as_str()
                    {
                        return Err(crate::agent_control::AgentControlError::Invalid(format!(
                            "task_run resume is bound to the current workspace ({}); target {} requires a handoff or direct workspace focus",
                            runtime.execution_scope().workspace_id(),
                            request.workspace_id
                        )));
                    }
                    let store = runtime.task_runtime().ok_or_else(|| {
                        crate::agent_control::AgentControlError::Runtime(
                            "workspace has no TaskRuntimeStore".to_string(),
                        )
                    })?;
                    let probe_run_id = run_id.clone();
                    let pool = runtime.pool().ok_or_else(|| {
                        crate::agent_control::AgentControlError::Runtime(
                            "workspace has no AgentPool".to_string(),
                        )
                    })?;
                    let pool_execution = pool
                        .acquire(&request.conversation_id)
                        .await
                        .map_err(|error| {
                            crate::agent_control::AgentControlError::Runtime(error.to_string())
                        })?;
                    let run_state = crate::tasks::task_runtime::executor::TaskRuntimeOperation::new(
                        Arc::clone(&store),
                    )
                    .run_store("load resume run state", move |store| {
                        store.get_run_state(&probe_run_id)
                    })
                    .await
                    .map_err(|error| {
                        crate::agent_control::AgentControlError::Runtime(error.to_string())
                    })?
                    .ok_or_else(|| {
                        crate::agent_control::AgentControlError::Invalid(format!(
                            "run {run_id} not found"
                        ))
                    })?;
                    let expected =
                        crate::tasks::task_runtime::types::TaskRunResumeIdentity::capture(
                            &run_state,
                        );
                    let launch = crate::tasks::task_runtime::launch_task_run_resume(
                        store,
                        expected,
                        pool_execution.agent().clone(),
                        Some(pool_execution),
                        runtime.review_integration(),
                        None,
                        echo_agent::agent::CancellationToken::new(),
                        None,
                    )
                    .await
                    .map_err(|error| {
                        crate::agent_control::AgentControlError::Runtime(error.to_string())
                    })?;
                    if let Some(text) = request.text {
                        let note = crate::agent_router::AgentMessage::agent_text(
                            None,
                            address,
                            text,
                        );
                        let _ = state.send_agent_message_owned(note).await;
                    }
                    Ok(serde_json::json!({
                        "workspace_id": request.workspace_id,
                        "conversation_id": request.conversation_id,
                        "run_id": run_id,
                        "policy": "task_run",
                        "launched": true,
                        "launch_run_id": launch.run_id,
                    }))
                }
            }
        })
    }

    fn handoff_conversation(
        &self,
        request: crate::agent_control::AgentHandoffRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<serde_json::Value, crate::agent_control::AgentControlError>,
    > {
        let weak = self.state.clone();
        Box::pin(async move {
            let state = weak
                .upgrade()
                .ok_or_else(|| {
                    crate::agent_control::AgentControlError::Runtime(
                        "app state unavailable".to_string(),
                    )
                })?;
            if request.workspace_id == request.destination_workspace_id {
                return Err(crate::agent_control::AgentControlError::Invalid(
                    "destination workspace must differ from the source".to_string(),
                ));
            }
            let (source_store, _) =
                Self::conversation_store_for(&state, &request.workspace_id).await?;
            let (destination_store, _) = Self::conversation_store_for(
                &state,
                &request.destination_workspace_id,
            )
            .await?;
            let conversation = source_store
                .get_conversation(&request.conversation_id)
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?
                .ok_or_else(|| {
                    crate::agent_control::AgentControlError::Invalid(format!(
                        "conversation '{}' does not exist in workspace '{}'",
                        request.conversation_id, request.workspace_id
                    ))
                })?;
            let messages = source_store
                .get_messages(&request.conversation_id)
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?;
            // 1. Recreate the conversation (same id) in the destination store.
            destination_store
                .create_conversation(echo_agent::memory::NewConversation {
                    conversation_id: conversation.conversation_id.clone(),
                    user_id: conversation.user_id.clone(),
                    agent_type: conversation.agent_type.clone(),
                    title: conversation.title.clone(),
                })
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?;
            // 2. Copy the transcript verbatim.
            destination_store
                .save_messages(&conversation.conversation_id, &messages)
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?;
            // 3. Retire the source: pooled agent first (in-memory context),
            //    then the durable transcript.
            if request.workspace_id
                == state
                    .current_chat_runtime_inner()
                    .await
                    .map(|runtime| runtime.execution_scope().workspace_id().to_string())
                    .unwrap_or_default()
                && let Some(pool) =
                    state.current_chat_runtime_inner().await.ok().and_then(|r| r.pool())
            {
                let _ = pool
                    .retire_conversation_and_wait(&request.conversation_id)
                    .await;
            }
            source_store
                .delete_conversation(&request.conversation_id)
                .await
                .map_err(|error| {
                    crate::agent_control::AgentControlError::Runtime(error.to_string())
                })?;
            // 4. Optional follow-up in the destination workspace.
            let new_address = crate::agent_router::AgentAddress::new(
                crate::workspace::WorkspaceId::from_raw(
                    request.destination_workspace_id.clone(),
                ),
                request.conversation_id.clone(),
            );
            let mut follow_up_delivered = false;
            if let Some(text) = request.follow_up {
                let message =
                    crate::agent_router::AgentMessage::agent_text(None, new_address, text);
                let _ = state.send_agent_message_owned(message).await;
                follow_up_delivered = true;
            }
            Ok(serde_json::json!({
                "workspace_id": request.destination_workspace_id,
                "source_workspace_id": request.workspace_id,
                "conversation_id": request.conversation_id,
                "messages_migrated": messages.len(),
                "follow_up_delivered": follow_up_delivered,
            }))
        })
    }
}
