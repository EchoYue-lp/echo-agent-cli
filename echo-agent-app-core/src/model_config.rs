use echo_agent::config::{AppConfig, ConfiguredModel, ModelProviderConfig};
use echo_agent::llm::core::capabilities::{
    ThinkingProfile, infer_context_window, resolve_thinking_profile,
};
use echo_agent::llm::{LlmApiProtocol, ModelInputModality, resolve_protocol_endpoint};
use serde::Serialize;
use ts_rs::TS;

const DEFAULT_CONTEXT_WINDOW: u32 = 396_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "LlmApiProtocol")]
pub enum LlmApiProtocolWire {
    ChatCompletions,
    Responses,
    Anthropic,
}

impl From<LlmApiProtocol> for LlmApiProtocolWire {
    fn from(protocol: LlmApiProtocol) -> Self {
        match protocol {
            LlmApiProtocol::ChatCompletions => Self::ChatCompletions,
            LlmApiProtocol::Responses => Self::Responses,
            LlmApiProtocol::Anthropic => Self::Anthropic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ModelInputModality")]
pub enum ModelInputModalityWire {
    Text,
    Image,
    Audio,
    Video,
}

impl From<ModelInputModality> for ModelInputModalityWire {
    fn from(modality: ModelInputModality) -> Self {
        match modality {
            ModelInputModality::Text => Self::Text,
            ModelInputModality::Image => Self::Image,
            ModelInputModality::Audio => Self::Audio,
            ModelInputModality::Video => Self::Video,
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ModelProviderView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub requires_api_key: bool,
    pub default_api_protocol: LlmApiProtocolWire,
    pub has_auth_token: bool,
    pub auth_source: String,
    pub model_count: usize,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, rename = "ConfiguredModel")]
pub struct ConfiguredModelView {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub api_protocol: LlmApiProtocolWire,
    pub input_modalities: Vec<ModelInputModalityWire>,
    /// Effective manual choices. `auto` is always implicit and is not repeated.
    pub thinking_levels: Vec<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub has_auth_token: bool,
    pub auth_source: String,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ModelProviderListResponse {
    pub providers: Vec<ModelProviderView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ConfiguredModelListResponse {
    pub models: Vec<ConfiguredModelView>,
    pub default_model_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelRuntimeConfig {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub api_protocol: LlmApiProtocol,
    pub input_modalities: Vec<ModelInputModality>,
    pub thinking_profile: ThinkingProfile,
    pub auth_token: Option<String>,
    pub auth_source: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub requires_api_key: bool,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelectionError {
    NotConfigured,
    Unknown { selector: String },
    Disabled { selector: String },
    AmbiguousName { selector: String },
}

impl std::fmt::Display for ModelSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(
                formatter,
                "No model is configured. Add a provider and at least one enabled model"
            ),
            Self::Unknown { selector } => write!(
                formatter,
                "Model '{selector}' is not configured. Select a configured model id or model name"
            ),
            Self::Disabled { selector } => write!(
                formatter,
                "Model '{selector}' is disabled. Select an enabled configured model"
            ),
            Self::AmbiguousName { selector } => write!(
                formatter,
                "Model name '{selector}' is ambiguous. Select the unique configured model id instead"
            ),
        }
    }
}

impl std::error::Error for ModelSelectionError {}

pub fn find_env_api_key(provider: &ModelProviderConfig) -> Option<String> {
    provider
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn configured_provider_views(config: &AppConfig) -> Vec<ModelProviderView> {
    config
        .model_providers
        .iter()
        .map(|(id, provider)| {
            let env_token = find_env_api_key(provider);
            let name = if provider.name.trim().is_empty() {
                id.clone()
            } else {
                provider.name.clone()
            };
            let has_config_token = provider
                .auth_token
                .as_deref()
                .map(str::trim)
                .is_some_and(|token| !token.is_empty());
            ModelProviderView {
                id: id.clone(),
                name,
                base_url: provider.base_url.clone().unwrap_or_default(),
                api_key_env: provider.api_key_env.clone(),
                requires_api_key: provider.requires_api_key,
                default_api_protocol: provider
                    .default_api_protocol
                    .unwrap_or(LlmApiProtocol::ChatCompletions)
                    .into(),
                has_auth_token: has_config_token || env_token.is_some(),
                auth_source: if has_config_token {
                    "config".to_string()
                } else if env_token.is_some() {
                    "env".to_string()
                } else {
                    "none".to_string()
                },
                model_count: config
                    .configured_models
                    .iter()
                    .filter(|model| model.provider == *id)
                    .count(),
            }
        })
        .collect()
}

pub fn configured_model_views(config: &AppConfig) -> Vec<ConfiguredModelView> {
    if config.configured_models.is_empty() {
        return Vec::new();
    }

    let default_id = config.model.default_model_id.clone();
    config
        .configured_models
        .iter()
        .map(|model| {
            let runtime = resolve_runtime_model(config, Some(&model.id));
            ConfiguredModelView {
                id: model.id.clone(),
                display_name: model.display_name.clone(),
                provider: model.provider.clone(),
                model: model.model.clone(),
                api_protocol: runtime.api_protocol.into(),
                input_modalities: model
                    .input_modalities
                    .iter()
                    .copied()
                    .map(Into::into)
                    .collect(),
                thinking_levels: thinking_level_specs(runtime.thinking_profile),
                enabled: model.enabled,
                is_default: Some(model.id.as_str()) == default_id.as_deref(),
                has_auth_token: runtime.auth_token.is_some(),
                auth_source: runtime.auth_source,
                base_url: runtime.base_url,
                temperature: model.temperature,
                max_tokens: model.max_tokens,
                context_window: Some(effective_context_window(model)),
            }
        })
        .collect()
}

pub fn thinking_level_specs(profile: ThinkingProfile) -> Vec<String> {
    profile
        .levels
        .iter()
        .map(|level| {
            match level {
                echo_agent::llm::ThinkingLevel::None => "none",
                echo_agent::llm::ThinkingLevel::Minimal => "minimal",
                echo_agent::llm::ThinkingLevel::Low => "low",
                echo_agent::llm::ThinkingLevel::Medium => "medium",
                echo_agent::llm::ThinkingLevel::High => "high",
                echo_agent::llm::ThinkingLevel::Xhigh => "xhigh",
                echo_agent::llm::ThinkingLevel::Max => "max",
            }
            .to_string()
        })
        .collect()
}

fn effective_context_window(model: &ConfiguredModel) -> u32 {
    let context_window = model
        .context_window
        .or_else(|| infer_context_window(&model.provider, &model.model))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    if context_window == 0 {
        0
    } else {
        context_window.min(10_000_000)
    }
}

pub fn upsert_model_provider(
    config: &mut AppConfig,
    provider_id: &str,
    mut provider: ModelProviderConfig,
) -> Result<String, String> {
    let provider_id = slug(provider_id);
    if provider_id.is_empty() {
        return Err("Provider id must not be empty".to_string());
    }
    let base_url = provider
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Provider '{provider_id}' requires a base_url"))?;
    let protocol = provider
        .default_api_protocol
        .unwrap_or(LlmApiProtocol::ChatCompletions);
    resolve_protocol_endpoint(base_url, protocol).map_err(|error| error.to_string())?;
    provider.name = provider.name.trim().to_string();
    if provider.name.is_empty() {
        provider.name = provider_id.clone();
    }
    provider.base_url = Some(base_url.to_string());
    provider.auth_token = provider
        .auth_token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    provider.api_key_env = provider
        .api_key_env
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    provider.default_api_protocol = Some(protocol);
    config.model_providers.insert(provider_id.clone(), provider);
    Ok(provider_id)
}

pub fn delete_model_provider(config: &mut AppConfig, provider_id: &str) -> Result<(), String> {
    if config
        .configured_models
        .iter()
        .any(|model| model.provider == provider_id)
    {
        return Err(format!(
            "Provider '{provider_id}' still has configured models; delete them first"
        ));
    }
    if config.model_providers.remove(provider_id).is_none() {
        return Err(format!("Provider '{provider_id}' is not configured"));
    }
    Ok(())
}

pub fn upsert_configured_model(
    config: &mut AppConfig,
    mut model: ConfiguredModel,
) -> Result<String, String> {
    model.provider = slug(&model.provider);
    if !config.model_providers.contains_key(&model.provider) {
        return Err(format!(
            "Provider '{}' is not configured; add the provider before adding models",
            model.provider
        ));
    }
    if model.model.trim().is_empty() {
        return Err("Model name must not be empty".to_string());
    }
    if model.id.trim().is_empty() {
        model.id = stable_model_id(&model.provider, &model.model);
    }
    if model.display_name.trim().is_empty() {
        model.display_name = display_name_from_model(&model.model);
    }
    if model.input_modalities.is_empty() {
        model.input_modalities = ModelInputModality::text_only();
    }
    if !model.input_modalities.contains(&ModelInputModality::Text) {
        model.input_modalities.insert(0, ModelInputModality::Text);
    }

    let id = model.id.clone();
    if let Some(existing) = config.configured_models.iter_mut().find(|m| m.id == id) {
        *existing = model;
    } else {
        config.configured_models.push(model);
    }
    if config.model.default_model_id.is_none() {
        config.model.default_model_id = Some(id.clone());
    }
    Ok(id)
}

pub fn set_default_model(
    config: &mut AppConfig,
    model_id: &str,
) -> Result<ModelRuntimeConfig, String> {
    let model = config
        .configured_models
        .iter()
        .find(|model| model.id == model_id && model.enabled)
        .cloned()
        .ok_or_else(|| format!("Model '{model_id}' is not configured or is disabled"))?;

    config.model.default_model_id = Some(model.id.clone());
    config.model.provider = model.provider.clone();
    config.model.name = model.model.clone();
    config.model.api_protocol = Some(model.api_protocol);
    config.model.temperature = model.temperature;
    config.model.max_tokens = model.max_tokens;
    config.model.context_window = model.context_window;
    let provider_config = config.model_providers.get(&model.provider);
    config.model.auth_token = provider_config.and_then(|provider| provider.auth_token.clone());
    config.model.base_url = provider_config.and_then(|provider| provider.base_url.clone());

    Ok(resolve_runtime_model(config, Some(model_id)))
}

/// Build the non-persistent configuration used by agents created later in the
/// current process. A startup selector changes only this session view; durable
/// configuration remains owned by the model-mutation transaction.
pub fn session_config_for_runtime(
    config: &AppConfig,
    runtime: &ModelRuntimeConfig,
) -> Result<AppConfig, String> {
    let mut session = config.clone();
    if session
        .configured_models
        .iter()
        .any(|model| model.id == runtime.id)
    {
        set_default_model(&mut session, &runtime.id)?;
    }
    Ok(session)
}

#[derive(Debug, Clone)]
pub enum DeleteConfiguredModelOutcome {
    RemovedNonDefault,
    ActivatedSuccessor(Box<ModelRuntimeConfig>),
}

pub fn delete_configured_model(
    config: &mut AppConfig,
    model_id: &str,
) -> Result<DeleteConfiguredModelOutcome, String> {
    let deleting_default = config.model.default_model_id.as_deref() == Some(model_id);
    let successor = deleting_default
        .then(|| {
            config
                .configured_models
                .iter()
                .find(|model| model.id != model_id && model.enabled)
                .cloned()
        })
        .flatten();
    if deleting_default && successor.is_none() {
        return Err(format!(
            "Cannot delete default model '{model_id}' without another enabled model"
        ));
    }
    let before = config.configured_models.len();
    config
        .configured_models
        .retain(|model| model.id != model_id);
    if config.configured_models.len() == before {
        return Err(format!("Model '{model_id}' is not configured"));
    }
    if let Some(successor) = successor {
        let runtime = set_default_model(config, &successor.id)?;
        return Ok(DeleteConfiguredModelOutcome::ActivatedSuccessor(Box::new(
            runtime,
        )));
    }
    Ok(DeleteConfiguredModelOutcome::RemovedNonDefault)
}

pub fn resolve_runtime_model(config: &AppConfig, model_id: Option<&str>) -> ModelRuntimeConfig {
    let selected = model_id
        .and_then(|id| config.configured_models.iter().find(|model| model.id == id))
        .or_else(|| {
            config
                .model
                .default_model_id
                .as_deref()
                .and_then(|id| config.configured_models.iter().find(|model| model.id == id))
        })
        .or_else(|| config.configured_models.iter().find(|model| model.enabled));

    let fallback_id = config
        .model
        .default_model_id
        .clone()
        .unwrap_or_else(|| stable_model_id(&config.model.provider, &config.model.name));

    let (
        id,
        display_name,
        provider,
        model,
        api_protocol,
        input_modalities,
        temperature,
        max_tokens,
        context_window,
    ) = if let Some(selected) = selected {
        (
            selected.id.clone(),
            selected.display_name.clone(),
            selected.provider.clone(),
            selected.model.clone(),
            selected.api_protocol,
            selected.input_modalities.clone(),
            selected.temperature,
            selected.max_tokens,
            Some(effective_context_window(selected)),
        )
    } else {
        (
            fallback_id,
            display_name_from_model(&config.model.name),
            config.model.provider.clone(),
            config.model.name.clone(),
            config
                .model
                .api_protocol
                .unwrap_or(LlmApiProtocol::ChatCompletions),
            ModelInputModality::text_only(),
            config.model.temperature,
            config.model.max_tokens,
            Some({
                let context_window = config
                    .model
                    .context_window
                    .or_else(|| infer_context_window(&config.model.provider, &config.model.name))
                    .unwrap_or(DEFAULT_CONTEXT_WINDOW);
                if context_window == 0 {
                    0
                } else {
                    context_window.min(10_000_000)
                }
            }),
        )
    };

    let provider_config = config.model_providers.get(&provider);
    let config_token = provider_config
        .and_then(|p| p.auth_token.clone())
        .or_else(|| {
            (provider == config.model.provider)
                .then(|| config.model.auth_token.clone())
                .flatten()
        })
        .filter(|token| !token.is_empty());
    let (auth_token, auth_source) = if let Some(token) = config_token {
        (Some(token), "config".to_string())
    } else if let Some(token) = provider_config.and_then(find_env_api_key) {
        (Some(token), "env".to_string())
    } else {
        (None, "none".to_string())
    };

    let base_url = provider_config
        .and_then(|p| p.base_url.clone())
        .or_else(|| {
            (provider == config.model.provider)
                .then(|| config.model.base_url.clone())
                .flatten()
        })
        .filter(|url| !url.trim().is_empty());
    let thinking_profile =
        resolve_thinking_profile(&provider, &model, api_protocol, base_url.as_deref());
    ModelRuntimeConfig {
        id,
        display_name,
        provider,
        model,
        api_protocol,
        input_modalities,
        thinking_profile,
        auth_token,
        auth_source,
        base_url,
        api_key_env: provider_config.and_then(|provider| provider.api_key_env.clone()),
        requires_api_key: provider_config.is_some_and(|provider| provider.requires_api_key),
        temperature,
        max_tokens,
        context_window,
    }
}

/// Resolve a CLI/TUI model selector without splitting model identity from its
/// provider credentials, endpoint, and protocol.
pub fn resolve_runtime_model_selector(
    config: &AppConfig,
    selector: Option<&str>,
) -> Result<ModelRuntimeConfig, ModelSelectionError> {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return config
            .configured_models
            .iter()
            .any(|model| model.enabled)
            .then(|| resolve_runtime_model(config, config.model.default_model_id.as_deref()))
            .ok_or(ModelSelectionError::NotConfigured);
    };

    if let Some(selected) = config
        .configured_models
        .iter()
        .find(|model| model.id == selector)
    {
        if !selected.enabled {
            return Err(ModelSelectionError::Disabled {
                selector: selector.to_string(),
            });
        }
        return Ok(resolve_runtime_model(config, Some(&selected.id)));
    }

    let mut matches = config
        .configured_models
        .iter()
        .filter(|model| model.enabled && model.model == selector);
    let selected = matches.next().ok_or_else(|| {
        if config
            .configured_models
            .iter()
            .any(|model| !model.enabled && model.model == selector)
        {
            ModelSelectionError::Disabled {
                selector: selector.to_string(),
            }
        } else {
            ModelSelectionError::Unknown {
                selector: selector.to_string(),
            }
        }
    })?;
    if matches.next().is_some() {
        return Err(ModelSelectionError::AmbiguousName {
            selector: selector.to_string(),
        });
    }

    Ok(resolve_runtime_model(config, Some(&selected.id)))
}

/// Validate that the provider root can resolve to the selected model protocol.
pub fn validate_runtime_model_endpoint(runtime: &ModelRuntimeConfig) -> Result<(), String> {
    let base_url = runtime
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Provider '{}' requires a model endpoint", runtime.provider))?;
    resolve_protocol_endpoint(base_url, runtime.api_protocol).map_err(|error| error.to_string())?;
    Ok(())
}

/// Apply EKO's product policy after provider/endpoint/protocol resolution.
pub fn validate_runtime_model_requirements(runtime: &ModelRuntimeConfig) -> Result<(), String> {
    validate_runtime_model_endpoint(runtime)?;
    if runtime.context_window == Some(0) {
        return Err(format!(
            "Model '{}' context_window must be greater than zero",
            runtime.id
        ));
    }
    let has_token = runtime
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .is_some();
    if runtime.requires_api_key && !has_token {
        let env_var = runtime.api_key_env.as_deref().unwrap_or("not configured");
        return Err(format!(
            "Provider '{}' requires an API key for endpoint '{}'. Configure auth_token or environment variable {}",
            runtime.provider,
            runtime.base_url.as_deref().unwrap_or_default(),
            env_var
        ));
    }
    Ok(())
}

pub fn stable_model_id(provider: &str, model: &str) -> String {
    let provider = slug(provider);
    let model = slug(model);
    if provider.is_empty() {
        model
    } else if model.is_empty() {
        provider
    } else {
        format!("{provider}:{model}")
    }
}

fn slug(input: &str) -> String {
    input
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn display_name_from_model(model: &str) -> String {
    model
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_model(
        provider: &str,
        model: &str,
        context_window: Option<u32>,
    ) -> ConfiguredModel {
        ConfiguredModel {
            provider: provider.to_string(),
            model: model.to_string(),
            context_window,
            ..ConfiguredModel::default()
        }
    }

    #[test]
    fn effective_context_window_prefers_explicit_value() {
        let model = configured_model("anthropic", "claude-4-sonnet", Some(353_000));
        assert_eq!(effective_context_window(&model), 353_000);
    }

    #[test]
    fn effective_context_window_infers_known_models() {
        let openai = configured_model("openai", "gpt-5.6-sol", None);
        let anthropic = configured_model("anthropic", "claude-sonnet-5", None);

        assert_eq!(effective_context_window(&openai), 1_050_000);
        assert_eq!(effective_context_window(&anthropic), 1_000_000);
    }

    #[test]
    fn effective_context_window_uses_framework_default_for_unknown_models() {
        let model = configured_model("custom", "local-model", None);
        assert_eq!(effective_context_window(&model), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn session_runtime_selection_does_not_mutate_the_persisted_default() -> Result<(), String> {
        let mut config = AppConfig {
            configured_models: vec![
                ConfiguredModel {
                    id: "local:a".to_string(),
                    display_name: "A".to_string(),
                    provider: "local".to_string(),
                    model: "a".to_string(),
                    context_window: Some(100_000),
                    ..ConfiguredModel::default()
                },
                ConfiguredModel {
                    id: "local:b".to_string(),
                    display_name: "B".to_string(),
                    provider: "local".to_string(),
                    model: "b".to_string(),
                    context_window: Some(200_000),
                    ..ConfiguredModel::default()
                },
            ],
            ..AppConfig::default()
        };
        config.model.default_model_id = Some("local:a".to_string());
        let selected = resolve_runtime_model_selector(&config, Some("local:b"))
            .map_err(|error| error.to_string())?;

        let session = session_config_for_runtime(&config, &selected)?;

        assert_eq!(config.model.default_model_id.as_deref(), Some("local:a"));
        assert_eq!(session.model.default_model_id.as_deref(), Some("local:b"));
        let future = resolve_runtime_model(&session, None);
        assert_eq!(future.model, "b");
        assert_eq!(future.context_window, Some(200_000));
        Ok(())
    }

    #[test]
    fn explicit_zero_context_window_is_rejected_by_runtime_preflight() -> Result<(), String> {
        let mut config = AppConfig {
            configured_models: vec![ConfiguredModel {
                id: "local:zero".to_string(),
                display_name: "Zero".to_string(),
                provider: "local".to_string(),
                model: "zero".to_string(),
                context_window: Some(0),
                ..ConfiguredModel::default()
            }],
            ..AppConfig::default()
        };
        config.model_providers.insert(
            "local".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        let runtime = resolve_runtime_model(&config, Some("local:zero"));

        assert_eq!(runtime.context_window, Some(0));
        let error = validate_runtime_model_requirements(&runtime)
            .err()
            .ok_or_else(|| "zero context window unexpectedly passed preflight".to_string())?;
        assert!(error.contains("context_window"));
        Ok(())
    }

    #[test]
    fn model_capabilities_keep_text_default_and_preserve_friendly_modalities() -> Result<(), String>
    {
        let mut config = AppConfig::default();
        upsert_model_provider(
            &mut config,
            "gateway",
            ModelProviderConfig {
                base_url: Some("https://gateway.example/v1".to_string()),
                ..Default::default()
            },
        )?;
        let model_id = upsert_configured_model(
            &mut config,
            ConfiguredModel {
                provider: "gateway".to_string(),
                model: "omni".to_string(),
                input_modalities: vec![
                    ModelInputModality::Image,
                    ModelInputModality::Audio,
                    ModelInputModality::Video,
                ],
                ..ConfiguredModel::default()
            },
        )?;
        let runtime = resolve_runtime_model(&config, Some(&model_id));

        assert_eq!(
            runtime.input_modalities,
            vec![
                ModelInputModality::Text,
                ModelInputModality::Image,
                ModelInputModality::Audio,
                ModelInputModality::Video,
            ]
        );
        Ok(())
    }

    #[test]
    fn deleting_default_selects_an_enabled_successor_before_removal() -> Result<(), String> {
        let mut config = AppConfig {
            configured_models: vec![
                ConfiguredModel {
                    id: "local:a".to_string(),
                    display_name: "A".to_string(),
                    provider: "local".to_string(),
                    model: "a".to_string(),
                    ..ConfiguredModel::default()
                },
                ConfiguredModel {
                    id: "local:disabled".to_string(),
                    display_name: "Disabled".to_string(),
                    provider: "local".to_string(),
                    model: "disabled".to_string(),
                    enabled: false,
                    ..ConfiguredModel::default()
                },
                ConfiguredModel {
                    id: "local:b".to_string(),
                    display_name: "B".to_string(),
                    provider: "local".to_string(),
                    model: "b".to_string(),
                    ..ConfiguredModel::default()
                },
            ],
            ..AppConfig::default()
        };
        set_default_model(&mut config, "local:a")?;

        let outcome = delete_configured_model(&mut config, "local:a")?;

        assert!(matches!(
            outcome,
            DeleteConfiguredModelOutcome::ActivatedSuccessor(ref runtime)
                if runtime.id == "local:b"
        ));
        assert_eq!(config.model.default_model_id.as_deref(), Some("local:b"));
        assert!(
            config
                .configured_models
                .iter()
                .all(|model| model.id != "local:a")
        );
        Ok(())
    }

    #[test]
    fn deleting_last_default_is_rejected_without_mutation() -> Result<(), String> {
        let mut config = AppConfig {
            configured_models: vec![ConfiguredModel {
                id: "local:a".to_string(),
                display_name: "A".to_string(),
                provider: "local".to_string(),
                model: "a".to_string(),
                ..ConfiguredModel::default()
            }],
            ..AppConfig::default()
        };
        set_default_model(&mut config, "local:a")?;

        let result = delete_configured_model(&mut config, "local:a");

        assert!(result.is_err());
        assert_eq!(config.model.default_model_id.as_deref(), Some("local:a"));
        assert_eq!(config.configured_models.len(), 1);
        Ok(())
    }

    #[test]
    fn successor_without_provider_config_clears_legacy_credentials() -> Result<(), String> {
        let mut config = AppConfig::default();
        config.model.auth_token = Some("old-provider-token".to_string());
        config.model.base_url = Some("https://old.example/v1/responses".to_string());
        config.configured_models = vec![
            ConfiguredModel {
                id: "old:a".to_string(),
                provider: "old".to_string(),
                model: "a".to_string(),
                ..ConfiguredModel::default()
            },
            ConfiguredModel {
                id: "new:b".to_string(),
                provider: "new".to_string(),
                model: "b".to_string(),
                ..ConfiguredModel::default()
            },
        ];
        config.model.default_model_id = Some("old:a".to_string());

        let outcome = delete_configured_model(&mut config, "old:a")?;

        assert!(matches!(
            outcome,
            DeleteConfiguredModelOutcome::ActivatedSuccessor(ref runtime)
                if runtime.id == "new:b" && runtime.auth_token.is_none()
        ));
        assert!(config.model.auth_token.is_none());
        assert!(config.model.base_url.is_none());
        Ok(())
    }

    #[test]
    fn api_key_requirement_is_explicit_not_assumed() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "gateway".to_string(),
            ModelProviderConfig {
                requires_api_key: true,
                ..Default::default()
            },
        );
        let view = configured_provider_views(&config).into_iter().next();
        assert!(view.is_some_and(|provider| provider.requires_api_key));
    }

    #[test]
    fn provider_key_policy_is_applied_without_provider_name_inference() {
        let mut config = AppConfig {
            configured_models: vec![ConfiguredModel {
                id: "gateway:model".to_string(),
                provider: "gateway".to_string(),
                model: "model".to_string(),
                enabled: true,
                ..ConfiguredModel::default()
            }],
            ..AppConfig::default()
        };
        config.model_providers.insert(
            "gateway".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/responses".to_string()),
                requires_api_key: true,
                ..Default::default()
            },
        );
        let runtime = resolve_runtime_model(&config, Some("gateway:model"));
        assert!(validate_runtime_model_requirements(&runtime).is_err());
    }

    #[test]
    fn configured_provider_views_project_user_protocol_defaults() -> Result<(), String> {
        let mut config = AppConfig::default();
        upsert_model_provider(
            &mut config,
            "my-gateway",
            ModelProviderConfig {
                name: "My Gateway".to_string(),
                base_url: Some("https://gateway.example/v1".to_string()),
                default_api_protocol: Some(LlmApiProtocol::Responses),
                ..Default::default()
            },
        )?;
        let provider = configured_provider_views(&config).into_iter().next();
        assert!(matches!(
            provider,
            Some(ModelProviderView {
                id,
                default_api_protocol: LlmApiProtocolWire::Responses,
                ..
            }) if id == "my-gateway"
        ));
        Ok(())
    }

    #[test]
    fn model_protocol_is_explicit_and_independent_of_provider_name() {
        for provider in ["openai", "anthropic", "custom"] {
            let mut config = AppConfig::default();
            let model_id = stable_model_id(provider, "model");
            config.configured_models = vec![ConfiguredModel {
                id: model_id.clone(),
                provider: provider.to_string(),
                model: "model".to_string(),
                api_protocol: LlmApiProtocol::Responses,
                ..ConfiguredModel::default()
            }];

            assert_eq!(
                resolve_runtime_model(&config, Some(&model_id)).api_protocol,
                LlmApiProtocol::Responses,
                "unexpected runtime protocol for {provider}"
            );
        }
    }

    #[test]
    fn one_provider_root_supports_models_with_different_protocols() -> Result<(), String> {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "custom".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1/responses".to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![ConfiguredModel {
            id: "custom:inferred".to_string(),
            provider: "custom".to_string(),
            model: "inferred".to_string(),
            api_protocol: LlmApiProtocol::Responses,
            ..ConfiguredModel::default()
        }];
        assert_eq!(
            resolve_runtime_model(&config, Some("custom:inferred")).api_protocol,
            LlmApiProtocol::Responses
        );

        let configured = config
            .configured_models
            .first_mut()
            .ok_or_else(|| "configured model fixture is missing".to_string())?;
        configured.api_protocol = LlmApiProtocol::Anthropic;
        let explicit = resolve_runtime_model(&config, Some("custom:inferred"));
        assert_eq!(explicit.api_protocol, LlmApiProtocol::Anthropic);
        assert!(validate_runtime_model_endpoint(&explicit).is_ok());
        Ok(())
    }

    #[test]
    fn provider_root_accepts_explicit_responses_protocol() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1".to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![ConfiguredModel {
            id: "openai:root-only".to_string(),
            provider: "openai".to_string(),
            model: "root-only".to_string(),
            api_protocol: LlmApiProtocol::Responses,
            enabled: true,
            ..ConfiguredModel::default()
        }];

        let runtime = resolve_runtime_model(&config, Some("openai:root-only"));
        assert_eq!(runtime.api_protocol, LlmApiProtocol::Responses);
        assert!(validate_runtime_model_endpoint(&runtime).is_ok());

        let configured = config.configured_models.first_mut();
        if let Some(configured) = configured {
            configured.api_protocol = LlmApiProtocol::Anthropic;
        }
        let explicit = resolve_runtime_model(&config, Some("openai:root-only"));
        assert_eq!(explicit.api_protocol, LlmApiProtocol::Anthropic);
        assert!(validate_runtime_model_endpoint(&explicit).is_ok());
    }

    #[test]
    fn complete_endpoint_is_rewritten_for_each_explicit_protocol() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![ConfiguredModel {
            id: "openai:compatible".to_string(),
            provider: "openai".to_string(),
            model: "compatible".to_string(),
            enabled: true,
            ..ConfiguredModel::default()
        }];

        let inferred = resolve_runtime_model(&config, Some("openai:compatible"));
        assert_eq!(inferred.api_protocol, LlmApiProtocol::ChatCompletions);
        assert!(validate_runtime_model_endpoint(&inferred).is_ok());

        let configured = config.configured_models.first_mut();
        if let Some(configured) = configured {
            configured.api_protocol = LlmApiProtocol::Responses;
        }
        let mismatched = resolve_runtime_model(&config, Some("openai:compatible"));
        assert_eq!(mismatched.api_protocol, LlmApiProtocol::Responses);
        assert!(validate_runtime_model_endpoint(&mismatched).is_ok());
    }

    #[test]
    fn selector_resolves_one_identity_and_types_unknown_disabled_and_ambiguous_errors() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: Some("openai-key".to_string()),
                base_url: Some("https://api.openai.com/v1/responses".to_string()),
                ..Default::default()
            },
        );
        config.model_providers.insert(
            "anthropic".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: Some("anthropic-key".to_string()),
                base_url: Some("https://api.anthropic.com/v1/messages".to_string()),
                ..Default::default()
            },
        );
        config.configured_models = vec![
            ConfiguredModel {
                id: "openai:shared".to_string(),
                provider: "openai".to_string(),
                model: "shared".to_string(),
                enabled: true,
                ..ConfiguredModel::default()
            },
            ConfiguredModel {
                id: "openai:disabled".to_string(),
                provider: "openai".to_string(),
                model: "disabled".to_string(),
                enabled: false,
                ..ConfiguredModel::default()
            },
            ConfiguredModel {
                id: "anthropic:shared".to_string(),
                provider: "anthropic".to_string(),
                model: "shared".to_string(),
                api_protocol: LlmApiProtocol::Anthropic,
                enabled: true,
                ..ConfiguredModel::default()
            },
        ];

        let selected = resolve_runtime_model_selector(&config, Some("anthropic:shared"));
        assert!(matches!(
            selected,
            Ok(ModelRuntimeConfig {
                provider,
                api_protocol: LlmApiProtocol::Anthropic,
                ..
            }) if provider == "anthropic"
        ));
        assert!(matches!(
            resolve_runtime_model_selector(&config, Some("missing-model")),
            Err(ModelSelectionError::Unknown { selector })
                if selector == "missing-model"
        ));
        assert!(matches!(
            resolve_runtime_model_selector(&config, Some("shared")),
            Err(ModelSelectionError::AmbiguousName { selector }) if selector == "shared"
        ));
        assert!(matches!(
            resolve_runtime_model_selector(&config, Some("openai:disabled")),
            Err(ModelSelectionError::Disabled { selector })
                if selector == "openai:disabled"
        ));
        assert!(matches!(
            resolve_runtime_model_selector(&config, Some("disabled")),
            Err(ModelSelectionError::Disabled { selector })
                if selector == "disabled"
        ));
    }

    #[test]
    fn yaml_protocol_defaults_to_chat_and_honors_explicit_override() -> Result<(), String> {
        let config: AppConfig = serde_yaml::from_str(
            r#"
model_providers:
  custom:
    base_url: https://gateway.example/v1/responses
configured_models:
  - id: openai:default
    provider: openai
    model: gpt-test
  - id: anthropic:chat-override
    provider: anthropic
    model: claude-test
    api_protocol: chat_completions
  - id: custom:inferred
    provider: custom
    model: custom-test
"#,
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(
            resolve_runtime_model(&config, Some("openai:default")).api_protocol,
            LlmApiProtocol::ChatCompletions
        );
        assert_eq!(
            resolve_runtime_model(&config, Some("anthropic:chat-override")).api_protocol,
            LlmApiProtocol::ChatCompletions
        );
        assert_eq!(
            resolve_runtime_model(&config, Some("custom:inferred")).api_protocol,
            LlmApiProtocol::ChatCompletions
        );
        Ok(())
    }

    #[test]
    fn empty_configuration_has_no_synthetic_model() {
        let mut config = AppConfig::default();
        config.model.provider = "ignored-legacy-provider".to_string();
        config.model.name = "ignored-legacy-model".to_string();

        let views = configured_model_views(&config);
        assert!(views.is_empty());
        assert!(matches!(
            resolve_runtime_model_selector(&config, None),
            Err(ModelSelectionError::NotConfigured)
        ));
    }

    #[test]
    fn model_views_expose_only_effective_thinking_levels() -> Result<(), String> {
        let mut config = AppConfig {
            configured_models: vec![ConfiguredModel {
                id: "openai:gpt-5.6-sol".to_string(),
                display_name: "GPT-5.6 Sol".to_string(),
                provider: "openai".to_string(),
                model: "gpt-5.6-sol".to_string(),
                api_protocol: LlmApiProtocol::Responses,
                ..ConfiguredModel::default()
            }],
            ..AppConfig::default()
        };
        config.model.default_model_id = Some("openai:gpt-5.6-sol".to_string());

        let views = configured_model_views(&config);
        let view = views
            .first()
            .ok_or_else(|| "GPT-5.6 model view was not created".to_string())?;
        assert_eq!(
            view.thinking_levels,
            ["none", "low", "medium", "high", "xhigh", "max"]
        );

        let configured = config
            .configured_models
            .first_mut()
            .ok_or_else(|| "configured model disappeared".to_string())?;
        configured.id = "zhipu:glm-4.6".to_string();
        configured.provider = "zhipu".to_string();
        configured.model = "glm-4.6".to_string();
        configured.api_protocol = LlmApiProtocol::ChatCompletions;
        config.model.default_model_id = Some("zhipu:glm-4.6".to_string());
        let older = configured_model_views(&config);
        let older_view = older
            .first()
            .ok_or_else(|| "GLM-4.6 model view was not created".to_string())?;
        assert!(older_view.thinking_levels.is_empty());

        let configured = config
            .configured_models
            .first_mut()
            .ok_or_else(|| "configured model disappeared".to_string())?;
        configured.id = "zhipu:glm-5.2".to_string();
        configured.model = "glm-5.2".to_string();
        config.model.default_model_id = Some("zhipu:glm-5.2".to_string());
        let current = configured_model_views(&config);
        let current_view = current
            .first()
            .ok_or_else(|| "GLM-5.2 model view was not created".to_string())?;
        assert_eq!(current_view.thinking_levels, ["none", "high", "max"]);
        Ok(())
    }
}
