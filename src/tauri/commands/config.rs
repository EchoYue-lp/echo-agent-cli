//! Tauri IPC commands for configuration management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::{Agent, ReactAgent};
use echo_agent_app_core::api::model_config;
use echo_agent_app_core::api::types::{
    AgentConfigResponse, ChannelsConfigResponse, FeishuConfigResponse, FullConfigResponse,
    LoggingConfigResponse, McpConfigResponse, ModelConfigResponse, QqConfigResponse,
    ServerConfigResponse, SessionConfigResponse, UpdateConfigRequest, UpdateFullConfigRequest,
};

fn configured_model_names(cfg: &echo_agent_app_core::api::config::EkoConfig) -> Vec<String> {
    cfg.configured_models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.display_name.clone())
        .collect()
}

fn full_config_response(cfg: &echo_agent_app_core::api::config::EkoConfig) -> FullConfigResponse {
    let runtime = model_config::resolve_runtime_model(cfg, cfg.model.default_model_id.as_deref());
    let token_limit = echo_agent_app_core::api::infra::effective_token_limit(cfg, Some(&runtime));
    let available_models = configured_model_names(cfg);
    FullConfigResponse {
        model: ModelConfigResponse {
            provider: runtime.provider.clone(),
            name: runtime.model.clone(),
            has_auth_token: runtime.auth_token.is_some(),
            base_url: runtime.base_url.clone(),
            max_tokens: runtime.max_tokens,
            temperature: runtime.temperature,
        },
        agent: AgentConfigResponse {
            model: runtime.model,
            system_prompt: cfg.agent.system_prompt.clone(),
            max_iterations: cfg.agent.max_iterations,
            token_limit,
            enable_memory: cfg.agent.enable_memory,
            enable_human_loop: cfg.agent.enable_human_in_loop,
            session_id: None,
            available_models,
        },
        mcp: McpConfigResponse {
            config_path: cfg.mcp.config_path.clone(),
        },
        channels: ChannelsConfigResponse {
            qq: QqConfigResponse {
                enabled: cfg.channels.qq.enabled,
                app_id: cfg.channels.qq.app_id.clone(),
            },
            feishu: FeishuConfigResponse {
                enabled: cfg.channels.feishu.enabled,
                app_id: cfg.channels.feishu.app_id.clone(),
                mode: cfg.channels.feishu.mode.clone(),
            },
            session: SessionConfigResponse {
                timeout_minutes: cfg.channels.session.timeout_minutes,
                reset_keywords: cfg.channels.session.reset_keywords.clone(),
                reset_commands: cfg.channels.session.reset_commands.clone(),
            },
        },
        server: ServerConfigResponse {
            host: cfg.server.host.clone(),
            port: cfg.server.port,
        },
        logging: LoggingConfigResponse {
            level: cfg.logging.level.clone(),
        },
    }
}

/// 从 agent 读取配置字段构造 AgentConfigResponse (P2-3 去重)。
/// get_config 与 update_config 此前各内联一遍相同构造, 抽出统一函数。
fn build_agent_config_response(
    agent: &ReactAgent,
    available_models: Vec<String>,
) -> AgentConfigResponse {
    AgentConfigResponse {
        model: agent.model_name().to_string(),
        system_prompt: agent.system_prompt().to_string(),
        max_iterations: agent.config().get_max_iterations(),
        token_limit: agent.config().get_token_limit(),
        enable_memory: agent.config().is_memory_enabled(),
        enable_human_loop: agent.config().is_human_in_loop_enabled(),
        session_id: agent.config().get_session_id().map(|s| s.to_string()),
        available_models,
    }
}

#[tauri::command]
pub async fn get_config(
    state: tauri::State<'_, TauriState>,
) -> Result<AgentConfigResponse, IpcError> {
    let available_models = {
        let cfg = state.app_state.config.app_config.read().await;
        configured_model_names(&cfg)
    };
    Ok(state
        .app_state
        .connection
        .agent
        .read(|agent| build_agent_config_response(agent, available_models))
        .await)
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, TauriState>,
    req: UpdateConfigRequest,
) -> Result<AgentConfigResponse, IpcError> {
    if req.model.is_some() {
        return Err(IpcError::Validation(
            "模型切换请使用“模型供应商”里的已配置模型，不再支持 config 旧入口".to_string(),
        ));
    }

    {
        let mut config = state.app_state.config.web_config.write().await;
        if let Some(ref system_prompt) = req.system_prompt {
            config.system_prompt = system_prompt.clone();
        }
        if let Some(token_limit) = req.token_limit {
            config.token_limit = token_limit;
        }
    }

    if let Some(system_prompt) = req.system_prompt.clone() {
        state
            .app_state
            .apply_system_prompt_to_agents(system_prompt)
            .await;
        tracing::info!("系统提示词已更新");
    }

    let available_models = {
        let cfg = state.app_state.config.app_config.read().await;
        configured_model_names(&cfg)
    };
    Ok(state
        .app_state
        .connection
        .agent
        .read(|agent| build_agent_config_response(agent, available_models))
        .await)
}

#[tauri::command]
pub async fn get_full_config(
    state: tauri::State<'_, TauriState>,
) -> Result<FullConfigResponse, IpcError> {
    let cfg = state.app_state.config.app_config.read().await;
    Ok(full_config_response(&cfg))
}

#[tauri::command]
pub async fn update_full_config(
    state: tauri::State<'_, TauriState>,
    req: UpdateFullConfigRequest,
) -> Result<FullConfigResponse, IpcError> {
    let reapply_active_model = req.model.is_some();
    let config = state
        .app_state
        .update_app_config_owned(reapply_active_model, move |cfg| {
            if let Some(m) = req.model {
                cfg.model.max_tokens = m.max_tokens.or(cfg.model.max_tokens);
                cfg.model.temperature = m.temperature.or(cfg.model.temperature);
                let max_tokens = cfg.model.max_tokens;
                let temperature = cfg.model.temperature;
                if let Some(default_id) = cfg.model.default_model_id.clone()
                    && let Some(default_model) = cfg
                        .configured_models
                        .iter_mut()
                        .find(|model| model.id == default_id)
                {
                    default_model.max_tokens = max_tokens;
                    default_model.temperature = temperature;
                }
            }

            if let Some(a) = req.agent {
                if let Some(v) = a.name {
                    cfg.agent.name = v;
                }
                if let Some(v) = a.system_prompt {
                    cfg.agent.system_prompt = v;
                }
                if let Some(v) = a.max_iterations {
                    cfg.agent.max_iterations = v;
                }
                if let Some(v) = a.enable_tools {
                    cfg.agent.enable_tools = v;
                }
                if let Some(v) = a.enable_memory {
                    cfg.agent.enable_memory = v;
                }
                if let Some(v) = a.enable_human_in_loop {
                    cfg.agent.enable_human_in_loop = v;
                }
                if let Some(v) = a.memory_path {
                    cfg.agent.memory_path = v;
                }
            }

            if let Some(m) = req.mcp {
                cfg.mcp.config_path = m.config_path.or(cfg.mcp.config_path.clone());
            }

            if let Some(ch) = req.channels {
                if let Some(qq) = ch.qq {
                    if let Some(v) = qq.enabled {
                        cfg.channels.qq.enabled = v;
                    }
                    if let Some(v) = qq.app_id {
                        cfg.channels.qq.app_id = v;
                    }
                    if let Some(v) = qq.client_secret {
                        cfg.channels.qq.client_secret = v;
                    }
                }
                if let Some(fs) = ch.feishu {
                    if let Some(v) = fs.enabled {
                        cfg.channels.feishu.enabled = v;
                    }
                    if let Some(v) = fs.app_id {
                        cfg.channels.feishu.app_id = v;
                    }
                    if let Some(v) = fs.app_secret {
                        cfg.channels.feishu.app_secret = v;
                    }
                    if let Some(v) = fs.mode {
                        cfg.channels.feishu.mode = v;
                    }
                }
                if let Some(s) = ch.session {
                    if let Some(v) = s.timeout_minutes {
                        cfg.channels.session.timeout_minutes = v;
                    }
                    if let Some(v) = s.reset_keywords {
                        cfg.channels.session.reset_keywords = v;
                    }
                    if let Some(v) = s.reset_commands {
                        cfg.channels.session.reset_commands = v;
                    }
                }
            }

            if let Some(s) = req.server {
                if let Some(v) = s.host {
                    cfg.server.host = v;
                }
                if let Some(v) = s.port {
                    cfg.server.port = v;
                }
            }

            if let Some(l) = req.logging
                && let Some(v) = l.level
            {
                cfg.logging.level = v;
            }
            Ok(())
        })
        .await
        .map_err(|error| match error {
            echo_agent_app_core::api::state::ModelMutationError::Validation(message) => {
                IpcError::Validation(message)
            }
            other => IpcError::Internal(other.to_string()),
        })?;

    // Model runtime settings were settled by the owned config mutation. The
    // system prompt is independent of model generation publication.
    let system_prompt = config.agent.system_prompt.clone();
    state
        .app_state
        .apply_system_prompt_to_agents(system_prompt)
        .await;
    tracing::info!("配置已同步到 Agent");

    Ok(full_config_response(&config))
}

#[tauri::command]
pub async fn discover_config() -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::api::config_discovery::ConfigDiscovery;

    let discovery = ConfigDiscovery::new();
    let inventory = discovery.discover_all();

    let files: Vec<serde_json::Value> = inventory
        .all_files()
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "path": f.path.display().to_string(),
                "scope": f.scope.to_string(),
                "category": f.category.to_string(),
                "accessible": f.accessible,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "total": inventory.total_count(),
        "files": files,
    }))
}
