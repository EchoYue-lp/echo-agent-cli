//! Tauri IPC commands for model provider management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::config::{ConfiguredModel, ModelProviderConfig};
use echo_agent::llm::LlmApiProtocol;
use echo_agent::prelude::Message;
use echo_agent_app_core::AppState;
use echo_agent_app_core::infra::prepare_runtime_llm;
use echo_agent_app_core::model_config::{self, ModelRuntimeConfig};
use echo_agent_app_core::state::{ConfiguredModelMutation, ModelMutationError};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct UpsertConfiguredModelRequest {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub provider: String,
    pub model: String,
    pub api_protocol: Option<LlmApiProtocol>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub enabled: Option<bool>,
    pub set_default: Option<bool>,
    /// 思考深度(`auto`/`disabled`/`minimal`/`low`/`medium`/`high`/`<number>`)。
    pub thinking: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedAuth {
    token: String,
    source: &'static str,
}

struct ConnectionProbe {
    runtime: ModelRuntimeConfig,
    auth: ResolvedAuth,
}

fn resolve_auth_token(
    user_key: Option<&str>,
    provider_config: Option<&ModelProviderConfig>,
    provider: &str,
) -> ResolvedAuth {
    if let Some(token) = user_key.map(str::trim).filter(|token| !token.is_empty()) {
        return ResolvedAuth {
            token: token.to_string(),
            source: "input",
        };
    }

    if let Some(token) = provider_config
        .and_then(|config| config.auth_token.as_deref())
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return ResolvedAuth {
            token: token.to_string(),
            source: "config",
        };
    }

    if let Some(token) = model_config::find_env_api_key(provider) {
        return ResolvedAuth {
            token,
            source: "env",
        };
    }

    ResolvedAuth {
        token: String::new(),
        source: "none",
    }
}

fn configured_model_mutation(req: UpsertConfiguredModelRequest) -> ConfiguredModelMutation {
    ConfiguredModelMutation {
        model: ConfiguredModel {
            id: req.id.unwrap_or_default(),
            display_name: req.display_name.unwrap_or_default(),
            provider: req.provider,
            model: req.model,
            api_protocol: req.api_protocol,
            enabled: req.enabled.unwrap_or(true),
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            context_window: req.context_window,
            thinking: req.thinking,
        },
        auth_token: req.api_key,
        base_url: req.base_url,
        set_default: req.set_default.unwrap_or(false),
    }
}

fn model_mutation_ipc_error(error: ModelMutationError) -> IpcError {
    match error {
        ModelMutationError::Validation(message) => IpcError::Validation(message),
        other => IpcError::Internal(other.to_string()),
    }
}

fn resolve_connection_probe(
    app_config: &echo_agent::config::AppConfig,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    api_protocol: Option<LlmApiProtocol>,
) -> ConnectionProbe {
    let provider_config = app_config.model_providers.get(&provider);
    let auth = resolve_auth_token(api_key.as_deref(), provider_config, &provider);
    let base_url = base_url
        .filter(|url| !url.trim().is_empty())
        .or_else(|| provider_config.and_then(|config| config.base_url.clone()))
        .or_else(|| model_config::default_base_url(&provider));
    let mut probe_config = app_config.clone();
    probe_config.model_providers.insert(
        provider.clone(),
        ModelProviderConfig {
            auth_token: (!auth.token.is_empty()).then(|| auth.token.clone()),
            base_url,
        },
    );
    let probe_id = format!("__connection_test__:{provider}:{model}");
    probe_config.configured_models = vec![ConfiguredModel {
        id: probe_id.clone(),
        display_name: "Connection test".to_string(),
        provider,
        model,
        api_protocol,
        enabled: true,
        ..ConfiguredModel::default()
    }];
    ConnectionProbe {
        runtime: model_config::resolve_runtime_model(&probe_config, Some(&probe_id)),
        auth,
    }
}

#[tauri::command]
pub async fn list_model_templates() -> Result<model_config::ProviderTemplateListResponse, IpcError>
{
    Ok(model_config::ProviderTemplateListResponse {
        providers: model_config::provider_templates(),
    })
}

#[tauri::command]
pub async fn list_configured_models(
    state: tauri::State<'_, TauriState>,
) -> Result<model_config::ConfiguredModelListResponse, IpcError> {
    let cfg = state.app_state.config.app_config.read().await;
    let models = model_config::configured_model_views(&cfg);
    let default_model_id = cfg.model.default_model_id.clone();
    Ok(model_config::ConfiguredModelListResponse {
        models,
        default_model_id,
    })
}

#[tauri::command]
pub async fn upsert_configured_model(
    state: tauri::State<'_, TauriState>,
    req: UpsertConfiguredModelRequest,
) -> Result<serde_json::Value, IpcError> {
    upsert_configured_model_inner(&state.app_state, req).await
}

async fn upsert_configured_model_inner(
    app_state: &Arc<AppState>,
    req: UpsertConfiguredModelRequest,
) -> Result<serde_json::Value, IpcError> {
    let mutation = app_state
        .upsert_configured_model_owned(configured_model_mutation(req))
        .await
        .map_err(model_mutation_ipc_error)?;
    let runtime = mutation.runtime.ok_or_else(|| {
        IpcError::Internal("configured model mutation lost its runtime receipt".to_string())
    })?;

    Ok(serde_json::json!({
        "success": true,
        "model_id": mutation.model_id,
        "auth_source": runtime.auth_source,
    }))
}

#[tauri::command]
pub async fn delete_configured_model(
    state: tauri::State<'_, TauriState>,
    model_id: String,
) -> Result<serde_json::Value, IpcError> {
    state
        .app_state
        .delete_configured_model_owned(model_id)
        .await
        .map_err(model_mutation_ipc_error)?;
    Ok(serde_json::json!({ "success": true }))
}

#[tauri::command]
pub async fn set_default_model(
    state: tauri::State<'_, TauriState>,
    model_id: String,
) -> Result<serde_json::Value, IpcError> {
    set_default_model_inner(&state.app_state, model_id).await
}

async fn set_default_model_inner(
    app_state: &Arc<AppState>,
    model_id: String,
) -> Result<serde_json::Value, IpcError> {
    let mutation = app_state
        .set_default_model_owned(model_id)
        .await
        .map_err(model_mutation_ipc_error)?;
    let runtime = mutation.runtime.ok_or_else(|| {
        IpcError::Internal("default model mutation lost its runtime receipt".to_string())
    })?;
    Ok(serde_json::json!({
        "success": true,
        "model_id": runtime.id,
        "display_name": runtime.display_name,
        "model": runtime.model,
        "provider": runtime.provider,
    }))
}

/// Query whether a given (provider, model) supports a thinking-depth control,
/// and which protocol it speaks. The frontend uses this to show/hide the
/// "思考深度" dropdown next to the model selector.
///
/// Returns:
/// - `supports`: bool — whether a thinking field would be honored (not None /
///   not AnthropicAdaptive, since adaptive models silently ignore it).
/// - `protocol`: one of "none" | "openai_reasoning_effort" |
///   "anthropic_thinking_budget" | "anthropic_adaptive" | "enable_thinking_flag".
/// - `levels`: list of user-facing level strings valid for this protocol.
#[tauri::command]
pub async fn get_thinking_support(
    provider: String,
    model: String,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent::llm::{ModelProfile, ProviderCapabilities, ThinkingProtocol};
    let caps = ProviderCapabilities::from_provider_name(&provider);
    let profile = ModelProfile::new(&model, &provider, caps);
    let protocol = match profile.thinking_protocol {
        ThinkingProtocol::None => "none",
        ThinkingProtocol::OpenaiReasoningEffort => "openai_reasoning_effort",
        ThinkingProtocol::AnthropicEffort => "anthropic_effort",
        ThinkingProtocol::AnthropicThinkingBudget => "anthropic_thinking_budget",
        ThinkingProtocol::AnthropicAdaptive => "anthropic_adaptive",
        ThinkingProtocol::EnableThinkingFlag => "enable_thinking_flag",
        ThinkingProtocol::GlmThinkingType => "glm_thinking_type",
        ThinkingProtocol::GlmReasoningEffort => "glm_reasoning_effort",
    };
    let supports = profile.thinking_protocol.emits_field();
    // Levels offered by the UI dropdown. Adaptive/None show no levels.
    let levels: Vec<&str> = if supports {
        vec!["auto", "minimal", "low", "medium", "high"]
    } else {
        vec!["auto"]
    };
    Ok(serde_json::json!({
        "supports": supports,
        "protocol": protocol,
        "levels": levels,
        "model": model,
        "provider": provider,
    }))
}

/// Dynamically set the thinking-depth for the active agent at runtime.
///
/// Unlike the model-config `thinking` field, this is a per-session toggle the
/// user changes from the chat input toolbar (next to "审批模式"/"模型管理"),
/// independent of which model is configured. Every model offers the dropdown;
/// the spec is translated to a `ThinkingConfig` and applied via
/// `agent.set_thinking()`. Models that don't support a thinking protocol
/// silently ignore it (the framework already warns in that case), so the
/// control is always safe to expose.
///
/// `spec` accepts: `"auto"`/`""` (reset to model default), `"disabled"`,
/// `"minimal"`/`"low"`/`"medium"`/`"high"`, or a bare number (token budget).
/// Invalid specs return an error so the UI can surface a typo.
#[tauri::command]
pub async fn set_thinking(
    state: tauri::State<'_, crate::tauri::TauriState>,
    spec: String,
) -> Result<serde_json::Value, IpcError> {
    let cfg = match echo_agent::llm::ThinkingConfig::parse_spec(&spec) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Err(IpcError::Validation(format!(
                "invalid thinking spec '{spec}': {e}"
            )));
        }
    };
    let applied = cfg.is_some();

    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move {
                agent.set_thinking(cfg);
            })
        })
        .await;

    Ok(serde_json::json!({
        "success": true,
        "spec": spec,
        "applied": applied,
    }))
}

#[tauri::command]
pub async fn test_connection(
    state: tauri::State<'_, TauriState>,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    api_protocol: Option<LlmApiProtocol>,
) -> Result<serde_json::Value, IpcError> {
    test_connection_inner(
        &state.app_state,
        provider,
        model,
        api_key,
        base_url,
        api_protocol,
    )
    .await
}

async fn test_connection_inner(
    app_state: &AppState,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    api_protocol: Option<LlmApiProtocol>,
) -> Result<serde_json::Value, IpcError> {
    let app_config = app_state.config.app_config.read().await.clone();
    let probe = resolve_connection_probe(
        &app_config,
        provider,
        model,
        api_key,
        base_url,
        api_protocol,
    );
    if model_config::runtime_requires_api_key(&probe.runtime) && probe.auth.token.is_empty() {
        return Ok(serde_json::json!({
            "success": false,
            "error": "没有可用的 API Key。请填写 API Key，或设置对应环境变量后重试。",
            "auth_source": probe.auth.source,
            "has_auth_token": false,
        }));
    }
    let prepared = prepare_runtime_llm(&probe.runtime).map_err(IpcError::Validation)?;
    let messages = vec![Message::user("Hi, respond with just 'OK'.".to_string())];
    match prepared.client.chat_simple(messages).await {
        Ok(response) => Ok(serde_json::json!({
            "success": true,
            "response": response,
            "model": prepared.client.model_name(),
            "auth_source": probe.auth.source,
            "has_auth_token": !probe.auth.token.is_empty(),
        })),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "error": format!("API call failed: {e}"),
            "auth_source": probe.auth.source,
            "has_auth_token": !probe.auth.token.is_empty(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_connection_probe;
    use echo_agent::config::{AppConfig, ModelProviderConfig};
    use echo_agent::llm::LlmApiProtocol;
    use echo_agent_app_core::infra::prepare_runtime_llm;

    #[test]
    fn connection_probe_uses_real_client_preflight() -> Result<(), String> {
        let mut config = AppConfig::default();
        config.model_providers.insert(
            "openai".to_string(),
            ModelProviderConfig {
                auth_token: Some("invalid\nauthorization".to_string()),
                base_url: Some("https://api.openai.com/v1/responses".to_string()),
            },
        );
        let probe = resolve_connection_probe(
            &config,
            "openai".to_string(),
            "gpt-test".to_string(),
            Some("invalid\nauthorization".to_string()),
            Some("https://api.openai.com/v1/responses".to_string()),
            Some(LlmApiProtocol::Responses),
        );
        let probe_error = prepare_runtime_llm(&probe.runtime)
            .err()
            .ok_or_else(|| "invalid connection probe unexpectedly passed preflight".to_string())?;
        assert!(probe_error.contains("header") || probe_error.contains("Header"));
        Ok(())
    }
}
