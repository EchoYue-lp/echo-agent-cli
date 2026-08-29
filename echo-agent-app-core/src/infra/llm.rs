/// Build an [`LlmConfig`] from the EkoConfig's model section.
///
/// Maps the provider string to the appropriate factory method and optionally
/// overrides the base URL. This enables auth_token / base_url from
/// `eko.yaml` to flow through to the agent's LLM client without
/// requiring `echo-agent-models.yaml` or provider-specific env vars.
pub fn build_llm_config(
    provider: &str,
    auth_token: &str,
    model: &str,
    base_url: &str,
    api_protocol: LlmApiProtocol,
    input_modalities: Vec<echo_agent::llm::ModelInputModality>,
) -> Result<LlmConfig, String> {
    LlmConfig::for_provider(provider, base_url, auth_token, model, api_protocol)
        .map(|config| config.with_input_modalities(input_modalities))
        .map_err(|error| error.to_string())
}

/// Convert one fully resolved EKO model into the framework wire configuration.
/// Empty API keys remain valid for local endpoints; required-key policy is
/// enforced separately before this adapter is called.
pub fn build_runtime_llm_config(
    runtime: &model_config::ModelRuntimeConfig,
) -> Result<LlmConfig, String> {
    model_config::validate_runtime_model_endpoint(runtime)?;
    let base_url = runtime
        .base_url
        .as_deref()
        .ok_or_else(|| format!("Provider '{}' requires a base_url", runtime.provider))?;
    build_llm_config(
        &runtime.provider,
        runtime.auth_token.as_deref().unwrap_or_default(),
        &runtime.model,
        base_url,
        runtime.api_protocol,
        runtime.input_modalities.clone(),
    )
}

/// One fully validated EKO runtime model and the client built from it.
///
/// The application uses this as a pre-commit value: invalid headers, tokens,
/// endpoints, and provider/client combinations fail before config persistence
/// or live-agent mutation begins.
#[derive(Clone)]
pub struct PreparedRuntimeLlm {
    pub config: LlmConfig,
    pub client: Arc<dyn LlmClient>,
    pub thinking: Option<echo_agent::llm::ThinkingConfig>,
}

/// Exact admission receipt for publishing one prepared model generation to a
/// live parent agent and its inherited subagent factories.
pub(crate) struct PreparedAgentModelPublication {
    generation: echo_agent::agent::PreparedAgentModelGeneration,
    inherited: Vec<PreparedInheritedSubagentPublication>,
    consumers: AgentModelConsumers,
    runtime: model_config::ModelRuntimeConfig,
    prepared: PreparedRuntimeLlm,
    token_limit: usize,
}

/// Exact admission receipt for removing the active model from a live parent
/// agent and all inherited subagent factories.
pub(crate) struct PreparedAgentModelDeactivation {
    generation: echo_agent::agent::PreparedAgentModelDeactivation,
    inherited: Vec<echo_agent::agent::PreparedAgentModelDeactivation>,
    consumers: AgentModelConsumers,
}

struct PreparedInheritedSubagentPublication {
    generation: echo_agent::agent::PreparedAgentModelGeneration,
}

impl PreparedInheritedSubagentPublication {
    fn commit(self) {
        self.generation.commit();
    }
}

impl PreparedAgentModelPublication {
    /// Publish only pre-owned values. All validation and context admission was
    /// completed by [`prepare_agent_model_publication`].
    pub(crate) async fn commit(self) {
        self.generation.commit();
        for inherited in self.inherited {
            inherited.commit();
        }
        self.consumers
            .publish_inherited_generation(&self.runtime, &self.prepared, self.token_limit)
            .await;
    }
}

impl PreparedAgentModelDeactivation {
    pub(crate) async fn commit(self) {
        self.generation.commit();
        for inherited in self.inherited {
            inherited.commit();
        }
        self.consumers.clear_inherited_generation().await;
    }
}

/// Resolve and construct the exact client used by every EKO model surface.
pub fn prepare_runtime_llm(
    runtime: &model_config::ModelRuntimeConfig,
) -> Result<PreparedRuntimeLlm, String> {
    model_config::validate_runtime_model_requirements(runtime)?;
    let config = build_runtime_llm_config(runtime)?;
    let client: Arc<dyn LlmClient> = config
        .build_client()
        .map(Arc::from)
        .map_err(|error| format!("Failed to create client: {error}"))?;
    Ok(PreparedRuntimeLlm {
        config,
        client,
        thinking: None,
    })
}

/// Result of the shared EKO connection probe used by GUI, TUI, and CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLlmConnectionTest {
    pub response: String,
    pub model: String,
}

/// Send one minimal request through the exact client configuration used by the
/// live agent. Surface-specific code only resolves user input and renders this
/// result; protocol behavior remains identical across GUI, TUI, and CLI.
pub async fn test_runtime_llm_connection(
    runtime: &model_config::ModelRuntimeConfig,
) -> Result<RuntimeLlmConnectionTest, String> {
    let prepared = prepare_runtime_llm(runtime)?;
    let messages = vec![Message::user("Hi, respond with just 'OK'.".to_string())];
    let response = prepared
        .client
        .chat_simple(messages)
        .await
        .map_err(|error| format!("API call failed: {error}"))?;
    Ok(RuntimeLlmConnectionTest {
        response,
        model: prepared.client.model_name().to_string(),
    })
}

/// Prepare one live agent and every model-coupled runtime consumer without
/// changing any published state.
pub(crate) async fn prepare_agent_model_publication(
    handle: &AgentHandle,
    consumers: AgentModelConsumers,
    runtime: &model_config::ModelRuntimeConfig,
    prepared: &PreparedRuntimeLlm,
    token_limit: usize,
) -> Result<PreparedAgentModelPublication, String> {
    let (critic_owner, cache_user_id) = handle
        .read(|agent| {
            (
                agent.critic_owner().map(str::to_string),
                agent.config().get_cache_user_id().map(str::to_string),
            )
        })
        .await;
    let critic = if critic_owner.as_deref() == Some(EKO_MODEL_CRITIC_OWNER) {
        let mut critic = echo_agent::agent::critic::LlmCritic::new(prepared.client.clone())
            .with_pass_threshold(7.0);
        if let Some(cache_user_id) = cache_user_id {
            critic = critic.with_cache_user_id(cache_user_id);
        }
        echo_agent::agent::PreparedCriticUpdate::ReplaceOwned {
            owner: EKO_MODEL_CRITIC_OWNER.to_string(),
            critic: Arc::new(critic),
        }
    } else {
        echo_agent::agent::PreparedCriticUpdate::Preserve
    };
    let generation = handle
        .prepare_model_generation(
            prepared.config.clone(),
            prepared.client.clone(),
            runtime.temperature,
            runtime.max_tokens.or(Some(DEFAULT_MAX_TOKENS)),
            prepared.thinking.clone(),
            token_limit,
            critic,
        )
        .await
        .map_err(|error| format!("Failed to prepare agent model generation: {error}"))?;
    let mut inherited_publications = Vec::with_capacity(consumers.inherited_factories.len());
    for inherited in consumers.inherited_factories.iter() {
        let inherited_generation = inherited
            .handle
            .prepare_model_generation(
                prepared.config.clone(),
                prepared.client.clone(),
                runtime.temperature,
                runtime.max_tokens.or(Some(DEFAULT_MAX_TOKENS)),
                prepared.thinking.clone(),
                token_limit,
                echo_agent::agent::PreparedCriticUpdate::Preserve,
            )
            .await
            .map_err(|error| {
                format!(
                    "Failed to prepare inherited subagent '{}' model generation: {error}",
                    inherited.definition.name
                )
            })?;
        inherited_publications.push(PreparedInheritedSubagentPublication {
            generation: inherited_generation,
        });
    }
    Ok(PreparedAgentModelPublication {
        generation,
        inherited: inherited_publications,
        consumers,
        runtime: runtime.clone(),
        prepared: prepared.clone(),
        token_limit,
    })
}

/// Lock every model consumer so deleting the final configured model can be
/// committed without leaving a stale client available to later turns.
pub(crate) async fn prepare_agent_model_deactivation(
    handle: &AgentHandle,
    consumers: AgentModelConsumers,
) -> PreparedAgentModelDeactivation {
    let generation = handle
        .prepare_model_deactivation(Some(EKO_MODEL_CRITIC_OWNER.to_string()))
        .await;
    let mut inherited = Vec::with_capacity(consumers.inherited_factories.len());
    for factory in consumers.inherited_factories.iter() {
        inherited.push(factory.handle.prepare_model_deactivation(None).await);
    }
    PreparedAgentModelDeactivation {
        generation,
        inherited,
        consumers,
    }
}
