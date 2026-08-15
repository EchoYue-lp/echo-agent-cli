use echo_agent::config::{AppConfig, ConfiguredModel};
use echo_agent::llm::LlmApiProtocol;
use echo_agent::llm::config::{
    all_provider_metadata, provider_base_url, provider_env_var_names, provider_metadata,
};
use echo_agent::llm::core::capabilities::infer_context_window;
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

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ProviderTemplate {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_env: String,
    pub default_models: Vec<String>,
    pub requires_api_key: bool,
    pub default_api_protocol: LlmApiProtocolWire,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, rename = "ConfiguredModel")]
pub struct ConfiguredModelView {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub api_protocol: LlmApiProtocolWire,
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
pub struct ProviderTemplateListResponse {
    pub providers: Vec<ProviderTemplate>,
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
    /// Whether the protocol was explicitly configured instead of resolved by
    /// endpoint inference/provider metadata.
    pub api_protocol_explicit: bool,
    pub auth_token: Option<String>,
    pub auth_source: String,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    /// 思考深度配置（可选）。可读值:`"auto"`/`""`(默认)、`"disabled"`、
    /// `"minimal"`/`"low"`/`"medium"`/`"high"`、或裸数字(token 预算)。
    /// 由前端 UI 设置,运行时翻译成 `ThinkingConfig` 注入到 agent。
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelectionError {
    Unknown { selector: String },
    Disabled { selector: String },
    AmbiguousName { selector: String },
}

impl std::fmt::Display for ModelSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

pub fn provider_env_vars(provider: &str) -> &'static [&'static str] {
    provider_env_var_names(provider)
}

pub fn find_env_api_key(provider: &str) -> Option<String> {
    provider_env_vars(provider).iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .map(|val| val.trim().to_string())
            .filter(|val| !val.is_empty())
    })
}

pub fn env_vars_display(provider: &str) -> String {
    provider_env_vars(provider).join(" / ")
}

pub fn default_base_url(provider: &str) -> Option<String> {
    provider_base_url(provider).map(ToString::to_string)
}

pub fn provider_templates() -> Vec<ProviderTemplate> {
    all_provider_metadata()
        .iter()
        .map(|metadata| ProviderTemplate {
            id: metadata.id.to_string(),
            name: metadata.name.to_string(),
            base_url: metadata.base_url.to_string(),
            api_key_env: metadata.env_vars.join(" / "),
            default_models: metadata
                .default_models
                .iter()
                .map(|model| model.to_string())
                .collect(),
            requires_api_key: metadata.requires_api_key,
            default_api_protocol: metadata.default_api_protocol.into(),
        })
        .chain(std::iter::once(ProviderTemplate {
            id: "custom".to_string(),
            name: "自定义".to_string(),
            base_url: String::new(),
            api_key_env: String::new(),
            default_models: Vec::new(),
            requires_api_key: false,
            default_api_protocol: LlmApiProtocol::ChatCompletions.into(),
        }))
        .collect()
}

pub fn provider_requires_api_key(provider: &str) -> bool {
    provider_metadata(provider)
        .map(|metadata| metadata.requires_api_key)
        .unwrap_or(false)
}

/// EKO only requires a token for a built-in provider's own hosted endpoint.
/// Compatible gateways (including local endpoints) and unknown providers may
/// accept an empty bearer token and must still receive a complete `LlmConfig`.
pub fn runtime_requires_api_key(runtime: &ModelRuntimeConfig) -> bool {
    let Some(metadata) = provider_metadata(&runtime.provider) else {
        return false;
    };
    if !metadata.requires_api_key {
        return false;
    }
    runtime
        .base_url
        .as_deref()
        .map(str::trim)
        .map(|endpoint| {
            endpoint
                .split(['?', '#'])
                .next()
                .unwrap_or(endpoint)
                .trim_end_matches('/')
                == metadata.base_url.trim_end_matches('/')
        })
        .unwrap_or(true)
}

pub fn configured_model_views(config: &AppConfig) -> Vec<ConfiguredModelView> {
    if config.configured_models.is_empty() {
        let legacy = ConfiguredModel {
            id: stable_model_id(&config.model.provider, &config.model.name),
            display_name: display_name_from_model(&config.model.name),
            provider: config.model.provider.clone(),
            model: config.model.name.clone(),
            api_protocol: config.model.api_protocol,
            enabled: true,
            max_tokens: config.model.max_tokens,
            temperature: config.model.temperature,
            context_window: config.model.context_window,
            thinking: config.model.thinking.clone(),
        };
        let runtime = resolve_runtime_model(config, None);
        return vec![ConfiguredModelView {
            id: legacy.id.clone(),
            display_name: legacy.display_name.clone(),
            provider: legacy.provider.clone(),
            model: legacy.model.clone(),
            api_protocol: runtime.api_protocol.into(),
            enabled: true,
            is_default: true,
            has_auth_token: runtime.auth_token.is_some(),
            auth_source: runtime.auth_source,
            base_url: runtime.base_url,
            temperature: legacy.temperature,
            max_tokens: legacy.max_tokens,
            context_window: Some(effective_context_window(&legacy)),
        }];
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

pub fn upsert_configured_model(config: &mut AppConfig, mut model: ConfiguredModel) -> String {
    if model.id.trim().is_empty() {
        model.id = stable_model_id(&model.provider, &model.model);
    }
    if model.display_name.trim().is_empty() {
        model.display_name = display_name_from_model(&model.model);
    }
    if model.provider.trim().is_empty() {
        model.provider = "custom".to_string();
    }
    if !model.enabled {
        model.enabled = true;
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
    id
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
    config.model.api_protocol = model.api_protocol;
    config.model.temperature = model.temperature;
    config.model.max_tokens = model.max_tokens;
    config.model.context_window = model.context_window;
    config.model.thinking = model.thinking.clone();

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
        temperature,
        max_tokens,
        context_window,
        thinking,
    ) = if let Some(selected) = selected {
        (
            selected.id.clone(),
            selected.display_name.clone(),
            selected.provider.clone(),
            selected.model.clone(),
            selected.api_protocol,
            selected.temperature,
            selected.max_tokens,
            Some(effective_context_window(selected)),
            selected.thinking.clone(),
        )
    } else {
        (
            fallback_id,
            display_name_from_model(&config.model.name),
            config.model.provider.clone(),
            config.model.name.clone(),
            config.model.api_protocol,
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
            config.model.thinking.clone(),
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
    } else if let Some(token) = find_env_api_key(&provider) {
        (Some(token), "env".to_string())
    } else {
        (None, "none".to_string())
    };

    let configured_base_url = provider_config
        .and_then(|p| p.base_url.clone())
        .or_else(|| {
            (provider == config.model.provider)
                .then(|| config.model.base_url.clone())
                .flatten()
        })
        .filter(|url| !url.trim().is_empty());
    let base_url = configured_base_url
        .clone()
        .or_else(|| default_base_url(&provider));

    let api_protocol_explicit = api_protocol.is_some();
    let api_protocol = api_protocol.unwrap_or_else(|| {
        configured_base_url
            .as_deref()
            .and_then(LlmApiProtocol::try_from_endpoint)
            .or_else(|| provider_metadata(&provider).map(|metadata| metadata.default_api_protocol))
            .unwrap_or(LlmApiProtocol::ChatCompletions)
    });
    ModelRuntimeConfig {
        id,
        display_name,
        provider,
        model,
        api_protocol,
        api_protocol_explicit,
        auth_token,
        auth_source,
        base_url,
        temperature,
        max_tokens,
        context_window,
        thinking,
    }
}

/// Resolve a CLI/TUI model selector without splitting model identity from its
/// provider credentials, endpoint, and protocol.
pub fn resolve_runtime_model_selector(
    config: &AppConfig,
    selector: Option<&str>,
) -> Result<ModelRuntimeConfig, ModelSelectionError> {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(resolve_runtime_model(
            config,
            config.model.default_model_id.as_deref(),
        ));
    };

    let legacy_id = stable_model_id(&config.model.provider, &config.model.name);
    if config.configured_models.is_empty()
        && (selector == config.model.name || selector == legacy_id)
    {
        return Ok(resolve_runtime_model(config, None));
    }

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

/// Validate the final application-level endpoint/protocol pair before an agent
/// or connectivity probe is allowed to use it. Auto mode may retain a provider
/// root that cannot identify a protocol; in that case provider metadata remains
/// authoritative. Explicit protocols require a recognized complete endpoint.
pub fn validate_runtime_model_endpoint(runtime: &ModelRuntimeConfig) -> Result<(), String> {
    let endpoint = runtime
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Provider '{}' requires a model endpoint", runtime.provider))?;
    let Some(endpoint_protocol) = LlmApiProtocol::try_from_endpoint(endpoint) else {
        if runtime.api_protocol_explicit {
            return Err(format!(
                "Explicit protocol '{:?}' requires a complete endpoint ending in /responses, /messages, or /chat/completions (got '{endpoint}')",
                runtime.api_protocol
            ));
        }
        return Ok(());
    };
    if endpoint_protocol != runtime.api_protocol {
        return Err(format!(
            "Configured protocol '{:?}' does not match model endpoint '{endpoint}' ({endpoint_protocol:?})",
            runtime.api_protocol
        ));
    }
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
    if runtime_requires_api_key(runtime) && !has_token {
        let env_vars = env_vars_display(&runtime.provider);
        return Err(format!(
            "Provider '{}' requires an API key for endpoint '{}'. Configure auth_token or one of: {}",
            runtime.provider,
            runtime.base_url.as_deref().unwrap_or_default(),
            if env_vars.is_empty() {
                "the provider API key environment variable"
            } else {
                env_vars.as_str()
            }
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
        assert!(provider_requires_api_key("openai"));
        assert!(provider_requires_api_key("ANTHROPIC"));
        assert!(provider_requires_api_key("qwen"));
        assert_eq!(
            provider_env_vars("qwen"),
            &["DASHSCOPE_API_KEY", "QWEN_API_KEY"]
        );
        assert!(!provider_requires_api_key("ollama"));
        assert!(!provider_requires_api_key("custom-local-provider"));
        assert_eq!(
            provider_templates()
                .iter()
                .find(|template| template.id == "custom")
                .map(|template| template.requires_api_key),
            Some(false)
        );
    }

    #[test]
    fn hosted_builtin_requires_key_but_compatible_override_does_not() {
        let mut config = AppConfig {
            configured_models: vec![ConfiguredModel {
                id: "openai:model".to_string(),
                provider: "openai".to_string(),
                model: "model".to_string(),
                enabled: true,
                ..ConfiguredModel::default()
            }],
            ..AppConfig::default()
        };
        let mut hosted = resolve_runtime_model(&config, Some("openai:model"));
        hosted.auth_token = None;
        assert!(runtime_requires_api_key(&hosted));

        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/responses".to_string()),
            },
        );
        let compatible = resolve_runtime_model(&config, Some("openai:model"));
        assert!(!runtime_requires_api_key(&compatible));
        assert!(validate_runtime_model_requirements(&compatible).is_ok());
        assert!(validate_runtime_model_requirements(&hosted).is_err());
    }

    #[test]
    fn gui_provider_templates_project_framework_protocol_defaults() {
        let templates = provider_templates();
        for (provider, expected) in [
            ("openai", LlmApiProtocol::Responses),
            ("anthropic", LlmApiProtocol::Anthropic),
            ("deepseek", LlmApiProtocol::ChatCompletions),
            ("dashscope", LlmApiProtocol::ChatCompletions),
            ("moonshot", LlmApiProtocol::ChatCompletions),
            ("zhipu", LlmApiProtocol::ChatCompletions),
            ("custom", LlmApiProtocol::ChatCompletions),
        ] {
            assert_eq!(
                templates
                    .iter()
                    .find(|template| template.id == provider)
                    .map(|template| template.default_api_protocol),
                Some(expected.into()),
                "unexpected template protocol for {provider}"
            );
        }
    }

    #[test]
    fn omitted_protocol_uses_framework_provider_metadata() {
        for (provider, expected) in [
            ("openai", LlmApiProtocol::Responses),
            ("anthropic", LlmApiProtocol::Anthropic),
            ("deepseek", LlmApiProtocol::ChatCompletions),
            ("qwen", LlmApiProtocol::ChatCompletions),
            ("custom", LlmApiProtocol::ChatCompletions),
        ] {
            let mut config = AppConfig::default();
            let model_id = stable_model_id(provider, "model");
            config.configured_models = vec![ConfiguredModel {
                id: model_id.clone(),
                provider: provider.to_string(),
                model: "model".to_string(),
                api_protocol: None,
                ..ConfiguredModel::default()
            }];

            assert_eq!(
                resolve_runtime_model(&config, Some(&model_id)).api_protocol,
                expected,
                "unexpected runtime protocol for {provider}"
            );
        }
    }

    #[test]
    fn custom_endpoint_is_inferred_unless_protocol_is_explicit() -> Result<(), String> {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "custom".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1/responses".to_string()),
            },
        );
        config.configured_models = vec![ConfiguredModel {
            id: "custom:inferred".to_string(),
            provider: "custom".to_string(),
            model: "inferred".to_string(),
            api_protocol: None,
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
        configured.api_protocol = Some(LlmApiProtocol::Anthropic);
        let explicit = resolve_runtime_model(&config, Some("custom:inferred"));
        assert_eq!(explicit.api_protocol, LlmApiProtocol::Anthropic);
        assert!(explicit.api_protocol_explicit);
        assert!(validate_runtime_model_endpoint(&explicit).is_err());
        Ok(())
    }

    #[test]
    fn auto_provider_root_uses_framework_metadata_without_false_rejection() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1".to_string()),
            },
        );
        config.configured_models = vec![ConfiguredModel {
            id: "openai:root-only".to_string(),
            provider: "openai".to_string(),
            model: "root-only".to_string(),
            enabled: true,
            ..ConfiguredModel::default()
        }];

        let runtime = resolve_runtime_model(&config, Some("openai:root-only"));
        assert_eq!(runtime.api_protocol, LlmApiProtocol::Responses);
        assert!(!runtime.api_protocol_explicit);
        assert!(validate_runtime_model_endpoint(&runtime).is_ok());

        let configured = config.configured_models.first_mut();
        if let Some(configured) = configured {
            configured.api_protocol = Some(LlmApiProtocol::Responses);
        }
        let explicit = resolve_runtime_model(&config, Some("openai:root-only"));
        assert!(explicit.api_protocol_explicit);
        assert!(validate_runtime_model_endpoint(&explicit).is_err());
    }

    #[test]
    fn complete_endpoint_overrides_provider_metadata_and_must_match_explicit_protocol() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("https://gateway.example/v1/chat/completions".to_string()),
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
        assert!(!inferred.api_protocol_explicit);
        assert!(validate_runtime_model_endpoint(&inferred).is_ok());

        let configured = config.configured_models.first_mut();
        if let Some(configured) = configured {
            configured.api_protocol = Some(LlmApiProtocol::Responses);
        }
        let mismatched = resolve_runtime_model(&config, Some("openai:compatible"));
        assert_eq!(mismatched.api_protocol, LlmApiProtocol::Responses);
        assert!(mismatched.api_protocol_explicit);
        assert!(validate_runtime_model_endpoint(&mismatched).is_err());
    }

    #[test]
    fn selector_resolves_one_identity_and_types_unknown_disabled_and_ambiguous_errors() {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: Some("openai-key".to_string()),
                base_url: Some("https://api.openai.com/v1/responses".to_string()),
            },
        );
        config.model_providers.insert(
            "anthropic".to_string(),
            echo_agent::config::ModelProviderConfig {
                auth_token: Some("anthropic-key".to_string()),
                base_url: Some("https://api.anthropic.com/v1/messages".to_string()),
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
    fn yaml_protocol_omission_override_and_endpoint_inference_remain_distinct() -> Result<(), String>
    {
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
            LlmApiProtocol::Responses
        );
        assert_eq!(
            resolve_runtime_model(&config, Some("anthropic:chat-override")).api_protocol,
            LlmApiProtocol::ChatCompletions
        );
        assert_eq!(
            resolve_runtime_model(&config, Some("custom:inferred")).api_protocol,
            LlmApiProtocol::Responses
        );
        Ok(())
    }

    #[test]
    fn legacy_model_is_projected_as_the_default_configured_model() -> Result<(), String> {
        let mut config = AppConfig::default();
        config.model.provider = "deepseek".to_string();
        config.model.name = "deepseek-v4-flash".to_string();
        config.model.context_window = Some(128_000);

        let views = configured_model_views(&config);
        let view = views
            .first()
            .ok_or_else(|| "legacy model view was not created".to_string())?;
        assert_eq!(views.len(), 1);
        assert_eq!(view.model, "deepseek-v4-flash");
        assert!(view.is_default);
        assert_eq!(view.context_window, Some(128_000));
        Ok(())
    }
}
