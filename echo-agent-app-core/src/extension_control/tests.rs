#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    struct FanoutFixture {
        temp: tempfile::TempDir,
        state: Arc<AppState>,
        seed_pool: Arc<crate::agent_pool::AgentPool>,
        workspaces: Vec<crate::workspace::Workspace>,
        enabled_config_path: PathBuf,
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
            enabled_config_path,
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
    async fn operation_and_content_identity_are_idempotent_and_conflicts_fail_closed()
    -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let service = Arc::clone(&fixture.state.extension_control);
        let first = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        std::fs::write(
            fixture.temp.path().join("skills/fanout-skill/SKILL.md"),
            "---\nname: fanout-skill\ndescription: changed after operation\n---\nchanged",
        )
        .map_err(|error| error.to_string())?;
        let changed_content = service
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert!(changed_content.desired_generation > first.desired_generation);
        let repeated = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(repeated.idempotent);
        assert_eq!(
            repeated.desired_generation,
            changed_content.desired_generation
        );

        let same_content = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-same-content",
                "fanout-skill",
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(same_content.idempotent);
        assert_eq!(
            same_content.desired_generation,
            changed_content.desired_generation
        );

        let conflict = service
            .set_skill_enabled_with_operation(
                &fixture.state,
                "operation-enable",
                "fanout-skill",
                false,
            )
            .await
            .err()
            .ok_or_else(|| "same operation with different content was accepted".to_string())?;
        assert!(matches!(
            conflict,
            SkillMutationError::OperationConflict { .. }
        ));
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert_eq!(
            config.desired_generation,
            changed_content.desired_generation
        );
        Ok(())
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_accepted_skill_settlement() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let state = Arc::clone(&fixture.state);
        let admission = Arc::clone(&state.extension_control);
        let service = Arc::clone(&admission);
        let mutation = admission.mutation.lock().await;
        let caller = tokio::spawn(async move {
            service
                .set_skill_enabled_with_operation(
                    &state,
                    "caller-drop-operation",
                    "fanout-skill",
                    true,
                )
                .await
        });
        let shutdown = begin_shutdown_after_extension_admission(&fixture.state).await?;
        caller.abort();
        drop(mutation);
        shutdown.await.map_err(|error| error.to_string())??;
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(config.desired_generation, config.settled_generation);
        assert!(config.repair_debt.is_none());
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn reopened_service_replays_durable_repair_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let mut config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        config.settled_generation = config.desired_generation.saturating_sub(1);
        config.set_repair_debt(SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "workspace-0".to_string(),
                component: "runtime_fanout".to_string(),
                expected_generation: config.desired_generation,
                observed_generation: None,
                reason: "simulated restart debt".to_string(),
                retryable: true,
            }],
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
        config
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;

        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let receipt = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Settled);
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(repaired.desired_generation, repaired.settled_generation);
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn reopened_service_replays_artifact_removal_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        let mut config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        config.set_repair_debt(SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "skill-artifact:fanout-skill".to_string(),
                component: "artifact".to_string(),
                expected_generation: config.desired_generation,
                observed_generation: None,
                reason: "simulated".to_string(),
                retryable: true,
            }],
            artifact_removals: vec!["fanout-skill".to_string()],
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
        config
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let receipt = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Settled);
        assert!(!fixture.temp.path().join("skills/fanout-skill").exists());
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn terminal_artifact_sync_failure_does_not_create_replay_debt() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let receipt = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.settlement.operation_id, "sync-operation");
        assert_eq!(receipt.settlement.status, SkillSettlementStatus::Degraded);
        assert!(receipt.results.iter().any(|result| !result.success));
        let committed = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(committed.repair_debt.is_none());
        let duplicate = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.results.is_empty());
        assert_eq!(duplicate.settlement.status, SkillSettlementStatus::Settled);
        let conflict = fixture
            .state
            .extension_control
            .sync_skills_with_operation(
                &fixture.state,
                "sync-operation",
                Some("fanout-skill"),
                true,
            )
            .await
            .err()
            .ok_or_else(|| "conflicting sync operation was accepted".to_string())?;
        assert!(
            conflict
                .to_string()
                .contains("conflicts with committed content")
        );

        let mut legacy_debt = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        legacy_debt.set_repair_debt(SkillRepairDebt {
            generation: legacy_debt.desired_generation,
            content_identity: legacy_debt.content_identity.clone(),
            attempts: 1,
            target_failures: vec![SkillRepairTargetDebt {
                target: "skill-artifact-sync:fanout-skill".to_string(),
                component: "artifact_sync".to_string(),
                expected_generation: legacy_debt.desired_generation,
                observed_generation: None,
                reason: "legacy untracked sync debt".to_string(),
                retryable: true,
            }],
            artifact_removals: Vec::new(),
            artifact_syncs: vec![SkillArtifactSyncDebt {
                name: "fanout-skill".to_string(),
                force: false,
            }],
            artifact_enablements: Vec::new(),
        });
        legacy_debt
            .save(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let replayed = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(replayed.status, SkillSettlementStatus::Degraded);
        let after_replay = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(after_replay.repair_debt.is_none());
        let next_mutation = reopened
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(next_mutation.status, SkillSettlementStatus::Settled);
        Ok(())
    }

    #[tokio::test]
    async fn installed_artifact_enablement_debt_replays_after_restart() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test installed artifact repair debt")
            .map_err(|error| error.to_string())?;
        record_install_repair_debt(
            &fixture.state,
            &flow,
            fixture.enabled_config_path.clone(),
            "fanout-skill",
            "simulated policy commit failure",
        )
        .await
        .map_err(|error| error.to_string())?;
        flow.settle(Some("simulated policy commit failure".to_string()));
        let committed = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(committed.repair_debt.as_ref().is_some_and(|debt| {
            debt.artifact_enablements
                .iter()
                .any(|name| name == "fanout-skill")
        }));

        let reopened = Arc::new(ExtensionControlService::with_enabled_config_path(
            fixture.enabled_config_path.clone(),
        ));
        let replayed = reopened
            .reconcile_enabled_skills_on_load(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(replayed.status, SkillSettlementStatus::Settled);
        let repaired = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            repaired
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(repaired.repair_debt.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn enabled_skill_content_change_advances_generation_on_refresh() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        let first = fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        std::fs::write(
            fixture.temp.path().join("skills/fanout-skill/SKILL.md"),
            "---\nname: fanout-skill\ndescription: changed fixture\n---\nchanged",
        )
        .map_err(|error| error.to_string())?;
        let refreshed = fixture
            .state
            .extension_control
            .refresh_enabled_skills(&fixture.state)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            refreshed.desired_generation,
            first.desired_generation.saturating_add(1)
        );
        assert_eq!(refreshed.status, SkillSettlementStatus::Settled);
        assert_ne!(refreshed.content_identity, first.content_identity);
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_cas_cannot_overwrite_newer_durable_policy() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let stale = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        let newer = fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test stale skill settlement")
            .map_err(|error| error.to_string())?;
        let stale_receipt = settle_skill_generation(
            &flow,
            fixture.enabled_config_path.clone(),
            stale,
            "stale-operation".to_string(),
            false,
            true,
            Vec::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        flow.settle(None);
        assert_eq!(stale_receipt.status, SkillSettlementStatus::Degraded);
        let durable = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert_eq!(durable.desired_generation, newer.desired_generation);
        assert_eq!(durable.content_identity, newer.content_identity);
        assert!(
            durable
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| !entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_is_rejected_before_runtime_fanout() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let stale = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        fixture
            .state
            .extension_control
            .disable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        let flow = fixture
            .state
            .session
            .product_data_io
            .begin_owned_flow("test pre-fanout generation CAS")
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .reconcile_skill_config(
                &fixture.state,
                &flow,
                stale,
                "stale-prefanout".to_string(),
                false,
                true,
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        flow.settle(None);
        assert_eq!(receipt.status, SkillSettlementStatus::Degraded);
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        for target in targets.iter() {
            assert!(
                !skill_source_present(
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
    async fn mid_fanout_failure_keeps_durable_policy_and_records_repair_debt() -> Result<(), String>
    {
        let fixture = fanout_fixture(3).await?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let middle_workspace = fixture
            .workspaces
            .get(1)
            .ok_or_else(|| "middle workspace fixture missing".to_string())?;
        let failing = targets
            .iter()
            .find(|target| target.scope() == middle_workspace.id.as_str())
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "middle workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        let receipt = fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.status, SkillSettlementStatus::Degraded);
        assert!(receipt.durable_committed);
        assert!(receipt.repair_debt.is_some());
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let first = targets
            .iter()
            .next()
            .ok_or_else(|| "global target missing".to_string())?;
        assert!(
            skill_source_present(
                &first.primary_agent(),
                "fanout-skill",
                &user_skill_source("fanout-skill"),
            )
            .await,
            "a settled target was incorrectly rolled back"
        );
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(config.desired_generation > config.settled_generation);
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn stale_artifact_operations_cannot_overwrite_or_remove_a_reinstall() -> Result<(), String>
    {
        let fixture = fanout_fixture(1).await?;
        let source_parent = fixture.temp.path().join("operation-sources");
        let source = source_parent.join("operation-skill");
        std::fs::create_dir_all(&source).map_err(|error| error.to_string())?;
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: operation-skill\ndescription: first install\n---\nfirst",
        )
        .map_err(|error| error.to_string())?;
        let service = Arc::clone(&fixture.state.extension_control);
        service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .uninstall_skill_with_operation(
                &fixture.state,
                "old-uninstall-operation",
                "operation-skill",
            )
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: operation-skill\ndescription: reinstall\n---\nsecond",
        )
        .map_err(|error| error.to_string())?;
        service
            .install_skill_with_operation(
                &fixture.state,
                "new-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let installed_path = fixture.temp.path().join("skills/operation-skill/SKILL.md");
        std::fs::write(
            &installed_path,
            "---\nname: operation-skill\ndescription: local edit\n---\nkeep-me",
        )
        .map_err(|error| error.to_string())?;

        let duplicate_install = service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                source.to_string_lossy().as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate_install.settlement.idempotent);
        let after_install_retry =
            std::fs::read_to_string(&installed_path).map_err(|error| error.to_string())?;
        assert!(after_install_retry.contains("keep-me"));

        let conflicting_source = source_parent.join("different-operation-skill");
        std::fs::create_dir_all(&conflicting_source).map_err(|error| error.to_string())?;
        std::fs::write(
            conflicting_source.join("SKILL.md"),
            "---\nname: different-operation-skill\ndescription: conflict\n---\nconflict",
        )
        .map_err(|error| error.to_string())?;
        let conflict = service
            .install_skill_with_operation(
                &fixture.state,
                "old-install-operation",
                conflicting_source.to_string_lossy().as_ref(),
            )
            .await
            .err()
            .ok_or_else(|| "conflicting install operation was accepted".to_string())?;
        assert!(matches!(
            conflict,
            SkillInstallError::Enable(SkillMutationError::OperationConflict { .. })
        ));
        assert!(
            !fixture
                .temp
                .path()
                .join("skills/different-operation-skill")
                .exists()
        );

        let duplicate_uninstall = service
            .uninstall_skill_with_operation(
                &fixture.state,
                "old-uninstall-operation",
                "operation-skill",
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate_uninstall.settlement.idempotent);
        assert!(!duplicate_uninstall.artifact_removed);
        assert!(installed_path.exists());
        let after_uninstall_retry =
            std::fs::read_to_string(installed_path).map_err(|error| error.to_string())?;
        assert!(after_uninstall_retry.contains("keep-me"));
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("operation-skill")
                .is_some_and(|entry| entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn install_degraded_settlement_keeps_installed_directory_and_desired_state()
    -> Result<(), String> {
        let fixture = fanout_fixture(2).await?;
        let source = fixture.temp.path().join("install-failure");
        std::fs::create_dir_all(&source).map_err(|error| error.to_string())?;
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: install-failure\ndescription: degraded install fixture\n---\ndegraded",
        )
        .map_err(|error| error.to_string())?;
        let targets = fixture
            .state
            .extension_runtime_targets()
            .await
            .map_err(|error| error.to_string())?;
        let failing = targets
            .iter()
            .find(|target| target.scope() != "global")
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        let installed = fixture
            .state
            .extension_control
            .install_skill(&fixture.state, source.to_string_lossy().as_ref())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(installed.name, "install-failure");
        assert_eq!(installed.settlement.status, SkillSettlementStatus::Degraded);
        assert!(
            fixture
                .state
                .skills_hub
                .read()
                .await
                .root()
                .join("install-failure")
                .exists()
        );
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("install-failure")
                .is_some_and(|entry| entry.enabled)
        );
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn uninstall_returns_typed_degraded_after_durable_disable() -> Result<(), String> {
        let fixture = fanout_fixture(1).await?;
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
        let failing = targets
            .iter()
            .find(|target| target.scope() != "global")
            .map(|target| target.plugin_runtime())
            .ok_or_else(|| "workspace target missing".to_string())?;
        drop(targets);
        failing
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .uninstall_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.settlement.status, SkillSettlementStatus::Degraded);
        assert!(receipt.artifact_removed);
        assert!(receipt.artifact_error.is_none());
        let config = EnabledSkillsConfig::load(&fixture.enabled_config_path)
            .map_err(|error| error.to_string())?;
        assert!(
            config
                .skills
                .get("fanout-skill")
                .is_some_and(|entry| !entry.enabled)
        );
        assert!(config.repair_debt.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn uninstall_of_absent_artifact_reports_not_removed() -> Result<(), String> {
        let fixture = fanout_fixture(0).await?;
        fixture
            .state
            .extension_control
            .enable_skill(&fixture.state, "fanout-skill")
            .await
            .map_err(|error| error.to_string())?;
        std::fs::remove_dir_all(fixture.temp.path().join("skills/fanout-skill"))
            .map_err(|error| error.to_string())?;
        let receipt = fixture
            .state
            .extension_control
            .uninstall_skill_with_operation(
                &fixture.state,
                "absent-artifact-uninstall",
                "fanout-skill",
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(!receipt.artifact_removed);
        assert!(receipt.artifact_error.is_none());
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
        assert_eq!(receipt.status, PluginSettlementStatus::Settled);
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
