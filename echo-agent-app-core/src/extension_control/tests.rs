#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    async fn skill_projection_present(
        agent: &crate::agent_handle::AgentHandle,
        name: &str,
    ) -> bool {
        context_projection_present(agent, format!("echo-agent:skill:{name}")).await
    }

    async fn context_projection_present(
        agent: &crate::agent_handle::AgentHandle,
        marker: String,
    ) -> bool {
        agent
            .read_async(|agent| {
                Box::pin(async move { agent.context().lock().await.has_projection(&marker) })
            })
            .await
    }

    struct FanoutFixture {
        temp: tempfile::TempDir,
        state: Arc<AppState>,
        seed_pool: Arc<crate::agent_pool::AgentPool>,
        workspaces: Vec<crate::workspace::Workspace>,
    }

    async fn fanout_fixture(workspace_count: usize) -> Result<FanoutFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let skill_root = temp.path().join("skills");
        let skill = skill_root.join("fanout-skill");
        std::fs::create_dir_all(&skill).map_err(|error| error.to_string())?;
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fanout-skill\ndescription: fanout fixture\n---\nfanout",
        )
        .map_err(|error| error.to_string())?;
        let primary = crate::agent_handle::AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("extension fanout test")
                .enable_tools()
                .working_dir(temp.path())
                .build()
                .map_err(|error| error.to_string())?,
        );
        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            primary.clone(),
            temp.path().to_path_buf(),
            temp.path().join("plugins.json"),
            temp.path().join("plugin-data"),
        )
        .await
        .map_err(|error| error.to_string())?;
        let seed_pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 8, false).await,
        );
        seed_pool
            .update_mcp_config_snapshot(Default::default())
            .await;
        plugin_runtime
            .bind_agent_pool(Arc::downgrade(&seed_pool))
            .await
            .map_err(|error| error.to_string())?;
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
        .with_plugin_runtime(Some(plugin_runtime));
        state.set_pool(seed_pool.clone());
        state.skills_hub = Arc::new(tokio::sync::RwLock::new(SkillsHub::with_root(
            skill_root.clone(),
        )));
        let enabled_config_path = temp.path().join("enabled-skills.json");
        state.extension_control = Arc::new(ExtensionControlService::with_enabled_config_path(
            enabled_config_path.clone(),
        ));
        let registry = Arc::new(
            crate::workspace::registry::WorkspaceRegistry::with_base_dir(
                temp.path().join("workspaces"),
            )
            .map_err(|error| error.to_string())?,
        );
        let mut workspaces = Vec::new();
        for index in 0..workspace_count {
            let name = format!("workspace-{index}");
            workspaces.push(
                registry
                    .create_at(
                        &name,
                        crate::workspace::WorkspaceKind::General,
                        temp.path().join(&name),
                    )
                    .map_err(|error| error.to_string())?,
            );
        }
        state.workspace.registry = registry;
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
        for workspace in &workspaces {
            state
                .switch_workspace(workspace.clone())
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(FanoutFixture {
            temp,
            state,
            seed_pool,
            workspaces,
        })
    }

    async fn begin_shutdown_after_extension_admission(
        state: &Arc<AppState>,
    ) -> Result<tokio::task::JoinHandle<Result<(), String>>, String> {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        state.session.product_data_io.begin_shutdown()?;
        let product_data_io = state.session.product_data_io.clone();
        let shutdown = tokio::spawn(async move { product_data_io.join_shutdown().await });
        tokio::task::yield_now().await;
        if shutdown.is_finished() {
            shutdown.await.map_err(|error| error.to_string())??;
            return Err(
                "extension mutation was not admitted into ProductData lifecycle".to_string(),
            );
        }
        Ok(shutdown)
    }

    #[test]
    fn user_skill_source_is_exact_and_stable() {
        assert_eq!(
            user_skill_source("paper-reader"),
            "eko:user-skill:paper-reader"
        );
    }

    #[test]
    fn curated_skill_artifact_and_lifecycle_commit_are_idempotent() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let echo_agent_dir = temp.path().join(".eko");
        let draft_path = echo_agent_dir.join("skills/_drafts/curated-fixture/SKILL.md");
        let draft_parent = draft_path
            .parent()
            .ok_or_else(|| "draft fixture has no parent".to_string())?;
        std::fs::create_dir_all(draft_parent).map_err(|error| error.to_string())?;
        std::fs::write(
            &draft_path,
            "---\nname: curated-fixture\ndescription: fixture\n---\nbody",
        )
        .map_err(|error| error.to_string())?;
        let curator = crate::evolution::workspace_curator(&echo_agent_dir);
        curator
            .register_candidate_at("curated-fixture", Some(&draft_path))
            .map_err(|error| error.to_string())?;
        if !curator
            .promote_to_draft_at("curated-fixture", Some(&draft_path))
            .map_err(|error| error.to_string())?
        {
            return Err("fixture candidate did not become Draft".to_string());
        }

        let committed = promote_curated_skill_artifact(echo_agent_dir.clone(), "curated-fixture")?;
        assert!(!committed.idempotent);
        assert!(committed.active_path.is_file());
        let active = curator
            .load_state()
            .map_err(|error| error.to_string())?
            .skills
            .get("curated-fixture")
            .map(|metadata| metadata.lifecycle);
        assert_eq!(active, Some(echo_agent::evolution::SkillLifecycle::Active));

        let repeated = promote_curated_skill_artifact(echo_agent_dir, "curated-fixture")?;
        assert!(repeated.idempotent);
        assert_eq!(repeated.active_path, committed.active_path);
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_curated_skill_promotion() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        let echo_agent_dir = fixture.temp.path().join("curated-workspace/.eko");
        let draft_path = echo_agent_dir.join("skills/_drafts/curated-drop/SKILL.md");
        let draft_parent = draft_path
            .parent()
            .ok_or_else(|| "draft fixture has no parent".to_string())?;
        std::fs::create_dir_all(draft_parent).map_err(|error| error.to_string())?;
        std::fs::write(
            &draft_path,
            "---\nname: curated-drop\ndescription: fixture\n---\nbody",
        )
        .map_err(|error| error.to_string())?;
        let curator = crate::evolution::workspace_curator(&echo_agent_dir);
        curator
            .register_candidate_at("curated-drop", Some(&draft_path))
            .map_err(|error| error.to_string())?;
        if !curator
            .promote_to_draft_at("curated-drop", Some(&draft_path))
            .map_err(|error| error.to_string())?
        {
            return Err("fixture candidate did not become Draft".to_string());
        }
        let store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration = crate::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            echo_agent_dir,
            store,
        );
        let generation = integration
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        fixture.state.session.product_data_io.install_test_barrier(
            "promote curated skill artifact",
            entered_tx,
            release_rx,
        );
        let state = Arc::clone(&fixture.state);
        let service = Arc::clone(&state.extension_control);
        let caller = tokio::spawn(async move {
            service
                .publish_curated_skill(&state, None, generation, "curated-drop")
                .await
        });
        entered_rx
            .await
            .map_err(|_| "curated promotion never reached durable I/O".to_string())?;
        caller.abort();
        release_tx
            .send(())
            .map_err(|_| "curated promotion barrier owner was dropped".to_string())?;
        fixture
            .state
            .extension_control
            .begin_shutdown(&fixture.state)?;
        fixture
            .state
            .extension_control
            .join_shutdown(&fixture.state)
            .await?;

        let active = curator
            .load_state()
            .map_err(|error| error.to_string())?
            .skills
            .get("curated-drop")
            .map(|metadata| metadata.lifecycle);
        assert_eq!(active, Some(echo_agent::evolution::SkillLifecycle::Active));
        let primary = fixture.state.connection.primary_agent();
        assert!(
            skill_source_present(&primary, "curated-drop", "eko:curated-skill:curated-drop").await
        );
        Ok(())
    }

    #[test]
    fn mcp_transport_preserves_protocol_shape() {
        let stdio = echo_agent::mcp::McpServerEntry {
            command: Some("npx".to_string()),
            ..Default::default()
        };
        assert_eq!(mcp_transport(&stdio), "stdio");
    }

    #[test]
    fn mcp_snapshot_preserves_gui_contract_fields() -> Result<(), String> {
        let snapshot = ExtensionMcpServer {
            name: "local".to_string(),
            status: "connected".to_string(),
            transport: "stdio".to_string(),
            tool_count: 1,
            tools: vec![ExtensionMcpTool {
                name: "read".to_string(),
                description: "Read a value".to_string(),
            }],
            connected_at: None,
            error: None,
            enabled: true,
        };
        let value = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("enabled").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .get("tools")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert!(
            value
                .get("connected_at")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_health_projection_rejects_same_id_workspace_aba() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let workspace = fixture
            .workspaces
            .first()
            .cloned()
            .ok_or_else(|| "workspace fixture missing".to_string())?;
        let service = Arc::clone(&fixture.state.extension_control);
        service
            .upsert_mcp_server(
                &fixture.state,
                "aba-mcp".to_string(),
                echo_agent::mcp::McpServerEntry {
                    command: Some("unused-disabled-mcp".to_string()),
                    disabled: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;

        let old_runtime = fixture
            .state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let old_scope = mcp_health_scope_key(&old_runtime).map_err(|error| error.to_string())?;
        drop(old_runtime);

        fixture
            .state
            .delete_workspace_owned(&workspace.id)
            .await
            .map_err(|error| error.to_string())?;
        let recreated = fixture
            .state
            .workspace
            .registry
            .create_at(&workspace.name, workspace.kind, workspace.root)
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .switch_workspace(recreated)
            .await
            .map_err(|error| error.to_string())?;
        let new_runtime = fixture
            .state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let new_scope = mcp_health_scope_key(&new_runtime).map_err(|error| error.to_string())?;
        assert_ne!(old_scope, new_scope);

        fixture.state.plugins.mcp_health.write().await.insert(
            old_scope,
            HashMap::from([(
                "aba-mcp".to_string(),
                McpHealthStatus {
                    name: "aba-mcp".to_string(),
                    healthy: false,
                    last_check: Some(chrono::Utc::now()),
                    error: Some("stale-generation-health".to_string()),
                },
            )]),
        );
        let stale_projection = service
            .list_mcp_servers_scoped(&fixture.state, Some(&new_runtime))
            .await
            .map_err(|error| error.to_string())?;
        let stale_server = stale_projection
            .iter()
            .find(|server| server.name == "aba-mcp")
            .ok_or_else(|| "MCP projection missing after workspace recreation".to_string())?;
        assert_eq!(stale_server.error, None);

        fixture.state.plugins.mcp_health.write().await.insert(
            new_scope,
            HashMap::from([(
                "aba-mcp".to_string(),
                McpHealthStatus {
                    name: "aba-mcp".to_string(),
                    healthy: false,
                    last_check: Some(chrono::Utc::now()),
                    error: Some("new-generation-health".to_string()),
                },
            )]),
        );
        let current_projection = service
            .list_mcp_servers_scoped(&fixture.state, Some(&new_runtime))
            .await
            .map_err(|error| error.to_string())?;
        let current_server = current_projection
            .iter()
            .find(|server| server.name == "aba-mcp")
            .ok_or_else(|| "current MCP projection missing".to_string())?;
        assert_eq!(
            current_server.error.as_deref(),
            Some("new-generation-health")
        );
        Ok(())
    }

    #[tokio::test]
    async fn exact_skill_load_discards_disabled_siblings() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        for name in ["enabled", "disabled"] {
            let root = temp.path().join(name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            std::fs::write(
                root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} fixture\n---\n{name}"),
            )
            .map_err(|error| error.to_string())?;
        }
        let agent = crate::agent_handle::AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(Arc::new(MockLlmClient::new()))
                .system_prompt("extension control test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let loaded = load_exact_user_skill(
            &agent,
            "enabled",
            temp.path().to_path_buf(),
            user_skill_source("enabled"),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(loaded, vec!["enabled".to_string()]);
        let (enabled, disabled) = agent
            .read(|agent| (agent.has_skill("enabled"), agent.has_skill("disabled")))
            .await;
        assert!(enabled);
        assert!(!disabled);
        Ok(())
    }

    #[tokio::test]
    async fn global_policy_reaches_three_loaded_workspaces_and_future_forks() -> Result<(), String>
    {
        let fixture = fanout_fixture(3).await?;
        let before = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let mut existing_agents = Vec::new();
        for target in before.iter() {
            let lease = target
                .pool()
                .acquire(&format!("existing-{}", target.scope()))
                .await
                .map_err(|error| error.to_string())?;
            existing_agents.push((target.scope().to_string(), lease.agent()));
            drop(lease);
        }
        drop(before);
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(targets.iter().count(), 4);
        for target in targets.iter() {
            assert!(
                skill_source_present(
                    &target.primary_agent(),
                    "fanout-skill",
                    &user_skill_source("fanout-skill"),
                )
                .await,
                "target {} missed global skill policy",
                target.scope()
            );
        }
        for (scope, agent) in existing_agents {
            assert!(
                agent.read(|agent| agent.has_skill("fanout-skill")).await,
                "existing pooled Agent in {scope} missed the generation"
            );
        }
        drop(targets);
        let future = fixture
            .seed_pool
            .acquire("future-global-consumer")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future
                .agent()
                .read(|agent| agent.has_skill("fanout-skill"))
                .await
        );
        drop(future);

        let future_workspace = fixture
            .state
            .workspace
            .registry
            .create_at(
                "future-workspace",
                crate::workspace::WorkspaceKind::General,
                fixture.temp.path().join("future-workspace"),
            )
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .switch_workspace(future_workspace)
            .await
            .map_err(|error| error.to_string())?;
        let future_control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future_control
                .runtime()
                .primary_agent()
                .read(|agent| agent.has_skill("fanout-skill"))
                .await
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_policy_does_not_block_the_next_skill_mutation() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        let config_path = fixture.temp.path().join("enabled-skills.json");
        std::fs::write(&config_path, "{ malformed").map_err(|error| error.to_string())?;

        let receipt = fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Settled);
        let config = EnabledSkillsConfig::load(&config_path).map_err(|error| error.to_string())?;
        assert!(config.is_enabled("fanout-skill"));
        Ok(())
    }

    #[tokio::test]
    async fn refresh_is_not_idempotent_when_runtime_entries_are_reloaded() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert!(!receipt.idempotent);
        assert!(
            receipt
                .target_receipts
                .iter()
                .any(|target| target.changed_entries.iter().any(|name| name == "fanout-skill"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn baseline_projection_tracks_enablement_on_primary_and_existing_pool_agents()
    -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        let pooled = fixture
            .seed_pool
            .acquire("baseline-conversation")
            .await
            .map_err(|error| error.to_string())?;
        let pooled_agent = pooled.agent();
        drop(pooled);
        let marker = "eko:methodology-baseline".to_string();

        fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "verification-before-completion")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            !context_projection_present(
                &fixture.state.connection.primary_agent(),
                marker.clone()
            )
            .await
        );
        assert!(!context_projection_present(&pooled_agent, marker.clone()).await);

        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "verification-before-completion")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            context_projection_present(
                &fixture.state.connection.primary_agent(),
                marker.clone()
            )
            .await
        );
        assert!(context_projection_present(&pooled_agent, marker.clone()).await);

        fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "verification-before-completion")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            !context_projection_present(&fixture.state.connection.primary_agent(), marker.clone())
                .await
        );
        assert!(!context_projection_present(&pooled_agent, marker).await);
        Ok(())
    }

    #[tokio::test]
    async fn list_projection_reads_loaded_state_from_agent_descriptors() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let entries = fixture
            .state
            .extension_control
            .list_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        let projected = entries
            .iter()
            .find(|entry| entry.catalog.name == "fanout-skill")
            .ok_or_else(|| "fanout-skill was absent from Extension projection".to_string())?;
        assert!(projected.loaded);
        Ok(())
    }

    #[tokio::test]
    async fn skill_activation_targets_only_the_requested_conversation_agent() -> Result<(), String>
    {
        use crate::extension_commands::{
            ExtensionCommand, ExtensionCommandDispatcher, ExtensionCommandIdentity,
            ExtensionCommandReceipt, ExtensionCommandRequest, ExtensionCommandStatus,
            ExtensionRequestScope, SkillCommand,
        };

        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let first = fixture
            .seed_pool
            .acquire("conversation-first")
            .await
            .map_err(|error| error.to_string())?;
        let first_agent = first.agent();
        drop(first);
        let second = fixture
            .seed_pool
            .acquire("conversation-second")
            .await
            .map_err(|error| error.to_string())?;
        let second_agent = second.agent();
        drop(second);

        let product_data = fixture
            .state
            .current_product_data()
            .await
            .map_err(|error| error.to_string())?;
        let scope = ExtensionRequestScope::new(
            product_data.workspace_id(),
            product_data.generation(),
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        let identity = ExtensionCommandIdentity::new("request-activate", "operation-activate")
            .map_err(|error| error.to_string())?;
        let receipt = ExtensionCommandDispatcher::new(fixture.state.clone())
            .dispatch_for_scope(
                scope.clone(),
                ExtensionCommandRequest {
                    request_id: identity.request_id,
                    operation_id: identity.operation_id,
                    scope: Some(scope),
                    command: ExtensionCommand::Skills(SkillCommand::Activate {
                        name: "fanout-skill".to_string(),
                    }),
                },
                "conversation-first".to_string(),
            )
            .await;
        match receipt {
            ExtensionCommandReceipt::Skills { meta, .. }
                if meta.status == ExtensionCommandStatus::Settled => {}
            other => return Err(format!("unexpected activation receipt: {other:?}")),
        }

        assert!(skill_projection_present(&first_agent, "fanout-skill").await);
        assert!(!skill_projection_present(&second_agent, "fanout-skill").await);
        assert!(
            !skill_projection_present(&fixture.state.connection.primary_agent(), "fanout-skill")
                .await
        );
        Ok(())
    }

    #[tokio::test]
    async fn plugin_package_install_enables_every_skill_atomically() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        let plugin = fixture.temp.path().join("plugin-package");
        std::fs::create_dir_all(&plugin).map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("plugin.json"),
            format!(
                r#"{{"$schema":"{}","name":"fixture-plugin"}}"#,
                echo_agent::plugin::AGENT_PLUGIN_SCHEMA_V1
            ),
        )
        .map_err(|error| error.to_string())?;
        for name in ["alpha", "beta"] {
            let skill = plugin.join("skills").join(name);
            std::fs::create_dir_all(&skill).map_err(|error| error.to_string())?;
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} fixture\n---\n{name}"),
            )
            .map_err(|error| error.to_string())?;
        }

        let source = plugin
            .to_str()
            .ok_or_else(|| "plugin fixture path is not UTF-8".to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .install_skill(&fixture.state, source)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            receipt.installed_names,
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert!(!receipt.settlement.idempotent);
        let config = EnabledSkillsConfig::load(&fixture.temp.path().join("enabled-skills.json"))
            .map_err(|error| error.to_string())?;
        assert!(config.is_enabled("alpha"));
        assert!(config.is_enabled("beta"));
        assert!(
            fixture
                .state
                .connection
                .primary_agent()
                .read(|agent| agent.has_skill("alpha") && agent.has_skill("beta"))
                .await
        );
        Ok(())
    }


    #[tokio::test]
    async fn mutation_permit_serializes_two_surface_commands() -> Result<(), String> {
        let service = Arc::new(ExtensionControlService::default());
        let first = service.mutation.lock().await;
        let contender = Arc::clone(&service);
        let second = tokio::spawn(async move {
            let _permit = contender.mutation.lock().await;
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        drop(first);
        second.await.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_plugin_authority_or_follower_reload() -> Result<(), String>
    {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let authority = control.plugin_runtime();
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let follower = targets
            .iter()
            .map(|target| target.plugin_runtime())
            .find(|runtime| !Arc::ptr_eq(runtime, &authority))
            .ok_or_else(|| "plugin follower fixture missing".to_string())?;
        let authority_before = authority.generation_for_test().await;
        let follower_before = follower.generation_for_test().await;
        drop(targets);
        drop(control);

        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller =
            tokio::spawn(async move { caller_service.reload_plugins(&caller_state).await });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("plugin settlement failed during shutdown: {error}"))?;

        assert!(authority.generation_for_test().await > authority_before);
        assert!(follower.generation_for_test().await > follower_before);
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_mcp_commit_and_reconcile_handoff() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller = tokio::spawn(async move {
            caller_service
                .upsert_mcp_server(
                    &caller_state,
                    "caller-drop-mcp".to_string(),
                    echo_agent::mcp::McpServerEntry {
                        command: Some("unused-disabled-mcp".to_string()),
                        disabled: true,
                        ..Default::default()
                    },
                )
                .await
        });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("MCP settlement failed during shutdown: {error}"))?;

        let snapshot = fixture.state.plugins.mcp_config.snapshot().await;
        assert!(
            snapshot
                .mcp_servers
                .get("caller-drop-mcp")
                .is_some_and(|entry| entry.disabled)
        );
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            let pool_snapshot = target
                .pool()
                .mcp_config_snapshot_for_test()
                .await
                .ok_or_else(|| format!("{} pool MCP snapshot missing", target.scope()))?;
            assert!(pool_snapshot.mcp_servers.contains_key("caller-drop-mcp"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_hook_reload() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let control = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let hook_dir = control.project_root().join(".eko");
        std::fs::create_dir_all(&hook_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            hook_dir.join("hooks.yaml"),
            "SessionStart:\n  - matcher: \"caller-drop-hook\"\n    hooks:\n      - type: prompt\n        prompt: \"settled\"\n",
        )
        .map_err(|error| error.to_string())?;
        let agent = control.runtime().primary_agent();
        drop(control);

        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let caller_state = Arc::clone(&fixture.state);
        let caller_service = Arc::clone(&service);
        let caller = tokio::spawn(async move { caller_service.reload_hooks(&caller_state).await });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(permit);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("Hook settlement failed during shutdown: {error}"))?;

        let sources = agent
            .read_async(|agent| {
                Box::pin(async move { agent.hook_registry().read().await.list_sources() })
            })
            .await;
        assert!(
            sources
                .iter()
                .any(|(source, rules)| source == "user_config" && *rules > 0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn plugin_reload_and_skill_enable_share_one_admission() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let permit = service.mutation.lock().await;
        let reload_state = Arc::clone(&fixture.state);
        let reload_service = Arc::clone(&service);
        let reload =
            tokio::spawn(async move { reload_service.reload_plugins(&reload_state).await });
        let enable_state = Arc::clone(&fixture.state);
        let enable_service = Arc::clone(&service);
        let enable = tokio::spawn(async move {
            enable_service
                .enable_skill(&enable_state, "fanout-skill")
                .await
        });
        tokio::task::yield_now().await;
        assert!(!reload.is_finished());
        assert!(!enable.is_finished());
        drop(permit);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        enable
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            assert!(
                skill_source_present(
                    &target.primary_agent(),
                    "fanout-skill",
                    &user_skill_source("fanout-skill"),
                )
                .await
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn lsp_rebind_and_gui_cli_mutations_share_one_admission() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let target = fixture
            .state
            .current_extension_control()
            .await
            .map_err(|error| error.to_string())?;
        let runtime = target.plugin_runtime();
        let root = target.project_root().to_path_buf();
        drop(target);
        let permit = service.mutation.lock().await;
        let watcher_service = Arc::clone(&service);
        let watcher =
            tokio::spawn(async move { watcher_service.rebind_plugin_runtime(runtime, root).await });
        let gui_state = Arc::clone(&fixture.state);
        let gui_service = Arc::clone(&service);
        let gui = tokio::spawn(async move { gui_service.reload_plugins(&gui_state).await });
        let cli_state = Arc::clone(&fixture.state);
        let cli_service = Arc::clone(&service);
        let cli =
            tokio::spawn(async move { cli_service.enable_skill(&cli_state, "fanout-skill").await });
        tokio::task::yield_now().await;
        assert!(!watcher.is_finished());
        assert!(!gui.is_finished());
        assert!(!cli.is_finished());
        drop(permit);
        watcher
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        gui.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        cli.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_snapshot_keeps_exact_focus_through_concurrent_switch() -> Result<(), String> {
        let fixture = fanout_fixture(2).await?;
        let expected = fixture
            .workspaces
            .get(1)
            .ok_or_else(|| "focused workspace fixture missing".to_string())?
            .id
            .to_string();
        let next = fixture
            .workspaces
            .first()
            .cloned()
            .ok_or_else(|| "switch target fixture missing".to_string())?;
        let (entered, release) = fixture
            .state
            .park_next_workspace_control_acquire_for_test()?;
        let read_state = Arc::clone(&fixture.state);
        let read = tokio::spawn(async move {
            read_state
                .extension_control
                .plugin_catalog(&read_state)
                .await
        });
        entered
            .await
            .map_err(|_| "plugin read did not enter control acquisition".to_string())?;
        let switch_state = Arc::clone(&fixture.state);
        let switch = tokio::spawn(async move { switch_state.switch_workspace(next).await });
        release
            .send(())
            .map_err(|_| "plugin read control release was dropped".to_string())?;
        let snapshot = read
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(snapshot.authority_scope, expected);
        switch
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_snapshot_settles_before_concurrent_host_eviction() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let workspace_id = fixture
            .workspaces
            .first()
            .ok_or_else(|| "workspace fixture missing".to_string())?
            .id
            .clone();
        let expected = workspace_id.to_string();
        let (entered, release) = fixture
            .state
            .park_next_workspace_control_acquire_for_test()?;
        let read_state = Arc::clone(&fixture.state);
        let read = tokio::spawn(async move {
            read_state
                .extension_control
                .plugin_catalog(&read_state)
                .await
        });
        entered
            .await
            .map_err(|_| "plugin read did not enter control acquisition".to_string())?;
        let evict_state = Arc::clone(&fixture.state);
        let evict = tokio::spawn(async move {
            evict_state
                .evict_workspace_runtime_if_idle_for_test(&workspace_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!evict.is_finished());
        release
            .send(())
            .map_err(|_| "plugin read control release was dropped".to_string())?;
        let snapshot = read
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(snapshot.authority_scope, expected);
        let _eviction = evict.await.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn plugin_install_receipt_carries_authority_and_entry_snapshot() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let source = fixture.temp.path().join("receipt-plugin-source");
        crate::plugin_runtime::PluginRuntimeService::scaffold(&source, "receipt-plugin")
            .map_err(|error| error.to_string())?;
        let expected = fixture
            .workspaces
            .first()
            .ok_or_else(|| "workspace fixture missing".to_string())?
            .id
            .to_string();
        let receipt = fixture
            .state
            .extension_control
            .install_plugin(
                &fixture.state,
                &echo_agent::plugin::InstallSource::Local(source),
                echo_agent::plugin::PluginScope::Project,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.authority_scope, expected);
        assert_eq!(receipt.plugin_id.as_deref(), Some("receipt-plugin"));
        assert_eq!(
            receipt
                .entry
                .as_ref()
                .map(|entry| entry.manifest.name.as_str()),
            Some("receipt-plugin")
        );
        assert_eq!(receipt.target_receipts.len(), 2);
        assert_eq!(
            receipt.status,
            PluginSettlementStatus::Settled,
            "plugin install settlement degraded: {:?}",
            receipt.target_receipts
        );
        for target in &receipt.target_receipts {
            assert!(!target.workspace_generation.is_empty());
            assert!(!target.previous_prepared_generation.is_empty());
            assert!(
                target
                    .candidate_prepared_generation
                    .as_deref()
                    .is_some_and(|generation| !generation.is_empty())
            );
            assert_eq!(target.status, PluginTargetSettlementStatus::Settled);
            assert!(target.diagnostics.is_empty());
        }
        Ok(())
    }
}
