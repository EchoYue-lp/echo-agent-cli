/// Pool of `ReactAgent` instances that share expensive resources while
/// maintaining isolated execution contexts.
///
/// Each agent in the pool has its own `execution_mutex` and `ContextManager`,
/// enabling true parallel execution of multiple conversations.
pub struct AgentPool {
    shared: SharedResources,
    agents: RwLock<HashMap<String, PooledAgent>>,
    /// Primary Agent owned by this pool generation. Workspace forks create a
    /// dedicated primary; the bootstrap pool references the process primary.
    primary_agent: RwLock<Option<AgentHandle>>,
    /// Model consumers for a primary that is owned by this pool. The bootstrap
    /// primary remains owned by `AppState`; workspace primary Agents are
    /// published through the same pool transaction as cached conversation Agents.
    primary_model_consumers: RwLock<Option<infra::AgentModelConsumers>>,
    /// Latest durable user MCP snapshot for future Agents and future workspace
    /// forks. Live ToolManagers are reconciled separately by McpConfigRuntime.
    mcp_config_snapshot: RwLock<Option<McpConfigFile>>,
    workspace_transitioning: AtomicBool,
    shutting_down: AtomicBool,
    admission: Arc<AgentPoolAdmission>,
    process_agent_execution: Arc<AgentExecutionGovernor>,
    config: PoolConfig,
    app_config: RwLock<EkoConfig>,
    /// Working directory applied to existing and future pooled agents.
    working_dir: RwLock<Option<std::path::PathBuf>>,
    permission_mode: RwLock<PermissionMode>,
    /// Exact plugin generation projected into existing and future agents.
    agent_generation: RwLock<AgentPluginGeneration>,
    /// Cancellation token for the cleanup monitor task.
    cleanup_cancel: CancellationToken,
    /// Sole owned cleanup monitor settlement handle. The monitor holds only a
    /// weak pool reference so a failed bootstrap cannot keep the pool alive.
    cleanup_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Workspace-scoped conversation store used by existing and future agents.
    conversation_store_override: RwLock<Option<Arc<dyn echo_agent::memory::ConversationStore>>>,
    /// Workspace-scoped runtime-state store used by existing and future agents.
    state_store_override: RwLock<Option<Arc<dyn echo_agent::state::RuntimeStateStore>>>,
    /// Product-owned complete tool-output artifact policy for existing and
    /// future pooled agents. Updated together with workspace routing.
    tool_output_artifacts: RwLock<echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    /// Active workspace profile applied to existing and future pooled agents.
    workspace_kind: RwLock<WorkspaceKind>,
    /// Last strictly-read instruction generation. Existing and future pool
    /// agents are always projected from this same snapshot.
    instruction_projection: RwLock<Option<crate::unified_memory::InstructionProjectionSnapshot>>,
    /// Shared EKO user policy for tool visibility. Workspace forks retain the
    /// same service; each pool projects its generation into live/future Agents.
    tool_control: Arc<crate::tool_control::ToolControlService>,
    /// Explicit Mock transport used only by integration tests that must fork
    /// real workspace pools without contacting an external model provider.
    #[cfg(test)]
    llm_client_override: RwLock<Option<Arc<dyn echo_agent::llm::LlmClient>>>,
}

pub(crate) struct AgentPoolWorkspaceTransition<'a> {
    pool: &'a AgentPool,
    committed: bool,
}

/// Exact pool-wide receipt prepared before config persistence.
///
/// The agents write guard prevents eviction or creation while every existing
/// agent generation is admitted. Dropping this value rolls back all prepared
/// context receipts without touching live or future pool state.
pub(crate) struct PreparedAgentPoolModelPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    _agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    publications: Vec<infra::PreparedAgentModelPublication>,
    app_config: EkoConfig,
    runtime: ModelRuntimeConfig,
}

pub(crate) struct PreparedAgentPoolModelDeactivation<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    _agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    publications: Vec<infra::PreparedAgentModelDeactivation>,
    app_config: EkoConfig,
}

/// Pool-wide plugin publication. The existing workspace-transition admission
/// guard prevents new leases and waits for current executions to settle while
/// every cached agent is moved to the same candidate generation.
pub(crate) struct PreparedAgentPoolPluginPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    previous: AgentPluginGeneration,
    candidate: Option<AgentPluginGeneration>,
    application_skill_repair: Option<ApplicationSkillProjectionRepair>,
}

/// Pool-wide instruction publication under the existing execution admission.
pub(crate) struct PreparedAgentPoolInstructionPublication<'a> {
    pool: &'a AgentPool,
    _transition: AgentPoolWorkspaceTransition<'a>,
    agents: tokio::sync::RwLockWriteGuard<'a, HashMap<String, PooledAgent>>,
    candidate: Option<crate::unified_memory::InstructionProjectionSnapshot>,
}

impl PreparedAgentPoolModelPublication<'_> {
    pub(crate) async fn commit(self) {
        let Self {
            pool,
            _transition,
            _agents,
            publications,
            app_config,
            runtime,
        } = self;
        for publication in publications {
            publication.commit().await;
        }
        *pool.app_config.write().await = app_config;
        tracing::info!(
            provider = %runtime.provider,
            model = %runtime.model,
            pooled_agents = _agents.len(),
            "AgentPool: prepared runtime generation committed"
        );
    }
}

impl PreparedAgentPoolModelDeactivation<'_> {
    pub(crate) async fn commit(self) {
        let Self {
            pool,
            _transition,
            _agents,
            publications,
            app_config,
        } = self;
        for publication in publications {
            publication.commit().await;
        }
        *pool.app_config.write().await = app_config;
        tracing::info!(
            pooled_agents = _agents.len(),
            "AgentPool: active model removed from pooled agents"
        );
    }
}

impl PreparedAgentPoolPluginPublication<'_> {
    pub(crate) async fn prepare(&mut self, candidate: AgentPluginGeneration) -> Result<(), String> {
        self.prepare_inner(candidate, None).await
    }

    pub(crate) async fn prepare_application_skill(
        &mut self,
        candidate: AgentPluginGeneration,
        name: &str,
        source: &str,
    ) -> Result<(), String> {
        self.prepare_inner(
            candidate,
            Some(ApplicationSkillProjectionRepair {
                name: name.to_string(),
                source: source.to_string(),
            }),
        )
        .await
    }

    async fn prepare_inner(
        &mut self,
        candidate: AgentPluginGeneration,
        application_skill_repair: Option<ApplicationSkillProjectionRepair>,
    ) -> Result<(), String> {
        if self.candidate.is_some() {
            return Err("AgentPool plugin publication is already prepared".to_string());
        }
        if let Some(repair) = application_skill_repair.as_ref()
            && candidate.skill_descriptors.iter().any(|descriptor| {
                descriptor.source.as_deref() == Some(repair.source.as_str())
                    && descriptor.name != repair.name
            })
        {
            return Err(format!(
                "application skill source '{}' contains a descriptor other than '{}'",
                repair.source, repair.name
            ));
        }

        let mut applied = Vec::new();
        let mut pooled_agents = self.agents.iter().collect::<Vec<_>>();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (conversation_id, pooled) in pooled_agents {
            if let Err(error) = replace_agent_plugin_generation(
                &pooled.handle,
                &self.previous,
                &candidate,
                application_skill_repair.as_ref(),
            )
            .await
            {
                let mut errors = vec![format!("{conversation_id}: {error}")];
                for (applied_id, applied_handle) in applied.into_iter().rev() {
                    if let Err(rollback_error) = replace_agent_plugin_generation(
                        &applied_handle,
                        &candidate,
                        &self.previous,
                        application_skill_repair.as_ref(),
                    )
                    .await
                    {
                        errors.push(format!("rollback {applied_id}: {rollback_error}"));
                    }
                }
                return Err(format!(
                    "AgentPool plugin generation preparation failed: {}",
                    errors.join("; ")
                ));
            }
            applied.push((conversation_id.clone(), pooled.handle.clone()));
        }

        self.candidate = Some(candidate);
        self.application_skill_repair = application_skill_repair;
        Ok(())
    }

    pub(crate) async fn commit(&mut self) -> Result<(), String> {
        let candidate = self.candidate.as_ref().ok_or_else(|| {
            "AgentPool plugin publication cannot commit before preparation".to_string()
        })?;
        let revision = candidate.revision;
        *self.pool.agent_generation.write().await = candidate.clone();
        tracing::info!(
            revision,
            pooled_agents = self.agents.len(),
            "AgentPool: plugin generation committed"
        );
        Ok(())
    }

    pub(crate) async fn rollback(&mut self) -> Result<(), String> {
        let Some(candidate) = self.candidate.take() else {
            return Ok(());
        };
        let mut errors = Vec::new();
        let mut pooled_agents = self.agents.iter().collect::<Vec<_>>();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (conversation_id, pooled) in pooled_agents {
            if let Err(error) = replace_agent_plugin_generation(
                &pooled.handle,
                &candidate,
                &self.previous,
                self.application_skill_repair.as_ref(),
            )
            .await
            {
                errors.push(format!("{conversation_id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "AgentPool plugin generation rollback failed: {}",
                errors.join("; ")
            ))
        }
    }
}

impl PreparedAgentPoolInstructionPublication<'_> {
    pub(crate) async fn prepare(
        &mut self,
        candidate: crate::unified_memory::InstructionProjectionSnapshot,
    ) -> Result<(), String> {
        if self.candidate.is_some() {
            return Err("AgentPool instruction publication is already prepared".to_string());
        }
        for pooled in self.agents.values() {
            let snapshot = candidate.clone();
            pooled
                .handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::unified_memory::apply_instruction_projection_snapshot(
                            agent, &snapshot,
                        )
                        .await;
                    })
                })
                .await;
        }
        self.candidate = Some(candidate);
        Ok(())
    }

    pub(crate) async fn commit(mut self) -> Result<(), String> {
        let candidate = self.candidate.take().ok_or_else(|| {
            "AgentPool instruction publication cannot commit before preparation".to_string()
        })?;
        tracing::info!(
            revision = candidate.revision(),
            pooled_agents = self.agents.len(),
            "AgentPool: instruction projection generation committed"
        );
        *self.pool.instruction_projection.write().await = Some(candidate);
        Ok(())
    }
}

impl AgentPoolWorkspaceTransition<'_> {
    #[cfg(test)]
    pub(crate) async fn commit(&mut self) {
        if self.committed {
            return;
        }
        let mut agents = self.pool.agents.write().await;
        let count = agents.len();
        agents.clear();
        self.committed = true;
        tracing::info!(
            agents_cleared = count,
            "AgentPool: cleared for workspace transition"
        );
    }

    pub(crate) async fn publish_instruction_snapshot(
        &self,
        expected_pool: &Arc<AgentPool>,
        snapshot: crate::unified_memory::InstructionProjectionSnapshot,
    ) -> Result<(), String> {
        if !std::ptr::eq(self.pool, expected_pool.as_ref()) {
            return Err("instruction snapshot targets a different AgentPool".to_string());
        }
        if !self.committed {
            return Err(
                "instruction snapshot cannot publish before the pool transition commits"
                    .to_string(),
            );
        }
        if !self.pool.agents.read().await.is_empty() {
            return Err(
                "instruction snapshot cannot publish while retired pool agents remain".to_string(),
            );
        }
        tracing::info!(
            revision = snapshot.revision(),
            "AgentPool: workspace instruction projection generation committed"
        );
        *self.pool.instruction_projection.write().await = Some(snapshot);
        Ok(())
    }
}

impl Drop for AgentPoolWorkspaceTransition<'_> {
    fn drop(&mut self) {
        self.pool
            .workspace_transitioning
            .store(false, Ordering::Release);
    }
}

impl AgentPool {
    pub(crate) fn retain_for_supervised_run(
        self: &Arc<Self>,
        key: String,
        execution: AgentPoolExecutionLease,
    ) -> OwnedRunPoolReceipt {
        OwnedRunPoolReceipt {
            pool: Arc::clone(self),
            key,
            execution: Some(execution),
        }
    }

    /// Create a pool from an already-bootstrapped `AgentRuntime`.
    ///
    /// Extracts shared resources from the runtime's primary agent and
    /// optionally pre-creates a background task agent.
    pub async fn from_runtime(
        runtime: &crate::runtime::AgentRuntime,
        config: PoolConfig,
        task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Arc<Self>> {
        let shared = SharedResources::extract_from(
            &runtime.agent_handle,
            runtime.review_integration.clone(),
        )
        .await;
        let mut shared = shared;
        shared.browser_runtime = Some(runtime.browser_runtime.clone());
        shared.task_runtime_store = task_runtime_store;
        shared.command_cell_runtime = Some(runtime.command_cell_runtime.clone());
        shared.product_data_io = Some(runtime.product_data_io.clone());

        // Extract skill descriptors from primary agent (avoids re-reading from disk)
        let skill_descriptors = runtime.agent_handle.read(|a| a.skill_descriptors()).await;
        let tool_output_artifacts = runtime
            .agent_handle
            .read(|agent| agent.tool_output_artifacts())
            .await
            .unwrap_or_else(|| crate::infra::tool_output_artifact_config(None));
        let working_dir = runtime.agent_handle.read(|agent| agent.working_dir()).await;
        shared.execution_scope = Some(crate::workspace::WorkspaceExecutionScope::global(
            working_dir
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        ));

        let pool = Arc::new(Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(Some(runtime.agent_handle.clone())),
            primary_model_consumers: RwLock::new(Some(runtime.model_consumers.clone())),
            mcp_config_snapshot: RwLock::new(Some(runtime.mcp_config_runtime.snapshot().await)),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: PROCESS_AGENT_EXECUTION.clone(),
            config,
            app_config: RwLock::new(runtime.session_app_config.clone()),
            working_dir: RwLock::new(working_dir),
            permission_mode: RwLock::new(PermissionMode::Default),
            agent_generation: RwLock::new(AgentPluginGeneration::new(
                0,
                skill_descriptors,
                Vec::new(),
                None,
            )),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(tool_output_artifacts),
            workspace_kind: RwLock::new(WorkspaceKind::General),
            instruction_projection: RwLock::new(None),
            tool_control: Arc::new(crate::tool_control::ToolControlService::default()),
            #[cfg(test)]
            llm_client_override: RwLock::new(None),
        });

        // Bind before creating the background agent so it and every later
        // conversation start from PluginRuntimeService's committed catalog.
        runtime
            .plugin_runtime
            .bind_agent_pool(Arc::downgrade(&pool))
            .await?;
        if let Some(review_integration) = runtime.review_integration.as_ref() {
            review_integration.bind_rule_projection_pool(&pool).await?;
        }

        // Pre-create background agent if enabled
        if pool.config.enable_background_agent {
            match pool.create_agent("__background__").await {
                Ok(pooled) => {
                    let mut agents = pool.agents.write().await;
                    agents.insert("__background__".to_string(), pooled);
                    tracing::info!("AgentPool: background agent created");
                }
                Err(e) => {
                    tracing::warn!("AgentPool: failed to create background agent: {e}");
                }
            }
        }

        Ok(pool)
    }

    /// Fork an independently admitted pool for one immutable workspace host.
    ///
    /// Expensive process-safe primitives remain shared, while every resource
    /// whose contents or tool behavior depend on workspace identity is replaced
    /// by the host-owned instance. Agents inside one host share that host's
    /// ToolManager (including its MCP clients); different hosts never share it.
    pub(crate) async fn fork_for_workspace(
        &self,
        resources: WorkspaceAgentPoolResources,
    ) -> anyhow::Result<(
        Arc<Self>,
        Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
        Arc<crate::mcp_config_runtime::McpNameOwnershipRegistry>,
    )> {
        let WorkspaceAgentPoolResources {
            root,
            kind,
            conversation_store,
            state_store,
            memory_store,
            task_runtime_store,
            review_integration,
            execution_scope,
            workspace_io_identity,
        } = resources;
        let plugin_target_scope = format!(
            "{}@{}",
            execution_scope.workspace_id(),
            workspace_io_identity.host_generation()
        );
        let authority_plugin_generation = self.agent_generation.read().await.clone();
        let mcp_config_snapshot = self.mcp_config_snapshot.read().await.clone();
        let shared = SharedResources {
            tool_manager: None,
            hook_registry: None,
            sandbox_manager: self.shared.sandbox_manager.clone(),
            store: Some(memory_store),
            conversation_store: Some(conversation_store),
            run_store: self.shared.run_store.clone(),
            token_tracker: self.shared.token_tracker.clone(),
            permission_service: self.shared.permission_service.clone(),
            state_store: Some(state_store),
            tool_execution_pipeline: self.shared.tool_execution_pipeline.clone(),
            review_integration: Some(review_integration),
            task_runtime_store: Some(task_runtime_store.clone()),
            browser_runtime: self.shared.browser_runtime.clone(),
            command_cell_runtime: self.shared.command_cell_runtime.clone(),
            product_data_io: self.shared.product_data_io.clone(),
            execution_scope: Some(execution_scope),
        };
        let workspace_product_data_io = shared.product_data_io.clone();
        let mut pool = Arc::new(Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(None),
            primary_model_consumers: RwLock::new(None),
            mcp_config_snapshot: RwLock::new(mcp_config_snapshot.clone()),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: self.process_agent_execution.clone(),
            config: self.config.clone(),
            app_config: RwLock::new(self.app_config.read().await.clone()),
            working_dir: RwLock::new(Some(root.clone())),
            permission_mode: RwLock::new(*self.permission_mode.read().await),
            agent_generation: RwLock::new(authority_plugin_generation.clone()),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(crate::infra::tool_output_artifact_config(Some(
                &root,
            ))),
            workspace_kind: RwLock::new(kind),
            instruction_projection: RwLock::new(self.instruction_projection.read().await.clone()),
            tool_control: crate::tool_control::shared(&self.tool_control),
            #[cfg(test)]
            llm_client_override: RwLock::new(self.llm_client_override.read().await.clone()),
        });

        let primary = pool.create_agent("__workspace_primary__").await?;
        let app_config = self.app_config.read().await.clone();
        crate::infra::load_user_hooks(&primary.handle, &app_config, Some(root.as_path())).await;
        let lsp_runtime = if mcp_config_snapshot.is_some() {
            Some(crate::runtime::register_lsp_tools(&primary.handle, &root).await)
        } else {
            None
        };
        primary
            .handle
            .write(move |agent| {
                if let Some(product_data_io) = workspace_product_data_io {
                    crate::research_connectors::install_auto_ingest_tools(
                        agent,
                        workspace_io_identity.clone(),
                        product_data_io.clone(),
                    );
                    agent.add_tool(Box::new(crate::research_tool::ResearchLibraryTool::new(
                        product_data_io,
                        workspace_io_identity,
                    )));
                }
            })
            .await;
        let primary_tool_manager = primary
            .handle
            .read(|agent| agent.tool_manager().clone())
            .await;
        let primary_hook_registry = primary
            .handle
            .read(|agent| agent.hook_registry().clone())
            .await;
        let pool_mut = Arc::get_mut(&mut pool).ok_or_else(|| {
            anyhow::anyhow!("workspace AgentPool escaped before host resources were installed")
        })?;
        pool_mut.shared.tool_manager = Some(primary_tool_manager);
        pool_mut.shared.hook_registry = Some(primary_hook_registry);
        *pool.primary_agent.write().await = Some(primary.handle.clone());
        *pool.primary_model_consumers.write().await = Some(primary.model_consumers.clone());
        crate::tasks::task_runtime::bind_task_execute_to_pool(
            &primary.handle,
            task_runtime_store,
            &pool,
        )
        .await;
        let (plugin_runtime, mcp_ownership) = match (lsp_runtime, mcp_config_snapshot.as_ref()) {
            (Some(lsp_runtime), Some(mcp_config)) => {
                let ownership = crate::mcp_config_runtime::McpNameOwnershipRegistry::new(
                    mcp_config.mcp_servers.keys().cloned(),
                );
                let runtime = crate::plugin_runtime::PluginRuntimeService::new_for_scope(
                    primary.handle.clone(),
                    lsp_runtime,
                    Arc::clone(&ownership),
                    plugin_target_scope,
                    Some(authority_plugin_generation.clone()),
                )
                .await?;
                runtime.bind_agent_pool(Arc::downgrade(&pool)).await?;
                (Some(runtime), ownership)
            }
            _ => (
                None,
                crate::mcp_config_runtime::McpNameOwnershipRegistry::new(Vec::<String>::new()),
            ),
        };
        crate::infra::fire_startup_hook(&primary.handle).await;

        if pool.config.enable_background_agent {
            match pool.create_agent("__background__").await {
                Ok(pooled) => {
                    pool.agents
                        .write()
                        .await
                        .insert("__background__".to_string(), pooled);
                }
                Err(error) => {
                    tracing::warn!(%error, "workspace AgentPool background agent unavailable");
                }
            }
        }
        pool.spawn_cleanup_monitor().await;
        Ok((pool, plugin_runtime, mcp_ownership))
    }

    #[cfg(test)]
    pub(crate) async fn for_model_mutation_test(
        primary: &AgentHandle,
        app_config: EkoConfig,
    ) -> Self {
        Self::new_for_test_with_config(primary, None, None, 8, false, app_config).await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent: AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
        store: Option<Arc<dyn echo_agent::memory::Store>>,
        max_agents: usize,
        enable_background_agent: bool,
    ) -> Self {
        let mut app_config = EkoConfig::default();
        app_config.model.provider = "test".to_string();
        app_config.model.name = "test-model".to_string();
        app_config.model.base_url = Some("http://127.0.0.1:11434/v1/chat/completions".to_string());
        Self::new_for_test_with_config(
            &agent,
            review_integration,
            store,
            max_agents,
            enable_background_agent,
            app_config,
        )
        .await
    }

    #[cfg(test)]
    async fn new_for_test_with_config(
        agent: &AgentHandle,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
        store: Option<Arc<dyn echo_agent::memory::Store>>,
        max_agents: usize,
        enable_background_agent: bool,
        app_config: EkoConfig,
    ) -> Self {
        let mut shared = SharedResources::extract_from(agent, review_integration).await;
        if let Some(store) = store {
            shared.store = Some(store);
        }
        Self {
            shared,
            agents: RwLock::new(HashMap::new()),
            primary_agent: RwLock::new(Some(agent.clone())),
            primary_model_consumers: RwLock::new(None),
            mcp_config_snapshot: RwLock::new(None),
            workspace_transitioning: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            admission: Arc::new(AgentPoolAdmission::default()),
            process_agent_execution: Arc::new(AgentExecutionGovernor::new(
                PROCESS_AGENT_EXECUTION_LIMIT,
            )),
            config: PoolConfig {
                max_agents,
                idle_timeout: Duration::from_secs(1800),
                enable_background_agent,
            },
            app_config: RwLock::new(app_config),
            working_dir: RwLock::new(None),
            permission_mode: RwLock::new(PermissionMode::Default),
            agent_generation: RwLock::new(AgentPluginGeneration::default()),
            cleanup_cancel: CancellationToken::new(),
            cleanup_handle: Mutex::new(None),
            conversation_store_override: RwLock::new(None),
            state_store_override: RwLock::new(None),
            tool_output_artifacts: RwLock::new(crate::infra::tool_output_artifact_config(None)),
            workspace_kind: RwLock::new(WorkspaceKind::General),
            instruction_projection: RwLock::new(None),
            tool_control: Arc::new(crate::tool_control::ToolControlService::default()),
            #[cfg(test)]
            llm_client_override: RwLock::new(None),
        }
    }

    #[cfg(test)]
    pub async fn set_llm_client_override_for_test(
        &self,
        client: Arc<dyn echo_agent::llm::LlmClient>,
    ) {
        *self.llm_client_override.write().await = Some(client);
    }

    /// Whether this key consumes one user-conversation capacity slot.
    fn is_conversation_agent(key: &str) -> bool {
        key != "__background__"
            && key != "__workspace_primary__"
            && !key.starts_with("__task__:")
            && !key.starts_with("__continuation__:")
    }

    /// Whether this key consumes one internal continuation capacity slot.
    fn is_continuation_agent(key: &str) -> bool {
        key.starts_with("__continuation__:")
    }

    /// Capacity is isolated by product ownership: foreground conversations
    /// cannot evict continuations, and continuations cannot evict conversations.
    fn shares_capacity_class(candidate: &str, requested: &str) -> bool {
        (Self::is_conversation_agent(requested) && Self::is_conversation_agent(candidate))
            || (Self::is_continuation_agent(requested) && Self::is_continuation_agent(candidate))
    }

    /// Acquire an agent for a given conversation ID.
    ///
    /// If an agent already exists for this ID, it is returned (with updated
    /// `last_used` timestamp). Otherwise, a new agent is created and added
    /// to the pool. Foreground conversations and internal continuations each
    /// have an independent capacity limit; task subagents and the background
    /// agent have separate product ownership.
    ///
    /// The write lock is held across the entire operation (including async
    /// agent creation) to prevent TOCTOU races between concurrent acquirers.
    pub async fn acquire(
        &self,
        conversation_id: &str,
    ) -> Result<AgentPoolExecutionLease, PoolError> {
        let mut agents = self.agents.write().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        if self.admission.is_retiring(conversation_id) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: conversation_id.to_string(),
            });
        }

        // Fast path: reuse existing agent
        if let Some(existing) = agents.get_mut(conversation_id) {
            existing.last_used = Instant::now();
            let permission_mode = *self.permission_mode.read().await;
            let _updated = existing.handle.try_write(|agent| {
                if agent.get_permission_mode() != permission_mode {
                    agent.set_permission_mode(permission_mode);
                }
            });
            let handle = existing.handle.clone();
            let lease = self.admission.issue_process_scoped(
                conversation_id,
                handle,
                &self.process_agent_execution,
            )?;
            drop(agents);
            return Ok(lease);
        }

        // Enforce the requested class limit and evict only from that class.
        // Dedicated background and task subagents own separate admission paths.
        let capacity_limited = Self::is_conversation_agent(conversation_id)
            || Self::is_continuation_agent(conversation_id);
        let active_count = agents
            .keys()
            .filter(|candidate| Self::shares_capacity_class(candidate, conversation_id))
            .count();
        if capacity_limited && active_count >= self.config.max_agents {
            // Find the oldest inactive agent in the requested capacity class.
            let mut candidates: Vec<(String, Instant)> = agents
                .iter()
                .filter(|(id, _)| {
                    Self::shares_capacity_class(id, conversation_id)
                        && !self.admission.is_active(id)
                })
                .map(|(id, agent)| (id.clone(), agent.last_used))
                .collect();
            candidates.sort_by_key(|(_, ts)| *ts);

            let mut evicted = false;
            for (candidate_id, _) in &candidates {
                // Check if the agent is currently executing by trying to
                // acquire its execution_mutex. If try_lock succeeds, the
                // agent is idle and safe to evict.
                let is_idle = agents
                    .get(candidate_id)
                    .and_then(|pa| pa.handle.inner().try_read().ok())
                    .map(|guard| guard.execution_mutex().try_lock().is_ok())
                    .unwrap_or(false);

                if is_idle {
                    agents.remove(candidate_id);
                    tracing::info!(
                        conv_id = %candidate_id,
                        "AgentPool: evicted idle agent to make room"
                    );
                    evicted = true;
                    break;
                }
            }

            if !evicted {
                return Err(PoolError::PoolFull {
                    max: self.config.max_agents,
                });
            }
        }

        // Create new agent (lock is held — prevents concurrent insert races)
        let pooled = self
            .create_agent(conversation_id)
            .await
            .map_err(|e| PoolError::AgentCreation(e.to_string()))?;
        let handle = pooled.handle.clone();

        let lease = self.admission.issue_process_scoped(
            conversation_id,
            handle,
            &self.process_agent_execution,
        )?;
        agents.insert(conversation_id.to_string(), pooled);

        tracing::info!(
            conv_id = %conversation_id,
            pool_size = agents.len(),
            "AgentPool: new agent created"
        );
        drop(agents);
        Ok(lease)
    }

    /// Lease an existing agent without creating a new one.
    ///
    /// Returns `None` if no agent is allocated for this conversation ID.
    pub async fn lease_existing(
        &self,
        conversation_id: &str,
    ) -> Result<Option<AgentPoolExecutionLease>, PoolError> {
        let agents = self.agents.write().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        if self.admission.is_retiring(conversation_id) {
            return Err(PoolError::ConversationRetirementPending {
                conversation_id: conversation_id.to_string(),
            });
        }
        let lease = agents
            .get(conversation_id)
            .map(|pooled| {
                self.admission.issue_process_scoped(
                    conversation_id,
                    pooled.handle.clone(),
                    &self.process_agent_execution,
                )
            })
            .transpose()?;
        drop(agents);
        Ok(lease)
    }

    /// Retire one cached agent using the exact execution receipt that owns it.
    /// The receipt and cache decision settle under the same pool lock, so reset
    /// cannot remove a generation still used by another accepted execution.
    pub async fn retire_execution(
        &self,
        conversation_id: &str,
        execution: AgentPoolExecutionLease,
    ) -> Result<bool, PoolError> {
        if !execution.owns(&self.admission, conversation_id) {
            return Err(PoolError::ExecutionLeaseMismatch);
        }
        Ok(self
            .release_supervised_execution(conversation_id, execution)
            .await)
    }

    /// Close admission for one conversation key, await every previously issued
    /// execution receipt, and remove that exact cached generation.
    ///
    /// New acquisitions fail with [`PoolError::ConversationRetirementPending`]
    /// until the operation settles. The admission guard is cancellation-safe:
    /// dropping the waiter reopens the key without claiming retirement, so a
    /// caller can retry rather than consuming a false terminal receipt.
    pub async fn retire_conversation_and_wait(
        &self,
        conversation_id: &str,
    ) -> Result<bool, PoolError> {
        let retirement = self.begin_conversation_retirement(conversation_id)?;
        self.complete_conversation_retirement(retirement).await
    }

    /// Close new admission for one exact conversation key before a caller
    /// settles its foreground owner and previously issued execution leases.
    pub fn begin_conversation_retirement(
        &self,
        conversation_id: &str,
    ) -> Result<AgentPoolConversationRetirement, PoolError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PoolError::ShuttingDown);
        }
        if self.workspace_transitioning.load(Ordering::Acquire) {
            return Err(PoolError::WorkspaceTransition);
        }
        let admission = self.admission.begin_retirement(conversation_id)?;
        Ok(AgentPoolConversationRetirement {
            key: conversation_id.to_string(),
            admission,
        })
    }

    /// Await old leases and remove the exact cached Agent protected by a
    /// receipt from [`Self::begin_conversation_retirement`].
    pub async fn complete_conversation_retirement(
        &self,
        retirement: AgentPoolConversationRetirement,
    ) -> Result<bool, PoolError> {
        let removed = self.drain_conversation_retirement(&retirement).await?;
        drop(retirement);
        Ok(removed)
    }

    /// Drain and remove one cached generation while retaining its closed
    /// admission receipt in the caller.
    ///
    /// Aggregate reset/delete owners use this form to keep the exact key closed
    /// through persisted runtime cleanup. Dropping `retirement` reopens the key
    /// only after their commit boundary.
    pub async fn drain_conversation_retirement(
        &self,
        retirement: &AgentPoolConversationRetirement,
    ) -> Result<bool, PoolError> {
        if !Arc::ptr_eq(&self.admission, &retirement.admission.admission) {
            return Err(PoolError::RetirementReceiptMismatch);
        }
        let conversation_id = retirement.key.clone();
        self.admission.wait_key_idle(&conversation_id).await;
        let mut agents = self.agents.write().await;
        let removed = agents.remove(&conversation_id).is_some();
        drop(agents);
        if removed {
            tracing::info!(
                conv_id = %conversation_id,
                "AgentPool: exact conversation generation retired after settlement"
            );
        }
        Ok(removed)
    }

    /// Release one exact supervised execution receipt. Dropping the receipt
    /// and deciding whether to remove the cached agent happen under the same
    /// agents lock used by acquire, so overlapping drivers for one key cannot
    /// remove each other's live agent.
    async fn release_supervised_execution(
        &self,
        conversation_id: &str,
        execution: AgentPoolExecutionLease,
    ) -> bool {
        let mut agents = self.agents.write().await;
        drop(execution);
        if self.admission.is_active(conversation_id) {
            return false;
        }
        let removed = agents.remove(conversation_id);
        if let Some(agent) = removed.as_ref() {
            tracing::info!(
                conv_id = %conversation_id,
                age_secs = agent.created_at.elapsed().as_secs(),
                "AgentPool: supervised agent released"
            );
        }
        removed.is_some()
    }

    #[cfg(test)]
    async fn background_agent(&self) -> Option<AgentHandle> {
        let agents = self.agents.read().await;
        agents.get("__background__").map(|pa| pa.handle.clone())
    }

    /// Update the pool's app config snapshot used for future agents.
    pub async fn update_app_config(&self, app_config: EkoConfig) {
        let _agents = self.agents.write().await;
        *self.app_config.write().await = app_config;
    }

    /// Publish the durable user MCP snapshot used by future conversation Agents
    /// and by workspace hosts opened after this generation commits.
    pub(crate) async fn update_mcp_config_snapshot(&self, snapshot: McpConfigFile) {
        *self.mcp_config_snapshot.write().await = Some(snapshot);
    }

    #[cfg(test)]
    pub(crate) async fn mcp_config_snapshot_for_test(&self) -> Option<McpConfigFile> {
        self.mcp_config_snapshot.read().await.clone()
    }

    /// Number of exact execution receipts currently retaining this pool.
    pub(crate) fn active_execution_count(&self) -> usize {
        self.admission
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .total
    }

    /// Admit every existing and future pool consumer before persistence.
    pub(crate) async fn prepare_model_publication(
        &self,
        app_config: EkoConfig,
        runtime: ModelRuntimeConfig,
        prepared: infra::PreparedRuntimeLlm,
    ) -> Result<PreparedAgentPoolModelPublication<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let token_limit = infra::effective_token_limit(&app_config, Some(&runtime));
        let primary_consumers = self.primary_model_consumers.read().await.clone();
        let primary_agent = self.primary_agent.read().await.clone();
        let mut publications = Vec::with_capacity(
            agents
                .len()
                .saturating_add(usize::from(primary_consumers.is_some())),
        );
        if let (Some(primary), Some(consumers)) = (primary_agent, primary_consumers) {
            publications.push(
                infra::prepare_agent_model_publication(
                    &primary,
                    consumers,
                    &runtime,
                    &prepared,
                    token_limit,
                )
                .await?,
            );
        }
        let mut pooled_agents: Vec<(&String, &PooledAgent)> = agents.iter().collect();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (_, pooled) in pooled_agents {
            publications.push(
                infra::prepare_agent_model_publication(
                    &pooled.handle,
                    pooled.model_consumers.clone(),
                    &runtime,
                    &prepared,
                    token_limit,
                )
                .await?,
            );
        }
        Ok(PreparedAgentPoolModelPublication {
            pool: self,
            _transition: transition,
            _agents: agents,
            publications,
            app_config,
            runtime,
        })
    }

    /// Admit every pooled agent before removing the final active model.
    pub(crate) async fn prepare_model_deactivation(
        &self,
        app_config: EkoConfig,
    ) -> Result<PreparedAgentPoolModelDeactivation<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let primary_consumers = self.primary_model_consumers.read().await.clone();
        let primary_agent = self.primary_agent.read().await.clone();
        let mut publications = Vec::with_capacity(
            agents
                .len()
                .saturating_add(usize::from(primary_consumers.is_some())),
        );
        if let (Some(primary), Some(consumers)) = (primary_agent, primary_consumers) {
            publications.push(infra::prepare_agent_model_deactivation(&primary, consumers).await);
        }
        let mut pooled_agents: Vec<(&String, &PooledAgent)> = agents.iter().collect();
        pooled_agents.sort_by(|left, right| left.0.cmp(right.0));
        for (_, pooled) in pooled_agents {
            publications.push(
                infra::prepare_agent_model_deactivation(
                    &pooled.handle,
                    pooled.model_consumers.clone(),
                )
                .await,
            );
        }
        Ok(PreparedAgentPoolModelDeactivation {
            pool: self,
            _transition: transition,
            _agents: agents,
            publications,
            app_config,
        })
    }

    /// Publish the current permission mode without waiting for an active turn.
    ///
    /// The shared permission service is the authority used by tool execution,
    /// so it is updated first. Idle agents mirror the mode immediately; a busy
    /// agent refreshes its informational config on its next pool acquisition.
    pub async fn apply_permission_mode(&self, mode: PermissionMode) {
        *self.permission_mode.write().await = mode;

        if let Some(service) = &self.shared.permission_service {
            service.set_mode(mode).await;
            service.clear_cache();
        }

        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();

        let mut updated_agents = 0usize;
        for handle in agents {
            if handle
                .try_write(|agent| {
                    if agent.get_permission_mode() != mode {
                        agent.set_permission_mode(mode);
                    }
                })
                .is_some()
            {
                updated_agents = updated_agents.saturating_add(1);
            }
        }

        let pooled_agents = self.agents.read().await.len();
        let deferred_agents = pooled_agents.saturating_sub(updated_agents);
        tracing::info!(
            mode = %mode,
            pooled_agents,
            updated_agents,
            deferred_agents,
            "AgentPool: permission mode published"
        );
    }

    /// Publish a product system prompt to the primary, every existing pooled
    /// Agent, and the config template used for future pool admissions.
    pub async fn apply_system_prompt(&self, system_prompt: String) {
        self.app_config.write().await.agent.system_prompt = system_prompt.clone();
        let mut handles = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect::<Vec<_>>();
        if let Some(primary) = self.primary_agent.read().await.clone()
            && !handles
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate.inner(), primary.inner()))
        {
            handles.push(primary);
        }
        for handle in handles {
            let system_prompt = system_prompt.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_system_prompt(system_prompt).await;
                    })
                })
                .await;
        }
    }

    /// Project the current EKO tool-control generation into the primary and
    /// every cached Agent. Runs already holding a snapshot remain unchanged;
    /// the next run observes the new generation.
    pub(crate) async fn publish_tool_control_generation(
        &self,
    ) -> Result<(), crate::tool_control::ToolControlError> {
        let agents = self.agents.write().await;
        // Read the authority only after publication owns the pool generation.
        // Concurrent older publishers therefore observe the newest revision
        // instead of overwriting a later mutation with a stale snapshot.
        let snapshot = self.tool_control.snapshot()?;
        let disabled = crate::tool_control::disabled_option(&snapshot);
        let mut handles = agents
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect::<Vec<_>>();
        let mut model_consumers = agents
            .values()
            .map(|pooled| pooled.model_consumers.clone())
            .collect::<Vec<_>>();
        if let Some(primary) = self.primary_agent.read().await.clone()
            && !handles
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate.inner(), primary.inner()))
        {
            handles.push(primary);
        }
        if let Some(primary_consumers) = self.primary_model_consumers.read().await.clone() {
            model_consumers.push(primary_consumers);
        }
        for handle in handles {
            let disabled = disabled.clone();
            handle
                .read(|agent| agent.set_disabled_tools(disabled))
                .await;
        }
        for consumers in model_consumers {
            consumers.apply_disabled_tools(disabled.clone()).await;
        }
        tracing::info!(
            revision = snapshot.revision,
            disabled_tools = snapshot.disabled_tools.len(),
            pooled_agents = agents.len(),
            "AgentPool: tool-control generation published"
        );
        Ok(())
    }

    pub(crate) fn tool_control(&self) -> Arc<crate::tool_control::ToolControlService> {
        crate::tool_control::shared(&self.tool_control)
    }

    /// Propagate `working_dir` to all pooled agents.
    ///
    /// Called after a workspace switch so that background tasks and
    /// multi-conversation agents operate in the new workspace root.
    pub async fn apply_working_dir(&self, path: Option<std::path::PathBuf>) {
        *self.working_dir.write().await = path.clone();
        let artifact_config = crate::infra::tool_output_artifact_config(path.as_deref());
        *self.tool_output_artifacts.write().await = artifact_config.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pa| pa.handle.clone())
            .collect();
        for handle in agents {
            let path = path.clone();
            let artifact_config = artifact_config.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        agent.set_working_dir(path.clone());
                        agent.set_tool_output_artifacts(Some(artifact_config));
                        crate::infra::refresh_dynamic_context(agent, path.as_deref()).await;
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(?path, pooled_agents, "AgentPool: working_dir applied");
    }

    /// Apply one workspace prompt/skill profile to existing and future agents.
    pub async fn apply_workspace_routing(&self, kind: WorkspaceKind) {
        *self.workspace_kind.write().await = kind.clone();
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let kind = kind.clone();
            handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::workspace_routing::configure_agent_for_workspace(agent, &kind).await;
                    })
                })
                .await;
        }
        let pooled_agents = self.agents.read().await.len();
        tracing::info!(?kind, pooled_agents, "AgentPool: workspace routing applied");
    }

    /// Rebind existing and future pooled agents to the active conversation store.
    pub async fn apply_conversation_store(
        &self,
        store: Arc<dyn echo_agent::memory::ConversationStore>,
    ) {
        *self.conversation_store_override.write().await = Some(store.clone());
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let store = store.clone();
            handle
                .write(|agent| agent.set_conversation_store(store))
                .await;
        }
    }

    /// Rebind existing and future pooled agents to the active checkpoint store.
    pub async fn apply_state_store(&self, store: Arc<dyn echo_agent::state::RuntimeStateStore>) {
        *self.state_store_override.write().await = Some(store.clone());
        let agents: Vec<AgentHandle> = self
            .agents
            .read()
            .await
            .values()
            .map(|pooled| pooled.handle.clone())
            .collect();
        for handle in agents {
            let store = store.clone();
            handle.write(|agent| agent.set_state_store(store)).await;
        }
    }

    /// Current number of agents in the pool (including background).
    pub async fn pool_size(&self) -> usize {
        self.agents.read().await.len()
    }

    /// Return the primary Agent for this pool generation.
    pub(crate) async fn primary_agent(&self) -> anyhow::Result<AgentHandle> {
        self.primary_agent
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("AgentPool primary Agent is unavailable"))
    }

    /// Maximum number of non-background agents this pool may create.
    pub fn max_agents(&self) -> usize {
        self.config.max_agents
    }

    /// Conservative default parallelism for background tasks backed by this pool.
    ///
    /// Keep one slot notionally reserved for foreground/multi-session work and
    /// cap the initial task fan-out to avoid overwhelming tools, LLM calls, and
    /// workspace writes.
    pub fn background_task_concurrency(&self) -> usize {
        self.config
            .max_agents
            .saturating_sub(self.foreground_agent_reserve())
            .clamp(1, 4)
    }

    /// Number of pool slots reserved for foreground/multi-session work.
    pub fn foreground_agent_reserve(&self) -> usize {
        1
    }

    /// Conservative default fan-out for a single composite parallel task.
    pub fn composite_parallelism(&self) -> usize {
        self.background_task_concurrency().clamp(1, 3)
    }

    /// Start a periodic cleanup task that evicts idle agents.
    ///
    /// The cleanup runs every 5 minutes, removing agents that have been
    /// idle longer than `config.idle_timeout`. The `__background__` agent
    /// is never evicted. Call `shutdown()` to stop the monitor.
    pub async fn spawn_cleanup_monitor(self: &Arc<Self>) {
        let mut cleanup_handle = self
            .cleanup_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.shutting_down.load(Ordering::Acquire) || cleanup_handle.is_some() {
            return;
        }

        let pool = Arc::downgrade(self);
        let cancel = self.cleanup_cancel.clone();
        *cleanup_handle = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 min
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("AgentPool: cleanup monitor stopped");
                        return;
                    }
                    _ = interval.tick() => {}
                }

                let Some(pool) = pool.upgrade() else {
                    return;
                };
                let idle_timeout = pool.config.idle_timeout;
                let mut agents = pool.agents.write().await;
                // First pass: find agents that exceed idle timeout (except background).
                let timed_out: Vec<String> = agents
                    .iter()
                    .filter(|(id, agent)| {
                        id.as_str() != "__background__" && agent.last_used.elapsed() > idle_timeout
                    })
                    .map(|(id, _)| id.clone())
                    .collect();

                // Second pass: only evict agents that are NOT currently executing.
                // Uses the same try_lock(execution_mutex) check as the acquire() path
                // so long-running tasks (e.g. TaskRuntime DAG subagents) aren't killed.
                let to_remove: Vec<String> = timed_out
                    .into_iter()
                    .filter(|id| {
                        if pool.admission.is_active(id) {
                            return false;
                        }
                        let is_idle = agents
                            .get(id)
                            .and_then(|pa| pa.handle.inner().try_read().ok())
                            .map(|guard| guard.execution_mutex().try_lock().is_ok())
                            .unwrap_or(false);
                        if !is_idle {
                            tracing::debug!(
                                conv_id = %id,
                                "AgentPool: skipping eviction — agent is executing"
                            );
                        }
                        is_idle
                    })
                    .collect();

                for id in to_remove {
                    if let Some(pa) = agents.remove(&id) {
                        tracing::info!(
                            conv_id = %id,
                            idle_secs = pa.last_used.elapsed().as_secs(),
                            "AgentPool: evicted idle agent"
                        );
                    }
                }
            }
        }));
    }

    /// Stop the cleanup monitor and release all pool agents.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.admission.close();
        self.cleanup_cancel.cancel();
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let agents = self.agents.write().await;
        self.begin_shutdown();
        drop(agents);
        let cleanup_handle = self
            .cleanup_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let monitor_error = if let Some(cleanup_handle) = cleanup_handle {
            cleanup_handle
                .await
                .err()
                .map(|error| format!("AgentPool cleanup monitor failed: {error}"))
        } else {
            None
        };
        self.admission.wait_until_idle().await;
        let mut agents = self.agents.write().await;
        let count = agents.len();
        agents.clear();
        tracing::info!(agents_cleared = count, "AgentPool: shutdown complete");
        match monitor_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Verify that cached conversations can be retired without mutating them.
    pub(crate) async fn preflight_workspace_transition(
        &self,
    ) -> anyhow::Result<AgentPoolWorkspaceTransition<'_>> {
        let agents = self.agents.write().await;
        if self.workspace_transitioning.swap(true, Ordering::AcqRel) {
            anyhow::bail!("Agent pool workspace transition is already in progress");
        }
        drop(agents);
        let transition = AgentPoolWorkspaceTransition {
            pool: self,
            committed: false,
        };

        // An issued handle is execution ownership even before its framework
        // execution mutex is locked. Closing admission under the agents write
        // lock above makes the counter stable in the downward direction; wait
        // for every existing receipt to reach its real settlement.
        self.admission.wait_until_idle().await;

        let agents = self.agents.write().await;
        for (conversation_id, pooled) in agents.iter() {
            let Ok(agent) = pooled.handle.inner().try_read() else {
                anyhow::bail!(
                    "Cannot change workspace while pooled conversation {conversation_id} is busy"
                );
            };
            if agent.execution_mutex().try_lock().is_err() {
                anyhow::bail!(
                    "Cannot change workspace while pooled conversation {conversation_id} is executing"
                );
            }
        }
        drop(agents);
        Ok(transition)
    }

    /// Reuse the pool's existing generation admission boundary for an active
    /// model publication. Dropping the returned guard reopens admission
    /// without clearing cached agents.
    pub(crate) async fn preflight_model_mutation(
        &self,
    ) -> anyhow::Result<AgentPoolWorkspaceTransition<'_>> {
        self.preflight_workspace_transition().await
    }

    pub(crate) async fn begin_plugin_publication(
        &self,
    ) -> Result<PreparedAgentPoolPluginPublication<'_>, String> {
        let transition = self
            .preflight_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        let previous = self.agent_generation.read().await.clone();
        Ok(PreparedAgentPoolPluginPublication {
            pool: self,
            _transition: transition,
            agents,
            previous,
            candidate: None,
            application_skill_repair: None,
        })
    }

    /// Close pool execution/creation admission and retain the agents write
    /// guard until one instruction snapshot is committed.
    pub(crate) async fn begin_instruction_publication(
        &self,
    ) -> Result<PreparedAgentPoolInstructionPublication<'_>, String> {
        let transition = self
            .preflight_model_mutation()
            .await
            .map_err(|error| error.to_string())?;
        let agents = self.agents.write().await;
        Ok(PreparedAgentPoolInstructionPublication {
            pool: self,
            _transition: transition,
            agents,
            candidate: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn transition_admission_closed_for_test(&self) -> bool {
        self.workspace_transitioning.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) async fn plugin_generation_revision_for_test(&self) -> u64 {
        self.agent_generation.read().await.revision
    }

    #[cfg(test)]
    fn conversation_retiring_for_test(&self, conversation_id: &str) -> bool {
        self.admission.is_retiring(conversation_id)
    }

    #[cfg(test)]
    pub(crate) async fn instruction_projection_revision_for_test(&self) -> Option<String> {
        self.instruction_projection
            .read()
            .await
            .as_ref()
            .map(|snapshot| snapshot.revision().to_string())
    }

    /// Internal: create a new agent with shared resources injected.
    ///
    /// `conversation_id` is used both as the pool key and as the
    /// `AgentConfig.conversation_id` — the latter is required by
    /// `save_runtime_checkpoint` and `ConversationStore` projection. We also
    /// keep it as `session_id` so existing `session_id`-keyed paths (e.g.
    /// background tasks) continue to work.
    async fn create_agent(&self, conversation_id: &str) -> anyhow::Result<PooledAgent> {
        // 1. Create a base agent — pass conversation_id + state_store at build
        //    time so the agent boots with everything the framework's checkpoint
        //    helpers need. (Previously the pool called `set_state_store` here,
        //    but `self.shared.state_store` was always None because the primary
        //    agent never had a store wired in — `extract_from` would only ever
        //    see None and the runtime checkpoint loop silently no-op'd.)
        let app_config = self.app_config.read().await.clone();
        let working_dir = self.working_dir.read().await.clone();
        let state_store = self
            .state_store_override
            .read()
            .await
            .clone()
            .or_else(|| self.shared.state_store.clone());
        let params = infra::AgentCreateParams {
            model: None, // will use app_config default
            system_prompt: None,
            project: None,
            session_id: Some(conversation_id.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            react_checkpoint_interval: None,
            state_store,
            memory_context_suffix: None,
            working_dir,
            // Thread the TaskRuntimeStore so pooled agents get task-management
            // tools registered (matches the primary agent wiring).
            // Formal Subagents created by TaskRuntime still have task_execute
            // disabled by invocation policy; pool conversation agents may drive it.
            task_runtime_store: self.shared.task_runtime_store.clone(),
            browser_runtime: self.shared.browser_runtime.clone(),
            command_cell_runtime: self.shared.command_cell_runtime.clone(),
            product_data_io: self.shared.product_data_io.clone(),
            execution_scope: self.shared.execution_scope.clone(),
        };
        let created = infra::create_agent_with_diagnostics(&params, &app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut agent = created.agent;
        let model_consumers = created.model_consumers;
        #[cfg(test)]
        if let Some(client) = self.llm_client_override.read().await.clone() {
            agent.set_llm_client(client);
        }
        if self.shared.tool_manager.is_none()
            && let Some(snapshot) = self.mcp_config_snapshot.read().await.clone()
            && let Err(error) = agent.load_mcp_config(snapshot).await
        {
            tracing::warn!(conversation_id, %error, "workspace pooled agent MCP connection failed");
        }
        agent.set_tool_output_artifacts(Some(self.tool_output_artifacts.read().await.clone()));

        // 2. Inject non-model shared resources. The model transport produced by
        // create_agent_with_diagnostics is authoritative and must not be
        // overwritten by the primary agent's startup client.
        if let Some(ref tm) = self.shared.tool_manager {
            agent.set_tool_manager(tm.clone());
        }
        if let Some(ref hr) = self.shared.hook_registry {
            agent.set_hook_registry(hr.clone());
        }
        if let Some(ref sm) = self.shared.sandbox_manager {
            agent.set_sandbox_manager(sm.clone());
        }
        if let Some(ref tt) = self.shared.token_tracker {
            agent.set_token_tracker(tt.clone());
        }
        // state_store is now injected via the builder above; nothing to set here.
        if let Some(ref rs) = self.shared.run_store {
            agent.set_run_store(rs.clone());
        }
        if let Some(ref tep) = self.shared.tool_execution_pipeline {
            agent.set_tool_execution_pipeline(tep.clone());
        }
        let conversation_store = self
            .conversation_store_override
            .read()
            .await
            .clone()
            .or_else(|| self.shared.conversation_store.clone());
        if let Some(ref cs) = conversation_store {
            agent.set_conversation_store(cs.clone());
        }
        if let Some(ref st) = self.shared.store {
            agent.install_store(st.clone()).await;
        }
        if let Some(ref review_integration) = self.shared.review_integration {
            let memory_generation = review_integration
                .lease_generation()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let layer_manager = memory_generation.layer_manager()?;
            agent.install_memory_layer_manager(layer_manager);
            agent.set_memory_trigger_sink(Some(review_integration.clone()));
            agent.set_skill_load_policy(Some(review_integration.clone()));
            agent.set_skill_curator(Some(review_integration.curator()));
            let mut projector = crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            )
            .with_hot_memory_source(review_integration.hot_memory_projection_source());
            if let (Some(command_cells), Some(execution_scope)) = (
                self.shared.command_cell_runtime.clone(),
                self.shared.execution_scope.clone(),
            ) {
                projector = projector.with_awaiter_results(command_cells, execution_scope);
            }
            agent.set_pre_model_context_projector(Some(Arc::new(projector)));
        }
        if let Some(ref ps) = self.shared.permission_service {
            agent.set_permission_service(ps.clone());
        }
        let permission_mode = *self.permission_mode.read().await;
        agent.set_permission_mode(permission_mode);
        let tool_control = self
            .tool_control
            .snapshot()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let disabled_tools = crate::tool_control::disabled_option(&tool_control);
        agent.set_disabled_tools(disabled_tools.clone());
        model_consumers.apply_disabled_tools(disabled_tools).await;

        // 3. Install the exact plugin generation committed by PluginRuntime.
        let agent_generation = self.agent_generation.read().await.clone();
        for desc in &agent_generation.skill_descriptors {
            agent.skill_registry_mut().register_descriptor(desc.clone());
        }
        register_plugin_agents(&mut agent, &agent_generation.plugin_agents)
            .await
            .map_err(anyhow::Error::msg)?;
        agent
            .replace_system_context_projection(
                crate::plugin_runtime::OUTPUT_STYLE_PROJECTION,
                agent_generation.output_style.clone(),
            )
            .await;
        crate::runtime::configure_intent_router(&mut agent);

        if let Some(snapshot) = self.instruction_projection.read().await.clone() {
            crate::unified_memory::apply_instruction_projection_snapshot(&mut agent, &snapshot)
                .await;
        }

        let workspace_kind = self.workspace_kind.read().await.clone();
        crate::workspace_routing::configure_agent_for_workspace(&mut agent, &workspace_kind).await;

        // 3b. Auto-compression — pooled agents must not rely solely on the
        // 200-msg hard cap. Mirror the primary agent wiring (runtime.rs) so
        // long GUI multi-session runs are protected by the configured strategy.
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::debug!(conversation_id, "pooled agent auto-compression configured");
        }

        // 4. Wrap in AgentHandle
        let handle = AgentHandle::new(agent);
        if let (Some(runtime), Some(scope)) = (
            self.shared.command_cell_runtime.as_ref(),
            self.shared.execution_scope.as_ref(),
        ) {
            runtime.bind_agent(scope.workspace_id(), conversation_id, &handle);
        }

        // Workspace pools own their ToolManagers, so complete the same task
        // tool suite used by the bootstrap primary. The execute tool captures
        // this exact Agent and host store; no process-global pool lookup is
        // needed or allowed here.
        if self.shared.tool_manager.is_none()
            && let Some(store) = self.shared.task_runtime_store.as_ref()
        {
            crate::tasks::task_runtime::register_task_tools_on_agent(&handle, store.clone()).await;
        }

        // TaskRuntime's formal Subagents are created by the framework registry,
        // not by this conversation pool. Their invocation policy continues to
        // disable task_execute so nested dispatch cannot recurse into L2.

        // 5. Configure HITL for this agent.
        // Use an empty HitlDispatcher (no REPL provider!) so that if the caller
        // hasn't yet called set_human_loop_provider, approval requests
        // auto-reject instead of blocking on terminal stdin (which hangs GUI).
        // The real provider (Tauri/TUI/REPL) is injected per-use via
        // set_human_loop_provider, which now does an in-place replace.
        {
            let dispatcher = Arc::new(crate::hitl::HitlDispatcher::new());
            handle
                .write_async(|a| {
                    let d = dispatcher.clone();
                    Box::pin(async move {
                        a.set_human_loop_provider(d);
                    })
                })
                .await;
        }

        Ok(PooledAgent::new(
            handle,
            model_consumers,
            conversation_id.to_string(),
        ))
    }
}
