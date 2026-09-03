impl ExtensionControlService {
    #[cfg(test)]
    pub(crate) fn with_enabled_config_path(path: PathBuf) -> Self {
        Self {
            mutation: Mutex::new(()),
            enabled_config_path: path,
        }
    }

    async fn context(&self, state: &AppState) -> anyhow::Result<ScopedExtensionControl> {
        state
            .current_extension_control()
            .await
            .map_err(anyhow::Error::new)
    }

    async fn scoped_context(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<ScopedExtensionControl> {
        match runtime {
            Some(runtime) => state
                .extension_control_for_runtime(runtime)
                .await
                .map_err(anyhow::Error::new),
            None => self.context(state).await,
        }
    }

    pub async fn plugin_catalog(&self, state: &AppState) -> anyhow::Result<PluginCatalogSnapshot> {
        self.plugin_catalog_scoped(state, None).await
    }

    pub async fn plugin_catalog_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginCatalogSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let plugins = control.plugin_runtime().list().await;
        Ok(PluginCatalogSnapshot {
            authority_scope,
            plugins,
        })
    }

    pub async fn plugin_entry(
        &self,
        state: &AppState,
        name: &str,
    ) -> anyhow::Result<(String, Option<echo_agent::plugin::PluginEntry>)> {
        self.plugin_entry_scoped(state, None, name).await
    }

    pub async fn plugin_entry_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
    ) -> anyhow::Result<(String, Option<echo_agent::plugin::PluginEntry>)> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let entry = control.plugin_runtime().get(name).await;
        Ok((authority_scope, entry))
    }

    pub async fn plugin_themes(&self, state: &AppState) -> anyhow::Result<PluginThemeSnapshot> {
        self.plugin_themes_scoped(state, None).await
    }

    pub async fn plugin_themes_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginThemeSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let runtime = control.plugin_runtime();
        let active = runtime.active_theme().await;
        let themes = runtime.themes().await;
        Ok(PluginThemeSnapshot {
            authority_scope,
            active,
            themes,
        })
    }

    pub async fn plugin_output_styles(
        &self,
        state: &AppState,
    ) -> anyhow::Result<PluginOutputStyleSnapshot> {
        self.plugin_output_styles_scoped(state, None).await
    }

    pub async fn plugin_output_styles_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginOutputStyleSnapshot> {
        let _read = self.mutation.lock().await;
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let runtime = control.plugin_runtime();
        let active = runtime.active_output_style().await;
        let styles = runtime.output_styles().await;
        Ok(PluginOutputStyleSnapshot {
            authority_scope,
            active,
            styles,
        })
    }

    pub async fn rebind_plugin_runtime(
        &self,
        runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
        root: PathBuf,
    ) -> anyhow::Result<crate::plugin_runtime::ReloadSummary> {
        let _mutation = self.mutation.lock().await;
        runtime.rebind_workspace(root).await
    }

    pub async fn reload_plugin_lsp(
        &self,
        runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
        root: PathBuf,
    ) -> anyhow::Result<usize> {
        let _mutation = self.mutation.lock().await;
        runtime.reload_lsp_generation(root).await
    }

    pub async fn reload_plugins(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.reload_plugins_scoped(state, None).await
    }

    pub async fn reload_plugins_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("reload and settle plugins")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.reload().await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    None,
                    None,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin reload settlement task failed: {error}"),
        )
        .await
    }

    pub async fn install_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        source: &echo_agent::plugin::InstallSource,
        scope: echo_agent::plugin::PluginScope,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.install_plugin_scoped(state, None, source, scope).await
    }

    pub async fn install_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        source: &echo_agent::plugin::InstallSource,
        scope: echo_agent::plugin::PluginScope,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let source = source.clone();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("install and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let (plugin_id, mut summary) = authority.install(&source, scope).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&plugin_id).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(plugin_id),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin install settlement task failed: {error}"),
        )
        .await
    }

    pub async fn uninstall_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.uninstall_plugin_scoped(state, None, name, keep_data)
            .await
    }

    pub async fn uninstall_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        keep_data: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("uninstall and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.uninstall(&name, keep_data).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    None,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin uninstall settlement task failed: {error}"),
        )
        .await
    }

    pub async fn set_plugin_enabled(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        enabled: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.set_plugin_enabled_scoped(state, None, name, enabled)
            .await
    }

    pub async fn set_plugin_enabled_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        enabled: bool,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("toggle and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = if enabled {
                    authority.enable(&name).await?
                } else {
                    authority.disable(&name).await?
                };
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&name).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin toggle settlement task failed: {error}"),
        )
        .await
    }

    pub async fn configure_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        self.configure_plugin_scoped(state, None, name, values)
            .await
    }

    pub async fn configure_plugin_scoped(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        name: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<PluginMutationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let targets = state.extension_runtime_targets().await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        if !captured_targets_include_authority(&targets, &authority) {
            anyhow::bail!(
                "captured plugin targets do not contain the selected authority generation"
            );
        }
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("configure and settle plugin")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let mut summary = authority.configure(&name, values).await?;
                let target_receipts =
                    settle_captured_plugin_targets(&targets, &authority, &mut summary).await;
                let entry = authority.get(&name).await;
                Ok(plugin_mutation_receipt(
                    authority_scope,
                    &authority,
                    Some(name),
                    entry,
                    summary,
                    target_receipts,
                )
                .await)
            },
            |error| anyhow::anyhow!("Plugin configuration settlement task failed: {error}"),
        )
        .await
    }

    /// Admit scaffold writes into the shared Extension and ProductData
    /// lifecycle. Dropping the caller cannot abort an accepted settlement.
    pub async fn scaffold_plugin(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        directory: String,
        name: String,
    ) -> anyhow::Result<crate::plugin_runtime::PluginScaffoldResult> {
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("scaffold plugin artifact")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow.clone(),
            async move {
                let _mutation = service.mutation.lock().await;
                flow.run("write plugin scaffold", move || {
                    crate::plugin_runtime::PluginRuntimeService::scaffold(directory, &name)
                })
                .await
                .map_err(anyhow::Error::new)
                .and_then(|result| result)
            },
            |error| anyhow::anyhow!("Plugin scaffold settlement task failed: {error}"),
        )
        .await
    }

    /// Validate plugin artifacts through EKO's bounded filesystem I/O owner.
    pub async fn validate_plugin(
        &self,
        state: &AppState,
        directory: String,
    ) -> anyhow::Result<crate::plugin_runtime::PluginValidationReport> {
        let _read = self.mutation.lock().await;
        state
            .session
            .product_data_io
            .run("validate plugin artifact", move || {
                crate::plugin_runtime::PluginRuntimeService::validate(directory)
            })
            .await
            .map_err(anyhow::Error::new)
    }

    pub async fn activate_output_style(
        self: &Arc<Self>,
        state: &AppState,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<()>> {
        self.activate_output_style_scoped(state, None, name).await
    }

    pub async fn activate_output_style_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<()>> {
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        let name = name.map(str::to_string);
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("activate plugin output style")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                authority.activate_output_style(name.as_deref()).await?;
                Ok(PluginPreferenceReceipt {
                    authority_scope,
                    active: name,
                    value: (),
                })
            },
            |error| anyhow::anyhow!("Plugin output-style settlement task failed: {error}"),
        )
        .await
    }

    pub async fn activate_theme(
        self: &Arc<Self>,
        state: &AppState,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<Option<crate::plugin_runtime::PluginThemeDefinition>>>
    {
        self.activate_theme_scoped(state, None, name).await
    }

    pub async fn activate_theme_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        name: Option<&str>,
    ) -> anyhow::Result<PluginPreferenceReceipt<Option<crate::plugin_runtime::PluginThemeDefinition>>>
    {
        let control = self.scoped_context(state, runtime).await?;
        let authority_scope = control
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let authority = control.plugin_runtime();
        let name = name.map(str::to_string);
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("activate plugin theme")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let value = authority.activate_theme(name.as_deref()).await?;
                Ok(PluginPreferenceReceipt {
                    authority_scope,
                    active: name,
                    value,
                })
            },
            |error| anyhow::anyhow!("Plugin theme settlement task failed: {error}"),
        )
        .await
    }

    pub async fn publish_curated_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        runtime: Option<&ScopedChatRuntime>,
        generation: crate::evolution::ReviewGenerationLease,
        name: &str,
    ) -> anyhow::Result<CuratedSkillPublicationReceipt> {
        let control = self.scoped_context(state, runtime).await?;
        let authority = control.plugin_runtime();
        let agent = control.runtime().primary_agent();
        let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("promote and publish curated skill")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let outcome: anyhow::Result<CuratedSkillPublicationReceipt> = async {
                let _mutation = service.mutation.lock().await;
                let _control = control;
                let artifact_name = name.clone();
                let artifact = flow
                    .run("promote curated skill artifact", move || {
                        promote_curated_skill_artifact(echo_agent_dir, &artifact_name)
                    })
                    .await
                    .map_err(anyhow::Error::new)?
                    .map_err(anyhow::Error::msg)?;
                let runtime_publication = authority
                    .enable_application_skill(
                        name.clone(),
                        artifact.load_root,
                        format!("eko:curated-skill:{name}"),
                    )
                    .await;
                let receipt = match runtime_publication {
                    Ok(mut loaded_entries) => {
                        loaded_entries.sort();
                        loaded_entries.dedup();
                        crate::evolution::fire_evolution_hook(
                            &agent,
                            echo_agent::hooks::HookEvent::SkillLifecycleTransition,
                            &name,
                        )
                        .await;
                        CuratedSkillPublicationReceipt {
                            name,
                            active_path: artifact.active_path,
                            durable_committed: true,
                            idempotent: artifact.idempotent,
                            status: SkillSettlementStatus::Settled,
                            loaded_entries,
                            runtime_error: None,
                        }
                    }
                    Err(error) => CuratedSkillPublicationReceipt {
                        name,
                        active_path: artifact.active_path,
                        durable_committed: true,
                        idempotent: artifact.idempotent,
                        status: SkillSettlementStatus::Degraded,
                        loaded_entries: Vec::new(),
                        runtime_error: Some(error.to_string()),
                    },
                };
                let _generation = generation;
                Ok(receipt)
            }
            .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => receipt
                    .runtime_error
                    .clone()
                    .or_else(|| Some("curated Skill runtime publication degraded".to_string())),
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| {
            anyhow::anyhow!("Curated skill publication settlement task failed: {error}")
        })?
    }

    pub async fn replace_mcp_config(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        config: echo_agent::mcp::McpConfigFile,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("replace and settle MCP config")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state.replace_mcp_config_owned(&targets, config).await?;
                state.plugins.mcp_health.write().await.clear();
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn upsert_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: String,
        entry: echo_agent::mcp::McpServerEntry,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("upsert and settle MCP server")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .upsert_mcp_server_owned(&targets, name.clone(), entry)
                    .await?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn remove_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<u64, crate::mcp_config_runtime::McpConfigRuntimeError> {
        let targets = state.extension_runtime_targets().await.map_err(|error| {
            crate::mcp_config_runtime::McpConfigRuntimeError::Validation(error.to_string())
        })?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("remove and settle MCP server")
            .map_err(|error| {
                crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask(error.to_string())
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state.remove_mcp_server_owned(&targets, &name).await?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            crate::mcp_config_runtime::McpConfigRuntimeError::MutationTask,
        )
        .await
    }

    pub async fn list_skills(&self, state: &AppState) -> anyhow::Result<Vec<ExtensionSkillEntry>> {
        self.list_skills_scoped(state, None).await
    }

    pub async fn list_skills_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<ExtensionSkillEntry>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        let loaded = context
            .runtime()
            .primary_agent()
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .iter()
                    .map(|descriptor| descriptor.name.clone())
                    .collect::<std::collections::HashSet<_>>()
            })
            .await;
        let mut hub = state.skills_hub.write().await;
        hub.refresh();
        let mut entries = hub
            .list()
            .into_iter()
            .map(|entry| ExtensionSkillEntry {
                loaded: loaded.contains(&entry.name),
                catalog: entry.clone(),
            })
            .collect::<Vec<_>>();
        drop(hub);

        let builtin_hub = crate::skills_hub::SkillsHub::with_root(
            crate::skills_hub::builtin_skills_root(),
        );
        let enabled_config = crate::skills_hub::EnabledSkillsConfig::load(
            &crate::data_root::user_data_path("enabled-skills.json"),
        )
        .ok();
        for entry in builtin_hub.list() {
            let builtin_active = enabled_config
                .as_ref()
                .is_some_and(|config| config.is_enabled(&entry.name));
            let builtin_entry = ExtensionSkillEntry {
                loaded: loaded.contains(&entry.name),
                catalog: entry.clone(),
            };
            if let Some(existing) = entries
                .iter_mut()
                .find(|current| current.catalog.name == entry.name)
            {
                if builtin_active {
                    *existing = builtin_entry;
                }
            } else {
                entries.push(builtin_entry);
            }
        }
        entries.sort_by(|left, right| left.catalog.name.cmp(&right.catalog.name));
        Ok(entries)
    }

    pub async fn enable_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.set_skill_enabled_with_operation(state, &uuid::Uuid::new_v4().to_string(), name, true)
            .await
    }

    pub async fn disable_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.set_skill_enabled_with_operation(state, &uuid::Uuid::new_v4().to_string(), name, false)
            .await
    }

    /// 直写直同步(2026-09 简化):锁内 读取 config → 修改条目 → 原子写 →
    /// reconcile 到所有运行时目标。不再做 operation identity 去重与
    /// generation CAS;重复操作天然幂等(条目值不变 → idempotent=true)。
    pub async fn set_skill_enabled_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        name: &str,
        enabled: bool,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle enabled skills mutation")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let operation_id = operation_id.to_string();
        let name = name.to_string();
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome = async {
                let mut config =
                    read_enabled_skills_config(&flow, service.enabled_config_path.clone()).await?;
                let idempotent = config.is_enabled(&name) == enabled
                    && config.skills.contains_key(&name);
                let category = match config.skills.get(&name) {
                    Some(entry) if entry.category != "builtin" => entry.category.clone(),
                    _ => skill_entry(&state, &name)
                        .await
                        .map(|(_, category)| category)
                        .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?,
                };
                match config.skills.get_mut(&name) {
                    Some(entry) => entry.enabled = enabled,
                    None => {
                        config.skills.insert(
                            name.clone(),
                            SkillEnableEntry {
                                category,
                                enabled,
                                baseline: false,
                            },
                        );
                    }
                }
                write_enabled_skills_config(&flow, service.enabled_config_path.clone(), config)
                    .await?;
                service.reconcile_skill_runtimes(&state, &flow, operation_id, idempotent).await
            }
            .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some("skill policy committed but runtime reconciliation is degraded".to_string())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    pub async fn refresh_enabled_skills(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.refresh_enabled_skills_with_operation(
            state,
            &format!("refresh-{}", uuid::Uuid::new_v4()),
        )
        .await
    }

    pub async fn refresh_enabled_skills_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("refresh enabled skills")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let operation_id = operation_id.to_string();
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome = service
                .reconcile_skill_runtimes(&state, &flow, operation_id, true)
                .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some("skill runtime reconciliation is degraded".to_string())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    /// Restart and workspace-load owners call the same reconciliation path as
    /// explicit refresh.
    pub async fn reconcile_enabled_skills_on_load(
        self: &Arc<Self>,
        state: &Arc<AppState>,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        self.refresh_enabled_skills(state).await
    }

    /// 把 enabled-skills.json 的当前状态同步到所有运行时目标:
    /// 每个 target 先 reconcile 内置目录,再对齐用户/插件 skill。
    async fn reconcile_skill_runtimes(
        &self,
        state: &Arc<AppState>,
        flow: &crate::product_data_io::ProductDataIoFlow,
        operation_id: String,
        idempotent: bool,
    ) -> Result<SkillSyncReceipt, SkillMutationError> {
        let skill_root = state.skills_hub.read().await.root().to_path_buf();
        let config = read_enabled_skills_config(flow, self.enabled_config_path.clone()).await?;
        let desired_config = config.clone();
        let desired_root = skill_root.clone();
        let desired = flow
            .run("resolve enabled skill catalog", move || {
                desired_skill_entries(&desired_config, desired_root)
            })
            .await
            .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?;

        let mut target_receipts = Vec::new();
        match state.extension_runtime_targets().await {
            Ok(targets) => {
                for target in targets.iter() {
                    let workspace_generation = target.workspace_generation().to_string();
                    let builtin_result = target
                        .pool()
                        .reconcile_builtin_skills(
                            crate::skills_hub::builtin_skills_root(),
                            self.enabled_config_path.clone(),
                        )
                        .await;
                    let receipt = match builtin_result {
                        Err(error) => SkillTargetSettlementReceipt {
                            target: target.scope().to_string(),
                            workspace_generation,
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(error),
                        },
                        Ok(mut changed_entries) => {
                            match reconcile_target_skills(target, &desired, &skill_root).await {
                            Ok(user_changed_entries) => {
                                changed_entries.extend(user_changed_entries);
                                changed_entries.sort();
                                changed_entries.dedup();
                                SkillTargetSettlementReceipt {
                                    target: target.scope().to_string(),
                                    workspace_generation,
                                    status: SkillTargetSettlementStatus::Settled,
                                    changed_entries,
                                    error: None,
                                }
                            }
                            Err(error) => SkillTargetSettlementReceipt {
                                target: target.scope().to_string(),
                                workspace_generation,
                                status: SkillTargetSettlementStatus::Degraded,
                                changed_entries,
                                error: Some(error.to_string()),
                            },
                            }
                        }
                    };
                    target_receipts.push(receipt);
                }
            }
            Err(error) => target_receipts.push(SkillTargetSettlementReceipt {
                target: "runtime-targets".to_string(),
                workspace_generation: "unknown".to_string(),
                status: SkillTargetSettlementStatus::Degraded,
                changed_entries: Vec::new(),
                error: Some(error.to_string()),
            }),
        }
        let status = if target_receipts
            .iter()
            .any(|receipt| receipt.status == SkillTargetSettlementStatus::Degraded)
        {
            SkillSettlementStatus::Degraded
        } else {
            SkillSettlementStatus::Settled
        };
        let runtime_changed = target_receipts
            .iter()
            .any(|receipt| !receipt.changed_entries.is_empty());
        Ok(SkillSyncReceipt {
            operation_id,
            idempotent: idempotent && !runtime_changed,
            status,
            target_receipts,
        })
    }

    /// Extension settlement is part of the application ProductData lifecycle;
    /// these methods deliberately do not create a second shutdown supervisor.
    pub fn begin_shutdown(&self, state: &AppState) -> Result<(), String> {
        state.session.product_data_io.begin_shutdown()
    }

    pub async fn join_shutdown(&self, state: &AppState) -> Result<(), String> {
        state.session.product_data_io.join_shutdown().await
    }

    pub async fn install_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        source: &str,
    ) -> Result<SkillInstallSettlementReceipt, SkillInstallError> {
        self.install_skill_with_operation(state, &uuid::Uuid::new_v4().to_string(), source)
            .await
    }

    pub async fn install_skill_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        source: &str,
    ) -> Result<SkillInstallSettlementReceipt, SkillInstallError> {
        if operation_id.trim().is_empty() {
            return Err(SkillInstallError::Enable(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            )));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("install and settle enabled skill")
            .map_err(|error| {
                SkillInstallError::Enable(SkillMutationError::Admission(error.to_string()))
            })?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let source = source.to_string();
        let operation_id = operation_id.to_string();
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: Result<
                (crate::skills_hub::install::InstallResult, SkillSyncReceipt),
                SkillInstallError,
            > = async {
                let root = state.skills_hub.read().await.root().to_path_buf();
                let mut hub = SkillsHub::with_root(root);
                let installed = if source.starts_with("http://")
                    || source.starts_with("https://")
                    || source.ends_with(".git")
                {
                    crate::skills_hub::install::install_from_git(&source, None, &mut hub)
                        .await
                        .map_err(SkillInstallError::Install)?
                } else {
                    crate::skills_hub::install::install_from_local(
                        PathBuf::from(&source).as_path(),
                        &mut hub,
                    )
                    .map_err(SkillInstallError::Install)?
                };
                // 安装即启用:写入 enabled 条目并 reconcile 运行时。
                let mut config = read_enabled_skills_config(
                    &flow,
                    service.enabled_config_path.clone(),
                )
                .await
                .map_err(SkillInstallError::Enable)?;
                for installed_name in &installed.installed_names {
                    config.skills.insert(
                        installed_name.clone(),
                        SkillEnableEntry {
                            category: "user".to_string(),
                            enabled: true,
                            baseline: false,
                        },
                    );
                }
                write_enabled_skills_config(&flow, service.enabled_config_path.clone(), config)
                    .await
                    .map_err(SkillInstallError::Enable)?;
                let receipt = service
                    .reconcile_skill_runtimes(&state, &flow, operation_id, false)
                    .await
                    .map_err(SkillInstallError::Enable)?;
                Ok((installed, receipt))
            }
            .await;
            let failure = match &outcome {
                Ok((_, receipt)) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some("installed skill runtime reconciliation is degraded".to_string())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome.map(|(installed, settlement)| SkillInstallSettlementReceipt {
                name: installed.name,
                installed_names: installed.installed_names,
                path: installed.path,
                source: installed.source,
                revision: installed.revision,
                settlement,
            })
        })
        .await
        .map_err(|error| {
            SkillInstallError::Enable(SkillMutationError::SettlementTask(error.to_string()))
        })?
    }

    pub async fn uninstall_skill(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<SkillUninstallSettlementReceipt, SkillMutationError> {
        self.uninstall_skill_with_operation(state, &uuid::Uuid::new_v4().to_string(), name)
            .await
    }

    pub async fn uninstall_skill_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        name: &str,
    ) -> Result<SkillUninstallSettlementReceipt, SkillMutationError> {
        if operation_id.trim().is_empty() {
            return Err(SkillMutationError::Admission(
                "operation_id must not be empty".to_string(),
            ));
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("uninstall and settle enabled skill")
            .map_err(|error| SkillMutationError::Admission(error.to_string()))?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let name = name.to_string();
        let operation_id = operation_id.to_string();
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: Result<SkillUninstallSettlementReceipt, SkillMutationError> = async {
                let mut config =
                    read_enabled_skills_config(&flow, service.enabled_config_path.clone()).await?;
                let policy_changed = config.skills.remove(&name).is_some();
                write_enabled_skills_config(&flow, service.enabled_config_path.clone(), config)
                    .await?;
                let mut settlement = service
                    .reconcile_skill_runtimes(&state, &flow, operation_id, !policy_changed)
                    .await?;
                let root = state.skills_hub.read().await.root().to_path_buf();
                let uninstall_name = name.clone();
                let artifact = flow
                    .run("remove disabled skill artifact", move || {
                        remove_skill_artifact(root, &uninstall_name)
                    })
                    .await;
                let (artifact_removed, artifact_error) = match artifact {
                    Ok(Ok(removed)) => (removed, None),
                    Ok(Err(error)) => (false, Some(error)),
                    Err(error) => (false, Some(error.to_string())),
                };
                settlement.idempotent = !policy_changed && !artifact_removed;
                if let Some(error) = artifact_error.as_ref() {
                    settlement.status = SkillSettlementStatus::Degraded;
                    settlement
                        .target_receipts
                        .push(SkillTargetSettlementReceipt {
                            target: format!("skill-artifact:{name}"),
                            workspace_generation: "global".to_string(),
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(error.clone()),
                        });
                }
                Ok(SkillUninstallSettlementReceipt {
                    name,
                    artifact_removed,
                    artifact_error,
                    settlement,
                })
            }
            .await;
            let failure = match &outcome {
                Ok(receipt) if receipt.settlement.status == SkillSettlementStatus::Degraded => {
                    Some("uninstalled skill runtime reconciliation is degraded".to_string())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome
        })
        .await
        .map_err(|error| SkillMutationError::SettlementTask(error.to_string()))?
    }

    pub async fn sync_skills(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        target: Option<&str>,
        force: bool,
    ) -> anyhow::Result<SkillArtifactSyncReceipt> {
        self.sync_skills_with_operation(
            state,
            &format!("sync-{}", uuid::Uuid::new_v4()),
            target,
            force,
        )
        .await
    }

    pub async fn sync_skills_with_operation(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        operation_id: &str,
        target: Option<&str>,
        force: bool,
    ) -> anyhow::Result<SkillArtifactSyncReceipt> {
        if operation_id.trim().is_empty() {
            anyhow::bail!("operation_id must not be empty");
        }
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("sync and settle enabled skills")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        let target = target.map(str::to_string);
        let operation_id = operation_id.to_string();
        tokio::spawn(async move {
            let _mutation = service.mutation.lock().await;
            let outcome: anyhow::Result<(
                Vec<crate::skills_hub::install::SkillSyncResult>,
                SkillSyncReceipt,
            )> = async {
                let root = state.skills_hub.read().await.root().to_path_buf();
                let mut hub = SkillsHub::with_root(root);
                let results = crate::skills_hub::sync_skills(&mut hub, target.as_deref(), force)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let idempotent = results.iter().all(|result| !result.updated);
                let mut receipt = service
                    .reconcile_skill_runtimes(&state, &flow, operation_id, idempotent)
                    .await
                    .map_err(anyhow::Error::new)?;
                let failures = results
                    .iter()
                    .filter(|result| !result.success)
                    .collect::<Vec<_>>();
                if !failures.is_empty() {
                    receipt.status = SkillSettlementStatus::Degraded;
                    receipt.target_receipts.extend(
                        failures.iter().map(|result| SkillTargetSettlementReceipt {
                            target: format!("skill-artifact-sync:{}", result.name),
                            workspace_generation: "global".to_string(),
                            status: SkillTargetSettlementStatus::Degraded,
                            changed_entries: Vec::new(),
                            error: Some(result.message.clone()),
                        }),
                    );
                }
                Ok((results, receipt))
            }
            .await;
            let failure = match &outcome {
                Ok((_, receipt)) if receipt.status == SkillSettlementStatus::Degraded => {
                    Some("synced skill runtime reconciliation is degraded".to_string())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            flow.settle(failure);
            outcome.map(|(results, settlement)| SkillArtifactSyncReceipt {
                results: results
                    .into_iter()
                    .map(|result| SkillArtifactSyncResult {
                        name: result.name,
                        success: result.success,
                        updated: result.updated,
                        revision: result.revision,
                        message: result.message,
                    })
                    .collect(),
                settlement,
            })
        })
        .await
        .map_err(|error| anyhow::anyhow!("Skill sync settlement task failed: {error}"))?
    }

    pub async fn list_hooks(&self, state: &AppState) -> anyhow::Result<Vec<HookSourceSnapshot>> {
        self.list_hooks_scoped(state, None).await
    }

    pub async fn list_hooks_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<HookSourceSnapshot>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        Ok(context
            .runtime()
            .primary_agent()
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .hook_registry()
                        .read()
                        .await
                        .list_sources()
                        .into_iter()
                        .map(|(source, rules)| HookSourceSnapshot { source, rules })
                        .collect()
                })
            })
            .await)
    }

    pub async fn reload_hooks(
        self: &Arc<Self>,
        state: &AppState,
    ) -> anyhow::Result<HookReloadReceipt> {
        self.reload_hooks_scoped(state, None).await
    }

    pub async fn reload_hooks_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<HookReloadReceipt> {
        let context = self.scoped_context(state, runtime).await?;
        let config_path = state
            .config_watcher
            .as_ref()
            .and_then(|watcher| watcher.config_path())
            .unwrap_or_else(|| state.config.config_path.clone());
        let project_root = context.project_root().to_path_buf();
        let agent = context.runtime().primary_agent();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("reload and settle hooks")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = context;
                service
                    .reload_hooks_target_locked(Some(config_path), project_root, agent, true)
                    .await
            },
            |error| anyhow::anyhow!("Hook reload settlement task failed: {error}"),
        )
        .await
    }

    pub(crate) async fn reload_hooks_for_target(
        &self,
        config_path: Option<PathBuf>,
        project_root: PathBuf,
        agent: crate::agent_handle::AgentHandle,
        preserve_on_error: bool,
    ) -> anyhow::Result<HookReloadReceipt> {
        let _mutation = self.mutation.lock().await;
        self.reload_hooks_target_locked(config_path, project_root, agent, preserve_on_error)
            .await
    }

    async fn reload_hooks_target_locked(
        &self,
        config_path: Option<PathBuf>,
        project_root: PathBuf,
        agent: crate::agent_handle::AgentHandle,
        preserve_on_error: bool,
    ) -> anyhow::Result<HookReloadReceipt> {
        let load_config = config_path.clone();
        let load_root = project_root.clone();
        let mut loaded = tokio::task::spawn_blocking(move || {
            HookConfigLoader::load_merged_from_disk_for_workspace(
                load_config.as_deref(),
                Some(load_root.as_path()),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Hook loader task failed: {error}"))?;
        let mut degraded_errors = Vec::new();
        if !loaded.errors.is_empty() && !preserve_on_error {
            degraded_errors = std::mem::take(&mut loaded.errors);
            let fallback_config = config_path;
            let fallback = tokio::task::spawn_blocking(move || {
                HookConfigLoader::load_merged_from_disk_for_workspace(
                    fallback_config.as_deref(),
                    None,
                )
            })
            .await
            .map_err(|error| anyhow::anyhow!("Hook fallback loader task failed: {error}"))?;
            if fallback.errors.is_empty() {
                loaded.definition = fallback.definition;
                loaded.loaded_from = fallback.loaded_from;
            } else {
                loaded.definition = Default::default();
                degraded_errors.extend(fallback.errors);
            }
        }
        ensure_hook_load_succeeded(&loaded)?;
        let rule_count = loaded.definition.rules.values().map(Vec::len).sum();
        let definition = loaded.definition;
        agent
            .write_async(|agent| {
                Box::pin(async move {
                    let mut registry = agent.hook_registry().write().await;
                    registry.clear_user_hooks();
                    if !definition.is_empty() {
                        registry.register_user_hooks(definition);
                    }
                })
            })
            .await;
        let receipt = HookReloadReceipt {
            loaded_from: loaded.loaded_from,
            rule_count,
        };
        if degraded_errors.is_empty() {
            Ok(receipt)
        } else {
            Err(anyhow::anyhow!(degraded_errors.join("; ")))
        }
    }

    pub async fn list_mcp_servers(
        &self,
        state: &AppState,
    ) -> anyhow::Result<Vec<ExtensionMcpServer>> {
        self.list_mcp_servers_scoped(state, None).await
    }

    pub async fn list_mcp_servers_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<Vec<ExtensionMcpServer>> {
        let _read = self.mutation.lock().await;
        let context = self.scoped_context(state, runtime).await?;
        let scope = mcp_health_scope_key(context.runtime())?;
        let config = state.plugins.mcp_config.snapshot().await;
        let health = state
            .plugins
            .mcp_health
            .read()
            .await
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        let agent = context.runtime().primary_agent();
        let mut connected = agent
            .read(|agent| agent.list_mcp_servers().into_iter().collect::<Vec<_>>())
            .await;
        connected.sort();
        let mut names = connected.clone();
        names.extend(config.mcp_servers.keys().cloned());
        names.sort();
        names.dedup();
        let mut servers = Vec::with_capacity(names.len());
        for name in names {
            let configured = config.mcp_servers.get(&name);
            let is_connected = connected.contains(&name);
            let health_entry = health.get(&name);
            let tools = agent
                .read(|agent| {
                    agent
                        .mcp_client(&name)
                        .map(|client| {
                            client
                                .tools()
                                .iter()
                                .map(|tool| ExtensionMcpTool {
                                    name: tool.name.clone(),
                                    description: tool.description.clone().unwrap_or_default(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .await;
            let tool_count = tools.len();
            let status = if configured.is_some_and(|entry| entry.disabled) {
                "disabled"
            } else if health_entry.is_some_and(|entry| !entry.healthy) {
                "error"
            } else if is_connected {
                "connected"
            } else {
                "disconnected"
            };
            servers.push(ExtensionMcpServer {
                name,
                status: status.to_string(),
                transport: configured
                    .map(mcp_transport)
                    .unwrap_or("plugin")
                    .to_string(),
                tool_count,
                tools,
                connected_at: None,
                error: health_entry.and_then(|entry| entry.error.clone()),
                enabled: configured.is_none_or(|entry| !entry.disabled),
            });
        }
        Ok(servers)
    }

    pub async fn connect_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> anyhow::Result<u64> {
        let targets = state.extension_runtime_targets().await?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("connect and settle MCP server")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .set_mcp_server_enabled_owned(&targets, &name, true)
                    .await
                    .map_err(anyhow::Error::new)?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            |error| anyhow::anyhow!("MCP connect settlement task failed: {error}"),
        )
        .await
    }

    pub async fn disconnect_mcp_server(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> anyhow::Result<u64> {
        let targets = state.extension_runtime_targets().await?;
        let name = name.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("disconnect and settle MCP server")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        let state = Arc::clone(state);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let generation = state
                    .set_mcp_server_enabled_owned(&targets, &name, false)
                    .await
                    .map_err(anyhow::Error::new)?;
                service.clear_mcp_health_for_server(&state, &name).await;
                Ok(generation)
            },
            |error| anyhow::anyhow!("MCP disconnect settlement task failed: {error}"),
        )
        .await
    }

    async fn clear_mcp_health_for_server(&self, state: &AppState, name: &str) {
        let mut health = state.plugins.mcp_health.write().await;
        for scoped in health.values_mut() {
            scoped.remove(name);
        }
    }

    pub async fn refresh_current_mcp_health(&self, state: &AppState) -> anyhow::Result<()> {
        let _mutation = self.mutation.lock().await;
        let context = self.context(state).await?;
        let scope = mcp_health_scope_key(context.runtime())?;
        let agent = context.runtime().primary_agent();
        let names = agent
            .read(|agent| agent.list_mcp_servers().into_iter().collect::<Vec<_>>())
            .await;
        let now = chrono::Utc::now();
        let mut scoped = HashMap::new();
        for name in names {
            let healthy = agent.read(|agent| agent.mcp_client(&name).is_some()).await;
            scoped.insert(
                name.clone(),
                McpHealthStatus {
                    name,
                    healthy,
                    last_check: Some(now),
                    error: (!healthy).then(|| "MCP client is unavailable".to_string()),
                },
            );
        }
        state.plugins.mcp_health.write().await.insert(scope, scoped);
        Ok(())
    }

    pub async fn lsp_command(
        self: &Arc<Self>,
        state: &AppState,
        action: &str,
        language: Option<&str>,
    ) -> anyhow::Result<String> {
        self.lsp_command_scoped(state, None, action, language).await
    }

    pub async fn lsp_command_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        action: &str,
        language: Option<&str>,
    ) -> anyhow::Result<String> {
        let context = self.scoped_context(state, runtime).await?;
        let specialist = context.plugin_runtime();
        match action {
            "list" | "ls" => {
                let _read = self.mutation.lock().await;
                let languages = specialist.lsp_configured_languages().await;
                Ok(if languages.is_empty() {
                    "No language servers are configured.".to_string()
                } else {
                    languages.join("\n")
                })
            }
            "status" | "" => {
                let _read = self.mutation.lock().await;
                let statuses = specialist.lsp_status().await;
                Ok(if statuses.is_empty() {
                    "No language servers are configured.".to_string()
                } else {
                    statuses
                        .into_iter()
                        .map(|status| {
                            let state = if status.running && status.initialized {
                                "ready"
                            } else if status.running {
                                "starting"
                            } else {
                                "stopped"
                            };
                            status.last_error.map_or_else(
                                || format!("{}: {state}", status.language),
                                |error| format!("{}: {state} ({error})", status.language),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            "start" | "stop" | "restart" => {
                let language = language
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("lsp {action} requires a language"))?
                    .to_string();
                let action = action.to_string();
                let flow = state
                    .session
                    .product_data_io
                    .begin_owned_flow("settle LSP control mutation")
                    .map_err(anyhow::Error::new)?;
                let service = Arc::clone(self);
                await_owned_extension_settlement(
                    flow,
                    async move {
                        let _mutation = service.mutation.lock().await;
                        let _control = context;
                        if action == "start" {
                            specialist.lsp_start(language.clone()).await?;
                        } else if action == "stop" {
                            specialist.lsp_stop(language.clone()).await?;
                        } else {
                            specialist.lsp_restart(language.clone()).await?;
                        }
                        Ok(format!("Language server '{language}' {action}ed."))
                    },
                    |error| anyhow::anyhow!("LSP settlement task failed: {error}"),
                )
                .await
            }
            _ => anyhow::bail!("usage: lsp <list|status|start|stop|restart> [language]"),
        }
    }

    pub async fn browser_command(
        self: &Arc<Self>,
        state: &AppState,
        conversation_id: &str,
        args: &[&str],
    ) -> anyhow::Result<String> {
        self.browser_command_scoped(state, None, conversation_id, args)
            .await
    }

    pub async fn browser_command_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        conversation_id: &str,
        args: &[&str],
    ) -> anyhow::Result<String> {
        let action = args.first().copied().unwrap_or("status");
        match action {
            "status" => {
                let status = self.browser_status_scoped(state, runtime).await?;
                Ok(format!(
                    "Browser extension: {}; token: {}",
                    if status.connected {
                        "connected"
                    } else {
                        "disconnected"
                    },
                    if status.token_configured {
                        "configured"
                    } else {
                        "missing"
                    }
                ))
            }
            "stop" => {
                self.browser_stop_scoped(state, runtime).await?;
                Ok("Browser stop completed.".to_string())
            }
            _ => {
                let (browser_action, parameters) = browser_specialist_action(action, args)?;
                self.execute_browser_action_scoped(
                    state,
                    runtime,
                    conversation_id,
                    browser_action,
                    parameters,
                )
                .await?;
                Ok(format!("Browser {action} completed."))
            }
        }
    }

    pub async fn browser_status_scoped(
        &self,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<crate::browser::BrowserExtensionStatus> {
        let _context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        Ok(browser.extension_status().await)
    }

    pub async fn browser_stop_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
    ) -> anyhow::Result<()> {
        let _context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle browser stop")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = _context;
                browser.interrupt().await;
                Ok(())
            },
            |error| anyhow::anyhow!("Browser stop settlement task failed: {error}"),
        )
        .await
    }

    pub async fn execute_browser_action_scoped(
        self: &Arc<Self>,
        state: &AppState,
        runtime: Option<&ScopedChatRuntime>,
        conversation_id: &str,
        browser_action: crate::browser::BrowserAction,
        parameters: echo_agent::prelude::ToolParameters,
    ) -> anyhow::Result<()> {
        let context = self.scoped_context(state, runtime).await?;
        let browser = state
            .browser_runtime
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Browser runtime is not initialized"))?;
        let workspace_id = context
            .runtime()
            .execution_scope()
            .workspace_id()
            .to_string();
        let workspace_root = context.runtime().execution_scope().root().to_path_buf();
        let conversation_id = conversation_id.to_string();
        let flow = state
            .session
            .product_data_io
            .begin_owned_flow("settle browser action")
            .map_err(anyhow::Error::new)?;
        let service = Arc::clone(self);
        await_owned_extension_settlement(
            flow,
            async move {
                let _mutation = service.mutation.lock().await;
                let _control = context;
                browser
                    .execute_main(
                        workspace_id,
                        workspace_root,
                        conversation_id,
                        browser_action,
                        parameters,
                        None,
                    )
                    .await?;
                Ok(())
            },
            |error| anyhow::anyhow!("Browser action settlement task failed: {error}"),
        )
        .await
    }
}
