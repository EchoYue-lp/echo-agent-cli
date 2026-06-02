//! Tauri IPC commands for model provider management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent::llm::LlmConfig;
use echo_agent::prelude::Message;

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
                "models": ["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo", "o1", "o1-mini", "o3-mini"],
                "base_url": "https://api.openai.com/v1/chat/completions",
                "api_key_env": "OPENAI_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("OPENAI_API_KEY").is_ok(),
            },
            {
                "id": "anthropic",
                "name": "Anthropic",
                "icon": "🟠",
                "models": ["claude-sonnet-4-20250514", "claude-3-5-haiku-20241022", "claude-3-opus-20240229"],
                "base_url": "https://api.anthropic.com/v1/messages",
                "api_key_env": "ANTHROPIC_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("ANTHROPIC_API_KEY").is_ok(),
            },
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "icon": "🔵",
                "models": ["deepseek-chat", "deepseek-reasoner"],
                "base_url": "https://api.deepseek.com/chat/completions",
                "api_key_env": "DEEPSEEK_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("DEEPSEEK_API_KEY").is_ok(),
            },
            {
                "id": "dashscope",
                "name": "通义千问",
                "icon": "🟣",
                "models": ["qwen-max", "qwen-plus", "qwen-turbo", "qwen-long"],
                "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
                "api_key_env": "DASHSCOPE_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("DASHSCOPE_API_KEY").is_ok(),
            },
            {
                "id": "moonshot",
                "name": "Moonshot",
                "icon": "🌙",
                "models": ["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"],
                "base_url": "https://api.moonshot.cn/v1/chat/completions",
                "api_key_env": "MOONSHOT_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("MOONSHOT_API_KEY").is_ok(),
            },
            {
                "id": "zhipu",
                "name": "智谱",
                "icon": "🔷",
                "models": ["glm-4-plus", "glm-4", "glm-4-flash"],
                "base_url": "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "api_key_env": "ZHIPU_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("ZHIPU_API_KEY").is_ok(),
            },
            {
                "id": "gemini",
                "name": "Gemini",
                "icon": "💎",
                "models": ["gemini-2.0-flash", "gemini-2.0-flash-lite", "gemini-1.5-pro"],
                "base_url": "https://generativelanguage.googleapis.com/v1beta/openai/",
                "api_key_env": "GEMINI_API_KEY",
                "requires_api_key": true,
                "configured": std::env::var("GEMINI_API_KEY").is_ok(),
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

fn resolve_api_key(user_key: &Option<String>, provider: &str) -> String {
    if let Some(key) = user_key
        && !key.is_empty()
    {
        return key.clone();
    }
    let env_var = match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "dashscope" => "DASHSCOPE_API_KEY",
        "moonshot" => "MOONSHOT_API_KEY",
        "zhipu" => "ZHIPU_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => return String::new(),
    };
    std::env::var(env_var).unwrap_or_default()
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
