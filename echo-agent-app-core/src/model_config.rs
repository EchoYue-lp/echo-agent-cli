use echo_agent::config::{AppConfig, ConfiguredModel};
use echo_agent::llm::config::{all_provider_metadata, provider_base_url, provider_env_var_names};
use echo_agent::llm::core::capabilities::infer_context_window;
use serde::Serialize;

const DEFAULT_CONTEXT_WINDOW: u32 = 396_000;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderTemplate {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_env: String,
    pub default_models: Vec<String>,
    pub requires_api_key: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfiguredModelView {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    pub is_default: bool,
    pub has_auth_token: bool,
    pub auth_source: String,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ModelRuntimeConfig {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
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
        })
        .chain(std::iter::once(ProviderTemplate {
            id: "custom".to_string(),
            name: "自定义".to_string(),
            base_url: String::new(),
            api_key_env: String::new(),
            default_models: Vec::new(),
            requires_api_key: true,
        }))
        .collect()
}

pub fn configured_model_views(config: &mut AppConfig) -> Vec<ConfiguredModelView> {
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
    model
        .context_window
        .or_else(|| infer_context_window(&model.provider, &model.model))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
        .clamp(1, 10_000_000)
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
    config.model.temperature = model.temperature;
    config.model.max_tokens = model.max_tokens;
    config.model.context_window = model.context_window;
    config.model.thinking = model.thinking.clone();

    if let Some(provider_config) = config.model_providers.get(&model.provider) {
        config.model.auth_token = provider_config.auth_token.clone();
        config.model.base_url = provider_config.base_url.clone();
    }

    Ok(resolve_runtime_model(config, Some(model_id)))
}

pub fn delete_configured_model(config: &mut AppConfig, model_id: &str) -> Result<(), String> {
    let before = config.configured_models.len();
    config
        .configured_models
        .retain(|model| model.id != model_id);
    if config.configured_models.len() == before {
        return Err(format!("Model '{model_id}' is not configured"));
    }
    if config.model.default_model_id.as_deref() == Some(model_id) {
        if let Some(next) = config
            .configured_models
            .iter()
            .find(|model| model.enabled)
            .cloned()
        {
            let _ = set_default_model(config, &next.id)?;
        } else {
            config.model.default_model_id = None;
        }
    }
    Ok(())
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

    let (id, display_name, provider, model, temperature, max_tokens, context_window) =
        if let Some(selected) = selected {
            (
                selected.id.clone(),
                selected.display_name.clone(),
                selected.provider.clone(),
                selected.model.clone(),
                selected.temperature,
                selected.max_tokens,
                selected.context_window,
            )
        } else {
            (
                fallback_id,
                display_name_from_model(&config.model.name),
                config.model.provider.clone(),
                config.model.name.clone(),
                config.model.temperature,
                config.model.max_tokens,
                config.model.context_window,
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

    let base_url = provider_config
        .and_then(|p| p.base_url.clone())
        .or_else(|| {
            (provider == config.model.provider)
                .then(|| config.model.base_url.clone())
                .flatten()
        })
        .or_else(|| default_base_url(&provider));

    ModelRuntimeConfig {
        id,
        display_name,
        provider,
        model,
        auth_token,
        auth_source,
        base_url,
        temperature,
        max_tokens,
        context_window,
        // Forward the configured thinking spec (e.g. "high", "4000") if any.
        thinking: config.model.thinking.clone(),
    }
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
}
