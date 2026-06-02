//! Tauri IPC commands for configuration management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent::llm::config::Config;
use echo_agent_app_core::types::{
    AgentConfigResponse, ChannelsConfigResponse, FeishuConfigResponse, FullConfigResponse,
    LoggingConfigResponse, McpConfigResponse, ModelConfigResponse, QqConfigResponse,
    ServerConfigResponse, SessionConfigResponse, UpdateConfigRequest, UpdateFullConfigRequest,
};

#[tauri::command]
pub async fn get_config(
    state: tauri::State<'_, TauriState>,
) -> Result<AgentConfigResponse, IpcError> {
    let available_models = Config::list_models();
    Ok(state
        .app_state
        .connection
        .agent
        .read(|agent| AgentConfigResponse {
            model: agent.model_name().to_string(),
            system_prompt: agent.system_prompt().to_string(),
            max_iterations: agent.config().get_max_iterations(),
            token_limit: agent.config().get_token_limit(),
            enable_memory: agent.config().is_memory_enabled(),
            enable_human_loop: agent.config().is_human_in_loop_enabled(),
            session_id: agent.config().get_session_id().map(|s| s.to_string()),
            available_models,
        })
        .await)
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, TauriState>,
    req: UpdateConfigRequest,
) -> Result<AgentConfigResponse, IpcError> {
    if let Some(ref model) = req.model
        && !Config::has_model(model)
    {
        let available = Config::list_models();
        return Err(IpcError::Validation(format!(
            "模型 '{}' 未配置，可用模型: {:?}",
            model, available
        )));
    }

    {
        let mut config = state.app_state.config.web_config.write().await;
        if let Some(ref model) = req.model {
            config.model = model.clone();
        }
        if let Some(ref system_prompt) = req.system_prompt {
            config.system_prompt = system_prompt.clone();
        }
        if let Some(token_limit) = req.token_limit {
            config.token_limit = token_limit;
        }
    }

    {
        state
            .app_state
            .connection
            .agent
            .write_async(|agent| {
                Box::pin(async move {
                    if let Some(ref model) = req.model {
                        agent.set_model(model);
                        tracing::info!("模型已切换为: {}", model);
                    }
                    if let Some(ref system_prompt) = req.system_prompt {
                        agent.set_system_prompt(system_prompt.clone()).await;
                        tracing::info!("系统提示词已更新");
                    }
                })
            })
            .await;
    }

    let available_models = Config::list_models();
    Ok(state
        .app_state
        .connection
        .agent
        .read(|agent| AgentConfigResponse {
            model: agent.model_name().to_string(),
            system_prompt: agent.system_prompt().to_string(),
            max_iterations: agent.config().get_max_iterations(),
            token_limit: agent.config().get_token_limit(),
            enable_memory: agent.config().is_memory_enabled(),
            enable_human_loop: agent.config().is_human_in_loop_enabled(),
            session_id: agent.config().get_session_id().map(|s| s.to_string()),
            available_models,
        })
        .await)
}

#[tauri::command]
pub async fn get_full_config(
    state: tauri::State<'_, TauriState>,
) -> Result<FullConfigResponse, IpcError> {
    let cfg = state.app_state.config.app_config.read().await;
    Ok(FullConfigResponse {
        model: ModelConfigResponse {
            name: cfg.model.name.clone(),
            max_tokens: cfg.model.max_tokens,
            temperature: cfg.model.temperature,
        },
        agent: AgentConfigResponse {
            model: cfg.model.name.clone(),
            system_prompt: cfg.agent.system_prompt.clone(),
            max_iterations: cfg.agent.max_iterations,
            token_limit: cfg.model.max_tokens.unwrap_or(8000) as usize,
            enable_memory: cfg.agent.enable_memory,
            enable_human_loop: cfg.agent.enable_human_in_loop,
            session_id: None,
            available_models: Config::list_models(),
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
    })
}

#[tauri::command]
pub async fn update_full_config(
    state: tauri::State<'_, TauriState>,
    req: UpdateFullConfigRequest,
) -> Result<FullConfigResponse, IpcError> {
    {
        let mut cfg = state.app_state.config.app_config.write().await;

        if let Some(m) = req.model {
            if let Some(v) = m.name {
                cfg.model.name = v;
            }
            cfg.model.max_tokens = m.max_tokens.or(cfg.model.max_tokens);
            cfg.model.temperature = m.temperature.or(cfg.model.temperature);
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
    }

    // Persist to YAML file
    {
        let cfg = state.app_state.config.app_config.read().await;
        if let Err(e) = echo_agent::config::save_config(&cfg) {
            tracing::warn!("Failed to persist config to file: {e}");
        }
    }

    // Sync model + system_prompt to agent (await completion before responding)
    let model_name;
    let system_prompt;
    {
        let cfg = state.app_state.config.app_config.read().await;
        model_name = cfg.model.name.clone();
        system_prompt = cfg.agent.system_prompt.clone();
    }
    state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            let model_name = model_name.clone();
            let system_prompt = system_prompt.clone();
            Box::pin(async move {
                agent.set_model(&model_name);
                agent.set_system_prompt(system_prompt.clone()).await;
                tracing::info!(model = %model_name, "配置已同步到 Agent");
            })
        })
        .await;

    // Return updated config
    let cfg = state.app_state.config.app_config.read().await;
    Ok(FullConfigResponse {
        model: ModelConfigResponse {
            name: cfg.model.name.clone(),
            max_tokens: cfg.model.max_tokens,
            temperature: cfg.model.temperature,
        },
        agent: AgentConfigResponse {
            model: cfg.model.name.clone(),
            system_prompt: cfg.agent.system_prompt.clone(),
            max_iterations: cfg.agent.max_iterations,
            token_limit: cfg.model.max_tokens.unwrap_or(8000) as usize,
            enable_memory: cfg.agent.enable_memory,
            enable_human_loop: cfg.agent.enable_human_in_loop,
            session_id: None,
            available_models: Config::list_models(),
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
    })
}

#[tauri::command]
pub async fn discover_config() -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::config_discovery::ConfigDiscovery;

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
