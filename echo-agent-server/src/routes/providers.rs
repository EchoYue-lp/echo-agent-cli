//! 模型供应商管理 API
//!
//! 提供供应商信息、模型列表和连接测试。

use axum::{
    Json, Router, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::Arc;

use echo_agent_app_core::state::AppState;

// ── Provider definitions ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub models: Vec<String>,
    pub api_key_env: String,
    pub base_url: String,
    pub requires_api_key: bool,
}

fn get_providers() -> Vec<ProviderInfo> {
    vec![
        ProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            icon: "🤖".into(),
            models: vec![
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
                "gpt-4-turbo".into(),
                "o1".into(),
                "o3-mini".into(),
            ],
            api_key_env: "OPENAI_API_KEY".into(),
            base_url: "https://api.openai.com/v1/chat/completions".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            icon: "🧠".into(),
            models: vec![
                "claude-sonnet-4-20250514".into(),
                "claude-opus-4-20250514".into(),
                "claude-haiku-3-5".into(),
            ],
            api_key_env: "ANTHROPIC_API_KEY".into(),
            base_url: "https://api.anthropic.com/v1/messages".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            icon: "🔍".into(),
            models: vec!["deepseek-chat".into(), "deepseek-reasoner".into()],
            api_key_env: "DEEPSEEK_API_KEY".into(),
            base_url: "https://api.deepseek.com/chat/completions".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "dashscope".into(),
            name: "通义千问 (Qwen)".into(),
            icon: "🌐".into(),
            models: vec![
                "qwen-max".into(),
                "qwen-plus".into(),
                "qwen-turbo".into(),
                "qwen3-max".into(),
            ],
            api_key_env: "DASHSCOPE_API_KEY".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "moonshot".into(),
            name: "Moonshot (Kimi)".into(),
            icon: "🌙".into(),
            models: vec!["moonshot-v1-128k".into(), "kimi-latest".into()],
            api_key_env: "MOONSHOT_API_KEY".into(),
            base_url: "https://api.moonshot.cn/v1/chat/completions".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "zhipu".into(),
            name: "智谱 (GLM)".into(),
            icon: "💎".into(),
            models: vec!["glm-4".into(), "glm-4-flash".into(), "glm-4-plus".into()],
            api_key_env: "ZHIPU_API_KEY".into(),
            base_url: "https://open.bigmodel.cn/api/paas/v4/chat/completions".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "gemini".into(),
            name: "Google Gemini".into(),
            icon: "✨".into(),
            models: vec!["gemini-2.0-flash".into(), "gemini-2.5-pro".into()],
            api_key_env: "GEMINI_API_KEY".into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai/".into(),
            requires_api_key: true,
        },
        ProviderInfo {
            id: "ollama".into(),
            name: "Ollama (本地)".into(),
            icon: "🏠".into(),
            models: vec![
                "llama3".into(),
                "qwen2.5".into(),
                "deepseek-r1".into(),
                "mistral".into(),
            ],
            api_key_env: String::new(),
            base_url: "http://localhost:11434/api/chat".into(),
            requires_api_key: false,
        },
    ]
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

/// GET /api/providers — 列出所有供应商及配置状态
#[cfg_attr(debug_assertions, debug_handler)]
async fn list_providers(State(_state): State<Arc<AppState>>) -> Response {
    let providers = get_providers();

    let providers_with_status: Vec<serde_json::Value> = providers
        .iter()
        .map(|p| {
            let has_key = if p.requires_api_key {
                !p.api_key_env.is_empty() && std::env::var(&p.api_key_env).is_ok()
            } else {
                true // Ollama doesn't need a key
            };
            let mut val = serde_json::to_value(p).unwrap_or_default();
            if let Some(obj) = val.as_object_mut() {
                obj.insert("configured".to_string(), serde_json::Value::Bool(has_key));
            }
            val
        })
        .collect();

    // Also include the current model name
    let current_model = _state
        .connection
        .agent
        .read(|a| a.config().get_model_name().to_string())
        .await;

    Json(serde_json::json!({
        "providers": providers_with_status,
        "current_model": current_model,
    }))
    .into_response()
}

/// Resolve API key: use provided key, or fall back to environment variable for the provider.
fn resolve_api_key(provided: &Option<String>, provider: &str) -> String {
    // If user provided a non-empty key, use it
    if let Some(key) = provided {
        if !key.trim().is_empty() {
            return key.trim().to_string();
        }
    }
    // Fall back to environment variable
    let env_vars: &[&str] = match provider {
        "openai" => &["OPENAI_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "dashscope" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "moonshot" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "zhipu" => &["ZHIPU_API_KEY", "GLM_API_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "ollama" => &[],
        _ => &["OPENAI_API_KEY"],
    };
    for var in env_vars {
        if let Ok(key) = std::env::var(var) {
            if !key.trim().is_empty() {
                return key;
            }
        }
    }
    // Return empty string if nothing found
    provided.clone().unwrap_or_default()
}

/// POST /api/providers/test — 测试供应商连接
#[derive(Debug, Deserialize)]
pub struct TestConnectionRequest {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[cfg_attr(debug_assertions, debug_handler)]
async fn test_connection(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<TestConnectionRequest>,
) -> Response {
    use echo_agent::llm::LlmConfig;
    #[allow(unused_imports)]
    use echo_agent::llm::LlmClient;
    use echo_agent::prelude::Message;

    let api_key = resolve_api_key(&req.api_key, &req.provider);
    let config = match req.provider.as_str() {
        "openai" => LlmConfig::openai(api_key, req.model),
        "anthropic" => LlmConfig::anthropic(api_key, req.model),
        "deepseek" => LlmConfig::deepseek(api_key, req.model),
        "dashscope" => LlmConfig::dashscope(api_key, req.model),
        "gemini" => LlmConfig::gemini(api_key, req.model),
        "ollama" => LlmConfig::ollama(req.model),
        _ => {
            // For moonshot, zhipu, and other OpenAI-compatible providers
            let base_url = req
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".into());
            LlmConfig::new(base_url, api_key, req.model)
        }
    };

    // Override base_url if explicitly provided
    let config = if let Some(ref url) = req.base_url {
        if req.provider != "ollama" {
            LlmConfig {
                base_url: url.clone(),
                ..config
            }
        } else {
            config
        }
    } else {
        config
    };

    match config.build_client() {
        Ok(client) => {
            let messages = vec![Message::user("Hi, respond with just 'OK'.".to_string())];
            match client.chat_simple(messages).await {
                Ok(response) => Json(serde_json::json!({
                    "success": true,
                    "response": response,
                    "model": client.model_name(),
                }))
                .into_response(),
                Err(e) => Json(serde_json::json!({
                    "success": false,
                    "error": format!("API call failed: {e}"),
                }))
                .into_response(),
            }
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("Failed to create client: {e}"),
        }))
        .into_response(),
    }
}

/// POST /api/providers/switch — 切换模型
#[derive(Debug, Deserialize)]
pub struct SwitchModelRequest {
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub provider: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

#[cfg_attr(debug_assertions, debug_handler)]
async fn switch_model(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwitchModelRequest>,
) -> Response {
    use echo_agent::llm::LlmConfig;

    let provider = req.provider.as_deref().unwrap_or("openai");

    // Resolve api_key: user-provided → env var → empty
    let api_key = resolve_api_key(&req.api_key, provider);

    // Resolve base_url: user-provided → provider default
    let base_url = req.base_url.clone().unwrap_or_else(|| {
        match provider {
            "openai" => "https://api.openai.com/v1/chat/completions".into(),
            "anthropic" => "https://api.anthropic.com/v1/messages".into(),
            "deepseek" => "https://api.deepseek.com/chat/completions".into(),
            "dashscope" => "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".into(),
            "moonshot" => "https://api.moonshot.cn/v1/chat/completions".into(),
            "zhipu" => "https://open.bigmodel.cn/api/paas/v4/chat/completions".into(),
            "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai/".into(),
            "ollama" => "http://localhost:11434/api/chat".into(),
            _ => "https://api.openai.com/v1/chat/completions".into(),
        }
    });

    let has_credentials = !api_key.is_empty() || provider == "ollama";

    if has_credentials {
        let config = match provider {
            "anthropic" => LlmConfig::anthropic(api_key, req.model.clone()),
            "deepseek" => LlmConfig::deepseek(api_key, req.model.clone()),
            "dashscope" => LlmConfig::dashscope(api_key, req.model.clone()),
            "gemini" => LlmConfig::gemini(api_key, req.model.clone()),
            "ollama" => LlmConfig::ollama(req.model.clone()),
            _ => LlmConfig::new(base_url.clone(), api_key, req.model.clone()),
        };

        // Override base_url if explicitly provided (for OpenAI-compatible providers)
        let config = if req.base_url.is_some() && provider != "ollama" && provider != "anthropic" && provider != "gemini" {
            LlmConfig {
                base_url: base_url.clone(),
                ..config
            }
        } else {
            config
        };

        state
            .connection
            .agent
            .write(|a| {
                a.set_llm_config(config);
                // Apply optional temperature and max_tokens
                if let Some(temp) = req.temperature {
                    a.set_temperature(Some(temp as f32));
                }
                if let Some(max_tok) = req.max_tokens {
                    a.set_max_tokens(Some(max_tok));
                }
            })
            .await;

        tracing::info!(model = %req.model, provider = %provider, "Model switched via provider panel");

        Json(serde_json::json!({
            "success": true,
            "model": req.model,
            "message": "Model and provider configuration updated",
        }))
        .into_response()
    } else {
        // No credentials available — just update model name
        let temp = req.temperature;
        let max_tok = req.max_tokens;

        state
            .connection
            .agent
            .write(|a| {
                a.set_model(&req.model);
                // Apply optional temperature and max_tokens
                if let Some(t) = temp {
                    a.set_temperature(Some(t as f32));
                }
                if let Some(m) = max_tok {
                    a.set_max_tokens(Some(m));
                }
            })
            .await;

        tracing::info!(model = %req.model, "Model name updated");

        Json(serde_json::json!({
            "success": true,
            "model": req.model,
            "message": "Model name updated",
        }))
        .into_response()
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn provider_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/providers", get(list_providers))
        .route("/api/providers/test", post(test_connection))
        .route("/api/providers/switch", post(switch_model))
}
