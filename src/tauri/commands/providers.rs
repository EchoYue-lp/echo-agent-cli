//! Tauri IPC commands for model provider management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::config::{ConfiguredModel, ModelProviderConfig};
use echo_agent::llm::{LlmApiProtocol, ModelInputModality};
use echo_agent_app_core::AppState;
use echo_agent_app_core::infra::test_runtime_llm_connection;
use echo_agent_app_core::model_config::{self, ModelRuntimeConfig};
use echo_agent_app_core::state::{
    ConfiguredModelMutation, ModelMutationError, ModelProviderMutation,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct UpsertConfiguredModelRequest {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub provider: String,
    pub model: String,
    pub api_protocol: LlmApiProtocol,
    pub input_modalities: Option<Vec<ModelInputModality>>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub enabled: Option<bool>,
    pub set_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertModelProviderRequest {
    pub id: String,
    pub name: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub base_url: String,
    pub default_api_protocol: LlmApiProtocol,
    pub requires_api_key: bool,
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

    if let Some(token) = provider_config.and_then(model_config::find_env_api_key) {
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
            input_modalities: req
                .input_modalities
                .unwrap_or_else(ModelInputModality::text_only),
            enabled: req.enabled.unwrap_or(true),
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            context_window: req.context_window,
        },
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
    api_protocol: LlmApiProtocol,
    input_modalities: Vec<ModelInputModality>,
) -> ConnectionProbe {
    let provider_config = app_config.model_providers.get(&provider);
    let auth = resolve_auth_token(api_key.as_deref(), provider_config);
    let base_url = base_url
        .filter(|url| !url.trim().is_empty())
        .or_else(|| provider_config.and_then(|config| config.base_url.clone()));
    let mut probe_config = app_config.clone();
    let mut probe_provider = provider_config.cloned().unwrap_or_default();
    probe_provider.auth_token = (!auth.token.is_empty()).then(|| auth.token.clone());
    probe_provider.base_url = base_url;
    probe_config
        .model_providers
        .insert(provider.clone(), probe_provider);
    let probe_id = format!("__connection_test__:{provider}:{model}");
    probe_config.configured_models = vec![ConfiguredModel {
        id: probe_id.clone(),
        display_name: "Connection test".to_string(),
        provider,
        model,
        api_protocol,
        input_modalities,
        enabled: true,
        ..ConfiguredModel::default()
    }];
    ConnectionProbe {
        runtime: model_config::resolve_runtime_model(&probe_config, Some(&probe_id)),
        auth,
    }
}

#[tauri::command]
pub async fn list_model_providers(
    state: tauri::State<'_, TauriState>,
) -> Result<model_config::ModelProviderListResponse, IpcError> {
    let config = state.app_state.config.app_config.read().await;
    Ok(model_config::ModelProviderListResponse {
        providers: model_config::configured_provider_views(&config),
    })
}

#[tauri::command]
pub async fn upsert_model_provider(
    state: tauri::State<'_, TauriState>,
    req: UpsertModelProviderRequest,
) -> Result<serde_json::Value, IpcError> {
    let preserve_auth_token = req.api_key.is_none();
    let receipt = state
        .app_state
        .upsert_model_provider_owned(ModelProviderMutation {
            id: req.id,
            provider: ModelProviderConfig {
                name: req.name,
                auth_token: req.api_key,
                api_key_env: req.api_key_env,
                base_url: Some(req.base_url),
                default_api_protocol: Some(req.default_api_protocol),
                requires_api_key: req.requires_api_key,
            },
            preserve_auth_token,
        })
        .await
        .map_err(model_mutation_ipc_error)?;
    Ok(serde_json::json!({ "success": true, "provider_id": receipt.model_id }))
}

#[tauri::command]
pub async fn delete_model_provider(
    state: tauri::State<'_, TauriState>,
    provider_id: String,
) -> Result<serde_json::Value, IpcError> {
    state
        .app_state
        .delete_model_provider_owned(provider_id)
        .await
        .map_err(model_mutation_ipc_error)?;
    Ok(serde_json::json!({ "success": true }))
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

/// Dynamically set the thinking-depth for the active agent at runtime.
///
/// This is a per-session control. The requested value must be one of the
/// centrally resolved effective levels for the active runtime model; `auto`
/// always resets to the model default.
#[tauri::command]
pub async fn set_thinking(
    state: tauri::State<'_, crate::tauri::TauriState>,
    spec: String,
) -> Result<serde_json::Value, IpcError> {
    let requested = spec.trim().to_ascii_lowercase();
    let available = {
        let config = state.app_state.config.app_config.read().await;
        let runtime = echo_agent_app_core::model_config::resolve_runtime_model(&config, None);
        echo_agent_app_core::model_config::thinking_level_specs(runtime.thinking_profile)
    };
    if requested != "auto" && !available.iter().any(|level| level == &requested) {
        return Err(IpcError::Validation(format!(
            "thinking level '{requested}' is not available for the active model"
        )));
    }
    let cfg = match echo_agent::llm::ThinkingConfig::parse_spec(&requested) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Err(IpcError::Validation(format!(
                "invalid thinking spec '{requested}': {e}"
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
        "spec": requested,
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
    api_protocol: LlmApiProtocol,
    input_modalities: Option<Vec<ModelInputModality>>,
) -> Result<serde_json::Value, IpcError> {
    test_connection_inner(
        &state.app_state,
        provider,
        model,
        api_key,
        base_url,
        api_protocol,
        input_modalities.unwrap_or_else(ModelInputModality::text_only),
    )
    .await
}

async fn test_connection_inner(
    app_state: &AppState,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    api_protocol: LlmApiProtocol,
    input_modalities: Vec<ModelInputModality>,
) -> Result<serde_json::Value, IpcError> {
    let app_config = app_state.config.app_config.read().await.clone();
    let probe = resolve_connection_probe(
        &app_config,
        provider,
        model,
        api_key,
        base_url,
        api_protocol,
        input_modalities,
    );
    if probe.runtime.requires_api_key && probe.auth.token.is_empty() {
        return Ok(serde_json::json!({
            "success": false,
            "error": "没有可用的 API Key。请填写 API Key，或设置对应环境变量后重试。",
            "auth_source": probe.auth.source,
            "has_auth_token": false,
        }));
    }
    match test_runtime_llm_connection(&probe.runtime).await {
        Ok(result) => Ok(serde_json::json!({
            "success": true,
            "response": result.response,
            "model": result.model,
            "auth_source": probe.auth.source,
            "has_auth_token": !probe.auth.token.is_empty(),
        })),
        Err(error) => Ok(serde_json::json!({
            "success": false,
            "error": error,
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
                ..Default::default()
            },
        );
        let probe = resolve_connection_probe(
            &config,
            "openai".to_string(),
            "gpt-test".to_string(),
            Some("invalid\nauthorization".to_string()),
            Some("https://api.openai.com/v1/responses".to_string()),
            LlmApiProtocol::Responses,
            echo_agent::llm::ModelInputModality::text_only(),
        );
        let probe_error = prepare_runtime_llm(&probe.runtime)
            .err()
            .ok_or_else(|| "invalid connection probe unexpectedly passed preflight".to_string())?;
        assert!(probe_error.contains("header") || probe_error.contains("Header"));
        Ok(())
    }
}
