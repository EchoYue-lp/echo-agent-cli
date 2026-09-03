#[cfg(test)]
mod reliability_contracts;

#[cfg(test)]
mod model_mutation_tests {
    use super::*;
    use crate::config::{ConfiguredModel, ModelProviderConfig};
    use echo_agent::llm::LlmApiProtocol;

    const MODEL_A: &str = "model-a";
    const MODEL_B: &str = "model-b";
    const ENDPOINT_A: &str = "http://127.0.0.1:11434/v1/chat/completions";
    const ENDPOINT_B: &str = "http://127.0.0.1:11435/v1/chat/completions";
    const ENDPOINT_C: &str = "http://127.0.0.1:11436/v1/chat/completions";
    const RESPONSES_ENDPOINT: &str = "http://127.0.0.1:11435/v1/responses";
    const WINDOW_A: usize = 120_000;
    const WINDOW_B: usize = 240_000;

    #[test]
    fn first_configured_model_becomes_the_active_generation() -> Result<(), String> {
        let mut config = crate::config::EkoConfig::default();
        config.model_providers.insert(
            "local".to_string(),
            ModelProviderConfig {
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        let mutation = prepare_model_mutation(
            &config,
            "",
            ModelMutationRequest::UpsertModel(ConfiguredModelMutation {
                model: model(MODEL_A, "local", "runtime-a", WINDOW_A as u32),
                set_default: false,
            }),
        )
        .map_err(|error| error.to_string())?;

        assert!(mutation.activated);
        assert_eq!(mutation.model_id, MODEL_A);
        assert_eq!(
            mutation.config.model.default_model_id.as_deref(),
            Some(MODEL_A)
        );
        assert_eq!(
            mutation.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        assert!(mutation.prepared.is_some());
        Ok(())
    }

    struct ModelMutationFixture {
        _temp: tempfile::TempDir,
        config_path: std::path::PathBuf,
        state: Arc<AppState>,
        pool: Arc<crate::agent_pool::AgentPool>,
        existing: AgentHandle,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AgentModelProjection {
        model: String,
        client_model: String,
        base_url: String,
        api_protocol: LlmApiProtocol,
        token_limit: usize,
    }

    fn model(id: &str, provider: &str, model: &str, context_window: u32) -> ConfiguredModel {
        ConfiguredModel {
            id: id.to_string(),
            display_name: id.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            enabled: true,
            context_window: Some(context_window),
            ..ConfiguredModel::default()
        }
    }

    fn provider_mutation(id: &str, base_url: &str) -> ModelProviderMutation {
        ModelProviderMutation {
            id: id.to_string(),
            provider: ModelProviderConfig {
                name: id.to_string(),
                base_url: Some(base_url.to_string()),
                default_api_protocol: Some(LlmApiProtocol::ChatCompletions),
                ..Default::default()
            },
            preserve_auth_token: false,
        }
    }

    fn valid_config() -> Result<crate::config::EkoConfig, String> {
        let mut config = crate::config::EkoConfig::default();
        config.model_providers.insert(
            "local-a".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        config.model_providers.insert(
            "local-b".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_B.to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            model(MODEL_A, "local-a", "runtime-a", WINDOW_A as u32),
            model(MODEL_B, "local-b", "runtime-b", WINDOW_B as u32),
        ];
        crate::model_config::set_default_model(&mut config, MODEL_A)?;
        Ok(config)
    }

    fn invalid_successor_config() -> Result<crate::config::EkoConfig, String> {
        let mut config = valid_config()?;
        let invalid = config
            .configured_models
            .iter_mut()
            .find(|model| model.id == MODEL_B)
            .ok_or_else(|| "missing invalid successor candidate".to_string())?;
        invalid.provider = "openai".to_string();
        invalid.api_protocol = LlmApiProtocol::Responses;
        config.model_providers.insert(
            "openai".to_string(),
            ModelProviderConfig {
                auth_token: Some("invalid\nheader".to_string()),
                base_url: Some("https://api.openai.com/v1/responses".to_string()),
                ..Default::default()
            },
        );
        Ok(config)
    }

    fn shared_provider_config() -> Result<crate::config::EkoConfig, String> {
        let mut config = crate::config::EkoConfig::default();
        config.model_providers.insert(
            "local-shared".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some(ENDPOINT_A.to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            model(MODEL_A, "local-shared", "runtime-a", WINDOW_A as u32),
            model(MODEL_B, "local-shared", "runtime-b", WINDOW_B as u32),
        ];
        crate::model_config::set_default_model(&mut config, MODEL_A)?;
        Ok(config)
    }

    async fn fixture(
        config: crate::config::EkoConfig,
        persistence_fails: bool,
    ) -> Result<ModelMutationFixture, String> {
        fixture_with_active(config, persistence_fails, MODEL_A).await
    }

    async fn fixture_with_active(
        config: crate::config::EkoConfig,
        persistence_fails: bool,
        active_model_id: &str,
    ) -> Result<ModelMutationFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = if persistence_fails {
            let path = temp.path().join("config-as-directory");
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            path
        } else {
            let path = temp.path().join("eko.yaml");
            crate::config::save_config_file(&path, &config)?;
            path
        };
        let created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                model: Some(active_model_id.to_string()),
                system_prompt: Some("model mutation test".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;
        let active_runtime = created
            .runtime_model
            .ok_or_else(|| "model mutation fixture did not resolve its active model".to_string())?;
        let primary_consumers = created.model_consumers;
        let primary = AgentHandle::new(created.agent);
        let session_config =
            crate::model_config::session_config_for_runtime(&config, &active_runtime)?;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::for_model_mutation_test(&primary, session_config).await,
        );
        pool.set_primary_model_consumers_for_test(primary_consumers.clone())
            .await;
        let existing_lease = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            primary,
            Some(primary_consumers),
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            config,
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_active_model_id(active_runtime.id)
        .with_config_path(config_path.clone());
        state.set_pool(pool.clone());
        Ok(ModelMutationFixture {
            _temp: temp,
            config_path,
            state: Arc::new(state),
            pool,
            existing,
        })
    }

    async fn agent_projection(handle: &AgentHandle) -> Result<AgentModelProjection, String> {
        handle
            .read(|agent| {
                let llm = agent
                    .llm_config()
                    .ok_or_else(|| "agent has no LLM config".to_string())?;
                Ok(AgentModelProjection {
                    model: llm.model.clone(),
                    client_model: agent
                        .llm_client()
                        .map(|client| client.model_name().to_string())
                        .ok_or_else(|| "agent has no prepared LLM client".to_string())?,
                    base_url: llm.base_url.clone(),
                    api_protocol: llm.api_protocol,
                    token_limit: agent.config().get_token_limit(),
                })
            })
            .await
    }

    async fn assert_live_generation(
        fixture: &ModelMutationFixture,
        model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert_eq!(snapshot.model.default_model_id.as_deref(), Some(model_id));
        drop(snapshot);
        let expected = AgentModelProjection {
            model: runtime_model.to_string(),
            client_model: runtime_model.to_string(),
            base_url: endpoint.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            token_limit: context_window,
        };
        assert_eq!(
            agent_projection(&fixture.state.connection.agent).await?,
            expected
        );
        assert_eq!(agent_projection(&fixture.existing).await?, expected);
        let new_lease = fixture
            .pool
            .acquire("new-after-mutation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(agent_projection(&new_lease.agent()).await?, expected);
        drop(new_lease);
        Ok(())
    }

    async fn assert_no_live_generation(fixture: &ModelMutationFixture) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert!(snapshot.model.default_model_id.is_none());
        assert!(snapshot.configured_models.is_empty());
        drop(snapshot);
        assert!(fixture.state.config.active_model_id.read().await.is_empty());

        for handle in [
            fixture.state.connection.agent.clone(),
            fixture.existing.clone(),
            inherited_handle(fixture)?,
        ] {
            let projection = handle
                .read(|agent| {
                    (
                        agent.model_name().to_string(),
                        agent.llm_config().is_none(),
                        agent.llm_client().is_none(),
                    )
                })
                .await;
            assert!(projection.0.is_empty());
            assert!(projection.1);
            assert!(projection.2);
        }

        let new_lease = fixture
            .pool
            .acquire("new-after-model-deactivation")
            .await
            .map_err(|error| error.to_string())?;
        let new_projection = new_lease
            .agent()
            .read(|agent| {
                (
                    agent.model_name().to_string(),
                    agent.llm_config().is_none(),
                    agent.llm_client().is_none(),
                )
            })
            .await;
        assert!(new_projection.0.is_empty());
        assert!(new_projection.1);
        assert!(new_projection.2);
        drop(new_lease);
        Ok(())
    }

    async fn assert_session_generation(
        fixture: &ModelMutationFixture,
        durable_default_id: &str,
        active_model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let snapshot = fixture.state.config.app_config.read().await;
        assert_eq!(
            snapshot.model.default_model_id.as_deref(),
            Some(durable_default_id)
        );
        drop(snapshot);
        assert_eq!(
            fixture.state.config.active_model_id.read().await.as_str(),
            active_model_id
        );
        let expected = AgentModelProjection {
            model: runtime_model.to_string(),
            client_model: runtime_model.to_string(),
            base_url: endpoint.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            token_limit: context_window,
        };
        assert_eq!(
            agent_projection(&fixture.state.connection.agent).await?,
            expected
        );
        assert_eq!(agent_projection(&fixture.existing).await?, expected);
        let new_lease = fixture
            .pool
            .acquire("new-after-session-mutation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(agent_projection(&new_lease.agent()).await?, expected);
        drop(new_lease);
        Ok(())
    }

    async fn assert_full_generation(
        fixture: &ModelMutationFixture,
        model_id: &str,
        runtime_model: &str,
        endpoint: &str,
        context_window: usize,
    ) -> Result<(), String> {
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(model_id));
        assert_live_generation(fixture, model_id, runtime_model, endpoint, context_window).await
    }

    async fn wait_for_pool_model_admission(
        pool: &crate::agent_pool::AgentPool,
    ) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if pool.transition_admission_closed_for_test() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "timed out waiting for pool model admission".to_string())?;
        Ok(())
    }

    async fn join_mutation(
        handle: tokio::task::JoinHandle<Result<ModelMutationReceipt, ModelMutationError>>,
    ) -> Result<ModelMutationReceipt, String> {
        let joined = tokio::time::timeout(std::time::Duration::from_secs(3), handle)
            .await
            .map_err(|_| "model mutation task timed out".to_string())?;
        let settled = joined.map_err(|error| error.to_string())?;
        settled.map_err(|error| error.to_string())
    }

    async fn invalidate_model_budget(handle: &AgentHandle) {
        let invalid_budget =
            echo_agent::budget::TokenBudgetConfig::enabled().with_total_window(0);
        handle
            .write(|agent| {
                let config = agent.config();
                let model = config.get_model_name().to_string();
                let name = config.get_agent_name().to_string();
                let prompt = config.get_system_prompt().to_string();
                let token_limit = config.get_token_limit();
                *agent.config_mut() = echo_agent::agent::AgentConfig::new(&model, &name, &prompt)
                    .token_limit(token_limit)
                    .token_budget(invalid_budget);
            })
            .await;
    }

    fn inherited_handle(fixture: &ModelMutationFixture) -> Result<AgentHandle, String> {
        fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())
    }

    #[tokio::test]
    async fn production_pool_does_not_reenter_primary_model_lock() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            fixture.state.set_default_model_owned(MODEL_B),
        )
        .await
        .map_err(|_| "production pool model publication deadlocked on the primary Agent".to_string())?
        .map_err(|error| error.to_string())?;

        assert_eq!(receipt.model_id, MODEL_B);
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn gui_and_tui_model_mutations_share_one_linearized_owner() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let primary = fixture.state.connection.agent.inner().clone();
        let barrier = primary.write().await;
        let gui_state = fixture.state.clone();
        let gui = tokio::spawn(async move { gui_state.set_default_model_owned(MODEL_B).await });
        wait_for_pool_model_admission(&fixture.pool).await?;

        let tui_state = fixture.state.clone();
        let tui = tokio::spawn(async move { tui_state.set_default_model_owned(MODEL_A).await });
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model
                .default_model_id
                .as_deref(),
            Some(MODEL_A)
        );
        drop(barrier);

        let gui_receipt = join_mutation(gui).await?;
        let tui_receipt = join_mutation(tui).await?;
        assert_eq!(gui_receipt.model_id, MODEL_B);
        assert_eq!(tui_receipt.model_id, MODEL_A);
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn active_model_generation_publishes_to_three_loaded_workspace_hosts()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let workspaces = tempfile::tempdir().map_err(|error| error.to_string())?;
        for position in 0..3 {
            let name = format!("model-workspace-{position}");
            let root = workspaces.path().join(&name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            fixture
                .state
                .switch_workspace(Workspace {
                    id: crate::workspace::WorkspaceId::from_name(&name),
                    name,
                    root,
                    project_root: None,
                    kind: crate::workspace::WorkspaceKind::General,
                    metadata: crate::workspace::WorkspaceMetadata::default(),
                    product_data_generation: String::new(),
                    created_at: Utc::now(),
                    last_active: Utc::now(),
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        let runtimes = fixture
            .state
            .workspace
            .runtimes
            .loaded_execution_runtimes()
            .await;
        assert_eq!(runtimes.len(), 3);

        fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;
        let expected = AgentModelProjection {
            model: "runtime-b".to_string(),
            client_model: "runtime-b".to_string(),
            base_url: ENDPOINT_B.to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            token_limit: WINDOW_B,
        };
        for (workspace_id, runtime) in runtimes {
            assert_eq!(agent_projection(&runtime.primary_agent()).await?, expected);
            let lease = runtime
                .pool()
                .acquire(&format!("future-{workspace_id}"))
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(agent_projection(&lease.agent()).await?, expected);
        }
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn tool_control_generation_reaches_loaded_and_future_workspace_agents()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let primary = fixture.state.connection.primary_agent();
        let tool_name = primary
            .read(|agent| agent.tool_names().into_iter().next())
            .await
            .ok_or_else(|| "tool-control fixture has no registered tool".to_string())?;
        let workspaces = tempfile::tempdir().map_err(|error| error.to_string())?;
        for position in 0..2 {
            let name = format!("tool-workspace-{position}");
            let root = workspaces.path().join(&name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            fixture
                .state
                .switch_workspace(Workspace {
                    id: crate::workspace::WorkspaceId::from_name(&name),
                    name,
                    root,
                    project_root: None,
                    kind: crate::workspace::WorkspaceKind::General,
                    metadata: crate::workspace::WorkspaceMetadata::default(),
                    product_data_generation: String::new(),
                    created_at: Utc::now(),
                    last_active: Utc::now(),
                })
                .await
                .map_err(|error| error.to_string())?;
        }

        let receipt = fixture
            .state
            .set_tool_enabled(&primary, &tool_name, false)
            .await
            .map_err(|error| error.to_string())?;
        assert!(receipt.changed);
        assert_eq!(receipt.revision, 1);
        assert!(!receipt.policy_enabled);
        assert!(!receipt.effective_enabled);
        let listed = fixture
            .state
            .get_tool_infos(&primary)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            listed
                .iter()
                .any(|tool| tool.name == tool_name && !tool.enabled)
        );
        for handle in [primary.clone(), fixture.existing.clone()] {
            assert!(
                crate::tool_control::snapshot_disabled_tools(&handle)
                    .await
                    .contains(&tool_name)
            );
        }
        let future_global = fixture
            .pool
            .acquire("future-tool-global")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            crate::tool_control::snapshot_disabled_tools(&future_global.agent())
                .await
                .contains(&tool_name)
        );

        let runtimes = fixture
            .state
            .workspace
            .runtimes
            .loaded_execution_runtimes()
            .await;
        assert_eq!(runtimes.len(), 2);
        for (workspace_id, runtime) in runtimes {
            assert!(
                crate::tool_control::snapshot_disabled_tools(&runtime.primary_agent())
                    .await
                    .contains(&tool_name)
            );
            let future = runtime
                .pool()
                .acquire(&format!("future-tool-{workspace_id}"))
                .await
                .map_err(|error| error.to_string())?;
            assert!(
                crate::tool_control::snapshot_disabled_tools(&future.agent())
                    .await
                    .contains(&tool_name)
            );
        }

        assert!(
            fixture
                .state
                .set_tool_enabled(&primary, "missing-tool", false)
                .await
                .is_err_and(|error| matches!(
                    error,
                    crate::tool_control::ToolControlError::NotRegistered { name }
                        if name == "missing-tool"
                ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn aborted_model_mutation_waiter_does_not_cancel_accepted_settlement()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let primary = fixture.state.connection.agent.inner().clone();
        let barrier = primary.write().await;
        let caller_state = fixture.state.clone();
        let caller =
            tokio::spawn(async move { caller_state.set_default_model_owned(MODEL_B).await });
        wait_for_pool_model_admission(&fixture.pool).await?;
        caller.abort();
        assert!(caller.await.is_err());
        drop(barrier);

        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            fixture.state.shutdown_model_mutations(),
        )
        .await
        .map_err(|_| "model mutation shutdown timed out".to_string())?
        .map_err(|error| error.to_string())?;
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn invalid_default_successor_delete_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(invalid_successor_config()?, false).await?;

        let result = fixture.state.delete_configured_model_owned(MODEL_A).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.configured_models.len(), 2);
        assert!(
            persisted
                .configured_models
                .iter()
                .any(|model| model.id == MODEL_A)
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn valid_default_successor_delete_settles_all_layers() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_A)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(receipt.activated);
        assert!(
            receipt
                .config
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_A)
        );
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_A)
        );
        assert_full_generation(&fixture, MODEL_B, "runtime-b", ENDPOINT_B, WINDOW_B).await
    }

    #[tokio::test]
    async fn omitted_subagent_model_tracks_parent_while_explicit_model_stays_fixed()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let registry = fixture
            .state
            .connection
            .agent
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let inherited_before = registry
            .get_agent("general-purpose")
            .await
            .ok_or_else(|| "inherit-parent subagent was not registered".to_string())?;
        let fixed_before = registry
            .get_agent("explorer")
            .await
            .ok_or_else(|| "explicit-model subagent was not registered".to_string())?;
        let inherited_handle = fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())?;
        let fixed_model = fixed_before.model_name().to_string();
        assert_eq!(inherited_before.model_name(), "runtime-a");

        fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        let inherited_after = registry
            .get_agent("general-purpose")
            .await
            .ok_or_else(|| "inherit-parent subagent was not refreshed".to_string())?;
        let fixed_after = registry
            .get_agent("explorer")
            .await
            .ok_or_else(|| "explicit-model subagent disappeared".to_string())?;
        assert_eq!(inherited_after.model_name(), "runtime-b");
        assert_eq!(fixed_after.model_name(), fixed_model);
        assert_eq!(
            agent_projection(&inherited_handle).await?,
            AgentModelProjection {
                model: "runtime-b".to_string(),
                client_model: "runtime-b".to_string(),
                base_url: ENDPOINT_B.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_B,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn model_mutation_preserves_primary_custom_critic() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let custom = Arc::new(echo_agent::agent::critic::StaticCritic::always_pass());
        fixture
            .state
            .connection
            .agent
            .write(|agent| agent.set_critic(custom.clone()))
            .await;

        fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(Arc::strong_count(&custom), 2);
        assert_eq!(
            fixture
                .state
                .connection
                .agent
                .read(|agent| agent.critic_owner().map(str::to_string))
                .await,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_last_default_deactivates_every_model_consumer() -> Result<(), String> {
        let mut config = valid_config()?;
        config.configured_models.retain(|model| model.id == MODEL_A);
        let fixture = fixture(config, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_A)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.runtime.is_none());
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert!(persisted.configured_models.is_empty());
        assert!(persisted.model.default_model_id.is_none());
        assert_no_live_generation(&fixture).await
    }

    #[tokio::test]
    async fn deleting_provider_cascades_its_models_and_deactivates_every_consumer()
    -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_model_provider_owned("local-shared")
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.config.model_providers.is_empty());
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert!(persisted.model_providers.is_empty());
        assert_no_live_generation(&fixture).await
    }

    #[tokio::test]
    async fn persistence_failure_rolls_back_snapshot_primary_and_pool() -> Result<(), String> {
        let fixture = fixture(valid_config()?, true).await?;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Persistence(_))));
        assert!(fixture.config_path.is_dir());
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn failed_settlement_is_stable_for_later_mutations_and_repeated_shutdown()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, true).await?;
        let first = fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .err()
            .ok_or_else(|| "persistence unexpectedly succeeded".to_string())?
            .to_string();
        let second = fixture
            .state
            .set_default_model_owned(MODEL_A)
            .await
            .err()
            .ok_or_else(|| "later mutation ignored failed settlement".to_string())?
            .to_string();
        assert_eq!(second, first);

        let first_shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "shutdown lost the settlement failure".to_string())?
            .to_string();
        let second_shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost the settlement failure".to_string())?
            .to_string();
        assert_eq!(first_shutdown, first);
        assert_eq!(second_shutdown, first);
        Ok(())
    }

    #[tokio::test]
    async fn join_error_is_stable_for_later_mutations_and_repeated_shutdown() -> Result<(), String>
    {
        let fixture = fixture(valid_config()?, false).await?;
        let first = fixture
            .state
            .run_owned_model_mutation(ModelMutationRequest::AbortSettlementForTest)
            .await
            .err()
            .ok_or_else(|| "aborted settlement unexpectedly succeeded".to_string())?;
        assert!(matches!(first, ModelMutationError::Settlement(_)));
        let first = first.to_string();

        let later = fixture
            .state
            .set_default_model_owned(MODEL_B)
            .await
            .err()
            .ok_or_else(|| "later mutation ignored JoinError".to_string())?
            .to_string();
        let shutdown = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "shutdown lost JoinError".to_string())?
            .to_string();
        let repeated = fixture
            .state
            .shutdown_model_mutations()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost JoinError".to_string())?
            .to_string();
        assert_eq!(later, first);
        assert_eq!(shutdown, first);
        assert_eq!(repeated, first);
        Ok(())
    }

    #[tokio::test]
    async fn later_pool_agent_prepare_failure_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let failing_lease = fixture
            .pool
            .acquire("z-failing")
            .await
            .map_err(|error| error.to_string())?;
        let failing = failing_lease.agent();
        drop(failing_lease);
        invalidate_model_budget(&failing).await;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Publication(_))));
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            agent_projection(&failing).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn inherited_subagent_prepare_failure_changes_no_layer() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let inherited = fixture
            .state
            .connection
            .model_consumers
            .as_ref()
            .and_then(|consumers| consumers.inherited_handle_for_test("general-purpose"))
            .ok_or_else(|| "inherit-parent handle was not retained".to_string())?;
        invalidate_model_budget(&inherited).await;

        let result = fixture.state.set_default_model_owned(MODEL_B).await;

        assert!(matches!(result, Err(ModelMutationError::Publication(_))));
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn zero_context_window_is_rejected_before_persistence_or_publication()
    -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let mutation = ConfiguredModelMutation {
            model: model(MODEL_A, "local-a", "runtime-a", 0),
            set_default: false,
        };

        let result = fixture.state.upsert_configured_model_owned(mutation).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }

    #[tokio::test]
    async fn provider_upsert_refreshes_active_generation_when_provider_is_shared()
    -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-shared", ENDPOINT_B))
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert_eq!(receipt.model_id, "local-shared");
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            persisted
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_B)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_B)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_B, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_B.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn session_override_stays_active_when_its_shared_provider_changes() -> Result<(), String>
    {
        let fixture = fixture_with_active(shared_provider_config()?, false, MODEL_B).await?;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-shared", ENDPOINT_C))
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert_eq!(receipt.model_id, "local-shared");
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_B)
        );
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_session_generation(
            &fixture,
            MODEL_A,
            MODEL_B,
            "runtime-b",
            ENDPOINT_C,
            WINDOW_B,
        )
        .await
    }

    #[tokio::test]
    async fn deleting_session_override_reactivates_the_durable_default() -> Result<(), String> {
        let fixture = fixture_with_active(valid_config()?, false, MODEL_B).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.activated);
        assert!(receipt.deleted);
        assert_eq!(
            receipt.runtime.as_ref().map(|runtime| runtime.id.as_str()),
            Some(MODEL_A)
        );
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_B)
        );
        assert_session_generation(
            &fixture,
            MODEL_A,
            MODEL_A,
            "runtime-a",
            ENDPOINT_A,
            WINDOW_A,
        )
        .await
    }

    #[tokio::test]
    async fn invalid_shared_provider_upsert_rolls_back_every_layer() -> Result<(), String> {
        let fixture = fixture(shared_provider_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        let mut mutation = provider_mutation("local-shared", RESPONSES_ENDPOINT);
        mutation.provider.auth_token = Some("invalid\nheader".to_string());
        let result = fixture.state.upsert_model_provider_owned(mutation).await;

        assert!(matches!(result, Err(ModelMutationError::Validation(_))));
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(
            persisted
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_A)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-shared")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_A)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn unrelated_provider_upsert_remains_persistence_only() -> Result<(), String> {
        let fixture = fixture(valid_config()?, false).await?;
        let inherited = inherited_handle(&fixture)?;
        invalidate_model_budget(&fixture.state.connection.agent).await;
        let receipt = fixture
            .state
            .upsert_model_provider_owned(provider_mutation("local-b", ENDPOINT_C))
            .await
            .map_err(|error| error.to_string())?;

        assert!(!receipt.activated);
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert_eq!(persisted.model.default_model_id.as_deref(), Some(MODEL_A));
        assert_eq!(
            persisted
                .model_providers
                .get("local-b")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_C)
        );
        assert_eq!(
            fixture
                .state
                .config
                .app_config
                .read()
                .await
                .model_providers
                .get("local-b")
                .and_then(|provider| provider.base_url.as_deref()),
            Some(ENDPOINT_C)
        );
        assert_live_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await?;
        assert_eq!(
            agent_projection(&inherited).await?,
            AgentModelProjection {
                model: "runtime-a".to_string(),
                client_model: "runtime-a".to_string(),
                base_url: ENDPOINT_A.to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                token_limit: WINDOW_A,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_non_default_commits_without_reapplying_active_runtime() -> Result<(), String>
    {
        let fixture = fixture(valid_config()?, false).await?;

        let receipt = fixture
            .state
            .delete_configured_model_owned(MODEL_B)
            .await
            .map_err(|error| error.to_string())?;

        assert!(receipt.deleted);
        assert!(!receipt.activated);
        assert!(receipt.runtime.is_none());
        let persisted = crate::config::load_config_file(&fixture.config_path)?;
        assert!(
            persisted
                .configured_models
                .iter()
                .all(|model| model.id != MODEL_B)
        );
        assert_full_generation(&fixture, MODEL_A, "runtime-a", ENDPOINT_A, WINDOW_A).await
    }
}

#[cfg(test)]
mod workspace_transition_tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::memory::{ConversationStore, FileConversationStore, StoredMessage};
    use echo_agent::testing::MockLlmClient;

    struct ParkedWorkspaceDeleteHook {
        browser: Arc<crate::browser::BrowserRuntime>,
        entered: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        calls: tokio::sync::Mutex<Vec<String>>,
    }

    impl ParkedWorkspaceDeleteHook {
        fn new(
            browser: Arc<crate::browser::BrowserRuntime>,
        ) -> (
            Arc<Self>,
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ) {
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            (
                Arc::new(Self {
                    browser,
                    entered: tokio::sync::Mutex::new(Some(entered_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    calls: tokio::sync::Mutex::new(Vec::new()),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceDeleteHook for ParkedWorkspaceDeleteHook {
        async fn remove_workspace(&self, workspace_id: &str) -> anyhow::Result<()> {
            self.browser.remove_workspace(workspace_id).await;
            self.calls.lock().await.push(workspace_id.to_string());
            if let Some(entered) = self.entered.lock().await.take() {
                let _ = entered.send(());
            }
            let release = self.release.lock().await.take();
            if let Some(release) = release {
                release
                    .await
                    .map_err(|_| anyhow::anyhow!("workspace delete hook release was dropped"))?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn workspace_delete_hook_settles_before_same_id_recreation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let old_root = temp.path().join("old-workspace");
        let new_root = temp.path().join("new-workspace");
        std::fs::create_dir_all(&old_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&new_root).map_err(|error| error.to_string())?;
        let primary = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace delete hook test")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("registry"))
                .map_err(|error| error.to_string())?,
        );
        let browser = crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig {
            enabled: false,
            extension_enabled: false,
            session_dir: temp.path().join("browser-sessions"),
            ..Default::default()
        })
        .await;
        let (hook, hook_entered, hook_release) = ParkedWorkspaceDeleteHook::new(browser.clone());
        let mut state = AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_workspace_delete_hook(hook.clone());
        state.workspace.registry = registry.clone();
        state.storage.chat_events = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(
                temp.path().join("tool-executions"),
            )
            .map_err(|error| error.to_string())?,
        );
        let state = Arc::new(state);
        let (old_workspace, created) = state
            .create_workspace_owned(
                "same-id",
                crate::workspace::WorkspaceKind::General,
                Some(old_root),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(created);
        let old_address = crate::browser::BrowserApprovalAddress::new(
            old_workspace.id.as_str(),
            "old-conversation",
        );
        let provider: Arc<dyn echo_agent::human_loop::HumanLoopProvider> =
            Arc::new(crate::hitl::HitlDispatcher::new());
        let _old_registration = browser
            .register_approval_provider(
                old_address.clone(),
                old_workspace.root.clone(),
                provider.clone(),
            )
            .await;
        let _old_lease = browser
            .session_manager()
            .lease_tab(&old_address, crate::browser::MAIN_TAB_OWNER, None)
            .await;
        assert_eq!(
            browser
                .workspace_projection_counts_for_test(old_workspace.id.as_str())
                .await,
            (1, 1, 1)
        );

        let delete_state = Arc::clone(&state);
        let workspace_id = old_workspace.id.clone();
        let delete =
            tokio::spawn(async move { delete_state.delete_workspace_owned(&workspace_id).await });
        hook_entered
            .await
            .map_err(|_| "workspace delete hook was not reached".to_string())?;
        assert!(registry.open(&old_workspace.id).is_ok());
        assert_eq!(
            browser
                .workspace_projection_counts_for_test(old_workspace.id.as_str())
                .await,
            (0, 0, 0)
        );

        let create_state = Arc::clone(&state);
        let recreate = tokio::spawn(async move {
            create_state
                .create_workspace_owned(
                    "same-id",
                    crate::workspace::WorkspaceKind::General,
                    Some(new_root),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(!recreate.is_finished());
        hook_release
            .send(())
            .map_err(|_| "workspace delete hook release receiver was dropped".to_string())?;
        delete
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let (recreated, created) = recreate
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(created);
        assert_eq!(recreated.id, old_workspace.id);
        assert_eq!(
            registry
                .open(&recreated.id)
                .map_err(|error| error.to_string())?
                .root,
            recreated.root
        );
        state
            .switch_workspace_registered(recreated.id.clone())
            .await
            .map_err(|error| error.to_string())?;
        let new_address =
            crate::browser::BrowserApprovalAddress::new(recreated.id.as_str(), "new-conversation");
        let _new_registration = browser
            .register_approval_provider(new_address.clone(), recreated.root.clone(), provider)
            .await;
        let _new_lease = browser
            .session_manager()
            .lease_tab(&new_address, crate::browser::MAIN_TAB_OWNER, None)
            .await;
        assert_eq!(
            browser
                .workspace_projection_counts_for_test(recreated.id.as_str())
                .await,
            (1, 1, 1)
        );
        let current_root = state
            .current_workspace()
            .await
            .map(|workspace| workspace.root)
            .ok_or_else(|| "recreated workspace was not focused".to_string())?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let recreated_root = recreated
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        assert_eq!(current_root, recreated_root);
        assert_eq!(hook.calls.lock().await.as_slice(), &["same-id".to_string()]);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_delete_drains_driver_before_taking_transition_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let primary = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace delete lock order")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("registry"))
                .map_err(|error| error.to_string())?,
        );
        let mut state = AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        state.workspace.registry = registry.clone();
        state.storage.chat_events = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(
                temp.path().join("tool-executions"),
            )
            .map_err(|error| error.to_string())?,
        );
        state.agent_router = Arc::new(crate::agent_router::AgentRouter::new(
            temp.path().join("agent-router"),
        ));
        let state = Arc::new(state);
        let (workspace, _) = state
            .create_workspace_owned(
                "lock-order",
                crate::workspace::WorkspaceKind::General,
                Some(root),
            )
            .await
            .map_err(|error| error.to_string())?;
        let transition_write = state.workspace.transition.write().await;
        let driver_entered = Arc::new(tokio::sync::Notify::new());
        let driver_read = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::clone(&driver_entered);
        let read = Arc::clone(&driver_read);
        let driver_state = Arc::clone(&state);
        let target =
            crate::agent_router::AgentAddress::new(workspace.id.clone(), "delivery-conversation");
        state
            .agent_deliveries
            .supervise(target, Arc::new(|_| {}), move |cycle| async move {
                entered.notify_one();
                let _transition_read = driver_state.workspace.transition.read().await;
                read.notify_one();
                let _ = cycle.complete();
            })
            .map_err(|error| error.to_string())?;
        driver_entered.notified().await;
        let delete_state = Arc::clone(&state);
        let delete_workspace_id = workspace.id.clone();
        let deleting = tokio::spawn(async move {
            delete_state
                .delete_workspace_owned(&delete_workspace_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!deleting.is_finished());
        drop(transition_write);
        tokio::time::timeout(std::time::Duration::from_secs(1), driver_read.notified())
            .await
            .map_err(|_| "delivery driver never acquired transition read".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), deleting)
            .await
            .map_err(|_| "workspace deletion deadlocked behind delivery drain".to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(registry.open(&workspace.id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn agent_group_target_resolver_acquires_remote_host_and_rejects_drift()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))
                .map_err(|error| error.to_string())?,
        );
        let source_workspace = registry
            .create_at(
                "source",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("source"),
            )
            .map_err(|error| error.to_string())?;
        let target_workspace = registry
            .create_at(
                "target",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("target"),
            )
            .map_err(|error| error.to_string())?;
        let primary = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("Agent group target resolver test")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary, None, None, 4, false).await,
        );
        let runtimes = Arc::new(crate::workspace::runtime::WorkspaceRuntimeRegistry::new());
        let target_host = runtimes
            .get_or_open(target_workspace.clone())
            .await
            .map_err(|error| error.to_string())?;
        target_host
            .resources()
            .conversation_store()
            .ensure_conversation(NewConversation {
                conversation_id: "target-conversation".to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some("Target".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;

        let router = Arc::new(crate::agent_router::AgentRouter::new(
            temp.path().join("router"),
        ));
        let leader = crate::agent_router::AgentAddress::new(
            source_workspace.id.clone(),
            "source-conversation",
        );
        let member_address = crate::agent_router::AgentAddress::new(
            target_workspace.id.clone(),
            "target-conversation",
        );
        let group = router
            .create_group(
                "Research group",
                leader.clone(),
                vec![crate::agent_router::AgentGroupMember {
                    address: member_address.clone(),
                    subagent_role: "explorer".to_string(),
                    label: None,
                }],
            )
            .await
            .map_err(|error| error.to_string())?;
        let resolver = WorkspaceTaskExecutionTargetResolver {
            workspace_registry: registry,
            runtimes,
            seed_pool: Arc::downgrade(&seed_pool),
            agent_router: router,
        };
        let target = crate::tasks::task_runtime::TaskExecutionTarget {
            group_id: group.group_id,
            subagent_role: "explorer".to_string(),
            address: member_address,
        };
        let lease = crate::tasks::task_runtime::TaskExecutionTargetResolver::acquire(
            &resolver, &leader, &target,
        )
        .await?;
        let working_dir = lease.agent().read(|agent| agent.working_dir()).await;
        let canonical_target_root = target_workspace
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            working_dir.as_deref(),
            Some(canonical_target_root.as_path())
        );
        drop(lease);

        let wrong_leader =
            crate::agent_router::AgentAddress::new(source_workspace.id, "another-conversation");
        let leader_error = crate::tasks::task_runtime::TaskExecutionTargetResolver::acquire(
            &resolver,
            &wrong_leader,
            &target,
        )
        .await
        .err()
        .ok_or_else(|| "wrong leader unexpectedly acquired Agent group".to_string())?;
        assert!(leader_error.contains("does not own Agent group"));

        let mut stale_target = target;
        stale_target.address.conversation_id = "stale-conversation".to_string();
        let stale_error = crate::tasks::task_runtime::TaskExecutionTargetResolver::acquire(
            &resolver,
            &leader,
            &stale_target,
        )
        .await
        .err()
        .ok_or_else(|| "stale target unexpectedly acquired Agent group".to_string())?;
        assert!(stale_error.contains("no longer matches frozen target"));
        Ok(())
    }

    #[tokio::test]
    async fn agent_send_queues_for_an_unloaded_validated_workspace_conversation()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))
                .map_err(|error| error.to_string())?,
        );
        let source_workspace = registry
            .create_at(
                "source",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("source"),
            )
            .map_err(|error| error.to_string())?;
        let target_workspace = registry
            .create_at(
                "target",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("target"),
            )
            .map_err(|error| error.to_string())?;
        let primary = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("Agent router test")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_agent_router(Arc::new(crate::agent_router::AgentRouter::new(
            temp.path().join("router"),
        )));
        state.workspace.registry = Arc::clone(&registry);
        let state = Arc::new(state);

        for (workspace, conversation_id) in [
            (source_workspace.clone(), "source-conversation"),
            (target_workspace.clone(), "target-conversation"),
        ] {
            let host = state
                .workspace
                .runtimes
                .get_or_open(workspace)
                .await
                .map_err(|error| error.to_string())?;
            host.resources()
                .conversation_store()
                .ensure_conversation(NewConversation {
                    conversation_id: conversation_id.to_string(),
                    user_id: "default".to_string(),
                    agent_type: None,
                    title: Some(conversation_id.to_string()),
                })
                .await
                .map_err(|error| error.to_string())?;
        }

        state
            .switch_workspace(source_workspace.clone())
            .await
            .map_err(|error| error.to_string())?;

        let source = crate::agent_router::AgentAddress::new(
            source_workspace.id.clone(),
            "source-conversation",
        );
        let target = crate::agent_router::AgentAddress::new(
            target_workspace.id.clone(),
            "target-conversation",
        );
        let endpoints = state
            .discover_agent_endpoints()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().any(|endpoint| endpoint.address == target));
        assert_eq!(
            state
                .current_agent_address(Some("source-conversation"))
                .await
                .map_err(|error| error.to_string())?,
            Some(source.clone())
        );
        assert_eq!(
            state
                .current_agent_address(Some("not-persisted"))
                .await
                .map_err(|error| error.to_string())?,
            None
        );

        let (lookup_entered, lookup_release) =
            state.workspace.runtimes.park_next_control_acquire()?;
        let lookup_state = Arc::clone(&state);
        let lookup = tokio::spawn(async move {
            lookup_state
                .current_agent_address(Some("source-conversation"))
                .await
        });
        lookup_entered
            .await
            .map_err(|_| "current address lookup did not reach control pin barrier".to_string())?;
        let switch_state = Arc::clone(&state);
        let switch =
            tokio::spawn(async move { switch_state.switch_workspace(target_workspace).await });
        while !state.workspace_transition_in_progress() {
            tokio::task::yield_now().await;
        }
        lookup_release
            .send(())
            .map_err(|_| "current address control pin release was dropped".to_string())?;
        assert_eq!(
            lookup
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?,
            Some(source.clone())
        );
        switch
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(
            state
                .current_agent_address(Some("source-conversation"))
                .await
                .map_err(|error| error.to_string())?,
            None
        );
        assert_eq!(
            state
                .current_agent_address(Some("target-conversation"))
                .await
                .map_err(|error| error.to_string())?,
            Some(target.clone())
        );

        let mut message = crate::agent_router::AgentMessage::user_text(
            Some(source),
            target.clone(),
            "What did you learn?",
        );
        message.message_id = "source-to-target".to_string();
        let receipt = state
            .send_agent_message_owned(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            receipt.phase,
            crate::agent_router::AgentDeliveryPhase::Persisted
        );
        assert_eq!(
            state
                .agent_router
                .pending(&target)
                .await
                .map_err(|error| error.to_string())?,
            vec![message]
        );
        let records = state
            .agent_delivery_records(&target)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(records.len(), 1);
        assert_eq!(
            records.first().map(|record| record.message_id.as_str()),
            Some("source-to-target")
        );
        let activity = state
            .workspace
            .runtimes
            .activity_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(activity.len(), 2);
        assert!(activity.iter().all(|host| !host.execution_loaded));
        Ok(())
    }

    #[tokio::test]
    async fn agent_delivery_cold_starts_target_and_routes_correlated_reply() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("workspaces"))
                .map_err(|error| error.to_string())?,
        );
        let source_workspace = registry
            .create_at(
                "source",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("source"),
            )
            .map_err(|error| error.to_string())?;
        let target_workspace = registry
            .create_at(
                "target",
                crate::workspace::WorkspaceKind::General,
                temp.path().join("target"),
            )
            .map_err(|error| error.to_string())?;
        let primary = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new().with_responses([
                "target model preflight",
                "target answer",
                "source model preflight",
                "source incorporated reply",
            ])))
            .system_prompt("Agent delivery integration test")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false).await,
        );
        seed_pool
            .set_llm_client_override_for_test(Arc::new(MockLlmClient::new().with_responses([
                "target model preflight",
                "target answer",
                "source model preflight",
                "source incorporated reply",
            ])))
            .await;
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_agent_router(Arc::new(crate::agent_router::AgentRouter::new(
            temp.path().join("router"),
        )));
        state.workspace.registry = Arc::clone(&registry);
        state.set_pool(seed_pool);
        let state = Arc::new(state);

        for (workspace, conversation_id) in [
            (source_workspace.clone(), "source-conversation"),
            (target_workspace.clone(), "target-conversation"),
        ] {
            let host = state
                .workspace
                .runtimes
                .get_or_open(workspace)
                .await
                .map_err(|error| error.to_string())?;
            host.resources()
                .conversation_store()
                .ensure_conversation(NewConversation {
                    conversation_id: conversation_id.to_string(),
                    user_id: "default".to_string(),
                    agent_type: None,
                    title: Some(conversation_id.to_string()),
                })
                .await
                .map_err(|error| error.to_string())?;
        }

        let source = crate::agent_router::AgentAddress::new(
            source_workspace.id.clone(),
            "source-conversation",
        );
        let target = crate::agent_router::AgentAddress::new(
            target_workspace.id.clone(),
            "target-conversation",
        );
        let mut message = crate::agent_router::AgentMessage::user_text(
            Some(source.clone()),
            target.clone(),
            "Ask the target",
        );
        message.message_id = "cold-delivery".to_string();
        state
            .send_agent_message_owned(message.clone())
            .await
            .map_err(|error| error.to_string())?;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let target_record = loop {
            let record = state
                .agent_router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|record| {
                    record.message_id == message.message_id
                        && record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled
                        && record.outcome
                            == Some(crate::agent_router::AgentDeliveryOutcome::Completed)
                });
            if let Some(record) = record {
                break record;
            }
            if tokio::time::Instant::now() >= deadline {
                let records = state
                    .agent_router
                    .records(&target)
                    .await
                    .map_err(|error| error.to_string())?;
                let activity = state
                    .workspace
                    .runtimes
                    .activity_snapshot()
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "target Agent delivery did not settle; records={records:?}; activity={activity:?}"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let reply_id = target_record
            .reply_message_id
            .clone()
            .ok_or_else(|| "correlated reply was not queued".to_string())?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let source_record = loop {
            let record = state
                .agent_router
                .records(&source)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|record| {
                    record.message_id == reply_id
                        && record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled
                        && record.outcome
                            == Some(crate::agent_router::AgentDeliveryOutcome::Completed)
                });
            if let Some(record) = record {
                break record;
            }
            if tokio::time::Instant::now() >= deadline {
                let records = state
                    .agent_router
                    .records(&source)
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "source Agent did not consume correlated reply; records={records:?}"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(
            source_record.payload.correlation_id.as_deref(),
            Some("cold-delivery")
        );
        assert_eq!(
            source_record.payload.causation_id.as_deref(),
            Some("cold-delivery")
        );
        assert!(matches!(
            source_record.payload.payload,
            crate::agent_router::AgentMessagePayload::Reply { ref text }
                if text == "target answer"
        ));

        let target_host = state
            .workspace
            .runtimes
            .get_or_open(target_workspace)
            .await
            .map_err(|error| error.to_string())?;
        let target_store = target_host.resources().conversation_store();
        let mut transcript = target_store
            .get_messages("target-conversation")
            .await
            .map_err(|error| error.to_string())?;
        assert!(transcript.iter().any(|stored| {
            stored.role == "assistant" && stored.content.as_deref() == Some("target answer")
        }));

        let mut crash_message = crate::agent_router::AgentMessage::user_text(
            Some(source.clone()),
            target.clone(),
            "Recover without running the model twice",
        );
        crash_message.message_id = "transcript-crash-window".to_string();
        state
            .agent_router
            .enqueue(crash_message.clone())
            .await
            .map_err(|error| error.to_string())?;
        let abandoned_claim = state
            .agent_router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "crash-window claim missing".to_string())?;
        assert_eq!(abandoned_claim.attempt, 1);
        let actual_turn_id = crash_message.delivery_turn_id();
        state
            .agent_router
            .begin_injection(&abandoned_claim, actual_turn_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        let crash_instruction = render_agent_delivery_instruction(&crash_message);
        let created_at = Utc::now().to_rfc3339();
        transcript.push(StoredMessage {
            id: None,
            conversation_id: target.conversation_id.clone(),
            role: "user".to_string(),
            content: Some(crash_instruction),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at: created_at.clone(),
        });
        transcript.push(StoredMessage {
            id: None,
            conversation_id: target.conversation_id.clone(),
            role: "assistant".to_string(),
            content: Some("unowned adjacent assistant text".to_string()),
            attachments_json: None,
            tool_calls_json: None,
            tool_result_json: None,
            created_at,
        });
        target_store
            .save_messages(&target.conversation_id, &transcript)
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            state
                .reconcile_agent_delivery_in_flight(&target, &[], &CancellationToken::new(),)
                .await
                .map_err(|error| error.to_string())?
        );
        let recovered_record = state
            .agent_router
            .records(&target)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.message_id == crash_message.message_id)
            .ok_or_else(|| "crash-window delivery record missing".to_string())?;
        assert_eq!(
            recovered_record.phase,
            crate::agent_router::AgentDeliveryPhase::TurnSettled
        );
        assert_eq!(
            recovered_record.outcome,
            Some(crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown)
        );
        assert_eq!(recovered_record.attempt, 1);
        assert_eq!(
            recovered_record.turn_id.as_deref(),
            Some(actual_turn_id.as_str())
        );
        assert!(recovered_record.reply_message_id.is_none());
        let recovered_transcript = target_store
            .get_messages(&target.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            recovered_transcript
                .iter()
                .filter(|stored| {
                    stored.role == "assistant"
                        && stored.content.as_deref() == Some("unowned adjacent assistant text")
                })
                .count(),
            1
        );

        let runtime = state
            .chat_runtime_for_agent(&target)
            .await
            .map_err(|error| error.to_string())?;
        let lease = runtime
            .begin_turn(
                &state.session.foreground_turns,
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                &target.conversation_id,
                "active-target-turn",
            )
            .await
            .map_err(|error| error.to_string())?;
        let execution = runtime
            .agent_for(&target.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        let active_agent = execution.agent();
        active_agent
            .write(|agent| {
                agent.set_llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_responses(["active turn draft", "active turn after steer"])
                        .with_delay(std::time::Duration::from_secs(1)),
                ));
            })
            .await;
        let spill_dir = crate::prepared_turn::resolve_user_input_spill_dir(Some(
            runtime.execution_scope().root(),
        ));
        let active_turn =
            crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
                text: "Start a delayed target turn",
                attachments: &[],
                spill_dir: &spill_dir,
                conversation_id: Some(&target.conversation_id),
                turn_id: Some("active-target-turn"),
            })
            .map_err(|error| error.to_string())?;
        let active_sink: Arc<dyn crate::chat_driver::ChatSink> =
            Arc::new(AgentDeliveryCaptureSink::default());
        let active_resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: runtime.execution_scope().clone(),
            workspace_io_receipt: Some(runtime.workspace_io_receipt()),
            pool: runtime.pool(),
            store: runtime.task_runtime(),
            sink: active_sink,
            webhook_emitter: Some(state.webhook.emitter.clone()),
            conv_id: Some(target.conversation_id.clone()),
            root_message_id: "active-target-turn".to_string(),
            attachments: Vec::new(),
            cancel: lease.cancellation_token(),
            review_integration: runtime.review_integration(),
            memory_generation: None,
            human_loop_provider: Some(Arc::new(crate::hitl::HitlDispatcher::new())),
        });
        let active_task = tokio::spawn(async move {
            let _execution = execution;
            crate::foreground_turn::drive_foreground_chat(
                lease,
                &active_agent,
                &active_turn,
                active_resources,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut live_message = crate::agent_router::AgentMessage::user_text(
            None,
            target.clone(),
            "Steer the active target turn",
        );
        live_message.message_id = "live-steer".to_string();
        state
            .send_agent_message_owned(live_message)
            .await
            .map_err(|error| error.to_string())?;
        let live_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let live_record = loop {
            let record = state
                .agent_router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|record| {
                    record.message_id == "live-steer"
                        && record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled
                        && record.outcome
                            == Some(crate::agent_router::AgentDeliveryOutcome::Completed)
                });
            if let Some(record) = record {
                break record;
            }
            if tokio::time::Instant::now() >= live_deadline {
                let records = state
                    .agent_router
                    .records(&target)
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "live Agent message was not steered; records={records:?}"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(live_record.turn_id.as_deref(), Some("active-target-turn"));
        let active_outcome = active_task.await.map_err(|error| error.to_string())??;
        assert_eq!(active_outcome, crate::chat_driver::TurnOutcome::Completed);

        let busy_execution = runtime
            .agent_for(&target.conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        busy_execution
            .agent()
            .write(|agent| {
                agent.set_llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_responses(["busy turn preflight", "processed after busy turn"]),
                ));
            })
            .await;
        drop(busy_execution);
        let busy_lease = runtime
            .begin_turn(
                &state.session.foreground_turns,
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                &target.conversation_id,
                "busy-target-turn",
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut busy_message =
            crate::agent_router::AgentMessage::user_text(None, target.clone(), "Wait for FIFO");
        busy_message.message_id = "busy-fifo".to_string();
        state
            .send_agent_message_owned(busy_message)
            .await
            .map_err(|error| error.to_string())?;
        let defer_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let deferred = state
                .agent_router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .any(|record| {
                    record.message_id == "busy-fifo"
                        && record.phase == crate::agent_router::AgentDeliveryPhase::Deferred
                        && record.attempt > 0
                });
            if deferred {
                break;
            }
            if tokio::time::Instant::now() >= defer_deadline {
                let records = state
                    .agent_router
                    .records(&target)
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "busy Agent delivery was not deferred; records={records:?}"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        busy_lease.settle(crate::chat_driver::TurnOutcome::Completed);
        let resume_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let resumed_record = loop {
            let record = state
                .agent_router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|record| {
                    record.message_id == "busy-fifo"
                        && record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled
                        && record.outcome
                            == Some(crate::agent_router::AgentDeliveryOutcome::Completed)
                });
            if let Some(record) = record {
                break record;
            }
            if tokio::time::Instant::now() >= resume_deadline {
                let records = state
                    .agent_router
                    .records(&target)
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "deferred Agent delivery did not resume; records={records:?}"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(resumed_record.attempt >= 2);
        state
            .shutdown_agent_deliveries()
            .await
            .map_err(|error| error.to_string())?;
        state
            .session
            .foreground_turns
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn workspace(name: &str, root: std::path::PathBuf) -> Workspace {
        Workspace {
            id: crate::workspace::WorkspaceId::from_name(name),
            name: name.to_string(),
            root,
            project_root: None,
            kind: crate::workspace::WorkspaceKind::General,
            metadata: crate::workspace::WorkspaceMetadata::default(),
            product_data_generation: String::new(),
            created_at: Utc::now(),
            last_active: Utc::now(),
        }
    }

    async fn extension_control_state(
        root: &std::path::Path,
    ) -> Result<
        (
            Arc<AppState>,
            Arc<crate::agent_pool::AgentPool>,
            Arc<crate::plugin_runtime::PluginRuntimeService>,
        ),
        String,
    > {
        let global_root = root.join("global");
        std::fs::create_dir_all(&global_root).map_err(|error| error.to_string())?;
        let global_root = global_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("extension control runtime identity test")
                .working_dir(global_root.clone())
                .build()
                .map_err(|error| error.to_string())?,
        );
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(agent.clone(), None, None, 4, false).await,
        );
        seed_pool
            .update_mcp_config_snapshot(Default::default())
            .await;
        let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            agent.clone(),
            global_root.clone(),
            root.join("global-plugins.json"),
            root.join("global-plugin-data"),
        )
        .await
        .map_err(|error| error.to_string())?;
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            root.join("mcp.json"),
            Default::default(),
        ));
        let skill_root = root.join("skills");
        std::fs::create_dir_all(&skill_root).map_err(|error| error.to_string())?;
        let mut state = AppState::from_shared(
            agent,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?
        .with_plugin_runtime(Some(Arc::clone(&plugin_runtime)));
        state.skills_hub = Arc::new(tokio::sync::RwLock::new(
            crate::skills_hub::SkillsHub::with_root(skill_root),
        ));
        state.extension_control = Arc::new(
            crate::extension_control::ExtensionControlService::with_enabled_config_path(
                root.join("enabled-skills.json"),
            ),
        );
        state.workspace.global_execution_root = global_root;
        state.workspace.registry = Arc::new(
            WorkspaceRegistry::with_base_dir(root.join("workspace-registry"))
                .map_err(|error| error.to_string())?,
        );
        state.tasks.runtime = Some(Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        ));
        state.storage.chat_events = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                root.join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(root.join("tool-executions"))
                .map_err(|error| error.to_string())?,
        );
        state.set_pool(Arc::clone(&seed_pool));
        Ok((Arc::new(state), seed_pool, plugin_runtime))
    }

    #[tokio::test]
    async fn extension_control_for_runtime_keeps_captured_workspace_after_focus_change()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_b = root_b.canonicalize().map_err(|error| error.to_string())?;
        let (state, _, _) = extension_control_state(temp.path()).await?;

        state
            .switch_workspace(workspace("workspace-a", root_a))
            .await
            .map_err(|error| error.to_string())?;
        let runtime_a = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        state
            .switch_workspace(workspace("workspace-b", root_b))
            .await
            .map_err(|error| error.to_string())?;

        let control_a = state
            .extension_control_for_runtime(&runtime_a)
            .await
            .map_err(|error| error.to_string())?;
        let control_b = state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            control_a.runtime().execution_scope().workspace_id(),
            "workspace-a"
        );
        assert_eq!(control_a.project_root(), canonical_a.as_path());
        assert_eq!(
            control_b.runtime().execution_scope().workspace_id(),
            "workspace-b"
        );
        assert_eq!(control_b.project_root(), canonical_b.as_path());
        assert!(!Arc::ptr_eq(
            &control_a.plugin_runtime(),
            &control_b.plugin_runtime()
        ));
        let enabled = crate::skills_hub::enabled_skills::EnabledSkillsConfig::load(
            &temp.path().join("enabled-skills.json"),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(enabled.settled_generation, enabled.desired_generation);
        assert!(
            enabled.repair_debt.is_none(),
            "unexpected extension repair debt: {:?}",
            enabled.repair_debt
        );
        Ok(())
    }

    #[tokio::test]
    async fn extension_control_for_runtime_rejects_replaced_workspace_generation()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let (state, _, _) = extension_control_state(temp.path()).await?;

        state
            .switch_workspace(workspace("workspace-a", root_a.clone()))
            .await
            .map_err(|error| error.to_string())?;
        let runtime_a = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        // Preserve the captured identity without its control lease so the test
        // can model a delayed surface request after the old host is evicted.
        let stale_runtime = ScopedChatRuntime {
            _lifetime: ScopedRuntimeLifetime::Global,
            execution_scope: runtime_a.execution_scope.clone(),
            workspace_io_identity: runtime_a.workspace_io_identity.clone(),
            primary_agent: runtime_a.primary_agent.clone(),
            pool: runtime_a.pool.clone(),
            task_runtime: runtime_a.task_runtime.clone(),
            review_integration: runtime_a.review_integration.clone(),
            conversation_store: runtime_a.conversation_store.clone(),
            runtime_state_store: runtime_a.runtime_state_store.clone(),
            deletions: Arc::clone(&runtime_a.deletions),
        };
        drop(runtime_a);
        state
            .switch_workspace(workspace("workspace-b", root_b))
            .await
            .map_err(|error| error.to_string())?;
        let workspace_a_id = crate::workspace::WorkspaceId::from_name("workspace-a");
        assert!(
            state
                .evict_workspace_runtime_if_idle_for_test(&workspace_a_id)
                .await
                .map_err(|error| error.to_string())?
        );
        state
            .switch_workspace(workspace("workspace-a", root_a))
            .await
            .map_err(|error| error.to_string())?;

        let error = state
            .extension_control_for_runtime(&stale_runtime)
            .await
            .err()
            .ok_or_else(|| "replaced workspace generation was accepted".to_string())?;
        assert!(
            error
                .to_string()
                .contains("workspace 'workspace-a' extension runtime generation was replaced")
        );
        Ok(())
    }

    #[tokio::test]
    async fn extension_control_for_runtime_matches_global_pool_identity() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (state, seed_pool, plugin_runtime) = extension_control_state(temp.path()).await?;
        let global_runtime = state
            .chat_runtime_for_scope("global")
            .await
            .map_err(|error| error.to_string())?;

        let control = state
            .extension_control_for_runtime(&global_runtime)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(control.runtime().execution_scope().workspace_id(), "global");
        assert!(Arc::ptr_eq(
            &control
                .runtime()
                .pool()
                .ok_or_else(|| "global runtime pool missing".to_string())?,
            &seed_pool
        ));
        assert!(Arc::ptr_eq(&control.plugin_runtime(), &plugin_runtime));

        let other_agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("replacement global runtime")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let mismatched_runtime = ScopedChatRuntime {
            pool: Some(Arc::new(
                crate::agent_pool::AgentPool::new_for_test(other_agent, None, None, 1, false).await,
            )),
            ..global_runtime.clone()
        };
        assert!(
            state
                .extension_control_for_runtime(&mismatched_runtime)
                .await
                .is_err_and(|error| error
                    .to_string()
                    .contains("global extension runtime generation was replaced"))
        );
        Ok(())
    }

    #[test]
    fn workspace_transition_receipt_serializes_generated_typescript_contract()
    -> std::result::Result<(), String> {
        let receipt = WorkspaceTransitionReceipt::committed(
            Some("workspace-a".to_string()),
            Some("workspace-b".to_string()),
            std::path::PathBuf::from("/workspace-b"),
            vec![WorkspaceSubsystemTransition {
                subsystem: "config_watcher".to_string(),
                target_root: std::path::PathBuf::from("/workspace-b"),
                stale_roots: Vec::new(),
                error: "watch settled with degraded cleanup".to_string(),
            }],
        );
        let serialized = serde_json::to_value(receipt).map_err(|error| error.to_string())?;

        assert_eq!(
            serialized.get("status").and_then(serde_json::Value::as_str),
            Some("degraded")
        );
        assert_eq!(
            serialized
                .get("target_root")
                .and_then(serde_json::Value::as_str),
            Some("/workspace-b")
        );
        assert_eq!(
            serialized
                .pointer("/degraded_subsystems/0/stale_roots")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn focus_changes_preserve_independent_running_workspace_hosts()
    -> std::result::Result<(), String> {
        let process_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_b = root_b.canonicalize().map_err(|error| error.to_string())?;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("workspace focus test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(agent.clone(), None, None, 4, false).await,
        );
        let global_store: Arc<dyn ConversationStore> = Arc::new(
            FileConversationStore::new(temp.path().join("global-conversations"))
                .map_err(|error| error.to_string())?,
        );
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            agent.clone(),
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            Some(global_store.clone()),
            None,
            Default::default(),
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        state.tasks.runtime = Some(Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        ));
        state.storage.chat_events = Arc::new(
            crate::chat_event_log::ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        state.storage.tool_executions = Arc::new(
            crate::tool_execution::ToolExecutionRepository::open(
                temp.path().join("tool-executions"),
            )
            .map_err(|error| error.to_string())?,
        );
        let workspace_registry = Arc::new(
            WorkspaceRegistry::with_base_dir(temp.path().join("workspace-registry"))
                .map_err(|error| error.to_string())?,
        );
        workspace_registry
            .create_at(
                "workspace-a",
                crate::workspace::WorkspaceKind::General,
                root_a.clone(),
            )
            .map_err(|error| error.to_string())?;
        workspace_registry
            .create_at(
                "workspace-b",
                crate::workspace::WorkspaceKind::General,
                root_b.clone(),
            )
            .map_err(|error| error.to_string())?;
        state.workspace.registry = workspace_registry;
        state.set_pool(seed_pool);
        let state = Arc::new(state);

        state
            .workspace
            .transitioning
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            state.current_control_runtime().await,
            Err(ScopedControlError::WorkspaceTransition)
        ));
        state
            .workspace
            .transitioning
            .store(false, std::sync::atomic::Ordering::Release);

        let missing_root = temp.path().join("missing-workspace");
        assert!(
            state
                .switch_workspace(workspace("missing", missing_root))
                .await
                .is_err()
        );
        assert_eq!(
            state
                .current_control_runtime()
                .await
                .map_err(|error| error.to_string())?
                .execution_scope()
                .workspace_id(),
            "global"
        );

        let (entered, release) = state.park_next_workspace_transition()?;
        let detached_state = Arc::clone(&state);
        let detached_workspace = workspace("workspace-a", root_a.clone());
        let waiter =
            tokio::spawn(async move { detached_state.switch_workspace(detached_workspace).await });
        entered
            .await
            .map_err(|_| "workspace transition did not reach test barrier".to_string())?;
        assert!(matches!(
            state.current_control_runtime().await,
            Err(ScopedControlError::WorkspaceTransition)
        ));
        waiter.abort();
        let _ = waiter.await;
        release
            .send(())
            .map_err(|_| "workspace transition release receiver was dropped".to_string())?;
        state
            .switch_workspace(workspace("workspace-a", root_a))
            .await
            .map_err(|error| error.to_string())?;
        let runtime_a = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let explicit_global = state
            .chat_runtime_for_scope("global")
            .await
            .map_err(|error| error.to_string())?;
        let explicit_global_store = explicit_global
            .conversation_store()
            .ok_or_else(|| "explicit global conversation store missing".to_string())?;
        let workspace_a_store = runtime_a
            .conversation_store()
            .ok_or_else(|| "workspace A conversation store missing".to_string())?;
        assert!(Arc::ptr_eq(&explicit_global_store, &global_store));
        assert!(!Arc::ptr_eq(&explicit_global_store, &workspace_a_store));
        assert!(explicit_global.runtime_state_store().is_none());
        assert!(runtime_a.runtime_state_store().is_some());
        let foreground_a = state
            .begin_conversation_turn_owned(
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                "same-conversation",
                "turn-a",
            )
            .await
            .map_err(|error| error.to_string())?;
        let execution_a = runtime_a
            .agent_for("same-conversation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            execution_a.agent().read(|agent| agent.working_dir()).await,
            Some(canonical_a.clone())
        );
        let task_store_a = runtime_a
            .task_runtime()
            .ok_or_else(|| "workspace A TaskRuntime missing".to_string())?;
        task_store_a
            .create_run(
                "shared-run",
                "workspace-a",
                "same-conversation",
                "root-a",
                crate::tasks::task_runtime::DomainProfile::General,
                "workspace A goal",
                "task",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let memory_a = runtime_a
            .review_integration()
            .ok_or_else(|| "workspace A memory integration missing".to_string())?
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let memory_manager_a = memory_a
            .layer_manager()
            .map_err(|error| error.to_string())?;
        memory_manager_a
            .write_memory(
                "shared-memory",
                "workspace A memory",
                echo_agent::memory::MemoryMeta::new(
                    echo_agent::memory::MemoryType::ProjectFact,
                    echo_agent::memory::MemorySource::ExplicitSave,
                    "explicit",
                ),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection_a = memory_a.settle_hot_memory_projection().await;
        if let Some(error) = projection_a.error {
            return Err(format!("workspace A projection did not settle: {error}"));
        }

        let receipt_b = state
            .switch_workspace(workspace("workspace-b", root_b))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            receipt_b.status,
            WorkspaceTransitionStatus::Committed,
            "workspace switch degraded: {receipt_b:?}"
        );
        assert!(
            state
                .delete_workspace_owned(&crate::workspace::WorkspaceId::from_name("workspace-a"))
                .await
                .is_err_and(|error| error.to_string().contains("active foreground"))
        );
        let runtime_b = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let prelink_product_b = state
            .current_product_data()
            .await
            .map_err(|error| error.to_string())?;
        let prelink_generation_b = prelink_product_b.generation();
        std::fs::write(canonical_b.join("same.txt"), "same bytes")
            .map_err(|error| error.to_string())?;
        drop(prelink_product_b);
        drop(runtime_b);
        let linked_project = temp.path().join("workspace-b-project");
        std::fs::create_dir_all(&linked_project).map_err(|error| error.to_string())?;
        std::fs::write(linked_project.join("same.txt"), "same bytes")
            .map_err(|error| error.to_string())?;
        let canonical_linked_project = linked_project
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let linked_workspace = state
            .link_current_workspace_project_owned(linked_project.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(linked_workspace.id.as_str(), "workspace-b");
        assert_eq!(
            linked_workspace.project_root,
            Some(canonical_linked_project.clone())
        );
        assert!(
            state
                .product_data_for_scope("workspace-b", &prelink_generation_b)
                .await
                .is_err()
        );
        let runtime_b = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let execution_b = runtime_b
            .agent_for("same-conversation")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            execution_b.agent().read(|agent| agent.working_dir()).await,
            Some(canonical_b.clone())
        );
        let task_store_b = runtime_b
            .task_runtime()
            .ok_or_else(|| "workspace B TaskRuntime missing".to_string())?;
        task_store_b
            .create_run(
                "shared-run",
                "workspace-b",
                "same-conversation",
                "root-b",
                crate::tasks::task_runtime::DomainProfile::General,
                "workspace B goal",
                "task",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let memory_b = runtime_b
            .review_integration()
            .ok_or_else(|| "workspace B memory integration missing".to_string())?
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let memory_manager_b = memory_b
            .layer_manager()
            .map_err(|error| error.to_string())?;
        memory_manager_b
            .write_memory(
                "shared-memory",
                "workspace B memory",
                echo_agent::memory::MemoryMeta::new(
                    echo_agent::memory::MemoryType::ProjectFact,
                    echo_agent::memory::MemorySource::ExplicitSave,
                    "explicit",
                ),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection_b = memory_b.settle_hot_memory_projection().await;
        if let Some(error) = projection_b.error {
            return Err(format!("workspace B projection did not settle: {error}"));
        }
        let profile_store_a =
            echo_agent::profiles::ProfileStore::new(memory_a.memory_store());
        let profile_store_b =
            echo_agent::profiles::ProfileStore::new(memory_b.memory_store());
        let mut profile_a = echo_agent::profiles::UserProfile::new();
        profile_a.set_preference("scope", "workspace A");
        profile_store_a
            .save_user_profile(&profile_a)
            .await
            .map_err(|error| error.to_string())?;
        let mut profile_b = echo_agent::profiles::UserProfile::new();
        profile_b.set_preference("scope", "workspace B");
        profile_store_b
            .save_user_profile(&profile_b)
            .await
            .map_err(|error| error.to_string())?;
        let evidence_a = memory_a.evidence_store();
        let evidence_b = memory_b.evidence_store();
        evidence_a
            .upsert(crate::evolution::EvidenceCandidateDraft {
                kind: crate::evolution::EvidenceKind::ProjectFact,
                scope: None,
                content: "workspace A evidence".to_string(),
                evidence: vec![crate::evolution::EvidenceRef {
                    source: crate::evolution::EvidenceSource::AutoMemory,
                    source_run_id: None,
                    source_role: Some("user".to_string()),
                    source_turn: Some(1),
                    source_memory_key: None,
                    quote: "A quote".to_string(),
                }],
                action: None,
                confidence: 0.9,
            })
            .map_err(|error| error.to_string())?;
        let evidence_projection_a = memory_a.settle_hot_memory_projection().await;
        if let Some(error) = evidence_projection_a.error {
            return Err(format!(
                "workspace A evidence projection did not settle: {error}"
            ));
        }
        evidence_b
            .upsert(crate::evolution::EvidenceCandidateDraft {
                kind: crate::evolution::EvidenceKind::ProjectFact,
                scope: None,
                content: "workspace B evidence".to_string(),
                evidence: vec![crate::evolution::EvidenceRef {
                    source: crate::evolution::EvidenceSource::AutoMemory,
                    source_run_id: None,
                    source_role: Some("user".to_string()),
                    source_turn: Some(1),
                    source_memory_key: None,
                    quote: "B quote".to_string(),
                }],
                action: None,
                confidence: 0.9,
            })
            .map_err(|error| error.to_string())?;
        let evidence_projection_b = memory_b.settle_hot_memory_projection().await;
        if let Some(error) = evidence_projection_b.error {
            return Err(format!(
                "workspace B evidence projection did not settle: {error}"
            ));
        }
        assert_eq!(
            task_store_a
                .get_run("shared-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.goal),
            Some("workspace A goal".to_string())
        );
        assert_eq!(
            task_store_b
                .get_run("shared-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.goal),
            Some("workspace B goal".to_string())
        );
        assert!(
            task_store_b
                .request_cancel("shared-run")
                .map_err(|error| error.to_string())?
        );
        assert_eq!(
            task_store_a
                .get_run("shared-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Pending)
        );
        assert_eq!(
            task_store_b
                .get_run("shared-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Cancelled)
        );
        let located_a = memory_manager_a
            .locate("shared-memory")
            .await
            .ok_or_else(|| "workspace A memory missing".to_string())?;
        let located_b = memory_manager_b
            .locate("shared-memory")
            .await
            .ok_or_else(|| "workspace B memory missing".to_string())?;
        assert_eq!(located_a.1.content, "workspace A memory");
        assert_eq!(located_b.1.content, "workspace B memory");
        assert_eq!(
            profile_store_a
                .load_user_profile()
                .await
                .map_err(|error| error.to_string())?
                .and_then(|profile| profile.preferences.get("scope").cloned()),
            Some("workspace A".to_string())
        );
        assert_eq!(
            profile_store_b
                .load_user_profile()
                .await
                .map_err(|error| error.to_string())?
                .and_then(|profile| profile.preferences.get("scope").cloned()),
            Some("workspace B".to_string())
        );
        assert!(
            evidence_a
                .review_items()
                .map_err(|error| error.to_string())?
                .iter()
                .all(|item| item.candidate.content.contains("workspace A"))
        );
        assert!(
            evidence_b
                .review_items()
                .map_err(|error| error.to_string())?
                .iter()
                .all(|item| item.candidate.content.contains("workspace B"))
        );
        let integration_a = runtime_a
            .review_integration()
            .ok_or_else(|| "workspace A review integration missing".to_string())?;
        let integration_b = runtime_b
            .review_integration()
            .ok_or_else(|| "workspace B review integration missing".to_string())?;
        assert_ne!(
            integration_a.echo_agent_dir(),
            integration_b.echo_agent_dir()
        );
        assert!(
            memory_manager_b
                .delete_memory("shared-memory")
                .await
                .map_err(|error| error.to_string())?
        );
        let projection_b = memory_b.settle_hot_memory_projection().await;
        if let Some(error) = projection_b.error {
            return Err(format!(
                "workspace B deletion projection did not settle: {error}"
            ));
        }
        assert!(memory_manager_b.locate("shared-memory").await.is_none());
        assert!(memory_manager_a.locate("shared-memory").await.is_some());
        let pool_a = runtime_a
            .pool()
            .ok_or_else(|| "workspace A pool missing".to_string())?;
        let pool_b = runtime_b
            .pool()
            .ok_or_else(|| "workspace B pool missing".to_string())?;
        assert!(!Arc::ptr_eq(&pool_a, &pool_b));
        assert_eq!(
            runtime_a
                .task_runtime()
                .ok_or_else(|| "workspace A TaskRuntime missing".to_string())?
                .active_workspace_id(),
            "workspace-a"
        );
        assert_eq!(
            runtime_b
                .task_runtime()
                .ok_or_else(|| "workspace B TaskRuntime missing".to_string())?
                .active_workspace_id(),
            "workspace-b"
        );
        assert!(
            state
                .session
                .foreground_turns
                .snapshot_scoped(
                    "workspace-a",
                    crate::foreground_turn::ForegroundTurnSurface::Gui,
                    "same-conversation"
                )
                .is_some()
        );

        foreground_a.settle(crate::chat_driver::TurnOutcome::Completed);
        drop(execution_a);
        drop(execution_b);
        let product_control_b = state
            .current_product_data()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(product_control_b.data_root(), canonical_b.as_path());
        assert_eq!(product_control_b.project_root(), canonical_linked_project);
        let stale_b_generation = product_control_b.generation();
        let auto_ingest_identity = product_control_b.runtime().workspace_io_identity.clone();
        let auto_ingest_scope = product_control_b.runtime().workspace_io_invocation();
        let auto_ingest_context = echo_agent::tools::ToolContext {
            working_dir: Some(auto_ingest_scope.data_root().to_path_buf()),
            resource_guards: auto_ingest_scope.resource_guards(),
            ..echo_agent::tools::ToolContext::default()
        };
        drop(auto_ingest_scope);
        drop(product_control_b);
        drop(runtime_b);
        let (io_entered_tx, io_entered_rx) = tokio::sync::oneshot::channel();
        let (io_release_tx, io_release_rx) = tokio::sync::oneshot::channel();
        let product_io = tokio::spawn(crate::research_connectors::run_auto_ingest_barrier_fixture(
            auto_ingest_context,
            auto_ingest_identity,
            io_entered_tx,
            io_release_rx,
        ));
        io_entered_rx
            .await
            .map_err(|_| "AutoIngest blocking closure did not park".to_string())?;
        product_io.abort();
        let _ = product_io.await;
        state
            .switch_workspace(workspace("workspace-a", canonical_a.clone()))
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            state
                .delete_workspace_owned(&crate::workspace::WorkspaceId::from_name("workspace-b"))
                .await
                .is_err_and(|error| error.to_string().contains("controls:"))
        );
        let workspace_b_id = crate::workspace::WorkspaceId::from_name("workspace-b");
        let before_failed_auto_ingest_relink = state
            .workspace
            .registry
            .open(&workspace_b_id)
            .map_err(|error| error.to_string())?;
        let auto_ingest_relink = temp.path().join("workspace-b-auto-ingest-relink");
        std::fs::create_dir_all(&auto_ingest_relink).map_err(|error| error.to_string())?;
        assert!(
            state
                .link_workspace_project_owned(&workspace_b_id, auto_ingest_relink)
                .await
                .is_err_and(|error| error.to_string().contains("controls:"))
        );
        let after_failed_auto_ingest_relink = state
            .workspace
            .registry
            .open(&workspace_b_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            after_failed_auto_ingest_relink.project_root,
            before_failed_auto_ingest_relink.project_root
        );
        assert_eq!(
            after_failed_auto_ingest_relink
                .metadata
                .project_root_revision,
            before_failed_auto_ingest_relink
                .metadata
                .project_root_revision
        );
        io_release_tx
            .send(())
            .map_err(|_| "AutoIngest blocking barrier receiver was dropped".to_string())?;
        for _ in 0..100 {
            if crate::research::list_sources(&canonical_b, None, None)
                .is_ok_and(|sources| !sources.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            crate::research::list_sources(&canonical_b, None, None)
                .map_err(|error| error.to_string())?
                .first()
                .map(|source| source.title.as_str()),
            Some("Auto-ingest lifetime barrier")
        );
        for _ in 0..100 {
            if state
                .ensure_workspace_idle_for_delete(&workspace_b_id)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        state
            .ensure_workspace_idle_for_delete(&workspace_b_id)
            .await
            .map_err(|error| format!("AutoIngest retained workspace after settlement: {error}"))?;
        let analysis_product_b = state
            .product_data_for_scope("workspace-b", &stale_b_generation)
            .await
            .map_err(|error| error.to_string())?;
        let (analysis_entered_tx, analysis_entered_rx) = tokio::sync::oneshot::channel();
        let (analysis_release_tx, analysis_release_rx) = tokio::sync::oneshot::channel();
        let analysis_receipt = analysis_product_b
            .start_analysis_fixture("owned-fixture", analysis_entered_tx, analysis_release_rx)
            .map_err(|error| error.to_string())?;
        analysis_entered_rx
            .await
            .map_err(|_| "owned analysis fixture did not start".to_string())?;
        drop(analysis_product_b);
        let relinked_project = temp.path().join("workspace-b-relinked-project");
        std::fs::create_dir_all(&relinked_project).map_err(|error| error.to_string())?;
        let before_failed_relink = state
            .workspace
            .registry
            .open(&workspace_b_id)
            .map_err(|error| error.to_string())?;
        assert!(
            state
                .link_workspace_project_owned(&workspace_b_id, relinked_project.clone())
                .await
                .is_err_and(|error| error.to_string().contains("controls:"))
        );
        let after_failed_relink = state
            .workspace
            .registry
            .open(&workspace_b_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            after_failed_relink.project_root,
            before_failed_relink.project_root
        );
        assert_eq!(
            after_failed_relink.metadata.project_root_revision,
            before_failed_relink.metadata.project_root_revision
        );
        assert!(
            state
                .delete_workspace_owned(&workspace_b_id)
                .await
                .is_err_and(|error| error.to_string().contains("controls:"))
        );
        analysis_release_tx
            .send(())
            .map_err(|_| "owned analysis fixture release was dropped".to_string())?;
        for _ in 0..100 {
            if state
                .ensure_workspace_idle_for_delete(&workspace_b_id)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        state
            .ensure_workspace_idle_for_delete(&workspace_b_id)
            .await
            .map_err(|error| format!("joined analysis retained active owner: {error}"))?;
        let analysis_product_b = state
            .product_data_for_scope("workspace-b", &stale_b_generation)
            .await
            .map_err(|error| error.to_string())?;
        for _ in 0..100 {
            match analysis_product_b.poll_analysis(&analysis_receipt) {
                Ok(crate::product_data_io::AnalysisWaitReceipt::Started { .. }) => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Ok(crate::product_data_io::AnalysisWaitReceipt::Joined {
                    execution_error: Some(_),
                    ..
                }) => break,
                Ok(other) => {
                    return Err(format!(
                        "owned analysis fixture returned unexpected status: {other:?}"
                    ));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        drop(analysis_product_b);
        let relinked = state
            .link_workspace_project_owned(&workspace_b_id, relinked_project)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            relinked.metadata.project_root_revision,
            before_failed_relink
                .metadata
                .project_root_revision
                .checked_add(1)
                .ok_or_else(|| "project-root revision overflow in test".to_string())?
        );
        assert!(
            state
                .product_data_for_scope("workspace-b", &stale_b_generation)
                .await
                .is_err()
        );
        for _ in 0..100 {
            if state
                .ensure_workspace_idle_for_delete(&workspace_b_id)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        state
            .delete_workspace_owned(&workspace_b_id)
            .await
            .map_err(|error| error.to_string())?;
        let recreated_b_root = temp.path().join("workspace-b-recreated");
        let (recreated_b, created) = state
            .create_workspace_owned(
                "workspace-b",
                crate::workspace::WorkspaceKind::General,
                Some(recreated_b_root),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(created);
        assert!(
            state
                .product_data_for_scope(recreated_b.id.as_str(), &stale_b_generation)
                .await
                .is_err()
        );
        let reopened_a = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(
            &pool_a,
            &reopened_a
                .pool()
                .ok_or_else(|| "reopened workspace A pool missing".to_string())?
        ));

        let exited = state
            .exit_workspace()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(exited.status, WorkspaceTransitionStatus::Committed);
        assert!(state.current_workspace().await.is_none());
        let restored = state
            .conversation_store()
            .await
            .ok_or_else(|| "global conversation store missing".to_string())?;
        assert!(Arc::ptr_eq(&restored, &global_store));
        assert_eq!(agent.read(|agent| agent.working_dir()).await, None);
        assert_eq!(
            std::env::current_dir().map_err(|error| error.to_string())?,
            process_cwd
        );
        drop(reopened_a);
        drop(runtime_a);
        state
            .delete_workspace_owned(&crate::workspace::WorkspaceId::from_name("workspace-a"))
            .await
            .map_err(|error| error.to_string())?;
        assert!(state.chat_runtime_for_scope("workspace-a").await.is_err());
        Ok(())
    }
}

#[cfg(test)]
mod service_bootstrap_tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::memory::{Store, StoreItem};
    use echo_agent::testing::MockLlmClient;
    use futures::future::BoxFuture;

    fn seed_recoverable_attended_run(
        store: &crate::tasks::task_runtime::TaskRuntimeStore,
    ) -> Result<(), String> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, ExecutionMode, PlanTask, TaskPlan, TaskRunStatus,
        };
        let workspace_id = store.active_workspace_id();
        store
            .create_run(
                "healthy-global-run",
                &workspace_id,
                "ordinary-conversation",
                "root-message",
                DomainProfile::General,
                "healthy global goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "healthy-plan".to_string(),
                run_id: "healthy-global-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("healthy global goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "healthy-task".to_string(),
                    title: "Wait for owner".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("healthy-global-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("healthy-global-run", true, true, None, None)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    struct SchedulerInitFailureStore;

    fn scheduler_store_failure<T>() -> echo_agent::error::Result<T> {
        Err(echo_agent::error::ReactError::Other(
            "injected scheduler store failure".to_string(),
        ))
    }

    impl Store for SchedulerInitFailureStore {
        fn put<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
            _value: serde_json::Value,
        ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn get<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<Option<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn search<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _query: &'a str,
            _limit: usize,
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn delete<'a>(
            &'a self,
            _namespace: &'a [&'a str],
            _key: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<bool>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn list_namespaces<'a>(
            &'a self,
            _prefix: Option<&'a [&'a str]>,
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<Vec<String>>>> {
            Box::pin(async { scheduler_store_failure() })
        }

        fn list<'a>(
            &'a self,
            _namespace: &'a [&'a str],
        ) -> BoxFuture<'a, echo_agent::error::Result<Vec<StoreItem>>> {
            Box::pin(async { scheduler_store_failure() })
        }
    }

    #[tokio::test]
    async fn scheduler_init_failure_does_not_start_task_service_or_run_driver()
    -> std::result::Result<(), String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("service bootstrap test")
            .build()
            .map_err(|error| error.to_string())?;
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            std::env::temp_dir().join(format!("eko-mcp-{}.json", uuid::Uuid::new_v4())),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            AgentHandle::new(agent),
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        let runtime_store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        state.tasks.runtime = Some(runtime_store.clone());

        let result = state
            .start_scheduler_and_task_service(Some(Arc::new(SchedulerInitFailureStore)))
            .await;

        assert!(result.is_err());
        assert!(state.scheduler.runner.is_none());
        assert!(state.tasks.service.is_none());
        assert_eq!(runtime_store.active_run_driver_count()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_workspace_does_not_block_healthy_global_boot_recovery()
    -> std::result::Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("boot isolation test")
            .build()
            .map_err(|error| error.to_string())?;
        let mcp_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut state = AppState::from_shared(
            AgentHandle::new(agent),
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        let global = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        seed_recoverable_attended_run(&global)?;
        state.tasks.runtime = Some(global.clone());
        let registry_root = temp.path().join("registry");
        let registry = Arc::new(
            crate::workspace::registry::WorkspaceRegistry::with_base_dir(registry_root)
                .map_err(|error| error.to_string())?,
        );
        let corrupt_root = temp.path().join("corrupt-workspace");
        registry
            .create_at(
                "corrupt",
                crate::workspace::WorkspaceKind::General,
                corrupt_root.clone(),
            )
            .map_err(|error| error.to_string())?;
        let tasks_root = crate::workspace::layout::WorkspaceLayout::tasks(&corrupt_root);
        std::fs::remove_dir_all(&tasks_root).map_err(|error| error.to_string())?;
        std::fs::write(&tasks_root, "not a directory").map_err(|error| error.to_string())?;
        state.workspace.registry = registry;

        let report = state.reconcile_task_runs_at_boot().await;
        assert_eq!(report.recovered, 1);
        assert_eq!(report.blocked, 1);
        assert_eq!(report.resumed, 0);
        assert!(
            report
                .failed_scopes
                .iter()
                .any(|failure| failure.contains("corrupt"))
        );
        assert_eq!(
            global
                .get_run("healthy-global-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Paused)
        );
        Ok(())
    }
}

async fn await_workspace_settlement(
    handle: &mut WorkspaceSettlementHandle,
) -> anyhow::Result<WorkspaceSettlementOutcome> {
    handle
        .await
        .map_err(|error| anyhow::anyhow!("workspace settlement task failed: {error}"))?
}

#[cfg(test)]
fn ensure_no_running_task_runs(
    transition: Option<&crate::tasks::task_runtime::store::TaskRuntimeWorkspaceTransition<'_>>,
) -> anyhow::Result<()> {
    let Some(transition) = transition else {
        return Ok(());
    };
    let running = transition
        .list_runs_in(&[crate::tasks::task_runtime::TaskRunStatus::Running])
        .map_err(|error| anyhow::anyhow!("Failed to inspect active task runs: {error}"))?;
    if running.is_empty() {
        return Ok(());
    }
    let run_ids = running
        .iter()
        .take(5)
        .map(|run| run.run_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!("Cannot change workspace while TaskRun is running: {run_ids}")
}

#[cfg(test)]
mod permission_rule_tests {
    use super::*;
    use echo_agent::tools::permission::{RuleBehavior, RuleMatcher, RuleSource, ToolPermission};

    #[tokio::test]
    async fn scheduler_shutdown_joins_owned_handle_and_is_idempotent()
    -> std::result::Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = crate::scheduler::CronTaskStore::new()
            .with_path(temp.path().join("scheduler-tasks.json"));
        let cancel_token = echo_agent::agent::CancellationToken::new();
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_task| Box::pin(async { Ok("done".to_string()) }));
        let runner = Arc::new(
            crate::scheduler::SchedulerRunner::new(store, cancel_token.clone(), fire_fn)
                .await
                .map_err(|error| error.to_string())?,
        );
        let handle = runner.clone().spawn();
        let scheduler = SchedulerState {
            runner: Some(runner),
            cancel_token,
            handle: Mutex::new(Some(handle)),
        };

        scheduler
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        scheduler
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        assert!(scheduler.handle.lock().await.is_none());
        Ok(())
    }

    #[test]
    fn framework_permission_rule_parsing_preserves_semantics()
    -> std::result::Result<(), String> {
        let rule = echo_agent::tools::permission::PermissionRule {
            matcher: "permission:write".parse().map_err(|error: String| error)?,
            behavior: "deny".parse().map_err(|error: String| error)?,
            source: "projectSettings"
                .parse()
                .map_err(|error: String| error)?,
            description: Some("EKO application permission rule".to_string()),
        };
        assert!(matches!(
            rule.matcher,
            RuleMatcher::Permission {
                permission: ToolPermission::Write
            }
        ));
        assert!(matches!(rule.behavior, RuleBehavior::Deny { .. }));
        assert_eq!(rule.source, RuleSource::ProjectSettings);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_preflight_rejects_running_task_runs() -> std::result::Result<(), String> {
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

        let runtime = crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
            .map_err(|error| error.to_string())?;
        runtime
            .create_run(
                "workspace-transition-run",
                "workspace-a",
                "conversation-a",
                "message-a",
                DomainProfile::General,
                "verify workspace transition",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        {
            let transition = runtime
                .begin_workspace_transition()
                .await
                .map_err(|error| error.to_string())?;
            assert!(ensure_no_running_task_runs(Some(&transition)).is_ok());
        }

        runtime
            .transition_run("workspace-transition-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let transition = runtime
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        let error = match ensure_no_running_task_runs(Some(&transition)) {
            Ok(()) => return Err("a running TaskRun did not block workspace change".to_string()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("workspace-transition-run"));
        Ok(())
    }
}
