//! 工具管理 API

use axum::{
    Json, debug_handler,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;

/// GET /api/tools - 列出所有已注册工具
#[debug_handler]
pub async fn list_tools(State(state): State<Arc<AppState>>) -> Response {
    let tools = state.get_tool_infos(&state.connection.agent).await;
    Json(tools).into_response()
}

/// GET /api/tools/{name} - 获取工具详情
#[debug_handler]
pub async fn get_tool(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let tools = state.get_tool_infos(&state.connection.agent).await;

    match tools.into_iter().find(|t| t.name == name) {
        Some(tool) => Json(tool).into_response(),
        None => WebError::ToolNotFound(name).into_response(),
    }
}

/// POST /api/tools/{name}/enable - 启用工具
#[debug_handler]
pub async fn enable_tool(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    // 更新工具状态
    {
        let mut tool_states = state.session.tool_states.write().await;
        tool_states
            .entry(name.clone())
            .and_modify(|s| s.enabled = true)
            .or_insert(crate::state::ToolState {
                enabled: true,
                need_approval: false,
            });
        // 锁在这里被释放
    }

    // 获取更新后的工具信息
    let tools = state.get_tool_infos(&state.connection.agent).await;

    match tools.into_iter().find(|t| t.name == name) {
        Some(tool) => Json(tool).into_response(),
        None => WebError::ToolNotFound(name).into_response(),
    }
}

/// POST /api/tools/{name}/disable - 禁用工具
#[debug_handler]
pub async fn disable_tool(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    // 更新工具状态
    {
        let mut tool_states = state.session.tool_states.write().await;
        tool_states
            .entry(name.clone())
            .and_modify(|s| s.enabled = false)
            .or_insert(crate::state::ToolState {
                enabled: false,
                need_approval: false,
            });
        // 锁在这里被释放
    }

    // 获取更新后的工具信息
    let tools = state.get_tool_infos(&state.connection.agent).await;

    match tools.into_iter().find(|t| t.name == name) {
        Some(tool) => Json(tool).into_response(),
        None => WebError::ToolNotFound(name).into_response(),
    }
}
