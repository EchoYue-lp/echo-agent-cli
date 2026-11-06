//! Eclip-owned product configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use echo_agent::config::AgentSettings;
use echo_agent::llm::{LlmApiProtocol, ModelInputModality};
use echo_agent::skills::hooks::HooksDefinition;
use serde::{Deserialize, Serialize};

pub const DEFAULT_EKO_SYSTEM_PROMPT: &str = r#"You are Eclip, a local personal AI assistant running on the user's machine.

- Establish facts from available files, configuration, logs, tests, and tool output before making claims.
- Use tools when they can verify, inspect, execute, or make concrete progress.
- Preserve user work and avoid changing unrelated files.
- Prefer root-cause fixes and validate changes with the relevant checks.
- For broad tasks, decompose the work and use Subagents when independent investigation helps.
- Be concise without hiding important uncertainty."#;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EkoConfig {
    pub model: ModelSelectionConfig,
    pub model_providers: BTreeMap<String, ModelProviderConfig>,
    pub configured_models: Vec<ConfiguredModel>,
    #[serde(
        default = "default_eko_agent_settings",
        deserialize_with = "deserialize_eko_agent_settings",
        serialize_with = "serialize_eko_agent_settings"
    )]
    pub agent: AgentSettings,
    /// Captured by each new run/turn; changing config never rewrites active work.
    pub max_subagent_depth: crate::subagent_policy::SubagentDepthLimit,
    pub mcp: McpYamlConfig,
    pub channels: ChannelsConfig,
    pub webhooks: WebhooksConfig,
    pub hooks: HooksDefinition,
    pub server: ServerConfig,
    pub logging: LoggingConfig,
}

impl Default for EkoConfig {
    fn default() -> Self {
        Self {
            model: ModelSelectionConfig::default(),
            model_providers: BTreeMap::new(),
            configured_models: Vec::new(),
            agent: default_eko_agent_settings(),
            max_subagent_depth: crate::subagent_policy::SubagentDepthLimit::default(),
            mcp: McpYamlConfig::default(),
            channels: ChannelsConfig::default(),
            webhooks: WebhooksConfig::default(),
            hooks: HooksDefinition::default(),
            server: ServerConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

fn default_eko_agent_settings() -> AgentSettings {
    AgentSettings {
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
        compress_window: 0, // Automatic token tail; positive values use legacy message caps.
        subagent_timeout_secs: 600,
        ..AgentSettings::default()
    }
}

fn deserialize_eko_agent_settings<'de, D>(deserializer: D) -> Result<AgentSettings, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let patch = serde_json::Value::deserialize(deserializer)?;
    if let Some(fields) = patch.as_object() {
        for removed in ["enable_tools", "enable_memory", "enable_human_in_loop"] {
            if fields.contains_key(removed) {
                return Err(serde::de::Error::custom(format!(
                    "agent.{removed} is not configurable in Eclip"
                )));
            }
        }
    }
    let mut merged =
        serde_json::to_value(default_eko_agent_settings()).map_err(serde::de::Error::custom)?;
    merge_config_value(&mut merged, patch);
    serde_json::from_value(merged).map_err(serde::de::Error::custom)
}

fn serialize_eko_agent_settings<S>(
    settings: &AgentSettings,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut value = serde_json::to_value(settings).map_err(serde::ser::Error::custom)?;
    if let Some(fields) = value.as_object_mut() {
        for removed in ["enable_tools", "enable_memory", "enable_human_in_loop"] {
            fields.remove(removed);
        }
    }
    value.serialize(serializer)
}

fn merge_config_value(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(target) => merge_config_value(target, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

impl EkoConfig {
    fn framework_config(&self) -> echo_agent::config::FrameworkConfig {
        let model = self
            .model
            .default_model_id
            .as_deref()
            .and_then(|id| {
                self.configured_models
                    .iter()
                    .find(|model| model.enabled && model.id == id)
            })
            .or_else(|| self.configured_models.iter().find(|model| model.enabled));
        echo_agent::config::FrameworkConfig {
            model: model.map_or_else(echo_agent::config::ModelConfig::default, |model| {
                echo_agent::config::ModelConfig {
                    provider: model.provider.clone(),
                    name: model.model.clone(),
                    auth_token: None,
                    base_url: None,
                    api_protocol: Some(model.api_protocol),
                    max_tokens: model.max_tokens,
                    temperature: model.temperature,
                    context_window: model.context_window,
                    // 当前EKO配置没有额外来源事实；保留上述直接配置，不伪造provider事实。
                    facts: Default::default(),
                }
            }),
            agent: self.agent.clone(),
        }
    }

    pub fn has_compressor(&self) -> bool {
        self.framework_config().has_compressor()
    }

    pub async fn apply_compressor(&self, agent: &echo_agent::agent::ReactAgent) {
        self.framework_config().apply_compressor(agent).await;
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSelectionConfig {
    pub default_model_id: Option<String>,
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
    pub weixin: WeixinChannelConfig,
    pub session: SessionYamlConfig,
}

/// Eclip's local account policy. Protocol types stay optional with `channels`,
/// so ordinary config loading does not activate a transport dependency.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WeixinChannelConfig {
    pub enabled: bool,
    pub account: Option<WeixinAccountConfig>,
    pub notifications: std::collections::HashMap<String, WeixinNotificationConfig>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WeixinAccountConfig {
    pub account_id: String,
    pub bot_token: String,
    pub base_url: String,
    pub user_id: String,
    /// Persisted authorization identity; a process restart does not rotate it.
    pub generation: String,
}

impl std::fmt::Debug for WeixinAccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeixinAccountConfig")
            .field("account_id", &self.account_id)
            .field("bot_token", &"[redacted]")
            .field("base_url", &"[redacted]")
            .field("user_id", &self.user_id)
            .field("generation", &self.generation)
            .finish()
    }
}

/// A user-selected destination for future occurrences. The original run
/// admission freezes this value; later configuration cannot retarget that run.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WeixinNotificationConfig {
    pub definition_id: String,
    pub account_id: String,
    pub credential_generation: String,
    pub peer_id: String,
    pub context_ref: String,
    pub workspace_id: String,
    pub channel_conversation: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct QqChannelConfig {
    pub enabled: bool,
    pub app_id: String,
    pub client_secret: String,
}

#[derive(Clone, Deserialize, Serialize)]
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

impl std::fmt::Debug for QqChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QqChannelConfig")
            .field("enabled", &self.enabled)
            .field("app_id", &self.app_id)
            .field("client_secret", &"[redacted]")
            .finish()
    }
}
impl std::fmt::Debug for FeishuChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuChannelConfig")
            .field("enabled", &self.enabled)
            .field("app_id", &self.app_id)
            .field("app_secret", &"[redacted]")
            .field("mode", &self.mode)
            .field("webhook_bind", &self.webhook_bind)
            .field("webhook_path", &self.webhook_path)
            .field(
                "webhook_verification_token",
                &self
                    .webhook_verification_token
                    .as_ref()
                    .map(|_| "[redacted]"),
            )
            .finish()
    }
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
    let content = format!("# Eclip Configuration\n\n{yaml}");
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
        .ok_or_else(|| "no Eclip configuration path is available".to_string())?;
    save_config_file(target, config)
}

pub fn load_config(explicit_path: Option<&str>) -> Result<EkoConfig, String> {
    let selected = explicit_path.map(PathBuf::from).or_else(|| {
        std::env::var("EKO_CONFIG")
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
    });
    if let Some(path) = selected {
        return load_config_file(&path);
    }
    for path in config_search_paths() {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return load_config_file(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Failed to inspect config file {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(EkoConfig::default())
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
    fn explicit_config_depth_errors_are_not_replaced_by_a_default()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("eko.yaml");
        let selected = path.to_str().ok_or("fixture config path is not UTF-8")?;
        assert!(load_config(Some(selected)).is_err());
        std::fs::write(&path, "max_subagent_depth: 4\n")?;
        assert!(load_config(Some(selected)).is_err());
        std::fs::write(&path, "max_subagent_depth: 3\n")?;
        assert_eq!(load_config(Some(selected))?.max_subagent_depth.get(), 3);
        std::fs::write(&path, "{}\n")?;
        assert_eq!(load_config(Some(selected))?.max_subagent_depth.get(), 1);
        Ok(())
    }

    #[test]
    fn eko_defaults_are_owned_by_the_application() {
        let config = EkoConfig::default();
        assert!(config.agent.system_prompt.contains("Eclip"));
        assert!(config.agent.enable_tools);
        assert!(config.agent.enable_memory);
        assert_eq!(config.server.host, "127.0.0.1");
    }

    #[test]
    fn top_level_model_rejects_runtime_mirror_fields() {
        let parsed = serde_yaml::from_str::<EkoConfig>(
            "model:\n  default_model_id: gateway:main\n  provider: gateway\n",
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn partial_agent_settings_keep_eko_product_defaults() -> Result<(), String> {
        let parsed = serde_yaml::from_str::<EkoConfig>("agent:\n  name: custom\n")
            .map_err(|error| error.to_string())?;
        assert_eq!(parsed.agent.name, "custom");
        assert!(parsed.agent.enable_tools);
        assert!(parsed.agent.enable_memory);
        assert_eq!(parsed.agent.system_prompt, DEFAULT_EKO_SYSTEM_PROMPT);
        Ok(())
    }

    #[test]
    fn eko_rejects_removed_capability_switches() {
        for field in ["enable_tools", "enable_memory", "enable_human_in_loop"] {
            let yaml = format!("agent:\n  {field}: false\n");
            assert!(serde_yaml::from_str::<EkoConfig>(&yaml).is_err());
        }
    }

    #[test]
    fn eko_does_not_serialize_removed_capability_switches() -> Result<(), String> {
        let yaml =
            serde_yaml::to_string(&EkoConfig::default()).map_err(|error| error.to_string())?;
        for field in ["enable_tools", "enable_memory", "enable_human_in_loop"] {
            assert!(!yaml.contains(field));
        }
        Ok(())
    }
}
