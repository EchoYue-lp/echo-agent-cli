//! EKO-owned product configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use echo_agent::llm::{LlmApiProtocol, ModelInputModality};
use echo_agent::skills::hooks::HooksDefinition;
use serde::{Deserialize, Serialize};

pub const DEFAULT_EKO_SYSTEM_PROMPT: &str = r#"You are EKO, a local personal AI assistant running on the user's machine.

- Establish facts from available files, configuration, logs, tests, and tool output before making claims.
- Use tools when they can verify, inspect, execute, or make concrete progress.
- Preserve user work and avoid changing unrelated files.
- Prefer root-cause fixes and validate changes with the relevant checks.
- For broad tasks, decompose the work and use Subagents when independent investigation helps.
- Be concise without hiding important uncertainty."#;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct EkoConfig {
    pub model: ModelConfig,
    pub model_providers: BTreeMap<String, ModelProviderConfig>,
    pub configured_models: Vec<ConfiguredModel>,
    pub agent: AgentYamlConfig,
    pub mcp: McpYamlConfig,
    pub channels: ChannelsConfig,
    pub webhooks: WebhooksConfig,
    pub hooks: HooksDefinition,
    pub server: ServerConfig,
    pub logging: LoggingConfig,
    pub tui: TuiConfig,
}

impl EkoConfig {
    fn framework_config(&self) -> echo_agent::config::FrameworkConfig {
        echo_agent::config::FrameworkConfig {
            model: echo_agent::config::ModelConfig {
                provider: self.model.provider.clone(),
                name: self.model.name.clone(),
                auth_token: self.model.auth_token.clone(),
                base_url: self.model.base_url.clone(),
                api_protocol: self.model.api_protocol,
                max_tokens: self.model.max_tokens,
                temperature: self.model.temperature,
                context_window: self.model.context_window,
            },
            agent: echo_agent::config::AgentYamlConfig {
                name: self.agent.name.clone(),
                system_prompt: self.agent.system_prompt.clone(),
                max_iterations: self.agent.max_iterations,
                enable_tools: self.agent.enable_tools,
                enable_memory: self.agent.enable_memory,
                enable_human_in_loop: self.agent.enable_human_in_loop,
                memory_path: self.agent.memory_path.clone(),
                tool_timeout_ms: self.agent.tool_timeout_ms,
                max_tool_output_tokens: self.agent.max_tool_output_tokens,
                token_limit: self.agent.token_limit,
                compress_strategy: self.agent.compress_strategy.clone(),
                compress_window: self.agent.compress_window,
                subagent_timeout_secs: self.agent.subagent_timeout_secs,
            },
        }
    }

    pub fn to_agent_config(&self) -> echo_agent::agent::AgentConfig {
        self.framework_config().to_agent_config()
    }

    pub fn has_compressor(&self) -> bool {
        self.framework_config().has_compressor()
    }

    pub async fn apply_compressor(&self, agent: &echo_agent::agent::ReactAgent) {
        self.framework_config().apply_compressor(agent).await;
    }

    pub fn resolve_llm_config(
        &self,
        selector: Option<&str>,
    ) -> echo_agent::error::Result<echo_agent::llm::LlmConfig> {
        let selected = match selector.map(str::trim).filter(|value| !value.is_empty()) {
            Some(selector) => {
                let mut matches = self.configured_models.iter().filter(|model| {
                    model.enabled && (model.id == selector || model.model == selector)
                });
                let selected = matches.next().ok_or_else(|| {
                    echo_agent::error::ConfigError::NotFindModelError(selector.to_string())
                })?;
                if matches.next().is_some() && selected.id != selector {
                    return Err(echo_agent::error::ConfigError::ConfigFileError(format!(
                        "model name '{selector}' is ambiguous; use a configured model id"
                    ))
                    .into());
                }
                selected
            }
            None => self
                .model
                .default_model_id
                .as_deref()
                .and_then(|id| {
                    self.configured_models
                        .iter()
                        .find(|model| model.enabled && model.id == id)
                })
                .or_else(|| self.configured_models.iter().find(|model| model.enabled))
                .ok_or_else(|| {
                    echo_agent::error::ConfigError::NotFindModelError(
                        "no enabled configured model".to_string(),
                    )
                })?,
        };
        let provider = self
            .model_providers
            .get(&selected.provider)
            .ok_or_else(|| {
                echo_agent::error::ConfigError::ConfigFileError(format!(
                    "model '{}' references missing provider '{}'",
                    selected.id, selected.provider
                ))
            })?;
        let base_url = provider
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                echo_agent::error::ConfigError::MissingConfig(
                    selected.provider.clone(),
                    "base_url".to_string(),
                )
            })?;
        let api_key = provider
            .auth_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                provider
                    .api_key_env
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .and_then(|name| std::env::var(name).ok())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default();
        if provider.requires_api_key && api_key.is_empty() {
            return Err(echo_agent::error::ConfigError::MissingConfig(
                selected.provider.clone(),
                provider
                    .api_key_env
                    .clone()
                    .unwrap_or_else(|| "auth_token".to_string()),
            )
            .into());
        }

        Ok(echo_agent::llm::LlmConfig::for_provider(
            &selected.provider,
            base_url,
            api_key,
            &selected.model,
            selected.api_protocol,
        )?
        .with_input_modalities(selected.input_modalities.clone()))
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelConfig {
    pub default_model_id: Option<String>,
    pub provider: String,
    pub name: String,
    pub auth_token: Option<String>,
    pub base_url: Option<String>,
    pub api_protocol: Option<LlmApiProtocol>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub context_window: Option<u32>,
}

impl ModelConfig {
    pub fn get_auth_token(&self) -> Option<String> {
        self.auth_token.clone().filter(|value| !value.is_empty())
    }

    pub fn get_base_url(&self) -> Option<String> {
        self.base_url.clone().filter(|value| !value.is_empty())
    }

    pub fn get_model_name(&self) -> String {
        self.name.clone()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelProviderConfig {
    pub name: String,
    pub auth_token: Option<String>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub default_api_protocol: Option<LlmApiProtocol>,
    pub requires_api_key: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ConfiguredModel {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub api_protocol: LlmApiProtocol,
    pub input_modalities: Vec<ModelInputModality>,
    pub enabled: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub context_window: Option<u32>,
}

impl Default for ConfiguredModel {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            provider: String::new(),
            model: String::new(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            input_modalities: ModelInputModality::text_only(),
            enabled: true,
            max_tokens: None,
            temperature: None,
            context_window: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentYamlConfig {
    pub name: String,
    pub system_prompt: String,
    pub max_iterations: usize,
    pub enable_tools: bool,
    pub enable_memory: bool,
    pub enable_human_in_loop: bool,
    pub memory_path: String,
    pub tool_timeout_ms: u64,
    pub max_tool_output_tokens: usize,
    pub token_limit: usize,
    pub compress_strategy: String,
    pub compress_window: usize,
    pub subagent_timeout_secs: u64,
}

impl Default for AgentYamlConfig {
    fn default() -> Self {
        Self {
            name: "eko".to_string(),
            system_prompt: DEFAULT_EKO_SYSTEM_PROMPT.to_string(),
            max_iterations: 10,
            enable_tools: true,
            enable_memory: true,
            enable_human_in_loop: true,
            memory_path: crate::data_root::user_data_path("store.json")
                .to_string_lossy()
                .into_owned(),
            tool_timeout_ms: 120_000,
            max_tool_output_tokens: 0,
            token_limit: 0,
            compress_strategy: "summary".to_string(),
            compress_window: 20,
            subagent_timeout_secs: 600,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct McpYamlConfig {
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ChannelsConfig {
    pub qq: QqChannelConfig,
    pub feishu: FeishuChannelConfig,
    pub session: SessionYamlConfig,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct QqChannelConfig {
    pub enabled: bool,
    pub app_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FeishuChannelConfig {
    pub enabled: bool,
    pub app_id: String,
    pub app_secret: String,
    pub mode: String,
    pub webhook_bind: String,
    pub webhook_path: String,
    pub webhook_verification_token: Option<String>,
}

impl Default for FeishuChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            mode: "long_poll".to_string(),
            webhook_bind: "127.0.0.1:9000".to_string(),
            webhook_path: "/webhook/feishu".to_string(),
            webhook_verification_token: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SessionYamlConfig {
    pub timeout_minutes: u64,
    pub reset_keywords: Vec<String>,
    pub reset_commands: Vec<String>,
}

impl Default for SessionYamlConfig {
    fn default() -> Self {
        Self {
            timeout_minutes: 60,
            reset_keywords: vec![
                "reset conversation".to_string(),
                "new conversation".to_string(),
            ],
            reset_commands: vec![
                "/reset".to_string(),
                "/clear".to_string(),
                "/new".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_body_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TuiConfig {
    pub max_display_chars: usize,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            max_display_chars: 20_000,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WebhooksConfig {
    pub endpoints: Vec<WebhookEntryConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebhookEntryConfig {
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

pub fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(explicit) = std::env::var("EKO_CONFIG")
        && !explicit.trim().is_empty()
    {
        paths.push(PathBuf::from(explicit));
    }
    paths.push(PathBuf::from("eko.yaml"));
    paths.push(crate::data_root::user_data_path("config.yaml"));
    paths
}

pub fn load_config_file(path: &Path) -> Result<EkoConfig, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("Failed to read config file: {error}"))?;
    serde_yaml::from_str(&content).map_err(|error| format!("Failed to parse config file: {error}"))
}

pub fn save_config_file(path: &Path, config: &EkoConfig) -> Result<(), String> {
    let yaml = serde_yaml::to_string(config)
        .map_err(|error| format!("configuration serialization failed: {error}"))?;
    let content = format!("# EKO Configuration\n\n{yaml}");
    echo_agent::utils::fs::atomic_write(path, content.as_bytes())
        .map_err(|error| format!("configuration write failed: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("set configuration permissions failed: {error}"))?;
    }
    Ok(())
}

pub fn save_config(config: &EkoConfig) -> Result<(), String> {
    let search = config_search_paths();
    let target = search
        .iter()
        .find(|path| path.exists())
        .or_else(|| search.get(1))
        .or_else(|| search.first())
        .ok_or_else(|| "no EKO configuration path is available".to_string())?;
    save_config_file(target, config)
}

pub fn load_config(explicit_path: Option<&str>) -> EkoConfig {
    if let Some(path) = explicit_path.map(PathBuf::from) {
        return load_config_file(&path).unwrap_or_else(|error| {
            tracing::error!(path = %path.display(), %error, "failed to load EKO config");
            EkoConfig::default()
        });
    }
    for path in config_search_paths() {
        if path.exists() {
            match load_config_file(&path) {
                Ok(config) => return config,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "invalid EKO config");
                }
            }
        }
    }
    EkoConfig::default()
}

pub fn apply_env_overrides(config: &mut EkoConfig) {
    if let Ok(value) = std::env::var("QQ_APP_ID") {
        config.channels.qq.app_id = value;
        config.channels.qq.enabled = !config.channels.qq.app_id.is_empty();
    }
    if let Ok(value) = std::env::var("QQ_CLIENT_SECRET") {
        config.channels.qq.client_secret = value;
    }
    if let Ok(value) = std::env::var("FEISHU_APP_ID") {
        config.channels.feishu.app_id = value;
        config.channels.feishu.enabled = !config.channels.feishu.app_id.is_empty();
    }
    if let Ok(value) = std::env::var("FEISHU_APP_SECRET") {
        config.channels.feishu.app_secret = value;
    }
    if let Ok(value) = std::env::var("MCP_CONFIG_PATH") {
        config.mcp.config_path = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eko_defaults_are_owned_by_the_application() {
        let config = EkoConfig::default();
        assert!(config.agent.system_prompt.contains("EKO"));
        assert!(config.agent.enable_tools);
        assert!(config.agent.enable_memory);
        assert_eq!(config.server.host, "127.0.0.1");
    }
}
