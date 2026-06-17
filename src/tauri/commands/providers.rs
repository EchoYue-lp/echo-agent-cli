//! Tauri IPC commands for model provider management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::config::{ConfiguredModel, ModelProviderConfig};
use echo_agent::prelude::Message;
use echo_agent_app_core::infra::build_llm_config;
use echo_agent_app_core::model_config;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct UpsertConfiguredModelRequest {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub enabled: Option<bool>,
    pub set_default: Option<bool>,
}

#[derive(Debug, Clone)]
struct ResolvedAuth {
    token: String,
    source: &'static str,
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

async fn apply_runtime_model(
    state: &tauri::State<'_, TauriState>,
    runtime: echo_agent_app_core::model_config::ModelRuntimeConfig,
) {
    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let runtime = runtime.clone();
            Box::pin(async move {
                if let Some(ref token) = runtime.auth_token {
                    let config = build_llm_config(
                        &runtime.provider,
                        token,
                        &runtime.model,
                        runtime.base_url.as_deref(),
                    );
                    agent.set_llm_config(config);
                } else {
                    agent.set_model(&runtime.model);
                }
                agent.set_temperature(runtime.temperature);
                agent.set_max_tokens(runtime.max_tokens);
                // Apply context_window: if set, use it as token_limit so the
                // agent gets the right budget/compression behavior. If not
                // set, leave token_limit unchanged (framework infers).
                if let Some(cw) = runtime.context_window {
                    agent.set_token_limit(cw as usize);
                }
                tracing::info!(
                    provider = %runtime.provider,
                    model = %runtime.model,
                    display_name = %runtime.display_name,
                    auth_source = %runtime.auth_source,
                    "模型配置已应用到当前 Agent"
                );
            })
        })
        .await;

    if let Some(pool) = state.app_state.connection.pool.as_ref() {
        let app_config = state.app_state.config.app_config.read().await.clone();
        pool.update_app_config(app_config).await;
        pool.apply_runtime_model(runtime).await;
    }
}

#[tauri::command]
pub async fn list_model_templates() -> Result<serde_json::Value, IpcError> {
    Ok(serde_json::json!({
        "providers": model_config::provider_templates(),
    }))
}

#[tauri::command]
pub async fn list_configured_models(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mut cfg = state.app_state.config.app_config.write().await;
    let models = model_config::configured_model_views(&mut cfg);
    let default_model_id = cfg.model.default_model_id.clone();
    Ok(serde_json::json!({
        "models": models,
        "default_model_id": default_model_id,
    }))
}

#[tauri::command]
pub async fn upsert_configured_model(
    state: tauri::State<'_, TauriState>,
    req: UpsertConfiguredModelRequest,
) -> Result<serde_json::Value, IpcError> {
    let model_id;
    let runtime;
    {
        let mut cfg = state.app_state.config.app_config.write().await;
        let provider_config = cfg
            .model_providers
            .entry(req.provider.clone())
            .or_insert_with(ModelProviderConfig::default);
        if let Some(key) = req
            .api_key
            .as_ref()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
        {
            provider_config.auth_token = Some(key);
        }
        if let Some(url) = req
            .base_url
            .as_ref()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
        {
            provider_config.base_url = Some(url);
        }

        let configured = ConfiguredModel {
            id: req.id.unwrap_or_default(),
            display_name: req.display_name.unwrap_or_default(),
            provider: req.provider,
            model: req.model,
            enabled: req.enabled.unwrap_or(true),
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            context_window: req.context_window,
        };
        model_id = model_config::upsert_configured_model(&mut cfg, configured);
        if req.set_default.unwrap_or(false) {
            runtime = model_config::set_default_model(&mut cfg, &model_id)
                .map_err(IpcError::Validation)?;
        } else {
            runtime = model_config::resolve_runtime_model(&cfg, Some(&model_id));
        }
        if let Err(e) = echo_agent::config::save_config(&cfg) {
            tracing::warn!("Failed to persist configured model: {e}");
        }
    }

    if req.set_default.unwrap_or(false) {
        apply_runtime_model(&state, runtime.clone()).await;
    }

    Ok(serde_json::json!({
        "success": true,
        "model_id": model_id,
        "auth_source": runtime.auth_source,
    }))
}

#[tauri::command]
pub async fn delete_configured_model(
    state: tauri::State<'_, TauriState>,
    model_id: String,
) -> Result<serde_json::Value, IpcError> {
    {
        let mut cfg = state.app_state.config.app_config.write().await;
        model_config::delete_configured_model(&mut cfg, &model_id).map_err(IpcError::Validation)?;
        if let Err(e) = echo_agent::config::save_config(&cfg) {
            tracing::warn!("Failed to persist configured model deletion: {e}");
        }
    }
    Ok(serde_json::json!({ "success": true }))
}

#[tauri::command]
pub async fn set_default_model(
    state: tauri::State<'_, TauriState>,
    model_id: String,
) -> Result<serde_json::Value, IpcError> {
    let runtime;
    {
        let mut cfg = state.app_state.config.app_config.write().await;
        runtime =
            model_config::set_default_model(&mut cfg, &model_id).map_err(IpcError::Validation)?;
        if let Err(e) = echo_agent::config::save_config(&cfg) {
            tracing::warn!("Failed to persist default model: {e}");
        }
    }

    apply_runtime_model(&state, runtime.clone()).await;
    Ok(serde_json::json!({
        "success": true,
        "model_id": runtime.id,
        "display_name": runtime.display_name,
        "model": runtime.model,
        "provider": runtime.provider,
    }))
}

#[tauri::command]
pub async fn test_connection(
    state: tauri::State<'_, TauriState>,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let app_config = state.app_state.config.app_config.read().await;
    let provider_config = app_config.model_providers.get(&provider);
    let auth = resolve_auth_token(api_key.as_deref(), provider_config, &provider);
    let base_url = base_url
        .filter(|url| !url.trim().is_empty())
        .or_else(|| provider_config.and_then(|config| config.base_url.clone()))
        .or_else(|| model_config::default_base_url(&provider));
    let requires_api_key = model_config::provider_templates()
        .iter()
        .find(|template| template.id == provider)
        .map(|template| template.requires_api_key)
        .unwrap_or(true);
    if requires_api_key && auth.token.is_empty() {
        return Ok(serde_json::json!({
            "success": false,
            "error": "没有可用的 API Key。请填写 API Key，或设置对应环境变量后重试。",
            "auth_source": auth.source,
            "has_auth_token": false,
        }));
    }

    let config = build_llm_config(&provider, &auth.token, &model, base_url.as_deref());

    match config.build_client() {
        Ok(client) => {
            let messages = vec![Message::user("Hi, respond with just 'OK'.".to_string())];
            match client.chat_simple(messages).await {
                Ok(response) => Ok(serde_json::json!({
                    "success": true,
                    "response": response,
                    "model": client.model_name(),
                    "auth_source": auth.source,
                    "has_auth_token": !auth.token.is_empty(),
                })),
                Err(e) => Ok(serde_json::json!({
                    "success": false,
                    "error": format!("API call failed: {e}"),
                    "auth_source": auth.source,
                    "has_auth_token": !auth.token.is_empty(),
                })),
            }
        }
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "error": format!("Failed to create client: {e}"),
            "auth_source": auth.source,
            "has_auth_token": !auth.token.is_empty(),
        })),
    }
}
