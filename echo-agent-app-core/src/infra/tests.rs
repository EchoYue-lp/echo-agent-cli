#[cfg(test)]
mod ssh_operations_consumer_tests {
    //! 验证原模型请求与项目投影，非真实 SSH/模型效果验收。
    use super::*;
    use echo_agent::agent::Agent;
    use echo_agent::sandbox::{ExecutionResult, IsolationLevel, SandboxCommand, SandboxExecutor};
    use echo_agent::testing::MockLlmClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NamedExecutionFixture {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl SandboxExecutor for NamedExecutionFixture {
        fn name(&self) -> &str {
            self.name
        }
        fn isolation_level(&self) -> IsolationLevel {
            IsolationLevel::None
        }
        fn is_available(&self) -> BoxFuture<'_, bool> {
            Box::pin(async { true })
        }
        fn execute(&self, _command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "projection-only fixture cannot execute commands",
                )
                .into())
            })
        }
    }

    fn bound_store(
        root: &std::path::Path,
        files: Arc<crate::project::startup_tests::CountingFiles>,
        executor: Arc<dyn SandboxExecutor>,
    ) -> anyhow::Result<Arc<crate::tasks::task_runtime::TaskRuntimeStore>> {
        let store = Arc::new(crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()?);
        let workspace_id = store.active_workspace_id().to_string();
        store.bind_project_environment(
            &workspace_id,
            crate::workspace::ProjectLocation::Ssh {
                host: "fixture.invalid".to_string(),
                user: Some("fixture".to_string()),
                port: Some(22),
                path: root.to_string_lossy().into_owned(),
            },
            1,
            files,
            executor,
        )?;
        Ok(store)
    }

    fn model_system_text(client: &MockLlmClient) -> anyhow::Result<String> {
        Ok(client
            .last_messages()
            .ok_or_else(|| anyhow::anyhow!("model request absent"))?
            .iter()
            .filter(|message| matches!(message.role, echo_agent::llm::types::Role::System))
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tokio::test]
    async fn ssh_operations_reach_actual_execute_requests_and_clear_on_local_binding()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("remote-home");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("AGENTS.md"), "OPERATIONS_PROJECT_RULE")?;
        let files = Arc::new(crate::project::startup_tests::CountingFiles::new(
            root.clone(),
        ));
        let commands = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(MockLlmClient::new().with_responses([
            "first query done",
            "second query done",
            "local query done",
        ]));
        let mut agent = ReactAgentBuilder::new()
            .name("operations-projection-consumer")
            .model("fixture-model")
            .system_prompt("QUERY_FIXTURE")
            .auto_project_rules(false)
            .file_system(files.clone())
            .working_dir(root.clone())
            .llm_client(transport.clone())
            .build()?;
        let ssh: Arc<dyn SandboxExecutor> = Arc::new(NamedExecutionFixture {
            name: "ssh",
            calls: commands.clone(),
        });
        agent.set_sandbox_executor(ssh.clone());
        prepare_project_context_at_location(
            Some(&root),
            Some(&root),
            Some(directory.path()),
            files.clone(),
            ssh,
            Vec::new(),
            agent.config().get_token_limit(),
        )
        .await
        .map_err(anyhow::Error::msg)?
        .publish_for_fresh_subagent(&mut agent)
        .await;
        // execute会重置上下文；检查真正请求而非预先拼好的提示词字符串。
        for query in ["query the specified service", "query it again"] {
            agent.execute(query).await?;
            let text = model_system_text(&transport)?;
            assert_eq!(text.matches("# SSH Operations").count(), 1);
            assert_eq!(text.matches("OPERATIONS_PROJECT_RULE").count(), 1);
        }
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 1);
        let local: Arc<dyn SandboxExecutor> = Arc::new(NamedExecutionFixture {
            name: "local",
            calls: commands.clone(),
        });
        agent.set_sandbox_executor(local.clone());
        prepare_project_context_at_location(
            Some(&root),
            Some(&root),
            Some(directory.path()),
            files.clone(),
            local,
            Vec::new(),
            agent.config().get_token_limit(),
        )
        .await
        .map_err(anyhow::Error::msg)?
        .publish_for_fresh_subagent(&mut agent)
        .await;
        agent.execute("query local files").await?;
        let text = model_system_text(&transport)?;
        assert!(!text.contains("# SSH Operations"));
        assert_eq!(text.matches("OPERATIONS_PROJECT_RULE").count(), 1);
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 2);
        assert_eq!(commands.load(Ordering::SeqCst), 0);
        assert_eq!(transport.call_count(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn ssh_operations_factory_initializes_complete_snapshot_for_non_driver_consumers()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("remote-home");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("AGENTS.md"), "INITIAL_OPERATIONS_RULE")?;
        let files = Arc::new(crate::project::startup_tests::CountingFiles::new(
            root.clone(),
        ));
        let commands = Arc::new(AtomicUsize::new(0));
        let executor: Arc<dyn SandboxExecutor> = Arc::new(NamedExecutionFixture {
            name: "ssh",
            calls: commands.clone(),
        });
        let store = bound_store(&root, files.clone(), executor.clone())?;
        let _resources = store.test_resource_scope();
        let created = create_agent_with_diagnostics(
            &AgentCreateParams {
                project: Some(root.to_string_lossy().into_owned()),
                working_dir: Some(root),
                workspace_data_root: Some(directory.path().join("data")),
                file_system: Some(files),
                sandbox_executor: Some(executor),
                task_runtime_store: Some(store.clone()),
                ..Default::default()
            },
            &EkoConfig::default(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        // 与原reflection消费者一致，只取得已初始化的完整快照。
        // 产品模型循环另由下方factory→driver用例验证，不能用裸chat绕过准入。
        let messages = created.agent.get_messages().await;
        let text = messages
            .iter()
            .filter(|message| matches!(message.role, echo_agent::llm::types::Role::System))
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("# SSH Operations").count(), 1);
        assert_eq!(text.matches("INITIAL_OPERATIONS_RULE").count(), 1);
        assert_eq!(commands.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn ssh_operations_reach_factory_and_driver_model_requests() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("remote-home");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("AGENTS.md"), "DRIVER_OPERATIONS_RULE_ONE")?;
        let files = Arc::new(crate::project::startup_tests::CountingFiles::new(
            root.clone(),
        ));
        let commands = Arc::new(AtomicUsize::new(0));
        let executor: Arc<dyn SandboxExecutor> = Arc::new(NamedExecutionFixture {
            name: "ssh",
            calls: commands.clone(),
        });
        let store = bound_store(&root, files.clone(), executor.clone())?;
        let _resources = store.test_resource_scope();
        let mut created = create_agent_with_diagnostics(
            &AgentCreateParams {
                context_prepared_by_invocation: true,
                project: Some(root.to_string_lossy().into_owned()),
                working_dir: Some(root.clone()),
                workspace_data_root: Some(directory.path().join("data")),
                file_system: Some(files.clone()),
                sandbox_executor: Some(executor),
                task_runtime_store: Some(store.clone()),
                ..Default::default()
            },
            &EkoConfig::default(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 0);
        let transport = Arc::new(
            MockLlmClient::new().with_responses(["first query done", "second query done"]),
        );
        created.agent.set_llm_client(transport.clone());
        let agent = AgentHandle::new(created.agent);
        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        let sink = Arc::new(crate::chat_driver::ChannelChatSink::new(events));
        for (index, rule) in ["DRIVER_OPERATIONS_RULE_ONE", "DRIVER_OPERATIONS_RULE_TWO"]
            .into_iter()
            .enumerate()
        {
            std::fs::write(root.join("AGENTS.md"), rule)?;
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: crate::workspace::WorkspaceExecutionScope::global(root.clone()),
                workspace_io_receipt: None,
                pool: None,
                store: None,
                sink: sink.clone(),
                webhook_emitter: None,
                conv_id: Some("driver-operations".to_string()),
                root_message_id: format!("driver-operations-{index}"),
                attachments: Vec::new(),
                cancel: echo_agent::agent::CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let turn = crate::prepared_turn::PreparedUserTurn {
                instruction: "query the specified service".to_string(),
                resources: Vec::new(),
                authorship: crate::prepared_turn::InstructionAuthorship::User,
                agent_input: None,
            };
            let outcome = crate::chat_driver::drive_chat(&agent, &turn, resources)
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            assert_eq!(outcome, crate::chat_driver::TurnOutcome::Completed);
            let text = model_system_text(&transport)?;
            assert_eq!(text.matches("# SSH Operations").count(), 1);
            assert_eq!(text.matches(rule).count(), 1);
            if index == 1 {
                assert!(!text.contains("DRIVER_OPERATIONS_RULE_ONE"));
            }
            assert_eq!(
                files.count("read", &root.join("AGENTS.md")),
                index.saturating_add(1)
            );
        }
        assert_eq!(commands.load(Ordering::SeqCst), 0);
        assert_eq!(transport.call_count(), 2);
        Ok(())
    }
}

#[cfg(test)]
mod llm_config_tests {
    use super::{
        AgentCreateParams, build_llm_config, build_runtime_llm_config,
        create_agent_with_diagnostics, prepare_runtime_llm, test_runtime_llm_connection,
    };
    use crate::config::{ConfiguredModel, EkoConfig, ModelProviderConfig};
    use crate::model_config::ModelRuntimeConfig;
    use echo_agent::agent::Agent;
    use echo_agent::llm::LlmApiProtocol;
    use std::sync::Arc;

    #[tokio::test]
    async fn agent_boots_without_a_configured_provider() -> Result<(), String> {
        let created = create_agent_with_diagnostics(
            &AgentCreateParams {
                system_prompt: Some("deferred model setup test".to_string()),
                ..Default::default()
            },
            &EkoConfig::default(),
        )
        .await?;

        assert!(created.runtime_model.is_none());
        assert!(created.agent.model_name().is_empty());
        assert!(created.agent.llm_config().is_none());
        assert!(created.agent.llm_client().is_none());
        assert_eq!(created.agent.config().get_temperature(), None);
        let parent_store = created
            .agent
            .store()
            .cloned()
            .ok_or_else(|| "primary Agent has no memory store".to_string())?;
        let inherited = created
            .model_consumers
            .inherited_handle_for_test("reviewer")
            .ok_or_else(|| "reviewer Subagent is missing".to_string())?;
        let inherited_store = inherited
            .read(|agent| agent.store().cloned())
            .await
            .ok_or_else(|| "reviewer Subagent has no memory store".to_string())?;
        assert!(Arc::ptr_eq(&parent_store, &inherited_store));
        assert_eq!(
            inherited
                .read(|agent| agent.config().get_temperature())
                .await,
            None
        );
        Ok(())
    }

    #[test]
    fn provider_neutral_builder_uses_explicit_protocol_and_modalities() -> Result<(), String> {
        let config = build_llm_config(
            "gateway",
            "test-key",
            "test-model",
            "https://gateway.example/v1",
            LlmApiProtocol::Responses,
            echo_agent::llm::ModelInputModality::text_only(),
        )?;
        assert_eq!(config.api_protocol, LlmApiProtocol::Responses);
        assert_eq!(config.base_url, "https://gateway.example/v1/responses");
        assert_eq!(
            config.input_modalities,
            echo_agent::llm::ModelInputModality::text_only()
        );
        Ok(())
    }

    #[test]
    fn runtime_builder_injects_local_endpoint_without_api_key() -> Result<(), String> {
        let runtime = ModelRuntimeConfig {
            id: "local:model".to_string(),
            display_name: "Local model".to_string(),
            provider: "local".to_string(),
            model: "model".to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            input_modalities: echo_agent::llm::ModelInputModality::text_only(),
            auth_token: None,
            auth_source: "none".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            api_key_env: None,
            requires_api_key: false,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::ThinkingProfile::unknown(),
        };

        let config = build_runtime_llm_config(&runtime)?;
        assert!(config.api_key.is_empty());
        assert_eq!(
            config.base_url,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(config.api_protocol, LlmApiProtocol::ChatCompletions);
        Ok(())
    }

    #[test]
    fn runtime_preflight_rejects_invalid_authorization_header() {
        let runtime = ModelRuntimeConfig {
            id: "openai:gpt-test".to_string(),
            display_name: "Invalid token".to_string(),
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            api_protocol: LlmApiProtocol::Responses,
            input_modalities: echo_agent::llm::ModelInputModality::text_only(),
            auth_token: Some("invalid\nheader".to_string()),
            auth_source: "input".to_string(),
            base_url: Some("https://api.openai.com/v1/responses".to_string()),
            api_key_env: None,
            requires_api_key: true,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::ThinkingProfile::unknown(),
        };

        assert!(prepare_runtime_llm(&runtime).is_err());
    }

    #[tokio::test]
    async fn shared_connection_probe_uses_runtime_key_policy() {
        let runtime = ModelRuntimeConfig {
            id: "gateway:model".to_string(),
            display_name: "Model".to_string(),
            provider: "gateway".to_string(),
            model: "model".to_string(),
            api_protocol: LlmApiProtocol::Responses,
            input_modalities: echo_agent::llm::ModelInputModality::text_only(),
            auth_token: None,
            auth_source: "none".to_string(),
            base_url: Some("https://gateway.example/v1".to_string()),
            api_key_env: Some("GATEWAY_API_KEY".to_string()),
            requires_api_key: true,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::ThinkingProfile::unknown(),
        };

        let error = test_runtime_llm_connection(&runtime)
            .await
            .err()
            .unwrap_or_default();
        assert!(error.contains("requires an API key"));
    }

    fn selectable_model_config(agent_token_limit: usize) -> Result<EkoConfig, String> {
        let mut config = EkoConfig::default();
        config.agent.token_limit = agent_token_limit;
        config.model_providers.insert(
            "local".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            ConfiguredModel {
                id: "local:default".to_string(),
                display_name: "Default".to_string(),
                provider: "local".to_string(),
                model: "default-runtime".to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                context_window: Some(16_384),
                ..ConfiguredModel::default()
            },
            ConfiguredModel {
                id: "local:selected".to_string(),
                display_name: "Selected".to_string(),
                provider: "local".to_string(),
                model: "selected-runtime".to_string(),
                api_protocol: LlmApiProtocol::ChatCompletions,
                context_window: Some(65_536),
                ..ConfiguredModel::default()
            },
        ];
        crate::model_config::set_default_model(&mut config, "local:default")?;
        Ok(config)
    }

    #[tokio::test]
    async fn default_and_cli_selected_models_use_their_effective_context_window()
    -> Result<(), String> {
        let config = selectable_model_config(0)?;
        let default = create_agent_with_diagnostics(&AgentCreateParams::default(), &config).await?;
        let selected = create_agent_with_diagnostics(
            &AgentCreateParams {
                model: Some("local:selected".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;

        assert_eq!(default.agent.model_name(), "default-runtime");
        assert_eq!(default.agent.config().get_token_limit(), 16_384);
        assert_eq!(
            default
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            16_384
        );
        assert_eq!(selected.agent.model_name(), "selected-runtime");
        assert_eq!(selected.agent.config().get_token_limit(), 65_536);
        assert_eq!(
            selected
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            65_536
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_agent_token_limit_overrides_cli_selected_model_window() -> Result<(), String>
    {
        let config = selectable_model_config(7_777)?;
        let selected = create_agent_with_diagnostics(
            &AgentCreateParams {
                model: Some("local:selected".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;

        assert_eq!(selected.agent.config().get_token_limit(), 7_777);
        assert_eq!(
            selected
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            7_777
        );
        Ok(())
    }
}

#[cfg(test)]
mod temperature_request_tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};

    #[test]
    fn configured_model_projection_drops_retired_temperature_without_losing_limits()
    -> anyhow::Result<()> {
        let model: crate::config::ConfiguredModel = serde_json::from_value(serde_json::json!({
            "id": "local:test", "provider": "local", "model": "test",
            "temperature": 1.2, "max_tokens": 4096, "context_window": 128000
        }))?;
        let encoded = serde_json::to_value(&model)?;
        assert!(encoded.get("temperature").is_none());
        assert_eq!(model.max_tokens, Some(4096));
        assert_eq!(model.context_window, Some(128000));
        let config = EkoConfig {
            configured_models: vec![model],
            ..EkoConfig::default()
        };
        let view = serde_json::to_value(crate::model_config::configured_model_views(&config))?;
        let first = view
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or_else(|| anyhow::anyhow!("configured model view missing"))?;
        assert!(first.get("temperature").is_none());
        let profile: crate::profiles::Profile = serde_json::from_value(serde_json::json!({
            "name": "test", "model": "test", "temperature": 0.9,
            "created_at": "2026-11-03", "updated_at": "2026-11-03"
        }))?;
        assert!(serde_json::to_value(profile)?.get("temperature").is_none());
        Ok(())
    }

    async fn capture_response(
        State(requests): State<Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>>,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        requests.lock().await.push(body);
        Json(serde_json::json!({
            "id": "resp_loopback", "object": "response", "created_at": 1,
            "model": "deepseek-flash", "status": "completed",
            "output": [{"type": "message", "id": "msg_loopback", "role": "assistant",
                "content": [{"type": "output_text", "text": "OK", "annotations": []}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }))
    }

    #[tokio::test]
    async fn shared_gui_connection_request_omits_temperature_over_real_responses_http()
    -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}/v1/responses", listener.local_addr()?);
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/v1/responses", post(capture_response))
            .with_state(requests.clone());
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        let mut config = EkoConfig::default();
        config.model_providers.insert(
            "deepseek".to_string(),
            crate::config::ModelProviderConfig {
                base_url: Some(endpoint),
                ..Default::default()
            },
        );
        config
            .configured_models
            .push(serde_json::from_value(serde_json::json!({
                "id": "deepseek:test", "provider": "deepseek", "model": "deepseek-flash",
                "api_protocol": "responses", "temperature": 0.8
            }))?);
        let runtime = model_config::resolve_runtime_model(&config, None)?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            test_runtime_llm_connection(&runtime),
        )
        .await;
        let doctor = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            probe_runtime_model_connectivity(&runtime),
        )
        .await;
        let _ = shutdown.send(());
        let fixture_result = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
        let response = result?.map_err(anyhow::Error::msg)?;
        doctor??;
        fixture_result???;
        assert_eq!(response.response, "OK");
        let bodies = requests.lock().await;
        assert_eq!(bodies.len(), 2);
        for body in bodies.iter() {
            assert!(
                body.get("temperature").is_none(),
                "request contains retired temperature: {body}"
            );
            assert_eq!(
                body.get("model").and_then(serde_json::Value::as_str),
                Some("deepseek-flash")
            );
            assert!(body.get("input").is_some());
        }
        assert_eq!(
            bodies
                .get(1)
                .and_then(|body| body.get("max_output_tokens"))
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        Ok(())
    }
}

#[tokio::test]
async fn unbounded_main_agent_ignores_a_legacy_finite_iteration_setting() -> Result<(), String> {
    let mut config = EkoConfig::default();
    config.agent.max_iterations = 10;
    let created = create_agent_with_diagnostics(&AgentCreateParams::default(), &config).await?;
    assert_eq!(created.agent.config().get_max_iterations(), usize::MAX);
    Ok(())
}

#[tokio::test]
async fn unbounded_main_agent_completes_after_twelve_tool_iterations() -> Result<(), String> {
    use echo_agent::agent::{Agent, critic::StaticCritic};
    use echo_agent::testing::{MockLlmClient, MockTool};
    use std::sync::Arc;

    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut script = MockLlmClient::new();
    for iteration in 0..12 {
        script = script.then_tool_call(format!("read-{iteration}"), "iteration_probe", "{}");
    }
    let transport = Arc::new(script.with_response("已完成十二轮工具查询。"));
    let mut config = EkoConfig::default();
    config.agent.max_iterations = 10;
    let created = create_agent_with_diagnostics(
        &AgentCreateParams {
            working_dir: Some(directory.path().to_path_buf()),
            ..Default::default()
        },
        &config,
    )
    .await?;
    let mut agent = created.agent;
    agent.set_llm_client(transport.clone());
    agent.set_critic(Arc::new(StaticCritic::always_pass()));
    agent.register_tool(Box::new(
        MockTool::new("iteration_probe").with_default_success("本轮查询已完成"),
    ));
    let answer = agent
        .execute("完成十二轮工具查询后给出结论。")
        .await
        .map_err(|error| error.to_string())?;
    assert!(answer.contains("已完成十二轮工具查询"));
    assert_eq!(transport.call_count(), 13);
    Ok(())
}

#[cfg(test)]
mod resolve_subagent_model_tests {
    use super::{
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS, SubagentRuntimeGeneration, TASK_MANAGEMENT_GUIDE,
        build_readonly_subagent_agent, build_writer_subagent_agent, configure_run_code_capability,
        eko_visibility_horizon, resolve_fixed_subagent_generation, resolved_max_tool_output_tokens,
        subagent_model_binding, tool_output_artifact_config,
    };
    use crate::config::{ConfiguredModel, EkoConfig, ModelProviderConfig};
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::sandbox::SandboxManager;
    use echo_agent::subagent::{SubagentPromptCompiler, SubagentRegistry};
    use std::sync::Arc;

    fn test_command_cells()
    -> echo_agent::error::Result<Arc<dyn echo_agent::tools::cell::CommandCellRegistry>> {
        Ok(Arc::new(
            echo_agent::tasks::BackgroundCommandManager::default(),
        ))
    }

    /// GitHub 托管 runner 的内核限制会让 bwrap 无法在新 network namespace
    /// 里配置 loopback(`RTM_NEWADDR: Operation not permitted`)。shell 策略
    /// 测试验证的是工具层权限让渡,不依赖网络隔离本身;netns 不可用的
    /// 环境降级为无 OS 沙箱执行,避免把环境限制误判为回归。
    fn shell_policy_sandbox() -> SandboxManager {
        if linux_unshare_net_available() {
            SandboxManager::local_sandbox()
        } else {
            SandboxManager::local_only()
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_unshare_net_available() -> bool {
        std::process::Command::new("bwrap")
            .args([
                "--unshare-net",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--",
                "true",
            ])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(not(target_os = "linux"))]
    fn linux_unshare_net_available() -> bool {
        true
    }

    #[test]
    fn stable_task_guide_stays_within_cache_budget() {
        assert!(TASK_MANAGEMENT_GUIDE.chars().count() <= 2_400);
    }

    #[test]
    fn tool_output_budget_uses_eko_default_when_unset() {
        assert_eq!(
            resolved_max_tool_output_tokens(0),
            DEFAULT_MAX_TOOL_OUTPUT_TOKENS
        );
        assert_eq!(resolved_max_tool_output_tokens(4_000), 4_000);
    }

    #[test]
    fn tool_trace_budget_scales_with_model_window_and_keeps_latest_turn() {
        let small = eko_visibility_horizon(16_384);
        assert_eq!(small.active_window_turns, 1);
        assert_eq!(small.retained_tool_tokens, Some(4_096));
        assert_eq!(small.compact_minimum_tokens, 4_000);

        let medium = eko_visibility_horizon(128_000);
        assert_eq!(medium.retained_tool_tokens, Some(32_000));
        assert_eq!(medium.compact_minimum_tokens, 16_000);

        let large = eko_visibility_horizon(396_000);
        assert_eq!(large.retained_tool_tokens, Some(40_000));
        assert_eq!(large.compact_minimum_tokens, 20_000);
    }

    #[test]
    fn workspace_tool_output_artifacts_stay_inside_workspace_state() -> anyhow::Result<()> {
        let workspace = tempfile::tempdir()?;
        let config = tool_output_artifact_config(Some(workspace.path()));

        assert_eq!(config.root_dir, workspace.path().join(".eko/artifacts"));
        assert!(config.root_dir.starts_with(workspace.path()));
        assert_eq!(config.threshold_bytes, 32 * 1024);
        assert_eq!(config.max_age_secs, Some(30 * 24 * 60 * 60));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_inherit_is_resolved_once_and_remains_fixed() {
        let initial = SubagentRuntimeGeneration {
            model: "parent-a".to_string(),
            llm_config: None,
            llm_client: None,
            max_tokens: None,
            token_limit: 16_384,
            thinking: None,
        };
        let authority = Arc::new(tokio::sync::RwLock::new(initial.clone()));
        let binding = subagent_model_binding(
            Some("inherit"),
            None,
            &EkoConfig::default(),
            &initial,
            &authority,
        );
        *authority.write().await = SubagentRuntimeGeneration {
            model: "parent-b".to_string(),
            token_limit: 65_536,
            ..initial
        };

        let fixed = binding.snapshot().await;
        assert_eq!(fixed.model, "parent-a");
        assert_eq!(fixed.token_limit, 16_384);
    }

    #[tokio::test]
    async fn fixed_binding_parses_role_thinking_once() {
        // Explicit model + explicit thinking → the Fixed generation carries the
        // parsed spec; parent hot-swaps no longer affect it.
        let parent = SubagentRuntimeGeneration {
            model: "parent-a".to_string(),
            llm_config: None,
            llm_client: None,
            max_tokens: None,
            token_limit: 16_384,
            thinking: Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::High,
            )),
        };
        let authority = Arc::new(tokio::sync::RwLock::new(parent.clone()));
        let config = EkoConfig::default();
        let binding =
            subagent_model_binding(Some("inherit"), Some("low"), &config, &parent, &authority);
        let fixed = binding.snapshot().await;
        assert_eq!(
            fixed.thinking,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );

        // Explicit model but NO thinking spec → the registration-time parent
        // thinking is copied (mirrors max_tokens).
        let no_spec = subagent_model_binding(Some("inherit"), None, &config, &parent, &authority);
        assert_eq!(no_spec.snapshot().await.thinking, parent.thinking);

        // No model spec → Inherit binding follows the parent generation's
        // thinking, so the published parent value flows into forks.
        let inherit = subagent_model_binding(None, None, &config, &parent, &authority);
        assert_eq!(inherit.snapshot().await.thinking, parent.thinking);
    }

    #[test]
    fn configured_subagent_selector_resolves_the_complete_profile() -> Result<(), String> {
        let mut config = EkoConfig::default();
        config.model_providers.insert(
            "fast-provider".to_string(),
            ModelProviderConfig {
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        config.configured_models.push(ConfiguredModel {
            id: "fast-profile".to_string(),
            display_name: "Fast profile".to_string(),
            provider: "fast-provider".to_string(),
            model: "fast-model".to_string(),
            enabled: true,
            context_window: Some(32_000),
            ..Default::default()
        });
        let parent = SubagentRuntimeGeneration {
            model: "parent-model".to_string(),
            llm_config: None,
            llm_client: None,
            max_tokens: None,
            token_limit: 16_384,
            thinking: None,
        };

        let fixed = resolve_fixed_subagent_generation(Some("fast-profile"), &config, &parent);
        assert_eq!(fixed.model, "fast-model");
        assert_eq!(fixed.token_limit, 32_000);
        let llm_config = fixed
            .llm_config
            .ok_or_else(|| "configured profile did not produce LlmConfig".to_string())?;
        assert_eq!(llm_config.model, "fast-model");
        assert_eq!(
            llm_config.base_url,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert!(fixed.llm_client.is_some());
        Ok(())
    }

    #[test]
    fn invalid_fixed_selector_falls_back_to_complete_parent_generation() {
        let parent = SubagentRuntimeGeneration {
            model: "parent-model".to_string(),
            llm_config: None,
            llm_client: None,
            max_tokens: Some(2_048),
            token_limit: 16_384,
            thinking: Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Medium,
            )),
        };
        let fixed = resolve_fixed_subagent_generation(
            Some("missing-profile"),
            &EkoConfig::default(),
            &parent,
        );
        assert_eq!(fixed.model, parent.model);
        assert_eq!(fixed.max_tokens, parent.max_tokens);
        assert_eq!(fixed.token_limit, parent.token_limit);
        assert_eq!(fixed.thinking, parent.thinking);
    }

    #[test]
    fn unavailable_os_sandbox_removes_run_code() -> echo_agent::error::Result<()> {
        let mut agent = ReactAgentBuilder::new()
            .model("test-model")
            .enable_tools()
            .build()?;
        assert!(agent.list_tools().iter().any(|name| name == "run_code"));

        configure_run_code_capability(&mut agent, false);
        assert!(!agent.list_tools().iter().any(|name| name == "run_code"));
        Ok(())
    }

    #[test]
    fn writer_subagent_inherits_sandbox_manager() -> echo_agent::error::Result<()> {
        let sandbox = Arc::new(SandboxManager::local_sandbox());
        let subagent = build_writer_subagent_agent(
            "writer",
            "write files",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler)
                as Arc<dyn SubagentPromptCompiler>,
            Arc::new(SubagentRegistry::new()),
            None,
            sandbox,
            test_command_cells()?,
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;

        assert!(subagent.sandbox_manager().is_some());
        assert_eq!(subagent.config().get_temperature(), None);
        assert!(!subagent.is_plan_mode());
        for tool in ["apply_patch", "shell"] {
            assert!(
                subagent.list_tools().iter().any(|name| name == tool),
                "writer subagent must expose {tool}"
            );
        }
        assert!(subagent.list_tools().iter().any(|name| name == "run_code"));
        assert!(
            !subagent
                .list_tools()
                .iter()
                .any(|name| name == "agent_tool")
        );
        Ok(())
    }

    #[tokio::test]
    async fn eko_writer_shell_defers_unknown_commands_to_permission_policy()
    -> echo_agent::error::Result<()> {
        let subagent = build_writer_subagent_agent(
            "writer",
            "exercise local shell policy",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(shell_policy_sandbox()),
            test_command_cells()?,
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;
        let result = subagent
            .tool_manager()
            .execute_tool(
                "shell",
                echo_agent::tools::ToolParameters::from([(
                    "command".to_string(),
                    serde_json::json!("sleep 0"),
                )]),
            )
            .await?;
        assert!(result.success, "{}", result.error.unwrap_or(result.output));
        Ok(())
    }

    #[test]
    fn delegation_capability_registers_agent_tool() -> echo_agent::error::Result<()> {
        let subagent = build_writer_subagent_agent(
            "delegator",
            "delegate bounded work",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            true,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;

        assert!(
            subagent
                .list_tools()
                .iter()
                .any(|name| name == "agent_tool")
        );
        Ok(())
    }

    #[test]
    fn readonly_subagent_applies_per_subagent_thinking() -> echo_agent::error::Result<()> {
        // The readonly builder must install an explicitly resolved thinking
        // profile on any ordinary role.
        let low = echo_agent::llm::ThinkingConfig::Level(echo_agent::llm::ThinkingLevel::Low);
        let subagent = build_readonly_subagent_agent(
            "low-thinking-reviewer",
            "review with reduced reasoning depth",
            "test-model",
            None,
            None,
            None,
            8_192,
            Some(low.clone()),
            30_000,
            1_024,
            "test-cache-user",
            64,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;
        assert_eq!(subagent.thinking(), Some(&low));

        // None → the agent keeps "use the model default" (no thinking field).
        let plain = build_readonly_subagent_agent(
            "explorer",
            "explore",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            0,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;
        assert!(plain.thinking().is_none());
        Ok(())
    }

    #[test]
    fn readonly_and_writer_subagents_share_cell_tools() -> echo_agent::error::Result<()> {
        // Both builder paths receive the scoped cell registry. A read-only
        // inspector has no shell launcher but may inspect existing cells.
        let readonly = build_readonly_subagent_agent(
            "cell-inspector",
            "inspect existing background cells",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            64,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;
        let readonly_names = readonly.tool_names();
        assert_eq!(readonly.config().get_temperature(), None);
        for expected in ["wait", "stop_cell", "list_cells"] {
            assert!(
                readonly_names.contains(&expected.to_string()),
                "readonly subagent missing cell tool {expected}: {readonly_names:?}"
            );
        }
        assert!(
            !readonly_names.contains(&"shell".to_string()),
            "readonly subagent must not gain shell: {readonly_names:?}"
        );
        Ok(())
    }

    #[test]
    fn loader_thinking_specs_parse_through_binding_path() -> Result<(), String> {
        let definition = crate::subagent_loader::parse_subagent_md(
            "---\nname: low-thinking-reviewer\ndescription: review\nreadonly: true\nthinking: low\n---\nReview the assigned evidence.",
            None,
        )?;
        let parsed = echo_agent::llm::ThinkingConfig::parse_spec(
            definition.thinking.as_deref().unwrap_or_default(),
        )
        .map_err(|error| format!("readonly role thinking spec must parse: {error}"))?;
        assert_eq!(
            parsed,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );
        Ok(())
    }

    #[test]
    fn readonly_subagent_uses_the_injected_workspace_memory_store() -> echo_agent::error::Result<()>
    {
        let memory_store: Arc<dyn echo_agent::memory::Store> =
            Arc::new(echo_agent::memory::InMemoryStore::new());
        let subagent = build_readonly_subagent_agent(
            "reader",
            "inspect files",
            "test-model",
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler)
                as Arc<dyn SubagentPromptCompiler>,
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            Some(memory_store.clone()),
            Arc::new(echo_agent::tools::files::LocalFileSystem),
        )?;

        let installed = subagent.store().ok_or_else(|| {
            echo_agent::error::ReactError::Other("memory store missing".to_string())
        })?;
        assert!(Arc::ptr_eq(installed, &memory_store));
        for tool in ["recall", "search_memory"] {
            assert!(subagent.tool_names().iter().any(|name| name == tool));
        }
        for tool in ["remember", "forget"] {
            assert!(!subagent.tool_names().iter().any(|name| name == tool));
        }
        Ok(())
    }
}

#[cfg(test)]
mod readonly_factory_consumer_tests {
    use super::*;
    use crate::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RunOutcome, TaskPlan,
        TaskRunStatus, TaskRuntimeStore,
    };
    use echo_agent::llm::{ChatRequest, ChatResponse};
    use echo_agent::testing::MockLlmClient;
    use futures::stream::BoxStream;

    struct ReadonlyFactoryRequestProbe {
        inner: MockLlmClient,
        schemas: std::sync::Mutex<Vec<Vec<echo_agent::llm::ToolDefinition>>>,
        project_checks: std::sync::Mutex<Vec<(bool, bool)>>,
    }

    impl ReadonlyFactoryRequestProbe {
        fn observe(&self, request: &ChatRequest) -> echo_agent::error::Result<()> {
            let system_text = request
                .messages
                .iter()
                .filter(|message| matches!(message.role, echo_agent::llm::types::Role::System))
                .filter_map(|message| message.content.as_text_ref())
                .collect::<Vec<_>>()
                .join("\n");
            self.project_checks
                .lock()
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                .push((
                    system_text.contains("READONLY_FACTORY_TMP_PROJECT"),
                    system_text.contains("# echo-agent-cli(Eclip)AGENTS.md"),
                ));
            self.schemas
                .lock()
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                .push(request.tools.clone().unwrap_or_default());
            Ok(())
        }
    }

    impl LlmClient for ReadonlyFactoryRequestProbe {
        fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse>> {
            Box::pin(async move {
                self.observe(&request)?;
                self.inner.chat(request).await
            })
        }

        fn chat_stream(
            &self,
            request: ChatRequest,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<echo_agent::llm::ChatChunk>>>> {
            Box::pin(async move {
                self.observe(&request)?;
                self.inner.chat_stream(request).await
            })
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }
    }

    fn isolated_readonly_factory_project() -> std::result::Result<tempfile::TempDir, String> {
        let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
        let old_root = crate::utils::find_project_root(fixture.path());
        let old_instructions =
            crate::instruction_provider::InstructionProvider::load_for_file_system(
                Some(fixture.path()),
                None,
                &echo_agent::tools::files::LocalFileSystem,
            )
            .map_err(|error| error.to_string())?;
        eprintln!(
            "未设独立项目边界时root={old_root:?}; inherited_repo_rules={}",
            old_instructions
                .repository_level
                .as_deref()
                .is_some_and(|text| text.contains("# echo-agent-cli(Eclip)AGENTS.md"))
        );
        // TMPDIR位于主仓内；用真实VCS边界代表独立项目，不改生产resolver或删仓库规则。
        let initialized = std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
            .arg(fixture.path())
            .output()
            .map_err(|error| error.to_string())?;
        if !initialized.status.success() {
            return Err(format!(
                "隔离测试项目初始化失败: {}",
                String::from_utf8_lossy(&initialized.stderr)
            ));
        }
        std::fs::write(
            fixture.path().join("AGENTS.md"),
            "READONLY_FACTORY_TMP_PROJECT\n仅检查这个隔离项目。\n",
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            crate::utils::find_project_root(fixture.path()),
            Some(fixture.path().to_path_buf())
        );
        let bound_instructions =
            crate::instruction_provider::InstructionProvider::load_for_file_system(
                Some(fixture.path()),
                None,
                &echo_agent::tools::files::LocalFileSystem,
            )
            .map_err(|error| error.to_string())?;
        let bound_rules = bound_instructions
            .repository_level
            .as_deref()
            .ok_or_else(|| "隔离项目规则未加载".to_string())?;
        assert!(bound_rules.contains("READONLY_FACTORY_TMP_PROJECT"));
        assert!(!bound_rules.contains("# echo-agent-cli(Eclip)AGENTS.md"));
        Ok(fixture)
    }

    #[tokio::test]
    async fn real_builtin_readonly_factory_keeps_delegate_schema_through_task_runtime_fork()
    -> std::result::Result<(), String> {
        use crate::subagent_policy::{SubagentDepthDefaults, SubagentDepthLimit};

        let fixture = isolated_readonly_factory_project()?;
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        let reviewer = definitions
            .iter()
            .find(|definition| definition.name == "reviewer")
            .ok_or_else(|| "真实 builtin reviewer 未加载".to_string())?;
        assert!(reviewer.readonly);
        assert!(reviewer.can_delegate);
        assert_eq!(reviewer.source, "builtin:reviewer");

        let transport = Arc::new(ReadonlyFactoryRequestProbe {
            inner: MockLlmClient::new()
                .then_tool_call(
                    "readonly-factory-dispatch-discovery",
                    "tool_search",
                    r#"{"query":"agent_tool"}"#,
                )
                .with_response("## Summary\n真实只读工厂工具曝光检查完成。"),
            schemas: std::sync::Mutex::new(Vec::new()),
            project_checks: std::sync::Mutex::new(Vec::new()),
        });
        let registry = Arc::new(echo_agent::subagent::SubagentRegistry::new());
        let mut events = registry.event_bus().subscribe();
        let mut primary = ReactAgentBuilder::new()
            .name("readonly-factory-primary")
            .model("mock-model")
            .enable_tools()
            .enable_subagent()
            .subagent_registry(registry.clone())
            .register_agent_dispatch_tool()
            .working_dir(fixture.path())
            .auto_project_rules(false)
            .build()
            .map_err(|error| error.to_string())?;
        let cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry> =
            Arc::new(echo_agent::tasks::BackgroundCommandManager::default());
        // 只替换模型传输；真实 loader、默认注册、readonly 工厂和动态 Fork adapter 均保留。
        let consumers = register_default_subagents(
            &mut primary,
            &EkoConfig::default(),
            SubagentRuntimeGeneration {
                model: "mock-model".to_string(),
                llm_config: None,
                llm_client: Some(transport.clone()),
                // Mock模型窗口仍为8k；显式输出配置1024避免默认8192占满整个窗口。
                max_tokens: Some(1_024),
                token_limit: 8_192,
                thinking: None,
            },
            30_000,
            DEFAULT_MAX_TOOL_OUTPUT_TOKENS,
            "readonly-factory-consumer",
            &definitions,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            registry,
            None,
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            cells,
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
            Some(fixture.path().join("product-data")),
            Some(fixture.path().to_path_buf()),
        )
        .await;

        let defaults = Arc::new(SubagentDepthDefaults::new(SubagentDepthLimit::new(3)?));
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?
                .with_subagent_depth_defaults(defaults),
        );
        let _resource_scope = store.test_resource_scope();
        let run_id = "readonly-real-factory-run";
        let goal = "核对真实只读工厂的受限委派曝光";
        store
            .create_run(
                run_id,
                "ws_test",
                "conv_test",
                "msg_test",
                DomainProfile::General,
                goal,
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: "readonly-real-factory-plan".to_string(),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            domain_binding: None,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256(goal),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "native-depth-probe".to_string(),
                title: "真实 readonly 工厂曝光".to_string(),
                description: "调用 tool_search 查找 agent_tool，再正常返回检查结果。".to_string(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "reviewer".to_string(),
                allowed_tools: vec![
                    "read_file".to_string(),
                    "tool_search".to_string(),
                    "agent_tool".to_string(),
                ],
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let primary = AgentHandle::new(primary);
        let outcome = crate::tasks::task_runtime::execute_run(
            store.clone(),
            Some(primary.clone()),
            None,
            None,
            None,
            None,
            run_id,
            echo_agent::agent::CancellationToken::new(),
            crate::tasks::task_runtime::memory_bridge::MemoryPolicy::None,
            None,
        )
        .await;
        // 原执行真正结算后再做断言，失败不能把工厂或活动 owner 留在后台。
        drop(primary);
        drop(consumers);
        let outcome = outcome.map_err(|error| error.to_string())?;
        assert!(
            matches!(&outcome, RunOutcome::Completed),
            "真实 TaskRuntime 结果={outcome:?}; 模型请求数={}; 已结算 Subagent={:?}",
            transport.inner.call_count(),
            store
                .list_subagent_runs(run_id)
                .map_err(|error| error.to_string())?
        );
        let runs = store
            .list_subagent_runs(run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs.first()
                .ok_or_else(|| "真实 Fork 执行记录缺失".to_string())?
                .subagent_name,
            "reviewer"
        );
        let mut dispatches = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let echo_agent::subagent::SubagentEvent::DispatchStarted { agent, mode, .. } =
                event.as_ref()
            {
                assert_eq!(*mode, echo_agent::subagent::ExecutionMode::Fork);
                dispatches.push(agent.clone());
            }
        }
        assert_eq!(dispatches, vec!["reviewer".to_string()]);
        let schemas = transport
            .schemas
            .lock()
            .map_err(|error| error.to_string())?;
        assert_eq!(schemas.len(), 2, "必须经过真实工具搜索后的第二次模型请求");
        let project_checks = transport
            .project_checks
            .lock()
            .map_err(|error| error.to_string())?;
        assert_eq!(project_checks.len(), schemas.len());
        assert!(
            project_checks
                .iter()
                .all(|(temporary, repository)| *temporary && !*repository),
            "真实模型必须只接收已绑定的隔离项目规则: {project_checks:?}"
        );
        eprintln!(
            "actual_model_calls={}; project_checks={project_checks:?}",
            transport.inner.call_count()
        );
        let dispatch = schemas
            .last()
            .and_then(|tools| tools.iter().find(|tool| tool.function.name == "agent_tool"));
        assert!(
            dispatch.is_some(),
            "真实 builtin reviewer 经 readonly factory→TaskRuntime→Fork 后，tool_search 未把合法 agent_tool 送进模型 schema"
        );
        let dispatch = dispatch.ok_or_else(|| "只读委派 schema 缺失".to_string())?;
        let target_names = dispatch
            .function
            .parameters
            .pointer("/properties/agent_name/enum")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "委派schema未提供实际目标枚举".to_string())?;
        assert!(
            target_names
                .iter()
                .any(|name| name.as_str() == Some("reviewer"))
        );
        assert!(
            !target_names
                .iter()
                .any(|name| name.as_str() == Some("implementer"))
        );
        Ok(())
    }
    #[derive(Debug, Clone)]
    struct ReadonlyScopeObservation {
        policy: Option<echo_agent::tasks::NestedDelegationPolicy>,
        access: Option<echo_agent::tools::ToolAccess>,
        available: Vec<String>,
        run_id: Option<String>,
        execution_id: Option<String>,
        working_dir: Option<std::path::PathBuf>,
        lineage: Option<echo_agent::tools::SubagentLineage>,
    }

    struct ReadonlyScopeProbe {
        name: &'static str,
        mutating: bool,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        observations: Option<Arc<std::sync::Mutex<Vec<ReadonlyScopeObservation>>>>,
    }

    impl echo_agent::tools::Tool for ReadonlyScopeProbe {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "Observe an exact test invocation; no file or process access."
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object", "properties":{}, "additionalProperties":false})
        }
        fn risk_level(&self) -> echo_agent::tools::ToolRiskLevel {
            if self.mutating {
                echo_agent::tools::ToolRiskLevel::Standard
            } else {
                echo_agent::tools::ToolRiskLevel::ReadOnly
            }
        }
        fn permissions(&self) -> Vec<echo_agent::prelude::ToolPermission> {
            vec![if self.mutating {
                echo_agent::prelude::ToolPermission::Write
            } else {
                echo_agent::prelude::ToolPermission::Read
            }]
        }
        fn execute_with_context<'a>(
            &'a self,
            _parameters: echo_agent::tools::ToolParameters,
            context: &'a echo_agent::tools::ToolContext,
        ) -> BoxFuture<'a, Result<echo_agent::tools::ToolResult>> {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(observations) = self.observations.as_ref() {
                    observations
                        .lock()
                        .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                        .push(ReadonlyScopeObservation {
                            policy: context.delegation_policy,
                            access: context.tool_access,
                            available: context
                                .tool_visibility
                                .as_ref()
                                .map(|visibility| visibility.available_names())
                                .unwrap_or_default(),
                            run_id: context.run_id.clone(),
                            execution_id: context.execution_id.clone(),
                            working_dir: context.working_dir.clone(),
                            lineage: context.subagent_lineage.clone(),
                        });
                }
                Ok(echo_agent::tools::ToolResult::success("scope observed"))
            })
        }
    }

    fn install_readonly_scope_probes(
        agent: &dyn echo_agent::agent::Agent,
        reads: &Arc<std::sync::atomic::AtomicUsize>,
        writes: &Arc<std::sync::atomic::AtomicUsize>,
        outside: &Arc<std::sync::atomic::AtomicUsize>,
        observations: &Arc<std::sync::Mutex<Vec<ReadonlyScopeObservation>>>,
    ) {
        for (name, mutating, calls, observed) in [
            (
                "i2_scope_probe",
                false,
                reads.clone(),
                Some(observations.clone()),
            ),
            ("i2_write_probe", true, writes.clone(), None),
            ("i2_outside_probe", false, outside.clone(), None),
        ] {
            agent.register_tool(Box::new(ReadonlyScopeProbe {
                name,
                mutating,
                calls,
                observations: observed,
            }));
        }
    }

    #[tokio::test]
    async fn real_builtin_readonly_factory_preserves_run_depth_and_tool_upper_bound()
    -> std::result::Result<(), String> {
        use crate::subagent_policy::{SubagentDepthDefaults, SubagentDepthLimit};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fixture = isolated_readonly_factory_project()?;
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        let reviewer = definitions
            .iter()
            .find(|definition| definition.name == "reviewer")
            .ok_or_else(|| "builtin reviewer missing".to_string())?;
        assert!(reviewer.readonly && reviewer.can_delegate);
        assert_eq!(reviewer.source, "builtin:reviewer");
        let mut script = MockLlmClient::new();
        for maximum in [1_u8, 2, 3] {
            for depth in 1_u8..=maximum {
                let prefix = format!("limit{maximum}-depth{depth}");
                script = script
                    .then_tool_call(format!("{prefix}-find-probe"), "tool_search", r#"{"query":"i2_scope_probe"}"#)
                    .then_tool_call(format!("{prefix}-probe"), "i2_scope_probe", "{}")
                    .then_tool_call(format!("{prefix}-write"), "i2_write_probe", "{}")
                    .then_tool_call(format!("{prefix}-outside"), "i2_outside_probe", "{}")
                    .then_tool_call(format!("{prefix}-find-delegate"), "tool_search", r#"{"query":"agent_tool"}"#)
                    .then_tool_call(format!("{prefix}-delegate"), "agent_tool",
                        serde_json::json!({"agent_name":"reviewer", "task":"继续真实只读下一层", "mode":"fork"}).to_string());
            }
            for depth in (1_u8..=maximum).rev() {
                script = script.with_response(format!("## Summary\n只读层数{depth}已结算。"));
            }
        }
        let transport = Arc::new(ReadonlyFactoryRequestProbe {
            inner: script,
            schemas: std::sync::Mutex::new(Vec::new()),
            project_checks: std::sync::Mutex::new(Vec::new()),
        });
        let registry = Arc::new(echo_agent::subagent::SubagentRegistry::new());
        let mut events = registry.event_bus().subscribe_envelopes();
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let outside = Arc::new(AtomicUsize::new(0));
        let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let mut primary = ReactAgentBuilder::new()
            .name("readonly-depth-primary")
            .model("mock-model")
            .enable_tools()
            .enable_subagent()
            .subagent_registry(registry.clone())
            .register_agent_dispatch_tool()
            .working_dir(fixture.path())
            .auto_project_rules(false)
            .build()
            .map_err(|error| error.to_string())?;
        install_readonly_scope_probes(&primary, &reads, &writes, &outside, &observations);
        let consumers = register_default_subagents(
            &mut primary,
            &EkoConfig::default(),
            SubagentRuntimeGeneration {
                model: "mock-model".to_string(),
                llm_config: None,
                llm_client: Some(transport.clone()),
                max_tokens: None,
                token_limit: effective_token_limit(&EkoConfig::default(), None),
                thinking: None,
            },
            30_000,
            DEFAULT_MAX_TOOL_OUTPUT_TOKENS,
            "readonly-depth-consumer",
            &definitions,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            registry.clone(),
            None,
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            Arc::new(echo_agent::tasks::BackgroundCommandManager::default()),
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
            None,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
            Some(fixture.path().join("product-data")),
            Some(fixture.path().to_path_buf()),
        )
        .await;
        // 包装原工厂仅计数并安装计数工具；不手造 child、不移除 readonly 或改变角色声明。
        for inherited in consumers.inherited_factories.iter() {
            let original = inherited.factory.clone();
            let factory_calls = factory_calls.clone();
            let reads = reads.clone();
            let writes = writes.clone();
            let outside = outside.clone();
            let observations = observations.clone();
            let factory = Arc::new(FnAgentFactory::new(move || {
                let original = original.clone();
                let factory_calls = factory_calls.clone();
                let reads = reads.clone();
                let writes = writes.clone();
                let outside = outside.clone();
                let observations = observations.clone();
                Box::pin(async move {
                    factory_calls.fetch_add(1, Ordering::SeqCst);
                    let agent = original.create().await?;
                    install_readonly_scope_probes(
                        agent.as_ref(),
                        &reads,
                        &writes,
                        &outside,
                        &observations,
                    );
                    Ok(agent)
                }) as BoxFuture<'static, Result<Box<dyn echo_agent::agent::Agent>>>
            }));
            registry
                .register_factory(inherited.definition.clone(), factory)
                .await;
        }
        let primary = AgentHandle::new(primary);
        let defaults = Arc::new(SubagentDepthDefaults::new(SubagentDepthLimit::new(1)?));
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?
                .with_subagent_depth_defaults(defaults.clone()),
        );
        let _resource_scope = store.test_resource_scope();
        for maximum in [1_u8, 2, 3] {
            defaults.publish(SubagentDepthLimit::new(maximum)?);
            let run_id = format!("readonly-real-depth-run-{maximum}");
            let goal = "沿真实内置工厂执行受限只读层数";
            store
                .create_run(
                    &run_id,
                    "ws_test",
                    "same-readonly-conversation",
                    "msg_test",
                    DomainProfile::General,
                    goal,
                    "",
                    AttendedMode::Attended,
                )
                .map_err(|error| error.to_string())?;
            let plan = TaskPlan {
                plan_id: format!("readonly-depth-plan-{maximum}"),
                run_id: run_id.clone(),
                revision: 1,
                domain_profile: DomainProfile::General,
                domain_binding: None,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(goal),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "native-depth-probe".to_string(),
                    title: "真实只读深度".to_string(),
                    description: "沿模型实际工具调用递归只读审查，到边界后正常结算。".to_string(),
                    kind: PlanTaskKind::ReadOnlyReview,
                    agent_role: "reviewer".to_string(),
                    allowed_tools: vec![
                        "read_file".to_string(),
                        "tool_search".to_string(),
                        "agent_tool".to_string(),
                        "i2_scope_probe".to_string(),
                        "i2_write_probe".to_string(),
                    ],
                    ..Default::default()
                }],
            };
            store
                .attach_plan_for_test(&plan)
                .map_err(|error| error.to_string())?;
            let snapshot = store
                .get_run_state(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "bound run missing".to_string())?;
            assert_eq!(
                snapshot
                    .execution_profile
                    .require_subagent_depth_limit()?
                    .get(),
                maximum
            );
            defaults.publish(SubagentDepthLimit::new(if maximum == 1 { 3 } else { 1 })?);
            store
                .transition_run(&run_id, TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
            let before_reads = reads.load(Ordering::SeqCst);
            let before_factory = factory_calls.load(Ordering::SeqCst);
            let before_models = transport.inner.call_count();
            let before_observed = observations
                .lock()
                .map_err(|error| error.to_string())?
                .len();
            let outcome = crate::tasks::task_runtime::execute_run(
                store.clone(),
                Some(primary.clone()),
                None,
                None,
                None,
                None,
                &run_id,
                echo_agent::agent::CancellationToken::new(),
                crate::tasks::task_runtime::memory_bridge::MemoryPolicy::None,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(
                matches!(&outcome, RunOutcome::Completed),
                "max={maximum}, outcome={outcome:?}"
            );
            assert_eq!(
                reads.load(Ordering::SeqCst).saturating_sub(before_reads),
                usize::from(maximum)
            );
            assert_eq!(
                factory_calls
                    .load(Ordering::SeqCst)
                    .saturating_sub(before_factory),
                usize::from(maximum)
            );
            assert_eq!(
                transport.inner.call_count().saturating_sub(before_models),
                usize::from(maximum).saturating_mul(7)
            );
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(outside.load(Ordering::SeqCst), 0);
            let actual = observations
                .lock()
                .map_err(|error| error.to_string())?
                .get(before_observed..)
                .ok_or_else(|| "observation slice missing".to_string())?
                .to_vec();
            let mut started = Vec::new();
            while let Ok(event) = events.try_recv() {
                if let echo_agent::subagent::SubagentEvent::DispatchStarted {
                    mode,
                    execution_id,
                    ..
                } = &event.payload.event
                {
                    assert_eq!(*mode, echo_agent::subagent::ExecutionMode::Fork);
                    assert_eq!(
                        event.run_id.as_ref().map(|id| id.as_str()),
                        Some(run_id.as_str())
                    );
                    let execution = execution_id
                        .clone()
                        .ok_or_else(|| "dispatch execution missing".to_string())?;
                    started.push((execution, event.payload.invocation.clone()));
                }
            }
            assert_eq!(started.len(), usize::from(maximum));
            for (index, observed) in actual.iter().enumerate() {
                let depth =
                    u8::try_from(index.saturating_add(1)).map_err(|error| error.to_string())?;
                assert_eq!(
                    observed.policy,
                    Some(echo_agent::tasks::NestedDelegationPolicy {
                        can_spawn_subagents: true,
                        delegate_depth: depth,
                        max_delegate_depth: maximum,
                    })
                );
                assert_eq!(
                    observed.access,
                    Some(echo_agent::tools::ToolAccess::ReadOnly)
                );
                assert_eq!(observed.run_id.as_deref(), Some(run_id.as_str()));
                assert_eq!(
                    observed.working_dir.as_deref(),
                    Some(fixture.path()),
                    "真实工具目标必须与提示词/TaskRuntime绑定的隔离项目一致"
                );
                assert!(
                    !observed
                        .available
                        .iter()
                        .any(|name| name == "i2_write_probe" || name == "i2_outside_probe")
                );
                let (execution, invocation) = started
                    .get(index)
                    .ok_or_else(|| "corresponding dispatch missing".to_string())?;
                assert_eq!(observed.execution_id.as_deref(), Some(execution.as_str()));
                let lineage = observed
                    .lineage
                    .as_ref()
                    .ok_or_else(|| "actual ToolContext lineage missing".to_string())?;
                assert_eq!(lineage.agent_name.as_deref(), Some("reviewer"));
                assert_eq!(lineage.run_id.as_deref(), Some(run_id.as_str()));
                assert_eq!(invocation.agent_name, "reviewer");
                assert_eq!(
                    invocation
                        .agent_path
                        .as_deref()
                        .map(|path| path.split('/').filter(|part| *part == "reviewer").count()),
                    Some(usize::from(depth))
                );
                if index > 0 {
                    let (parent_execution, _) = started
                        .get(index.saturating_sub(1))
                        .ok_or_else(|| "parent dispatch missing".to_string())?;
                    assert_eq!(
                        invocation.parent_execution_id.as_deref(),
                        Some(parent_execution.as_str())
                    );
                }
            }
            let schemas = transport
                .schemas
                .lock()
                .map_err(|error| error.to_string())?;
            let case_schemas = schemas
                .get(before_models..)
                .ok_or_else(|| "actual request range missing".to_string())?;
            assert_eq!(
                case_schemas
                    .iter()
                    .any(|tools| tools.iter().any(|tool| tool.function.name == "agent_tool")),
                maximum > 1
            );
            assert!(case_schemas.iter().all(|tools| {
                tools.iter().all(|tool| {
                    tool.function.name != "i2_write_probe"
                        && tool.function.name != "i2_outside_probe"
                })
            }));
        }
        drop(primary);
        drop(consumers);
        Ok(())
    }
}

#[cfg(test)]
mod provider_identity_tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode, Uri, header::CONTENT_TYPE},
        response::{IntoResponse, Response},
        routing::post,
    };
    use echo_agent::llm::ChatRequest;
    use echo_agent::llm::types::Message;
    use serde_json::{Value, json};

    type Captured = Arc<tokio::sync::Mutex<Vec<(HeaderMap, Value)>>>;

    fn completed_response() -> Value {
        json!({
            "id": "resp_identity", "object": "response", "created_at": 1,
            "model": "identity-model", "status": "completed",
            "output": [{"type": "message", "id": "msg_identity", "role": "assistant",
                "content": [{"type": "output_text", "text": "OK", "annotations": []}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
    }

    async fn capture(
        State(captured): State<Captured>,
        uri: Uri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Response, StatusCode> {
        captured.lock().await.push((headers.clone(), body.clone()));
        // 模拟本次网关的关键边界，遗漏真实应用身份时必须失败。
        if headers.get("user-agent").and_then(|v| v.to_str().ok())
            != Some(concat!("Eclip/", env!("CARGO_PKG_VERSION")))
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
        let response = match (uri.path(), streaming) {
            ("/v1/chat/completions", false) => Json(json!({
                "id": "chat_identity", "object": "chat.completion", "created": 1,
                "model": "identity-model",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })).into_response(),
            ("/v1/responses", false) => Json(completed_response()).into_response(),
            ("/v1/messages", false) => Json(json!({
                "id": "msg_identity", "type": "message", "role": "assistant", "model": "identity-model",
                "content": [{"type": "text", "text": "OK"}], "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })).into_response(),
            ("/v1/chat/completions", true) => {
                let chunk = json!({"id": "chat_identity", "object": "chat.completion.chunk", "created": 1,
                    "model": "identity-model", "choices": [{"index": 0,
                        "delta": {"role": "assistant", "content": "OK"}, "finish_reason": "stop"}]});
                ([(CONTENT_TYPE, "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response()
            }
            ("/v1/responses", true) => {
                let delta = json!({"type": "response.output_text.delta", "item_id": "msg_identity",
                    "output_index": 0, "content_index": 0, "delta": "OK"});
                let completed = json!({"type": "response.completed", "response": completed_response()});
                ([(CONTENT_TYPE, "text/event-stream")], format!(
                    "event: response.output_text.delta\ndata: {delta}\n\nevent: response.completed\ndata: {completed}\n\n"
                )).into_response()
            }
            ("/v1/messages", true) => {
                let events = [
                    json!({"type": "message_start", "message": {"id": "msg_identity", "type": "message",
                        "role": "assistant", "model": "identity-model", "content": [],
                        "usage": {"input_tokens": 1, "output_tokens": 0}}}),
                    json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                    json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "OK"}}),
                    json!({"type": "content_block_stop", "index": 0}),
                    json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
                    json!({"type": "message_stop"}),
                ];
                let wire = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
                ([(CONTENT_TYPE, "text/event-stream")], wire).into_response()
            }
            _ => return Err(StatusCode::NOT_FOUND),
        };
        Ok(response)
    }

    #[tokio::test]
    async fn eclip_identity_reaches_all_runtime_protocols_and_streams() -> anyhow::Result<()> {
        use futures::StreamExt;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let root = format!("http://{}/v1", listener.local_addr()?);
        let captured: Captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/v1/chat/completions", post(capture))
            .route("/v1/responses", post(capture))
            .route("/v1/messages", post(capture))
            .with_state(captured.clone());
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            for protocol in [LlmApiProtocol::ChatCompletions, LlmApiProtocol::Responses, LlmApiProtocol::Anthropic] {
                let mut config = EkoConfig::default();
                config.model_providers.insert("identity-gateway".into(), crate::config::ModelProviderConfig {
                    base_url: Some(root.clone()), auth_token: Some("fixture-key".into()), ..Default::default()
                });
                config.configured_models.push(serde_json::from_value(json!({
                    "id": "identity:model", "provider": "identity-gateway", "model": "identity-model",
                    "api_protocol": protocol
                }))?);
                let runtime = model_config::resolve_runtime_model(&config, None).map_err(anyhow::Error::msg)?;
                let probe = test_runtime_llm_connection(&runtime).await.map_err(anyhow::Error::msg)?;
                assert_eq!(probe.response, "OK", "connection probe {protocol:?}");
                let prepared = prepare_runtime_llm(&runtime).map_err(anyhow::Error::msg)?;
                let mut stream = prepared.client.chat_stream(ChatRequest {
                    messages: vec![Message::user("Reply OK".to_string())], max_tokens: Some(64), ..Default::default()
                }).await?;
                let mut text = String::new();
                let mut finished = false;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if let Some(content) = chunk.delta.content { text.push_str(&content); }
                    finished |= chunk.finish_reason.is_some();
                }
                assert_eq!(text, "OK", "stream {protocol:?}");
                assert!(finished, "stream ended without finish reason: {protocol:?}");
            }
            anyhow::Ok(())
        }).await;
        let _ = shutdown.send(());
        tokio::time::timeout(std::time::Duration::from_secs(5), server).await???;
        outcome??;
        let requests = captured.lock().await;
        assert_eq!(requests.len(), 6);
        for (headers, _) in requests.iter() {
            assert_eq!(
                headers.get("authorization").and_then(|v| v.to_str().ok()),
                (!headers.contains_key("x-api-key")).then_some("Bearer fixture-key")
            );
        }
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
pub(crate) mod native_manager_runtime_contract {
    use super::*;
    use echo_agent::agent::{Agent, AgentEvent, CancellationToken};
    use echo_agent::testing::MockLlmClient;
    use echo_agent::tools::{ToolContext, ToolParameters, ToolResult};
    use futures::StreamExt;
    use std::path::PathBuf;

    fn failure_metadata(failure: &echo_agent::error::AgentFailure) -> String {
        // 只输出稳定分类和已知固定错误的布尔诊断，禁止输出原 message/prompt/正文。
        let missing_llm_client = failure
            .message
            .contains("has no LlmClient; inject an explicit LlmConfig or LlmClient");
        format!(
            "category={:?}, terminal={:?}, code={}, http_status={:?}, missing_llm_client={missing_llm_client}",
            failure.category, failure.terminal_kind, failure.code, failure.http_status
        )
    }

    fn run_error_metadata(error: &echo_agent::error::ReactError) -> String {
        match echo_agent::error::AgentFailure::try_from(error) {
            Ok(failure) => failure_metadata(&failure),
            Err(_) => format!(
                "unclassified_error_kind={:?}",
                std::mem::discriminant(error)
            ),
        }
    }

    async fn collect_actual_shell(
        target: Arc<dyn Agent>,
        cancel: CancellationToken,
    ) -> Result<ToolResult, String> {
        let mut events = target
            .execute_stream_with_cancel("执行本次受控 shell 并读取真实工具结果。", cancel)
            .await
            .map_err(|error| format!("stream creation failed: {}", run_error_metadata(&error)))?;
        let mut shell = None;
        let mut source_errors = Vec::new();
        let mut statuses = Vec::new();
        let mut tool_results = Vec::new();
        while let Some(event) = events.next().await {
            match event {
                Err(error) => source_errors.push(run_error_metadata(&error)),
                Ok(AgentEvent::Error {
                    source, failure, ..
                }) => {
                    source_errors.push(format!("source={source}, {}", failure_metadata(&failure)));
                }
                Ok(AgentEvent::ExecutionStatus(status)) => {
                    statuses.push(format!("{:?}/{:?}", status.phase, status.status));
                    if status.status == echo_agent::agent::ExecutionStatusKind::Failed {
                        source_errors.push("original execution status Failed".to_string());
                    }
                }
                Ok(AgentEvent::ToolResult {
                    name,
                    call_id,
                    result,
                }) => {
                    tool_results.push(format!(
                        "name={name}, call_id={call_id}, success={}",
                        result.success
                    ));
                    if name == "shell" && call_id == "native-manager-shell" {
                        shell = Some(result);
                    }
                }
                _ => {}
            }
        }
        eprintln!(
            "actual final Agent event metadata: statuses={statuses:?}; errors={source_errors:?}; tools={tool_results:?}"
        );
        if !source_errors.is_empty() {
            return Err(format!(
                "actual final Agent source failed: {source_errors:?}"
            ));
        }
        shell.ok_or_else(|| format!("actual final Agent produced no typed shell result; statuses={statuses:?}; tools={tool_results:?}"))
    }

    pub(crate) struct Fixture {
        pub(crate) root: tempfile::TempDir,
        pub(crate) created: CreatedAgent,
        pub(crate) sandbox: Arc<dyn echo_agent::sandbox::SandboxExecutor>,
        pub(crate) cells: Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
        pub(crate) io: crate::product_data_io::ProductDataIoService,
    }

    impl Fixture {
        pub(crate) async fn new() -> Result<Self, String> {
            let root = tempfile::tempdir().map_err(|error| error.to_string())?;
            let sandbox: Arc<dyn echo_agent::sandbox::SandboxExecutor> =
                Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox());
            if !sandbox
                .is_available_at(echo_agent::sandbox::IsolationLevel::OsSandbox)
                .await
            {
                return Err(
                    "final manager fixture requires the actual OS-isolated Local backend".into(),
                );
            }
            let io = crate::product_data_io::ProductDataIoService::new();
            let cells = crate::tasks::task_runtime::command_cells::CommandCellRuntimeService::new(
                sandbox.clone(),
                Arc::new(
                    crate::chat_event_log::ChatEventLog::open(
                        root.path().join("chat-events"),
                        Default::default(),
                    )
                    .map_err(|error| error.to_string())?,
                ),
                io.clone(),
            )?;
            let created = create_agent_with_diagnostics(
                &AgentCreateParams {
                    working_dir: Some(root.path().to_path_buf()),
                    workspace_data_root: Some(root.path().join("product-data")),
                    command_cell_runtime: Some(cells.clone()),
                    product_data_io: Some(io.clone()),
                    sandbox_executor: Some(sandbox.clone()),
                    ..Default::default()
                },
                &EkoConfig::default(),
            )
            .await;
            let created = match created {
                Ok(created) => created,
                Err(error) => {
                    let closed = cells.shutdown().await;
                    drop(cells);
                    let joined = io.join_shutdown().await;
                    return Err(format!(
                        "final manager factory failed: {error}; cleanup cells={closed:?}, io={joined:?}"
                    ));
                }
            };
            Ok(Self {
                root,
                created,
                sandbox,
                cells,
                io,
            })
        }

        pub(crate) async fn finish(self) -> Result<(), String> {
            let Self {
                root,
                created,
                sandbox,
                cells,
                io,
            } = self;
            let closed = cells.shutdown().await;
            drop(created);
            drop(cells);
            drop(sandbox);
            let joined = io.join_shutdown().await;
            eprintln!("final manager fixture owner cleanup: cells={closed:?}; io={joined:?}");
            drop(root);
            closed?;
            joined
        }
    }

    pub(crate) struct Case {
        directory: PathBuf,
        cwd: PathBuf,
        script: PathBuf,
        pub(crate) client: Arc<MockLlmClient>,
    }

    fn quote(path: &std::path::Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    impl Case {
        pub(crate) fn new(fixture: &Fixture, name: &str) -> Result<Self, String> {
            let directory = fixture.root.path().join(name);
            std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
            let script = directory.join("payload.bash");
            // 同一实际负载临界区的 mkdir 是原子判据；冲突立即失败并保留标记，不另排队。
            std::fs::write(
                &script,
                r#"#!/bin/bash
occupied="$1"
overlap="$2"
role="$3"
release="$4"
started="$5"
if ! mkdir "$occupied"; then printf overlap > "$overlap"; exit 91; fi
trap 'rmdir "$occupied"' EXIT
printf started > "$started"
while [ ! -e "$release" ]; do sleep 0.01; done
printf 'native-manager-%s-effect' "$role"
"#,
            )
            .map_err(|error| error.to_string())?;
            std::fs::write(directory.join("release-target"), "release")
                .map_err(|error| error.to_string())?;
            let command = Self::command(&directory, &script, "target");
            let client = Arc::new(MockLlmClient::new()
                .then_tool_call("native-manager-search", "tool_search", serde_json::json!({"query":"shell", "limit":1}).to_string())
                .then_tool_call("native-manager-shell", "shell", serde_json::json!({"command":command, "timeout":5}).to_string())
                .with_response("受控执行已观察。")
                // Pool 若保有产品 Critic，仍走真实 verifier，只替换它原本共享的模型传输。
                .with_response(serde_json::json!({"score":10.0,"passed":true,"feedback":"受控传输已返回；物理执行另以 typed tool result 断言。","suggestions":[]}).to_string()));
            Ok(Self {
                directory,
                cwd: fixture.root.path().to_path_buf(),
                script,
                client,
            })
        }

        fn command(directory: &std::path::Path, script: &std::path::Path, role: &str) -> String {
            format!(
                "bash {} {} {} {} {} {}",
                quote(script),
                quote(&directory.join("occupied")),
                quote(&directory.join("overlap")),
                role,
                quote(&directory.join(format!("release-{role}"))),
                quote(&directory.join(format!("started-{role}")))
            )
        }

        pub(crate) async fn run(
            self,
            fixture: &Fixture,
            target: Arc<dyn Agent>,
        ) -> Result<(), String> {
            let root_manager = fixture.created.agent.tool_manager().clone();
            let cancel = CancellationToken::new();
            let context = ToolContext {
                working_dir: Some(self.cwd.clone()),
                cancel: Some(Arc::new(cancel.clone())),
                ..Default::default()
            };
            let parameters: ToolParameters = serde_json::from_value(serde_json::json!({
                "command": Self::command(&self.directory, &self.script, "root"), "timeout":5,
            }))
            .map_err(|error| error.to_string())?;
            let observation_started = tokio::time::Instant::now();
            let completion_deadline = observation_started
                .checked_add(std::time::Duration::from_secs(5))
                .ok_or("final manager completion observation deadline overflow")?;
            let observation_deadline = observation_started
                .checked_add(std::time::Duration::from_secs(3))
                .ok_or("final manager observation deadline overflow")?;
            let readiness_deadline = observation_deadline
                .checked_sub(std::time::Duration::from_millis(40))
                .ok_or("final manager observation reserve overflow")?;
            let mut root_owner = tokio::spawn(async move {
                root_manager
                    .execute_tool_with_context_draining_started("shell", parameters, &context)
                    .await
                    .map_err(|error| error.to_string())
            });
            let first_started = tokio::time::timeout_at(readiness_deadline, async {
                while !self.directory.join("started-root").exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok();
            let target_cancel = cancel.clone();
            let declared_names = target.tool_names();
            let definitions = target.tool_definitions();
            let definition_names: Vec<_> = definitions
                .iter()
                .map(|definition| definition.function.name.clone())
                .collect();
            let schema_bytes = serde_json::to_vec(&definitions)
                .map(|bytes| bytes.len())
                .unwrap_or(0);
            eprintln!(
                "actual final Agent startup metadata: model_name_empty={}; tools={declared_names:?}; definition_names={definition_names:?}; schema_bytes={schema_bytes}",
                target.model_name().is_empty()
            );
            let mut executing = tokio::spawn(collect_actual_shell(target, target_cancel));
            let model_reached_shell = tokio::time::timeout_at(readiness_deadline, async {
                while self.client.call_count() < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok();
            let early_deadline = tokio::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(40))
                .map(|deadline| deadline.min(observation_deadline))
                .unwrap_or(observation_deadline);
            let early = tokio::time::timeout_at(early_deadline, &mut executing)
                .await
                .ok();
            let returned_before_release = early.is_some();
            let root_still_owned = !root_owner.is_finished();
            let held = crate::tasks::task_runtime::executor::process_execution_governor()
                .shell_semaphore()
                .available_permits();
            // 先放原真实 root 的门闩，再收集同一原工具/模型任务；失败不能遗留自己的进程。
            let released = std::fs::write(self.directory.join("release-root"), "release")
                .map_err(|error| error.to_string());
            if !first_started || !model_reached_shell || released.is_err() {
                cancel.cancel();
            }
            let mut observer_timed_out = false;
            let release_owned_gates = || {
                let root_release = std::fs::write(self.directory.join("release-root"), "release");
                let target_release =
                    std::fs::write(self.directory.join("release-target"), "release");
                eprintln!(
                    "final manager observation timeout cleanup: root_gate={root_release:?}; target_gate={target_release:?}"
                );
            };
            let target_result = match early {
                Some(result) => result.map_err(|error| error.to_string()),
                None => match tokio::time::timeout_at(completion_deadline, &mut executing).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => {
                        observer_timed_out = true;
                        cancel.cancel();
                        release_owned_gates();
                        // 只撤回同一 scope 的观察，仍 join 原任务；不 abort/drop owner 或伪造结算。
                        executing.await.map_err(|error| error.to_string())
                    }
                },
            };
            let root_result =
                match tokio::time::timeout_at(completion_deadline, &mut root_owner).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => {
                        observer_timed_out = true;
                        cancel.cancel();
                        release_owned_gates();
                        root_owner.await.map_err(|error| error.to_string())
                    }
                };
            eprintln!(
                "actual model request metadata: calls={}; tool_schema_counts={:?}; queued_responses={}",
                self.client.call_count(),
                self.client.all_tool_counts(),
                self.client.remaining()
            );
            let after = crate::tasks::task_runtime::executor::process_execution_governor()
                .shell_semaphore()
                .available_permits();
            let overlap = self.directory.join("overlap").exists();
            eprintln!(
                "final manager real execution: case={}; root_started={first_started}; model_reached_shell={model_reached_shell}; returned_before_release={returned_before_release}; root_owned={root_still_owned}; held={held}; after={after}; atomic_overlap={overlap}; observer_timed_out={observer_timed_out}; root={root_result:?}; target={target_result:?}",
                self.directory.display()
            );
            released?;
            if observer_timed_out {
                return Err("final manager absolute observation deadline elapsed; original scopes cancelled, gates released and tasks joined".into());
            }
            // target 的原 source 错误优先于为回收控制负载产生的 root cancel 次因。
            let target_result: ToolResult = target_result??;
            let root_result = root_result??;
            if !first_started || !model_reached_shell || !root_still_owned {
                return Err("final manager fixture did not reach real root ownership and the original shell model call".into());
            }
            if returned_before_release || overlap || held != 0 || after != 1 {
                return Err("actual final manager bypassed shared global1 or failed to retire its original resource".into());
            }
            if !root_result.success
                || !root_result.output.contains("native-manager-root-effect")
                || root_result.metadata.get("exit_code").map(String::as_str) != Some("0")
            {
                return Err("original root shell lacks typed success/exit0/real output".into());
            }
            if !target_result.success
                || !target_result
                    .output
                    .contains("native-manager-target-effect")
                || target_result.metadata.get("exit_code").map(String::as_str) != Some("0")
            {
                return Err(
                    "actual final Agent shell lacks typed success/exit0/real output".into(),
                );
            }
            if self.directory.join("occupied").exists() {
                return Err("actual payload did not retire its atomic occupancy".into());
            }
            // 错误以 Result 回传，外层先 join 原 I/O/cell owner，再让测试失败。
            Ok(())
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn registered_child_and_real_fork_keep_final_native_manager_policy() -> Result<(), String>
    {
        // 原误接 Mock 的反例独占另一真实 registry；缓存和实例不能流入后面的阳性。
        let negative_fixture = Fixture::new().await?;
        let negative_outcome = async {
            let registry = negative_fixture.created.agent.subagent_registry().clone();
            let case = Case::new(&negative_fixture, "unconfigured-control")?;
            let old_handle = negative_fixture.created.model_consumers.inherited_handle_for_test("implementer")
                .ok_or("original built implementer handle missing")?;
            old_handle.write(|agent| agent.set_llm_client(case.client.clone())).await;
            if negative_fixture.created.model_consumers.inherited_generation.read().await.llm_client.is_some() {
                return Err("missing-client negative fixture requires original unconfigured generation".to_string());
            }
            let unconfigured = registry.get_agent("implementer").await.ok_or("unconfigured registered factory control missing")?;
            let negative_cancel = CancellationToken::new();
            let mut negative = tokio::spawn(collect_actual_shell(unconfigured, negative_cancel.clone()));
            let negative_result = match tokio::time::timeout(std::time::Duration::from_secs(3), &mut negative).await {
                Ok(result) => result.map_err(|error| error.to_string())?,
                Err(_) => {
                    negative_cancel.cancel();
                    let joined = negative.await.map_err(|error| error.to_string());
                    return Err(format!("unconfigured factory control observation expired; original joined={joined:?}"));
                }
            };
            eprintln!("original factory missing-client negative metadata: result={negative_result:?}; mock_calls={}", case.client.call_count());
            if !matches!(negative_result, Err(ref error) if error.contains("missing_llm_client=true"))
                || case.client.call_count() != 0 {
                return Err("original factory control did not expose the expected missing-client source failure".to_string());
            }
            Ok::<(), String>(())
        }.await;
        let negative_closed = negative_fixture.finish().await;
        negative_outcome?;
        negative_closed?;

        let fixture = Fixture::new().await?;
        let outcome = async {
            let registry = fixture.created.agent.subagent_registry().clone();
            let registered_case = Case::new(&fixture, "registered-writer")?;
            // 原注册路径以 factory 覆盖初始 handle；在首次 get_agent 前绑定实际 generation。
            fixture
                .created
                .model_consumers
                .inherited_generation
                .write()
                .await
                .llm_client = Some(registered_case.client.clone());
            let registered = registry
                .get_agent("implementer")
                .await
                .ok_or("registered implementer missing")?;
            registered_case.run(&fixture, registered).await?;
            let fork_case = Case::new(&fixture, "fresh-writer")?;
            fixture
                .created
                .model_consumers
                .inherited_generation
                .write()
                .await
                .llm_client = Some(fork_case.client.clone());
            let fork = registry
                .create_fresh_agent("implementer")
                .await
                .map_err(|error| error.to_string())?
                .ok_or("actual implementer fork missing")?;
            fork_case.run(&fixture, fork).await?;
            let readonly = registry
                .get_agent("reviewer")
                .await
                .ok_or("registered reviewer missing")?;
            if readonly.tool_names().iter().any(|name| name == "shell") {
                return Err("readonly child unexpectedly gained shell".to_string());
            }
            Ok::<(), String>(())
        }
        .await;
        let closed = fixture.finish().await;
        outcome?;
        closed
    }
}
