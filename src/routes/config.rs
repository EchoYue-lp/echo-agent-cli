//! 配置管理 API

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use echo_agent::agent::Agent;
use echo_agent::llm::config::Config;

use crate::state::AppState;
use crate::types::{AgentConfigResponse, UpdateConfigRequest};

/// GET /api/config - 获取当前配置
#[debug_handler]
pub async fn get_config(
    State(state): State<Arc<AppState>>,
) -> Response {
    // 从 Agent 获取实际配置（这是真正的运行时配置）
    let agent = state.agent.lock().await;

    // 获取可用的模型列表
    let available_models = Config::list_models();

    Json(AgentConfigResponse {
        model: agent.model_name().to_string(),
        system_prompt: agent.system_prompt().to_string(),
        max_iterations: 10, // 从 agent config 获取
        token_limit: 8000,
        enable_memory: true,
        enable_human_loop: agent.config().is_human_in_loop_enabled(),
        session_id: agent.config().get_session_id().map(|s| s.to_string()),
        available_models,
    }).into_response()
}

/// PUT /api/config - 更新配置
#[debug_handler]
pub async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateConfigRequest>,
) -> Response {
    // 验证模型配置是否存在
    if let Some(ref model) = req.model {
        if !Config::has_model(model) {
            let available = Config::list_models();
            return Json(serde_json::json!({
                "error": format!("模型 '{}' 未配置，可用模型: {:?}", model, available)
            })).into_response();
        }
    }

    // 更新 WebConfig
    {
        let mut config = state.config.write().unwrap();

        if let Some(ref model) = req.model {
            config.model = model.clone();
        }
        if let Some(ref system_prompt) = req.system_prompt {
            config.system_prompt = system_prompt.clone();
        }
        if let Some(max_iterations) = req.max_iterations {
            config.max_iterations = max_iterations;
        }
        if let Some(token_limit) = req.token_limit {
            config.token_limit = token_limit;
        }
    }

    // 同步更新 Agent 配置
    {
        let mut agent = state.agent.lock().await;

        // 更新模型
        if let Some(ref model) = req.model {
            agent.set_model(model);
            tracing::info!("模型已切换为: {}", model);
        }

        // 更新系统提示词
        if let Some(ref system_prompt) = req.system_prompt {
            agent.set_system_prompt(system_prompt.clone());
            tracing::info!("系统提示词已更新");
        }
    }

    // 返回更新后的配置
    let agent = state.agent.lock().await;
    let available_models = Config::list_models();

    Json(AgentConfigResponse {
        model: agent.model_name().to_string(),
        system_prompt: agent.system_prompt().to_string(),
        max_iterations: 10,
        token_limit: 8000,
        enable_memory: true,
        enable_human_loop: true,
        session_id: None,
        available_models,
    }).into_response()
}