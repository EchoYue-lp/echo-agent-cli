//! Tauri IPC commands for model provider management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::llm::LlmConfig;
use echo_agent::prelude::Message;

/// All accepted environment variable names for each provider.
///
/// Priority order: first match wins. This mirrors
/// `ProviderFactory::env_api_key()` in echo-integration/src/providers/config.rs
/// but extends to cover alternative names users commonly set.
fn provider_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "openai" => &["OPENAI_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "dashscope" | "qwen" | "aliyun" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "moonshot" | "kimi" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "zhipu" | "glm" => &["ZHIPU_API_KEY", "GLM_API_KEY"],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        _ => &[],
    }
}

/// Check if any accepted env var for a provider is set and non-empty.
/// Returns the first match found.
fn find_env_api_key(provider: &str) -> Option<String> {
    for var in provider_env_vars(provider) {
        if let Ok(val) = std::env::var(var)
            && !val.is_empty()
        {
            return Some(val);
        }
    }
    None
}

/// Resolve API key: user-filled key takes priority, then fall back to any
/// accepted environment variable for the provider.
fn resolve_api_key(user_key: &Option<String>, provider: &str) -> String {
    // Priority 1: user explicitly filled in a key via GUI
    if let Some(key) = user_key
        && !key.is_empty()
    {
        return key.clone();
    }
    // Priority 2: any accepted env var
    find_env_api_key(provider).unwrap_or_default()
}

/// Format accepted env var names for display (e.g. "DASHSCOPE_API_KEY / QWEN_API_KEY").
fn env_vars_display(provider: &str) -> String {
    provider_env_vars(provider).join(" / ")
}

#[tauri::command]
pub async fn list_providers(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let current_model = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.config().get_model_name().to_string())
        .await;

    Ok(serde_json::json!({
        "providers": [
            {
                "id": "openai",
                "name": "OpenAI",
                "icon": "🟢",
                "models": ["gpt-5.5"],
                "base_url": "https://api.openai.com/v1/chat/completions",
                "api_key_env": env_vars_display("openai"),
                "requires_api_key": true,
                "configured": find_env_api_key("openai").is_some(),
            },
            {
                "id": "anthropic",
                "name": "Anthropic",
                "icon": "🟠",
                "models": ["claude-opus-4-8", "claude-opus-4-7"],
                "base_url": "https://api.anthropic.com/v1/messages",
                "api_key_env": env_vars_display("anthropic"),
                "requires_api_key": true,
                "configured": find_env_api_key("anthropic").is_some(),
            },
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "icon": "🔵",
                "models": ["deepseek-v4-flash", "deepseek-v4-pro"],
                "base_url": "https://api.deepseek.com/chat/completions",
                "api_key_env": env_vars_display("deepseek"),
                "requires_api_key": true,
                "configured": find_env_api_key("deepseek").is_some(),
            },
            {
                "id": "dashscope",
                "name": "通义千问",
                "icon": "🟣",
                "models": ["qwen3.7-max", "qwen3.6-plus"],
                "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
                "api_key_env": env_vars_display("dashscope"),
                "requires_api_key": true,
                "configured": find_env_api_key("dashscope").is_some(),
            },
            {
                "id": "moonshot",
                "name": "Moonshot",
                "icon": "🌙",
                "models": ["kimi-k2.6"],
                "base_url": "https://api.moonshot.cn/v1/chat/completions",
                "api_key_env": env_vars_display("moonshot"),
                "requires_api_key": true,
                "configured": find_env_api_key("moonshot").is_some(),
            },
            {
                "id": "zhipu",
                "name": "智谱",
                "icon": "🔷",
                "models": ["glm-5.1"],
                "base_url": "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "api_key_env": env_vars_display("zhipu"),
                "requires_api_key": true,
                "configured": find_env_api_key("zhipu").is_some(),
            },
            {
                "id": "gemini",
                "name": "Gemini",
                "icon": "💎",
                "models": ["gemini-3.5-flash"],
                "base_url": "https://generativelanguage.googleapis.com/v1beta/openai/",
                "api_key_env": env_vars_display("gemini"),
                "requires_api_key": true,
                "configured": find_env_api_key("gemini").is_some(),
            },
            {
                "id": "ollama",
                "name": "Ollama",
                "icon": "🦙",
                "models": ["llama3.1", "qwen2.5", "deepseek-r1", "codellama", "mistral"],
                "base_url": "http://localhost:11434/api/chat",
                "api_key_env": "",
                "requires_api_key": false,
                "configured": true,
            },
        ],
        "current_model": current_model,
    }))
}

#[tauri::command]
pub async fn test_connection(
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let key = resolve_api_key(&api_key, &provider);

    let config = match provider.as_str() {
        "openai" => LlmConfig::openai(key, model.clone()),
        "anthropic" => LlmConfig::anthropic(key, model.clone()),
        "deepseek" => LlmConfig::deepseek(key, model.clone()),
        "dashscope" => LlmConfig::dashscope(key, model.clone()),
        "gemini" => LlmConfig::gemini(key, model.clone()),
        "ollama" => LlmConfig::ollama(model.clone()),
        _ => {
            let url = base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".into());
            LlmConfig::new(url, key, model.clone())
        }
    };

    let config = if let Some(ref url) = base_url {
        if provider != "ollama" {
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
                Ok(response) => Ok(serde_json::json!({
                    "success": true,
                    "response": response,
                    "model": client.model_name(),
                })),
                Err(e) => Ok(serde_json::json!({
                    "success": false,
                    "error": format!("API call failed: {e}"),
                })),
            }
        }
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "error": format!("Failed to create client: {e}"),
        })),
    }
}

#[tauri::command]
pub async fn switch_model(
    state: tauri::State<'_, TauriState>,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    provider: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<serde_json::Value, IpcError> {
    let prov = provider.as_deref().unwrap_or("openai");
    let key = resolve_api_key(&api_key, prov);
    let url = base_url.clone().unwrap_or_else(|| default_base_url(prov));

    let has_credentials = !key.is_empty() || prov == "ollama";

    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let model = model.clone();
            let key = key.clone();
            let url = url.clone();
            let prov = prov.to_string();
            Box::pin(async move {
                if has_credentials {
                    let config = match prov.as_str() {
                        "anthropic" => LlmConfig::anthropic(key, model.clone()),
                        "deepseek" => LlmConfig::deepseek(key, model.clone()),
                        "dashscope" => LlmConfig::dashscope(key, model.clone()),
                        "gemini" => LlmConfig::gemini(key, model.clone()),
                        "ollama" => LlmConfig::ollama(model.clone()),
                        _ => LlmConfig::new(url, key, model.clone()),
                    };
                    agent.set_llm_config(config);
                } else {
                    agent.set_model(&model);
                }
                agent.set_temperature(temperature);
                agent.set_max_tokens(max_tokens);
            })
        })
        .await;

    Ok(serde_json::json!({
        "success": true,
        "model": model,
        "message": format!("Switched to model '{}'", model),
    }))
}

fn default_base_url(provider: &str) -> String {
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
}
