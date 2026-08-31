impl PluginRuntimeService {
    pub(crate) async fn new(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_for_scope(agent_handle, lsp, mcp_ownership, "global".to_string(), None).await
    }

    pub(crate) async fn new_for_scope(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
        target_scope: String,
        authority_generation: Option<AgentPluginGeneration>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_with_source(
            agent_handle,
            lsp,
            mcp_ownership,
            RegistrySource::Default,
            target_scope,
            authority_generation,
        )
        .await
    }

    async fn new_with_source(
        agent_handle: AgentHandle,
        lsp: PluginLspRuntime,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
        registry_source: RegistrySource,
        target_scope: String,
        authority_generation: Option<AgentPluginGeneration>,
    ) -> anyhow::Result<Arc<Self>> {
        let framework_generation = authority_generation
            .as_ref()
            .and_then(AgentPluginGeneration::framework_generation);
        let authority_revision = authority_generation
            .as_ref()
            .map_or(0, AgentPluginGeneration::revision);
        let preferences_file = match &registry_source {
            RegistrySource::Default => crate::data_root::user_data_dir()
                .join("plugins")
                .join("preferences.json"),
            #[cfg(test)]
            RegistrySource::Custom { state_file, .. } => {
                state_file.with_file_name("preferences.json")
            }
        };
        let preferences = load_preferences(&preferences_file);
        let registry = match &registry_source {
            RegistrySource::Default => PluginRegistry::new(crate::data_root::user_data_dir(), None),
            #[cfg(test)]
            RegistrySource::Custom {
                state_file,
                data_dir,
                ..
            } => PluginRegistry::with_paths(state_file.clone(), data_dir.clone(), None),
        };
        let service = Arc::new(Self {
            agent_handle,
            lsp,
            scheduler: RwLock::new(None),
            mcp_ownership,
            integrator: PluginIntegrator::default(),
            target_scope,
            registry_source,
            preferences_file,
            state: Mutex::new(PluginRuntimeState {
                registry,
                framework_components: HashMap::new(),
                framework_generation,
                framework_receipt: None,
                mcp_ownership: HashMap::new(),
                prepared: PreparedApplicationComponents::default(),
                lifecycle: PluginLifecycleManager::new(),
                cleanup_quarantine: Vec::new(),
                active_theme: preferences.active_theme,
                active_output_style: preferences.active_output_style,
                generation: authority_revision,
                shut_down: false,
            }),
            agent_pool: RwLock::new(None),
            mutation_supervisor: Mutex::new(PluginMutationSupervisor::default()),
        });
        if let Some(authority_generation) = authority_generation {
            // A cold workspace primary is created from the global pool's exact
            // committed projection. Retire it before applying the workspace's
            // full User + Project + Local prepared set so global project-only
            // descriptors cannot survive in the new target.
            service
                .agent_handle
                .write_async(|agent| {
                    Box::pin(async move {
                        crate::agent_pool::remove_agent_plugin_generation(
                            agent,
                            &authority_generation,
                        )
                        .await;
                    })
                })
                .await;
        }
        service.reload().await?;
        Ok(service)
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_for_test_with_ownership(
            agent_handle,
            project_root,
            state_file,
            data_dir,
            McpNameOwnershipRegistry::new(Vec::<String>::new()),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test_with_ownership(
        agent_handle: AgentHandle,
        project_root: PathBuf,
        state_file: PathBuf,
        data_dir: PathBuf,
        mcp_ownership: Arc<McpNameOwnershipRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        let manager = Arc::new(RwLock::new(LspManager::new()));
        let target_scope = format!("test:{}", project_root.display());
        let lsp = PluginLspRuntime::new(manager, LspConfig::default(), project_root);
        Self::new_with_source(
            agent_handle,
            lsp,
            mcp_ownership,
            RegistrySource::Custom {
                state_file,
                data_dir,
                scopes: vec![PluginScope::Project, PluginScope::Local],
            },
            target_scope,
            None,
        )
        .await
    }

    async fn run_owned_mutation<T, F, Fut>(self: &Arc<Self>, operation: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let mut supervisor = self.mutation_supervisor.lock().await;
        if !supervisor.accepting {
            return Err(anyhow::anyhow!("plugin runtime is shutting down"));
        }
        let sequence_permit = Arc::clone(&supervisor.sequence)
            .acquire_owned()
            .await
            .map_err(|error| anyhow::anyhow!("plugin mutation sequence is closed: {error}"))?;
        while let Some(result) = supervisor.settlements.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(%error, "completed plugin mutation owner failed");
            }
        }
        let service = Arc::clone(self);
        supervisor.settlements.spawn(async move {
            let _sequence_permit = sequence_permit;
            let result = match tokio::spawn(operation(service)).await {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(
                    "plugin mutation task failed before settlement: {error}"
                )),
            };
            let _ = result_sender.send(result);
        });
        drop(supervisor);
        result_receiver
            .await
            .map_err(|_| anyhow::anyhow!("plugin mutation settlement task stopped unexpectedly"))?
    }

    /// Bind the process pool to the currently committed plugin generation.
    pub async fn bind_agent_pool(self: &Arc<Self>, pool: Weak<AgentPool>) -> anyhow::Result<()> {
        self.run_owned_mutation(
            move |service| async move { service.bind_agent_pool_inner(pool).await },
        )
        .await
    }

    async fn bind_agent_pool_inner(&self, pool: Weak<AgentPool>) -> anyhow::Result<()> {
        let pool_owner = pool
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("AgentPool was released before plugin binding"))?;
        if let Some(existing) = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
            && !Arc::ptr_eq(&existing, &pool_owner)
        {
            return Err(anyhow::anyhow!(
                "plugin runtime is already bound to another live AgentPool"
            ));
        }
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let generation = self
            .capture_agent_generation(
                state.generation,
                &state.prepared,
                state.active_output_style.as_deref(),
            )
            .await
            .with_framework_generation(state.framework_generation.clone());
        let mut publication = pool_owner
            .begin_plugin_publication()
            .await
            .map_err(anyhow::Error::msg)?;
        publication
            .prepare(generation)
            .await
            .map_err(anyhow::Error::msg)?;
        publication.commit().await.map_err(anyhow::Error::msg)?;
        *self.agent_pool.write().await = Some(pool);
        Ok(())
    }

    async fn capture_agent_generation(
        &self,
        revision: u64,
        prepared: &PreparedApplicationComponents,
        active_output_style: Option<&str>,
    ) -> AgentPluginGeneration {
        let descriptors = self
            .agent_handle
            .read(|agent| agent.skill_descriptors())
            .await;
        let output_style = active_output_style_instructions_for(active_output_style, prepared);
        AgentPluginGeneration::new(revision, descriptors, prepared.agents.clone(), output_style)
    }

    /// Atomically load one EKO-owned skill into the primary and pool catalog.
    /// The registry edit happens inside the same mutation owner as plugin
    /// reload/rebind, preventing a plugin generation from overwriting it.
    pub(crate) async fn enable_application_skill(
        self: &Arc<Self>,
        name: String,
        load_root: PathBuf,
        source: String,
    ) -> anyhow::Result<Vec<String>> {
        self.run_owned_mutation(move |service| async move {
            service
                .enable_application_skill_inner(&name, load_root, &source)
                .await
        })
        .await
    }

    async fn enable_application_skill_inner(
        &self,
        name: &str,
        load_root: PathBuf,
        source: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut state = self.state.lock().await;
        if state.shut_down {
            anyhow::bail!("plugin runtime is shut down");
        }
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let primary_had_skill = primary.skill_descriptors().iter().any(|descriptor| {
            descriptor.name == name && descriptor.source.as_deref() == Some(source)
        });
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if primary_had_skill && pool.is_none() {
            return Ok(vec![name.to_string()]);
        }
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let loaded = if primary_had_skill {
            vec![name.to_string()]
        } else {
            load_exact_application_skill(&mut primary, name, load_root, source).await?
        };
        if !loaded.iter().any(|loaded_name| loaded_name == name) {
            anyhow::bail!("Skill '{name}' was not discovered");
        }
        crate::runtime::configure_intent_router(&mut primary);
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            active_output_style_instructions(&state),
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication
                .prepare_application_skill(generation, name, source)
                .await
        {
            if !primary_had_skill {
                primary.unregister_skills_by_source(source).await;
                crate::runtime::configure_intent_router(&mut primary);
            }
            return Err(anyhow::Error::msg(error));
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            if !primary_had_skill {
                primary.unregister_skills_by_source(source).await;
                crate::runtime::configure_intent_router(&mut primary);
            }
            let rollback = publication.rollback().await.err();
            return Err(match rollback {
                Some(rollback) => anyhow::anyhow!(
                    "Skill pool commit failed: {error}; rollback failed: {rollback}"
                ),
                None => anyhow::Error::msg(error),
            });
        }
        state.generation = revision;
        Ok(loaded)
    }

    /// Atomically remove one EKO-owned skill from primary and pool catalogs.
    pub(crate) async fn disable_application_skill(
        self: &Arc<Self>,
        name: String,
        load_root: PathBuf,
        source: String,
    ) -> anyhow::Result<Vec<String>> {
        self.run_owned_mutation(move |service| async move {
            service
                .disable_application_skill_inner(&name, load_root, &source)
                .await
        })
        .await
    }

    async fn disable_application_skill_inner(
        &self,
        name: &str,
        load_root: PathBuf,
        source: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut state = self.state.lock().await;
        if state.shut_down {
            anyhow::bail!("plugin runtime is shut down");
        }
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let primary_had_skill = primary.skill_descriptors().iter().any(|descriptor| {
            descriptor.name == name && descriptor.source.as_deref() == Some(source)
        });
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if !primary_had_skill && pool.is_none() {
            return Ok(Vec::new());
        }
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let removed = if primary_had_skill {
            primary.unregister_skills_by_source(source).await
        } else {
            Vec::new()
        };
        crate::runtime::configure_intent_router(&mut primary);
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            active_output_style_instructions(&state),
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication
                .prepare_application_skill(generation, name, source)
                .await
        {
            let restore_error = if primary_had_skill {
                let restore =
                    load_exact_application_skill(&mut primary, name, load_root.clone(), source)
                        .await
                        .err();
                crate::runtime::configure_intent_router(&mut primary);
                restore
            } else {
                None
            };
            return Err(match restore_error {
                Some(restore) => anyhow::anyhow!(
                    "Skill pool preparation failed: {error}; primary restore failed: {restore}"
                ),
                None => anyhow::Error::msg(error),
            });
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            let restore = if primary_had_skill {
                let restore =
                    load_exact_application_skill(&mut primary, name, load_root, source).await;
                crate::runtime::configure_intent_router(&mut primary);
                Some(restore)
            } else {
                None
            };
            let rollback = publication.rollback().await.err();
            let mut errors = vec![format!("Skill pool commit failed: {error}")];
            if let Some(Err(error)) = restore {
                errors.push(format!("primary restore failed: {error}"));
            }
            errors.extend(rollback.map(|error| format!("pool rollback failed: {error}")));
            return Err(anyhow::anyhow!(errors.join("; ")));
        }
        state.generation = revision;
        Ok(removed)
    }

    async fn drain_owned_mutations(&self) -> anyhow::Result<()> {
        let mut settlements = {
            let mut supervisor = self.mutation_supervisor.lock().await;
            supervisor.accepting = false;
            std::mem::take(&mut supervisor.settlements)
        };
        let mut errors = Vec::new();
        while let Some(settlement) = settlements.join_next().await {
            if let Err(error) = settlement {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "plugin mutation settlement failed: {}",
                errors.join("; ")
            ))
        }
    }

    pub(crate) async fn reload(self: &Arc<Self>) -> anyhow::Result<ReloadSummary> {
        self.run_owned_mutation(|service| async move { service.reload_inner().await })
            .await
    }

    async fn reload_inner(&self) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        self.apply_candidate(&mut state, candidate, &binding).await
    }

    /// Validate a target generation without replacing live plugin resources.
    pub async fn preflight_workspace(&self, project_root: PathBuf) -> anyhow::Result<()> {
        let binding = PluginLspBinding {
            base_config: PluginLspRuntime::config_for_workspace(&project_root),
            project_root,
        };
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let framework_generation =
            require_applicable_generation(self.integrator.prepare(&mut candidate).await)?;
        let prepared = prepare_application_components(&framework_generation, &self.target_scope)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let declarations = plugin_mcp_declarations(&framework_generation)?;
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        self.validate_agent_collisions(&state, &prepared).await?;
        let ownership_guard = self.mcp_ownership.lock().await;
        validate_plugin_mcp_claims(&ownership_guard, &declarations, &state.mcp_ownership)
            .map_err(anyhow::Error::msg)?;
        drop(ownership_guard);
        let mut lsp = self.prepare_lsp(&prepared, &binding).await?;
        lsp.shutdown_all().await;
        Ok(())
    }

    /// Replace project/local plugins and LSP processes for a committed workspace.
    /// A target failure converges to the target's User-only generation instead
    /// of clearing user-scoped plugins or retaining old workspace plugins.
    pub(crate) async fn rebind_workspace(
        self: &Arc<Self>,
        project_root: PathBuf,
    ) -> anyhow::Result<ReloadSummary> {
        self.run_owned_mutation(move |service| async move {
            service.rebind_workspace_inner(project_root).await
        })
        .await
    }

    async fn rebind_workspace_inner(&self, project_root: PathBuf) -> anyhow::Result<ReloadSummary> {
        let previous_binding = self.lsp.binding().await;
        let binding = PluginLspBinding {
            base_config: PluginLspRuntime::config_for_workspace(&project_root),
            project_root,
        };
        let mut candidate = self.registry_for(binding.project_root.clone());
        let scan = self.scan_registry(&mut candidate);
        let mut state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let quarantine_errors = self.retry_cleanup_quarantine(&mut state).await;
        let previous_workspace_plugins = workspace_scope_plugin_ids(&state.registry);
        let result = match scan {
            Ok(()) => self.apply_candidate(&mut state, candidate, &binding).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(summary) => {
                let mut retirement_errors = quarantine_errors;
                let (errors, failed_plugin_ids) =
                    retire_plugin_lifecycles(&mut state.lifecycle, &previous_workspace_plugins);
                retirement_errors.extend(errors);
                if !failed_plugin_ids.is_empty() {
                    state.cleanup_quarantine.push(PluginCleanupQuarantine {
                        root: previous_binding.project_root.clone(),
                        lifecycle: None,
                        lifecycle_plugin_ids: failed_plugin_ids,
                        monitors: Vec::new(),
                        last_errors: retirement_errors.clone(),
                    });
                }
                if retirement_errors.is_empty() {
                    Ok(summary)
                } else {
                    Err(anyhow::anyhow!(
                        "Target plugin generation committed, but previous workspace lifecycle retirement failed: {}",
                        retirement_errors.join("; ")
                    ))
                }
            }
            Err(error) => {
                let primary = error.to_string();
                let mut user_candidate = self.registry_for(binding.project_root.clone());
                let fallback =
                    match self.scan_registry_scopes(&mut user_candidate, &[PluginScope::User]) {
                        Ok(()) => self
                            .apply_candidate(&mut state, user_candidate, &binding)
                            .await
                            .map(|_| ()),
                        Err(error) => Err(error),
                    };
                match fallback {
                    Ok(()) => {
                        let mut retirement_errors = quarantine_errors;
                        let (errors, failed_plugin_ids) = retire_plugin_lifecycles(
                            &mut state.lifecycle,
                            &previous_workspace_plugins,
                        );
                        retirement_errors.extend(errors);
                        if !failed_plugin_ids.is_empty() {
                            state.cleanup_quarantine.push(PluginCleanupQuarantine {
                                root: previous_binding.project_root.clone(),
                                lifecycle: None,
                                lifecycle_plugin_ids: failed_plugin_ids,
                                monitors: Vec::new(),
                                last_errors: retirement_errors.clone(),
                            });
                        }
                        Err(anyhow::anyhow!(append_errors(
                            format!(
                                "Target workspace plugins were rejected; committed User-scope plugin generation instead: {primary}"
                            ),
                            retirement_errors,
                        )))
                    }
                    Err(fallback_error) => {
                        let retirement_errors = self
                            .retire_generation_fail_closed(
                                &mut state,
                                &binding,
                                previous_binding.project_root.clone(),
                            )
                            .await;
                        let mut all_errors = quarantine_errors;
                        all_errors.extend(retirement_errors);
                        Err(anyhow::anyhow!(append_errors(
                            format!(
                                "{primary}; failed to converge target User-scope plugin generation: {fallback_error}; retired all plugin-owned components fail-closed, including degraded User-scope plugins"
                            ),
                            all_errors,
                        )))
                    }
                }
            }
        }
    }

    async fn retire_generation_fail_closed(
        &self,
        state: &mut PluginRuntimeState,
        binding: &PluginLspBinding,
        previous_root: PathBuf,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            match pool.begin_plugin_publication().await {
                Ok(publication) => Some(publication),
                Err(error) => {
                    errors.push(format!(
                        "Failed to close AgentPool admission for fail-closed plugin retirement: {error}"
                    ));
                    return errors;
                }
            }
        } else {
            None
        };
        let previous_prepared = std::mem::take(&mut state.prepared);
        let mut failed_monitors = Vec::new();
        if let Some(scheduler) = self.scheduler.read().await.clone() {
            let monitor_errors =
                remove_plugin_monitors_best_effort(&scheduler, &previous_prepared.monitors).await;
            if !monitor_errors.is_empty() {
                failed_monitors = previous_prepared.monitors.clone();
                errors.extend(monitor_errors);
            }
        }

        let mut previous_lifecycle =
            std::mem::replace(&mut state.lifecycle, PluginLifecycleManager::new());
        let lifecycle_errors = previous_lifecycle.shutdown();
        let quarantine_lifecycle = !lifecycle_errors.is_empty();
        errors.extend(lifecycle_errors);
        if quarantine_lifecycle || !failed_monitors.is_empty() {
            state.cleanup_quarantine.push(PluginCleanupQuarantine {
                root: previous_root,
                lifecycle: Some(previous_lifecycle),
                lifecycle_plugin_ids: Vec::new(),
                monitors: failed_monitors,
                last_errors: errors.clone(),
            });
        }

        state.framework_components.clear();
        state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let mut ownership_guard = self.mcp_ownership.lock().await;
        if let Some(receipt) = previous_framework_receipt.as_ref() {
            self.integrator.rollback(&mut primary, receipt).await;
        }
        unload_application_components(&mut primary, &previous_prepared).await;
        primary
            .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
            .await;
        crate::runtime::configure_intent_router(&mut primary);
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);
        drop(ownership_guard);

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, LspManager::new())
        };
        previous_lsp.shutdown_all().await;
        self.lsp.publish_binding(binding.clone()).await;

        state.registry = self.registry_for(binding.project_root.clone());
        state.active_theme = None;
        state.active_output_style = None;
        match state.generation.checked_add(1) {
            Some(revision) => {
                let generation = AgentPluginGeneration::new(
                    revision,
                    primary.skill_descriptors(),
                    state.prepared.agents.clone(),
                    None,
                );
                if let Some(publication) = pool_publication.as_mut() {
                    match publication.prepare(generation).await {
                        Ok(()) => match publication.commit().await {
                            Ok(()) => state.generation = revision,
                            Err(error) => errors.push(format!(
                                "Failed to commit fail-closed AgentPool generation: {error}"
                            )),
                        },
                        Err(error) => errors.push(format!(
                            "Failed to prepare fail-closed AgentPool generation: {error}"
                        )),
                    }
                } else {
                    state.generation = revision;
                }
            }
            None => errors.push("plugin generation revision exhausted".to_string()),
        }
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: None,
                active_output_style: None,
            },
        ) {
            errors.push(format!(
                "Failed to persist fail-closed plugin preferences: {error}"
            ));
        }
        drop(pool_publication);
        drop(primary);
        errors
    }

    async fn retry_cleanup_quarantine(&self, state: &mut PluginRuntimeState) -> Vec<String> {
        let scheduler = self.scheduler.read().await.clone();
        let quarantined = std::mem::take(&mut state.cleanup_quarantine);
        let mut retry_errors = Vec::new();
        for mut debt in quarantined {
            let mut debt_errors = Vec::new();
            if let Some(lifecycle) = debt.lifecycle.as_mut() {
                debt_errors.extend(lifecycle.shutdown());
            } else {
                for plugin_id in &debt.lifecycle_plugin_ids {
                    if let Err(error) = state.lifecycle.unregister(plugin_id) {
                        debt_errors.push(error);
                    }
                }
            }

            if !debt.monitors.is_empty() {
                match scheduler.as_ref() {
                    Some(scheduler) => {
                        debt_errors.extend(
                            remove_plugin_monitors_best_effort(scheduler, &debt.monitors).await,
                        );
                    }
                    None => debt_errors.push(format!(
                        "Scheduler unavailable while retrying {} plugin monitor cleanup receipt(s)",
                        debt.monitors.len()
                    )),
                }
            }

            if debt_errors.is_empty() {
                continue;
            }
            debt.last_errors = debt_errors
                .iter()
                .map(|error| format!("{}: {error}", debt.root.display()))
                .collect();
            retry_errors.extend(debt.last_errors.clone());
            state.cleanup_quarantine.push(debt);
        }
        retry_errors
    }

    /// Roots with plugin-owned external cleanup that has not yet settled.
    pub async fn cleanup_debt_roots(&self) -> Vec<PathBuf> {
        let mut roots = self
            .state
            .lock()
            .await
            .cleanup_quarantine
            .iter()
            .map(|debt| debt.root.clone())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }

    pub async fn workspace_root(&self) -> PathBuf {
        self.lsp.binding().await.project_root
    }

    /// Opaque identity of the prepared framework generation currently exposed
    /// by this target.
    pub(crate) async fn prepared_generation_identity(&self) -> String {
        let state = self.state.lock().await;
        state
            .framework_generation
            .as_ref()
            .map(|generation| generation.identity().to_string())
            .unwrap_or_else(|| format!("unprepared:{}", state.generation))
    }

    pub(crate) async fn mcp_reconcile_target(
        &self,
    ) -> crate::mcp_config_runtime::McpReconcileTarget {
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        crate::mcp_config_runtime::McpReconcileTarget::new(
            self.agent_handle.clone(),
            Arc::clone(&self.mcp_ownership),
            pool,
        )
    }

    pub async fn lsp_configured_languages(&self) -> Vec<String> {
        let _state = self.state.lock().await;
        let manager = self.lsp.manager.read().await;
        let mut languages = manager
            .configured_languages()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        languages.sort();
        languages
    }

    pub async fn lsp_status(&self) -> Vec<LspServerStatus> {
        let _state = self.state.lock().await;
        let manager = self.lsp.manager.read().await;
        let mut statuses = manager.status_all().await;
        statuses.sort_by(|left, right| left.language.cmp(&right.language));
        statuses
    }

    pub(crate) async fn lsp_start(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            let mut manager = service.lsp.manager.write().await;
            if manager.get_client(&language).is_some() {
                return Err(anyhow::anyhow!(
                    "language server '{language}' is already running"
                ));
            }
            manager
                .start_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    pub(crate) async fn lsp_stop(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            service
                .lsp
                .manager
                .write()
                .await
                .stop_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    pub(crate) async fn lsp_restart(self: &Arc<Self>, language: String) -> anyhow::Result<()> {
        self.run_owned_mutation(move |service| async move {
            let mut manager = service.lsp.manager.write().await;
            manager
                .stop_server(&language)
                .await
                .map_err(anyhow::Error::msg)?;
            manager
                .start_server(&language)
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Rebuild only the LSP manager for this exact target. Config-file changes
    /// must not rescan plugins or republish Skills, MCP, Subagents and monitors.
    pub(crate) async fn reload_lsp_generation(
        self: &Arc<Self>,
        project_root: PathBuf,
    ) -> anyhow::Result<usize> {
        self.run_owned_mutation(move |service| async move {
            let state = service.state.lock().await;
            if state.shut_down {
                return Err(anyhow::anyhow!("plugin runtime is shut down"));
            }
            let current = service.lsp.binding().await;
            if current.project_root != project_root {
                anyhow::bail!(
                    "LSP target root changed from '{}' to '{}'",
                    current.project_root.display(),
                    project_root.display()
                );
            }
            let binding = PluginLspBinding {
                base_config: PluginLspRuntime::config_for_workspace(&project_root),
                project_root,
            };
            let replacement = service.prepare_lsp(&state.prepared, &binding).await?;
            let configured = replacement.configured_languages().len();
            let mut previous = {
                let mut current = service.lsp.manager.write().await;
                std::mem::replace(&mut *current, replacement)
            };
            service.lsp.publish_binding(binding).await;
            previous.shutdown_all().await;
            drop(state);
            Ok(configured)
        })
        .await
    }

    pub async fn bind_scheduler(
        self: &Arc<Self>,
        scheduler: Arc<SchedulerRunner>,
    ) -> anyhow::Result<usize> {
        self.run_owned_mutation(move |service| async move {
            service.bind_scheduler_inner(scheduler).await
        })
        .await
    }

    async fn bind_scheduler_inner(&self, scheduler: Arc<SchedulerRunner>) -> anyhow::Result<usize> {
        let state = self.state.lock().await;
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let monitors = state.prepared.monitors.clone();
        let mut slot = self.scheduler.write().await;
        if slot.is_some() {
            return Ok(monitors.len());
        }
        replace_plugin_monitors(&scheduler, &[], &monitors).await?;
        *slot = Some(scheduler);
        Ok(monitors.len())
    }

    /// Release all plugin-owned resources. Repeated calls are harmless.
    pub async fn shutdown(self: &Arc<Self>) -> anyhow::Result<()> {
        let settlement_error = self.drain_owned_mutations().await.err();
        let mut state = self.state.lock().await;
        let mut errors = self.retry_cleanup_quarantine(&mut state).await;
        if state.shut_down {
            errors.extend(state.lifecycle.shutdown());
            errors.extend(settlement_error.map(|error| error.to_string()));
            return if errors.is_empty() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Plugin runtime shutdown retry failed: {}",
                    errors.join("; ")
                ))
            };
        }

        errors.extend(state.lifecycle.shutdown());
        errors.extend(settlement_error.map(|error| error.to_string()));
        state.framework_components.clear();
        state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let previous_prepared = std::mem::take(&mut state.prepared);
        if !previous_prepared.monitors.is_empty()
            && let Some(scheduler) = self.scheduler.read().await.clone()
        {
            let monitor_errors =
                remove_plugin_monitors_best_effort(&scheduler, &previous_prepared.monitors).await;
            if !monitor_errors.is_empty() {
                let root = self.lsp.binding().await.project_root;
                state.cleanup_quarantine.push(PluginCleanupQuarantine {
                    root,
                    lifecycle: None,
                    lifecycle_plugin_ids: Vec::new(),
                    monitors: previous_prepared.monitors.clone(),
                    last_errors: monitor_errors.clone(),
                });
                errors.extend(monitor_errors);
            }
        }

        let mut ownership_guard = self.mcp_ownership.lock().await;
        let integrator = self.integrator.clone();
        self.agent_handle
            .write_async(|agent| {
                Box::pin(async move {
                    if let Some(receipt) = previous_framework_receipt.as_ref() {
                        integrator.rollback(agent, receipt).await;
                    }
                    unload_application_components(agent, &previous_prepared).await;
                    agent
                        .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                        .await;
                })
            })
            .await;
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, LspManager::new())
        };
        previous_lsp.shutdown_all().await;

        let project_root = self.lsp.binding().await.project_root;
        state.registry = self.registry_for(project_root);
        state.active_theme = None;
        state.active_output_style = None;

        if errors.is_empty() {
            state.shut_down = true;
            Ok(())
        } else {
            // Mutation admission is already closed by drain_owned_mutations,
            // but keep the runtime unsettled so a later shutdown retries the
            // retained lifecycle/monitor cleanup receipts.
            state.shut_down = false;
            Err(anyhow::anyhow!(
                "Plugin runtime shutdown failed: {}",
                errors.join("; ")
            ))
        }
    }

    pub(crate) async fn enable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move { service.enable_inner(&name).await })
            .await
    }

    async fn enable_inner(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .enable(name)
            .map_err(|error| anyhow::anyhow!("Enable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.restore_enabled_state(name, previously_enabled).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn disable(self: &Arc<Self>, name: &str) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move { service.disable_inner(&name).await })
            .await
    }

    async fn disable_inner(&self, name: &str) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previously_enabled = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        candidate
            .disable(name)
            .map_err(|error| anyhow::anyhow!("Disable plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => {
                self.fire_plugin_disabled(name).await;
                Ok(summary)
            }
            Err(error) => {
                self.restore_enabled_state(name, previously_enabled).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn install(
        self: &Arc<Self>,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let source = source.clone();
        self.run_owned_mutation(move |service| async move {
            service.install_inner(&source, scope).await
        })
        .await
    }

    async fn install_inner(
        &self,
        source: &InstallSource,
        scope: PluginScope,
    ) -> anyhow::Result<(String, ReloadSummary)> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let plugin_id = candidate
            .install(source, scope)
            .map_err(|error| anyhow::anyhow!("Install plugin failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok((plugin_id, summary)),
            Err(error) => {
                self.rollback_install(&plugin_id).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn uninstall(
        self: &Arc<Self>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.uninstall_inner(&name, keep_data).await
        })
        .await
    }

    async fn uninstall_inner(&self, name: &str, keep_data: bool) -> anyhow::Result<ReloadSummary> {
        let was_enabled = self
            .get(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        let mut summary = if was_enabled {
            self.disable_inner(name).await?
        } else {
            let state = self.state.lock().await;
            ReloadSummary {
                total: state.registry.count(),
                enabled: state.registry.list_enabled().len(),
                skills_loaded: state
                    .framework_components
                    .values()
                    .map(|components| components.skills.len())
                    .sum(),
                hooks_registered: state
                    .framework_components
                    .values()
                    .filter(|components| components.hooks_registered)
                    .count(),
                mcp_connected: state
                    .framework_components
                    .values()
                    .map(|components| components.mcp_servers.len())
                    .sum(),
                agents_loaded: state.prepared.agents.len(),
                lsp_languages_loaded: state
                    .prepared
                    .lsp_configs
                    .iter()
                    .map(|(_, config)| config.servers.len())
                    .sum(),
                monitors_loaded: state.prepared.monitors.len(),
                themes_loaded: state.prepared.themes.len(),
                output_styles_loaded: state.prepared.output_styles.len(),
                errors: Vec::new(),
            }
        };
        let mut state = self.state.lock().await;
        state
            .registry
            .uninstall(name, keep_data)
            .map_err(|error| anyhow::anyhow!("Uninstall plugin '{name}' failed: {error}"))?;
        let lifecycle_error = state.lifecycle.unregister(name).err();
        summary.total = state.registry.count();
        summary.enabled = state.registry.list_enabled().len();
        if !was_enabled {
            self.fire_plugin_disabled(name).await;
        }
        match lifecycle_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(summary),
        }
    }

    pub async fn list(&self) -> Vec<PluginEntry> {
        self.state
            .lock()
            .await
            .registry
            .list()
            .into_iter()
            .cloned()
            .collect()
    }

    #[cfg(test)]
    pub(crate) async fn generation_for_test(&self) -> u64 {
        self.state.lock().await.generation
    }

    pub async fn get(&self, name: &str) -> Option<PluginEntry> {
        self.state.lock().await.registry.get(name).cloned()
    }

    pub(crate) async fn configure(
        self: &Arc<Self>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<ReloadSummary> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.configure_inner(&name, values).await
        })
        .await
    }

    async fn configure_inner(
        &self,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<ReloadSummary> {
        let mut state = self.state.lock().await;
        let binding = self.lsp.binding().await;
        let mut candidate = self.registry_for(binding.project_root.clone());
        self.scan_registry(&mut candidate)?;
        let previous = candidate
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .user_config
            .clone();
        candidate
            .configure(name, values)
            .map_err(|error| anyhow::anyhow!("Configure plugin '{name}' failed: {error}"))?;
        match self.apply_candidate(&mut state, candidate, &binding).await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.restore_plugin_config(name, previous).await;
                Err(error)
            }
        }
    }

    /// Register native lifecycle callbacks and synchronize them immediately.
    pub async fn register_lifecycle(
        self: &Arc<Self>,
        name: &str,
        callbacks: Arc<dyn PluginLifecycle>,
    ) -> anyhow::Result<()> {
        let name = name.to_string();
        self.run_owned_mutation(move |service| async move {
            service.register_lifecycle_inner(&name, callbacks).await
        })
        .await
    }

    async fn register_lifecycle_inner(
        &self,
        name: &str,
        callbacks: Arc<dyn PluginLifecycle>,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let enabled = state
            .registry
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{name}' not found"))?
            .enabled;
        state
            .lifecycle
            .register(name, callbacks)
            .map_err(anyhow::Error::msg)?;
        if enabled && let Err(error) = state.lifecycle.activate(name) {
            let cleanup_error = state.lifecycle.unregister(name).err();
            return Err(anyhow::anyhow!(append_errors(
                error,
                cleanup_error.into_iter().collect(),
            )));
        }
        Ok(())
    }

    pub async fn themes(&self) -> Vec<PluginThemeDefinition> {
        self.state.lock().await.prepared.themes.clone()
    }

    pub async fn active_theme(&self) -> Option<String> {
        self.state.lock().await.active_theme.clone()
    }

    pub(crate) async fn activate_theme(
        self: &Arc<Self>,
        name: Option<&str>,
    ) -> anyhow::Result<Option<PluginThemeDefinition>> {
        let name = name.map(str::to_string);
        self.run_owned_mutation(move |service| async move {
            service.activate_theme_inner(name.as_deref()).await
        })
        .await
    }

    async fn activate_theme_inner(
        &self,
        name: Option<&str>,
    ) -> anyhow::Result<Option<PluginThemeDefinition>> {
        let mut state = self.state.lock().await;
        let theme = match name {
            Some(name) => Some(
                state
                    .prepared
                    .themes
                    .iter()
                    .find(|theme| theme.name == name)
                    .ok_or_else(|| anyhow::anyhow!("Theme '{name}' not found"))?
                    .clone(),
            ),
            None => None,
        };
        let selected = name.map(str::to_string);
        persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: selected.clone(),
                active_output_style: state.active_output_style.clone(),
            },
        )?;
        state.active_theme = selected;
        Ok(theme)
    }

    pub async fn output_styles(&self) -> Vec<PluginOutputStyle> {
        self.state.lock().await.prepared.output_styles.clone()
    }

    pub async fn active_output_style(&self) -> Option<String> {
        self.state.lock().await.active_output_style.clone()
    }

    pub(crate) async fn activate_output_style(
        self: &Arc<Self>,
        name: Option<&str>,
    ) -> anyhow::Result<()> {
        let name = name.map(str::to_string);
        self.run_owned_mutation(move |service| async move {
            service.activate_output_style_inner(name.as_deref()).await
        })
        .await
    }

    async fn activate_output_style_inner(&self, name: Option<&str>) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let instructions = match name {
            Some(name) => Some(
                state
                    .prepared
                    .output_styles
                    .iter()
                    .find(|style| style.name == name)
                    .ok_or_else(|| anyhow::anyhow!("Output style '{name}' not found"))?
                    .instructions
                    .clone(),
            ),
            None => None,
        };
        let selected = name.map(str::to_string);
        let previous_selected = state.active_output_style.clone();
        let previous = active_output_style_instructions(&state);
        let revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            Some(
                pool.begin_plugin_publication()
                    .await
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        primary
            .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions.clone())
            .await;
        let generation = AgentPluginGeneration::new(
            revision,
            primary.skill_descriptors(),
            state.prepared.agents.clone(),
            instructions,
        )
        .with_framework_generation(state.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.prepare(generation).await
        {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous.clone())
                .await;
            return Err(anyhow::Error::msg(error));
        }
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: selected.clone(),
            },
        ) {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous.clone())
                .await;
            let rollback = match pool_publication.as_mut() {
                Some(publication) => publication.rollback().await.err(),
                None => None,
            };
            return Err(match rollback {
                Some(rollback) => anyhow::anyhow!(
                    "Output-style persistence failed: {error}; pool rollback failed: {rollback}"
                ),
                None => error,
            });
        }
        if let Some(publication) = pool_publication.as_mut()
            && let Err(error) = publication.commit().await
        {
            primary
                .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, previous)
                .await;
            let rollback = publication.rollback().await.err();
            let preference_rollback = persist_preferences(
                &self.preferences_file,
                &PluginPreferences {
                    active_theme: state.active_theme.clone(),
                    active_output_style: previous_selected,
                },
            )
            .err();
            let mut errors = vec![format!("Output-style pool commit failed: {error}")];
            errors.extend(rollback.map(|error| format!("pool rollback failed: {error}")));
            errors.extend(
                preference_rollback.map(|error| format!("preference rollback failed: {error}")),
            );
            return Err(anyhow::anyhow!(errors.join("; ")));
        }
        state.generation = revision;
        state.active_output_style = selected;
        Ok(())
    }

    pub fn scaffold(
        directory: impl AsRef<Path>,
        name: &str,
    ) -> anyhow::Result<PluginScaffoldResult> {
        let directory = directory.as_ref();
        let name = name.trim();
        validate_plugin_name(name)?;
        if directory.exists() {
            return Err(anyhow::anyhow!(
                "Plugin scaffold target already exists: {}",
                directory.display()
            ));
        }

        std::fs::create_dir_all(directory).map_err(|error| {
            anyhow::anyhow!(
                "Failed to create plugin directory '{}': {error}",
                directory.display()
            )
        })?;
        let result = write_scaffold(directory, name);
        if let Err(error) = result {
            let cleanup = std::fs::remove_dir_all(directory).err();
            return Err(match cleanup {
                Some(cleanup) => anyhow::anyhow!(
                    "{error}; failed to roll back scaffold '{}': {cleanup}",
                    directory.display()
                ),
                None => error,
            });
        }
        Ok(PluginScaffoldResult {
            path: directory.to_path_buf(),
            name: name.to_string(),
        })
    }

    pub fn validate(directory: impl AsRef<Path>) -> PluginValidationReport {
        let directory = directory.as_ref();
        match PluginRegistry::validate_plugin_dir(directory) {
            Ok((manifest, resolved)) => {
                let defaults = manifest
                    .config
                    .iter()
                    .filter_map(|(name, entry)| {
                        entry.default.clone().map(|value| (name.clone(), value))
                    })
                    .collect::<HashMap<_, _>>();
                let project_dir =
                    std::env::current_dir().unwrap_or_else(|_| directory.to_path_buf());
                let variables = echo_agent::plugin::PluginVariables::new(
                    directory.to_path_buf(),
                    std::env::temp_dir(),
                    project_dir,
                )
                .with_json_user_config(&defaults);
                let errors = validate_application_component_files(
                    &manifest.name,
                    directory,
                    &resolved,
                    &variables,
                );
                let components = component_names(directory, &resolved);
                PluginValidationReport {
                    valid: errors.is_empty(),
                    name: Some(manifest.name),
                    components,
                    errors,
                }
            }
            Err(errors) => PluginValidationReport {
                valid: false,
                name: None,
                components: Vec::new(),
                errors,
            },
        }
    }

    async fn apply_candidate(
        &self,
        state: &mut PluginRuntimeState,
        mut candidate: PluginRegistry,
        binding: &PluginLspBinding,
    ) -> anyhow::Result<ReloadSummary> {
        if state.shut_down {
            return Err(anyhow::anyhow!("plugin runtime is shut down"));
        }
        let candidate_revision = state
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("plugin generation revision exhausted"))?;
        let framework_generation =
            require_applicable_generation(self.integrator.prepare(&mut candidate).await)?;
        let candidate_plugins = candidate
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<Vec<_>>();
        let previous_plugins = state
            .registry
            .list_enabled()
            .into_iter()
            .map(|entry| entry.manifest.name.clone())
            .collect::<Vec<_>>();
        let prepared = prepare_application_components(&framework_generation, &self.target_scope)
            .map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        let candidate_mcp_declarations = plugin_mcp_declarations(&framework_generation)?;
        self.validate_agent_collisions(state, &prepared).await?;
        let mut replacement_lsp = self.prepare_lsp(&prepared, binding).await?;

        // Publication lock order: primary execution, primary agent write,
        // then pool transition/agents. No primary or pooled turn can observe
        // a half-published skill/Subagent/router catalog.
        let primary_execution = self
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let _primary_execution_guard = primary_execution.lock_owned().await;
        let primary_owner = Arc::clone(self.agent_handle.inner());
        let mut primary = primary_owner.write_owned().await;
        let pool = self
            .agent_pool
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        let mut pool_publication = if let Some(pool) = pool.as_ref() {
            match pool.begin_plugin_publication().await {
                Ok(publication) => Some(publication),
                Err(error) => {
                    replacement_lsp.shutdown_all().await;
                    return Err(anyhow::anyhow!(
                        "Failed to close AgentPool plugin publication admission: {error}"
                    ));
                }
            }
        } else {
            None
        };

        let deactivate_errors = state.lifecycle.deactivate_all();
        if !deactivate_errors.is_empty() {
            let mut errors = deactivate_errors;
            errors.extend(
                state
                    .lifecycle
                    .activate_enabled(previous_plugins.iter().map(String::as_str)),
            );
            replacement_lsp.shutdown_all().await;
            return Err(anyhow::anyhow!(
                "Plugin lifecycle deactivation failed: {}",
                errors.join("; ")
            ));
        }

        let scheduler = self.scheduler.read().await.clone();
        if let Some(scheduler) = scheduler.as_ref()
            && let Err(error) =
                replace_plugin_monitors(scheduler, &state.prepared.monitors, &prepared.monitors)
                    .await
        {
            replacement_lsp.shutdown_all().await;
            let mut errors = vec![error.to_string()];
            errors.extend(
                state
                    .lifecycle
                    .activate_enabled(previous_plugins.iter().map(String::as_str)),
            );
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        let previous_registry = std::mem::replace(
            &mut state.registry,
            self.registry_for(binding.project_root.clone()),
        );
        let previous_framework = std::mem::take(&mut state.framework_components);
        let previous_framework_generation = state.framework_generation.take();
        let previous_framework_receipt = state.framework_receipt.take();
        let previous_mcp_ownership = std::mem::take(&mut state.mcp_ownership);
        let previous_prepared = std::mem::take(&mut state.prepared);
        let apply = self
            .replace_agent_components(
                &mut primary,
                previous_registry,
                previous_framework,
                previous_framework_generation,
                previous_framework_receipt,
                previous_mcp_ownership,
                previous_prepared,
                candidate,
                Some(framework_generation),
                candidate_mcp_declarations,
                prepared,
            )
            .await;

        let applied = match apply {
            Ok(applied) => applied,
            Err(mut failed) => {
                state.registry = failed.registry;
                state.framework_components = failed.framework_components;
                state.framework_generation = failed.framework_generation;
                state.framework_receipt = failed.framework_receipt;
                state.mcp_ownership = failed.mcp_ownership;
                state.prepared = failed.prepared;
                if let Some(scheduler) = scheduler.as_ref()
                    && let Err(error) = replace_plugin_monitors(
                        scheduler,
                        &failed.candidate_monitors,
                        &state.prepared.monitors,
                    )
                    .await
                {
                    failed.error =
                        format!("{}; rollback plugin monitors failed: {error}", failed.error);
                }
                replacement_lsp.shutdown_all().await;
                failed.error = append_errors(
                    failed.error,
                    state
                        .lifecycle
                        .activate_enabled(previous_plugins.iter().map(String::as_str)),
                );
                return Err(anyhow::anyhow!(failed.error));
            }
        };

        let candidate_generation = AgentPluginGeneration::new(
            candidate_revision,
            primary.skill_descriptors(),
            applied.prepared.agents.clone(),
            active_output_style_instructions_for(
                state.active_output_style.as_deref(),
                &applied.prepared,
            ),
        )
        .with_framework_generation(applied.framework_generation.clone());
        if let Some(publication) = pool_publication.as_mut()
            && let Err(pool_error) = publication.prepare(candidate_generation).await
        {
            let candidate_monitors = applied.prepared.monitors.clone();
            let previous_monitors = applied.previous_prepared.monitors.clone();
            let candidate_framework = applied
                .wiring
                .as_ref()
                .map(|receipt| receipt.components_by_plugin.clone())
                .unwrap_or_default();
            let rollback = self
                .replace_agent_components(
                    &mut primary,
                    applied.registry,
                    candidate_framework,
                    applied.framework_generation,
                    applied.wiring,
                    applied.mcp_ownership,
                    applied.prepared,
                    applied.previous_registry,
                    applied.previous_framework_generation,
                    applied.previous_mcp_declarations,
                    applied.previous_prepared,
                )
                .await;
            let mut errors = vec![format!(
                "AgentPool plugin generation publication failed: {pool_error}"
            )];
            match rollback {
                Ok(restored) => {
                    if let Some(scheduler) = scheduler.as_ref()
                        && let Err(error) = replace_plugin_monitors(
                            scheduler,
                            &candidate_monitors,
                            &previous_monitors,
                        )
                        .await
                    {
                        errors.push(format!("rollback plugin monitors failed: {error}"));
                    }
                    state.registry = restored.registry;
                    state.framework_components = restored
                        .wiring
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default();
                    state.framework_generation = restored.framework_generation;
                    state.framework_receipt = restored.wiring;
                    state.mcp_ownership = restored.mcp_ownership;
                    state.prepared = restored.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(previous_plugins.iter().map(String::as_str)),
                    );
                }
                Err(failed) => {
                    errors.push(format!(
                        "rollback agent components failed: {}",
                        failed.error
                    ));
                    state.registry = failed.registry;
                    state.framework_components = failed.framework_components;
                    state.framework_generation = failed.framework_generation;
                    state.framework_receipt = failed.framework_receipt;
                    state.mcp_ownership = failed.mcp_ownership;
                    state.prepared = failed.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(candidate_plugins.iter().map(String::as_str)),
                    );
                }
            }
            replacement_lsp.shutdown_all().await;
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        let mut previous_lsp = {
            let mut current = self.lsp.manager.write().await;
            std::mem::replace(&mut *current, replacement_lsp)
        };

        let activation_errors = state
            .lifecycle
            .activate_enabled(candidate_plugins.iter().map(String::as_str));
        if !activation_errors.is_empty() {
            let mut errors = vec![format!(
                "Plugin lifecycle activation failed: {}",
                activation_errors.join("; ")
            )];
            errors.extend(state.lifecycle.deactivate_all());

            let candidate_monitors = applied.prepared.monitors.clone();
            let previous_monitors = applied.previous_prepared.monitors.clone();
            let candidate_framework = applied
                .wiring
                .as_ref()
                .map(|receipt| receipt.components_by_plugin.clone())
                .unwrap_or_default();
            let rollback = self
                .replace_agent_components(
                    &mut primary,
                    applied.registry,
                    candidate_framework,
                    applied.framework_generation,
                    applied.wiring,
                    applied.mcp_ownership,
                    applied.prepared,
                    applied.previous_registry,
                    applied.previous_framework_generation,
                    applied.previous_mcp_declarations,
                    applied.previous_prepared,
                )
                .await;
            match rollback {
                Ok(restored) => {
                    if let Some(scheduler) = scheduler.as_ref()
                        && let Err(error) = replace_plugin_monitors(
                            scheduler,
                            &candidate_monitors,
                            &previous_monitors,
                        )
                        .await
                    {
                        errors.push(format!("rollback plugin monitors failed: {error}"));
                    }
                    {
                        let mut current = self.lsp.manager.write().await;
                        let mut candidate_lsp = std::mem::replace(&mut *current, previous_lsp);
                        candidate_lsp.shutdown_all().await;
                    }
                    state.registry = restored.registry;
                    state.framework_components = restored
                        .wiring
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default();
                    state.framework_generation = restored.framework_generation;
                    state.framework_receipt = restored.wiring;
                    state.mcp_ownership = restored.mcp_ownership;
                    state.prepared = restored.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(previous_plugins.iter().map(String::as_str)),
                    );
                }
                Err(failed) => {
                    errors.push(format!(
                        "rollback agent components failed: {}",
                        failed.error
                    ));
                    previous_lsp.shutdown_all().await;
                    state.registry = failed.registry;
                    state.framework_components = failed.framework_components;
                    state.framework_generation = failed.framework_generation;
                    state.framework_receipt = failed.framework_receipt;
                    state.mcp_ownership = failed.mcp_ownership;
                    state.prepared = failed.prepared;
                    errors.extend(
                        state
                            .lifecycle
                            .activate_enabled(candidate_plugins.iter().map(String::as_str)),
                    );
                }
            }
            if let Some(publication) = pool_publication.as_mut()
                && let Err(error) = publication.rollback().await
            {
                errors.push(error);
            }
            return Err(anyhow::anyhow!(errors.join("; ")));
        }

        if let Some(publication) = pool_publication.as_mut() {
            publication.commit().await.map_err(anyhow::Error::msg)?;
        }

        previous_lsp.shutdown_all().await;
        self.lsp.publish_binding(binding.clone()).await;

        let active_style = state.active_output_style.clone();
        let active_theme = state.active_theme.clone();
        state.registry = applied.registry;
        state.framework_components = applied
            .wiring
            .as_ref()
            .map(|receipt| receipt.components_by_plugin.clone())
            .unwrap_or_default();
        state.framework_generation = applied.framework_generation;
        state.framework_receipt = applied.wiring;
        state.mcp_ownership = applied.mcp_ownership;
        state.prepared = applied.prepared;
        state.generation = candidate_revision;
        if let Some(style) = active_style {
            if state
                .prepared
                .output_styles
                .iter()
                .any(|candidate| candidate.name == style)
            {
                let instructions = state
                    .prepared
                    .output_styles
                    .iter()
                    .find(|candidate| candidate.name == style)
                    .map(|candidate| candidate.instructions.clone());
                primary
                    .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, instructions)
                    .await;
            } else {
                state.active_output_style = None;
                primary
                    .replace_system_context_projection(OUTPUT_STYLE_PROJECTION, None)
                    .await;
            }
        }

        if let Some(theme) = active_theme
            && !state
                .prepared
                .themes
                .iter()
                .any(|candidate| candidate.name == theme)
        {
            state.active_theme = None;
        }

        let total = state.registry.count();
        let enabled = state.registry.list_enabled().len();
        let mut summary = ReloadSummary::from_components(
            total,
            enabled,
            state.framework_receipt.as_ref(),
            state.framework_generation.as_deref(),
            &state.prepared,
        );
        if let Err(error) = persist_preferences(
            &self.preferences_file,
            &PluginPreferences {
                active_theme: state.active_theme.clone(),
                active_output_style: state.active_output_style.clone(),
            },
        ) {
            summary.errors.push(error.to_string());
        }
        drop(pool_publication);
        drop(primary);
        self.fire_loaded_events(&candidate_plugins).await;
        tracing::info!(
            total,
            enabled,
            agents = summary.agents_loaded,
            lsp = summary.lsp_languages_loaded,
            monitors = summary.monitors_loaded,
            themes = summary.themes_loaded,
            output_styles = summary.output_styles_loaded,
            "plugin runtime replaced atomically"
        );
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    async fn replace_agent_components(
        &self,
        agent: &mut echo_agent::agent::ReactAgent,
        previous_registry: PluginRegistry,
        previous_framework: HashMap<String, WiredPluginComponents>,
        previous_framework_generation: Option<Arc<PreparedPluginSet>>,
        previous_framework_receipt: Option<PluginWiringResult>,
        previous_mcp_ownership: PluginMcpOwnership,
        previous_prepared: PreparedApplicationComponents,
        candidate: PluginRegistry,
        candidate_framework_generation: Option<Arc<PreparedPluginSet>>,
        candidate_mcp_declarations: PluginMcpDeclarations,
        candidate_prepared: PreparedApplicationComponents,
    ) -> Result<AppliedAgentComponents, FailedAgentComponents> {
        let candidate_monitors = candidate_prepared.monitors.clone();
        let previous_mcp_declarations = match previous_framework_generation
            .as_deref()
            .map(plugin_mcp_declarations)
            .transpose()
        {
            Ok(declarations) => declarations.unwrap_or_default(),
            Err(error) => {
                return Err(FailedAgentComponents {
                    error: format!("Failed to inspect prepared plugin MCP receipts: {error}"),
                    registry: previous_registry,
                    framework_components: previous_framework,
                    framework_generation: previous_framework_generation,
                    framework_receipt: previous_framework_receipt,
                    mcp_ownership: previous_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                });
            }
        };
        let mut ownership_guard = self.mcp_ownership.lock().await;
        if let Err(error) = validate_plugin_mcp_claims(
            &ownership_guard,
            &candidate_mcp_declarations,
            &previous_mcp_ownership,
        ) {
            return Err(FailedAgentComponents {
                error,
                registry: previous_registry,
                framework_components: previous_framework,
                framework_generation: previous_framework_generation,
                framework_receipt: previous_framework_receipt,
                mcp_ownership: previous_mcp_ownership,
                prepared: previous_prepared,
                candidate_monitors,
            });
        }

        if let Some(receipt) = previous_framework_receipt.as_ref() {
            self.integrator.rollback(agent, receipt).await;
        }
        unload_application_components(agent, &previous_prepared).await;
        release_plugin_mcp_claims(&mut ownership_guard, &previous_mcp_ownership);
        let candidate_mcp_ownership = match claim_plugin_mcp_names(
            &mut ownership_guard,
            &candidate_mcp_declarations,
        ) {
            Ok(ownership) => ownership,
            Err(error) => {
                let restored_mcp_ownership = match claim_plugin_mcp_names(
                    &mut ownership_guard,
                    &previous_mcp_declarations,
                ) {
                    Ok(ownership) => ownership,
                    Err(restore_error) => {
                        return Err(FailedAgentComponents {
                            error: format!(
                                "{error}; rollback MCP ownership failed: {restore_error}"
                            ),
                            registry: previous_registry,
                            framework_components: HashMap::new(),
                            framework_generation: None,
                            framework_receipt: None,
                            mcp_ownership: HashMap::new(),
                            prepared: PreparedApplicationComponents::default(),
                            candidate_monitors,
                        });
                    }
                };
                let restored = match previous_framework_generation.as_deref() {
                    Some(generation) => {
                        match self.integrator.wire_prepared(agent, generation).await {
                            Ok(receipt) => Some(receipt),
                            Err(restore_error) => {
                                return Err(FailedAgentComponents {
                                    error: format!(
                                        "{error}; rollback framework wiring failed: {restore_error}"
                                    ),
                                    registry: previous_registry,
                                    framework_components: HashMap::new(),
                                    framework_generation: None,
                                    framework_receipt: None,
                                    mcp_ownership: restored_mcp_ownership,
                                    prepared: PreparedApplicationComponents::default(),
                                    candidate_monitors,
                                });
                            }
                        }
                    }
                    None => None,
                };
                let restore_agent_error = register_plugin_agents(agent, &previous_prepared.agents)
                    .await
                    .err();
                crate::runtime::configure_intent_router(agent);
                let mut errors = vec![error];
                if let Some(error) = restore_agent_error {
                    errors.push(format!("rollback Subagent wiring failed: {error}"));
                }
                return Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry: previous_registry,
                    framework_components: restored
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default(),
                    framework_generation: previous_framework_generation,
                    framework_receipt: restored,
                    mcp_ownership: restored_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                });
            }
        };

        let wiring = match candidate_framework_generation.as_deref() {
            Some(generation) => self
                .integrator
                .wire_prepared(agent, generation)
                .await
                .map(Some)
                .map_err(|error| error.to_string()),
            None => Ok(None),
        };
        let candidate_outcome = match wiring {
            Ok(wiring) => match register_plugin_agents(agent, &candidate_prepared.agents).await {
                Ok(_) => {
                    crate::runtime::configure_intent_router(agent);
                    Ok((candidate, wiring))
                }
                Err(error) => {
                    if let Some(receipt) = wiring.as_ref() {
                        self.integrator.rollback(agent, receipt).await;
                    }
                    unload_application_components(agent, &candidate_prepared).await;
                    Err((
                        format!("Plugin Subagent registration failed: {error}"),
                        candidate,
                    ))
                }
            },
            Err(error) => Err((format!("Plugin wiring failed: {error}"), candidate)),
        };

        match candidate_outcome {
            Ok((registry, wiring)) => Ok(AppliedAgentComponents {
                registry,
                wiring,
                framework_generation: candidate_framework_generation,
                mcp_ownership: candidate_mcp_ownership,
                prepared: candidate_prepared,
                previous_registry,
                previous_framework_generation,
                previous_mcp_declarations,
                previous_prepared,
            }),
            Err((error, _candidate_registry)) => {
                release_plugin_mcp_claims(&mut ownership_guard, &candidate_mcp_ownership);
                let restored_mcp_ownership = match claim_plugin_mcp_names(
                    &mut ownership_guard,
                    &previous_mcp_declarations,
                ) {
                    Ok(ownership) => ownership,
                    Err(restore_error) => {
                        return Err(FailedAgentComponents {
                            error: format!(
                                "{error}; rollback MCP ownership failed: {restore_error}"
                            ),
                            registry: previous_registry,
                            framework_components: HashMap::new(),
                            framework_generation: None,
                            framework_receipt: None,
                            mcp_ownership: HashMap::new(),
                            prepared: PreparedApplicationComponents::default(),
                            candidate_monitors,
                        });
                    }
                };
                let restored = match previous_framework_generation.as_deref() {
                    Some(generation) => self
                        .integrator
                        .wire_prepared(agent, generation)
                        .await
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                };
                let restore_agent_error = register_plugin_agents(agent, &previous_prepared.agents)
                    .await
                    .err();
                crate::runtime::configure_intent_router(agent);
                let registry = previous_registry;
                let mut errors = vec![error];
                let restored = match restored {
                    Ok(restored) => restored,
                    Err(error) => {
                        errors.push(format!("rollback framework wiring failed: {error}"));
                        None
                    }
                };
                if let Some(error) = restore_agent_error {
                    errors.push(format!("rollback Subagent wiring failed: {error}"));
                }
                Err(FailedAgentComponents {
                    error: errors.join("; "),
                    registry,
                    framework_components: restored
                        .as_ref()
                        .map(|receipt| receipt.components_by_plugin.clone())
                        .unwrap_or_default(),
                    framework_generation: previous_framework_generation,
                    framework_receipt: restored,
                    mcp_ownership: restored_mcp_ownership,
                    prepared: previous_prepared,
                    candidate_monitors,
                })
            }
        }
    }

    async fn validate_agent_collisions(
        &self,
        state: &PluginRuntimeState,
        prepared: &PreparedApplicationComponents,
    ) -> anyhow::Result<()> {
        let existing = self
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await
            .agent_names()
            .await
            .into_iter()
            .collect::<HashSet<_>>();
        let previous = state
            .prepared
            .agents
            .iter()
            .map(agent_name)
            .collect::<HashSet<_>>();
        let collisions = prepared
            .agents
            .iter()
            .map(agent_name)
            .filter(|name| existing.contains(name) && !previous.contains(name))
            .collect::<Vec<_>>();
        if collisions.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Plugin Subagent names collide with existing runtime roles: {}",
                collisions.join(", ")
            ))
        }
    }

    async fn prepare_lsp(
        &self,
        prepared: &PreparedApplicationComponents,
        binding: &PluginLspBinding,
    ) -> anyhow::Result<LspManager> {
        let mut config = binding.base_config.clone();
        let current_binding = self.lsp.binding().await;
        let mut required = if current_binding.project_root == binding.project_root {
            self.lsp
                .manager
                .read()
                .await
                .running_servers()
                .into_iter()
                .map(str::to_string)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        for (_, plugin_config) in &prepared.lsp_configs {
            required.extend(plugin_config.servers.keys().cloned());
            config.merge(plugin_config.clone());
        }
        let mut manager = LspManager::new();
        manager.load_config(&config);
        manager.set_project_root(&binding.project_root);
        let languages = manager
            .configured_languages()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        for language in languages {
            if let Err(error) = manager.start_server(&language).await {
                if required.contains(&language) {
                    manager.shutdown_all().await;
                    return Err(anyhow::anyhow!(
                        "Plugin LSP server '{language}' failed to start: {error}"
                    ));
                }
                tracing::warn!(%language, %error, "base LSP server unavailable during plugin reload");
            }
        }
        Ok(manager)
    }

    pub(crate) async fn project_root(&self) -> PathBuf {
        self.lsp.binding().await.project_root
    }

    fn registry_for(&self, project_root: PathBuf) -> PluginRegistry {
        match &self.registry_source {
            RegistrySource::Default => {
                PluginRegistry::new(crate::data_root::user_data_dir(), Some(project_root))
            }
            #[cfg(test)]
            RegistrySource::Custom {
                state_file,
                data_dir,
                ..
            } => {
                PluginRegistry::with_paths(state_file.clone(), data_dir.clone(), Some(project_root))
            }
        }
    }

    fn scan_registry(&self, registry: &mut PluginRegistry) -> anyhow::Result<()> {
        match &self.registry_source {
            RegistrySource::Default => registry.scan_all().map(|_| ()),
            #[cfg(test)]
            RegistrySource::Custom { scopes, .. } => registry.scan_scopes(scopes).map(|_| ()),
        }
        .map_err(|error| anyhow::anyhow!("Plugin scan failed: {error}"))
    }

    fn scan_registry_scopes(
        &self,
        registry: &mut PluginRegistry,
        requested: &[PluginScope],
    ) -> anyhow::Result<()> {
        let scopes = match &self.registry_source {
            RegistrySource::Default => requested.to_vec(),
            #[cfg(test)]
            RegistrySource::Custom { scopes, .. } => requested
                .iter()
                .filter(|scope| scopes.contains(scope))
                .copied()
                .collect(),
        };
        registry
            .scan_scopes(&scopes)
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("Plugin scan failed: {error}"))
    }

    async fn restore_enabled_state(&self, name: &str, enabled: bool) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok() {
            let result = if enabled {
                registry.enable(name)
            } else {
                registry.disable(name)
            };
            if let Err(error) = result {
                tracing::error!(plugin = %name, %error, "failed to roll back plugin enabled state");
            }
        }
    }

    async fn restore_plugin_config(&self, name: &str, values: HashMap<String, serde_json::Value>) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok()
            && let Err(error) = registry.configure(name, values)
        {
            tracing::error!(plugin = %name, %error, "failed to roll back plugin configuration");
        }
    }

    async fn rollback_install(&self, name: &str) {
        let mut registry = self.registry_for(self.project_root().await);
        if self.scan_registry(&mut registry).is_ok()
            && let Err(error) = registry.uninstall(name, false)
        {
            tracing::error!(plugin = %name, %error, "failed to roll back plugin install");
        }
    }

    async fn fire_loaded_events(&self, names: &[String]) {
        let (hook_registry, session_id, agent_name) = self
            .agent_handle
            .read(|agent| {
                (
                    agent.hook_registry().clone(),
                    agent
                        .config()
                        .get_session_id()
                        .unwrap_or_default()
                        .to_string(),
                    agent.config().get_agent_name().to_string(),
                )
            })
            .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginLoaded,
            names,
            &session_id,
            &agent_name,
        )
        .await;
    }

    async fn fire_plugin_disabled(&self, name: &str) {
        let (hook_registry, session_id, agent_name) = self
            .agent_handle
            .read(|agent| {
                (
                    agent.hook_registry().clone(),
                    agent
                        .config()
                        .get_session_id()
                        .unwrap_or_default()
                        .to_string(),
                    agent.config().get_agent_name().to_string(),
                )
            })
            .await;
        fire_plugin_events(
            &hook_registry,
            echo_agent::skills::hooks::HookEvent::PluginDisabled,
            &[name.to_string()],
            &session_id,
            &agent_name,
        )
        .await;
    }
}
