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
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::core::capabilities::ThinkingProfile::unknown(),
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
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::core::capabilities::ThinkingProfile::unknown(),
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
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::core::capabilities::ThinkingProfile::unknown(),
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

#[test]
fn zero_primary_iteration_config_uses_framework_unlimited_sentinel() {
    assert_eq!(resolved_max_iterations(0), usize::MAX);
    assert_eq!(resolved_max_iterations(1), 1);
    assert_eq!(resolved_max_iterations(100), 100);
}

#[tokio::test]
async fn zero_primary_iteration_config_builds_a_framework_valid_agent() -> Result<(), String> {
    let mut config = EkoConfig::default();
    config.agent.max_iterations = 0;
    let created = create_agent_with_diagnostics(&AgentCreateParams::default(), &config).await?;
    assert_eq!(created.agent.config().get_max_iterations(), usize::MAX);
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
    use echo_agent::agent::subagent::{SubagentPromptCompiler, SubagentRegistry};
    use echo_agent::sandbox::SandboxManager;
    use std::sync::Arc;

    fn test_command_cells()
    -> echo_agent::error::Result<Arc<dyn echo_agent::tools::cell::CommandCellRegistry>> {
        Ok(Arc::new(
            echo_agent::tasks::BackgroundCommandManager::default(),
        ))
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
            temperature: None,
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
            temperature: None,
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
        // thinking is copied (mirrors temperature/max_tokens).
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
            temperature: None,
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
            temperature: Some(0.4),
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
        assert_eq!(fixed.temperature, parent.temperature);
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
        )?;

        assert!(subagent.sandbox_manager().is_some());
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
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            Arc::new(crate::analysis_runtime::AnalyticsRuntime::default()),
            true,
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
        // build_readonly_subagent_agent must install the resolved thinking on
        // the built agent (ReactAgent::thinking getter) — this is the wire the
        // awaiter's `thinking: low` frontmatter rides on.
        let low = echo_agent::llm::ThinkingConfig::Level(echo_agent::llm::ThinkingLevel::Low);
        let subagent = build_readonly_subagent_agent(
            "awaiter",
            "wait for one background cell",
            "test-model",
            None,
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
            test_command_cells()?,
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
            test_command_cells()?,
        )?;
        assert!(plain.thinking().is_none());
        Ok(())
    }

    #[test]
    fn readonly_and_writer_subagents_share_cell_tools() -> echo_agent::error::Result<()> {
        // C2b:两个构建路径都注入进程级共享 cell registry——readonly 子智能体
        // (如 awaiter)没有 shell,但必须拥有 wait/stop_cell/list_cells。
        let readonly = build_readonly_subagent_agent(
            "awaiter",
            "wait for one background cell",
            "test-model",
            None,
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
            test_command_cells()?,
        )?;
        let readonly_names = readonly.tool_names();
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
        // The builtin awaiter declares `thinking: low`; the same parse_spec the
        // binding uses must accept it, and the loader spec string must flow
        // unchanged from the .md frontmatter.
        let defs = crate::subagent_loader::discover_subagents(None, None);
        let awaiter = defs
            .iter()
            .find(|d| d.name == "awaiter")
            .ok_or_else(|| "builtin awaiter.md must load".to_string())?;
        let parsed = echo_agent::llm::ThinkingConfig::parse_spec(
            awaiter.thinking.as_deref().unwrap_or_default(),
        )
        .map_err(|error| format!("awaiter thinking spec must parse: {error}"))?;
        assert_eq!(
            parsed,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );
        Ok(())
    }
}
