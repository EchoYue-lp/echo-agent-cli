//! Skills Hub REST API

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::state::AppState;

// ── Request types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub q: String,
}

#[derive(Debug, Deserialize)]
pub struct InstallLocalRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct InstallGitRequest {
    pub repo: String,
    pub subdir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    pub name: String,
}

// ── Handlers ────────────────────────────────────────────────────

/// GET /api/skills-hub — 列出 Skills Hub 中所有可用技能
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn list_hub_skills(State(state): State<Arc<AppState>>) -> Response {
    let hub = state.skills_hub.read().await;
    let entries = hub.list();
    Json(entries).into_response()
}

/// GET /api/skills-hub/search?q=xxx — 搜索技能
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn search_hub_skills(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let query = params.get("q").cloned().unwrap_or_default();
    if query.is_empty() {
        return Json(serde_json::json!({ "error": "q parameter is required" })).into_response();
    }
    let hub = state.skills_hub.read().await;
    let results = hub.search(&query);
    Json(results).into_response()
}

/// GET /api/skills-hub/:name — 获取技能详情
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_hub_skill(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let hub = state.skills_hub.read().await;
    match hub.get(&name) {
        Some(entry) => Json(entry).into_response(),
        None => Json(serde_json::json!({ "error": format!("Skill '{}' not found in hub", name) }))
            .into_response(),
    }
}

/// POST /api/skills-hub/install/local — 从本地目录安装
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn install_local(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InstallLocalRequest>,
) -> Response {
    let mut hub = state.skills_hub.write().await;
    let source = std::path::PathBuf::from(&req.path);
    match crate::skills_hub::install::install_from_local(&source, &mut hub) {
        Ok(result) => {
            // 同步已加载列表
            sync_loaded(&state, &mut hub).await;
            Json(serde_json::json!({
                "success": true,
                "name": result.name,
                "path": result.path.to_string_lossy(),
                "source": result.source,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({ "error": e })).into_response(),
    }
}

/// POST /api/skills-hub/install/git — 从 Git 仓库安装
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn install_git(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InstallGitRequest>,
) -> Response {
    let mut hub = state.skills_hub.write().await;
    match crate::skills_hub::install::install_from_git(&req.repo, req.subdir.as_deref(), &mut hub)
        .await
    {
        Ok(result) => {
            sync_loaded(&state, &mut hub).await;
            Json(serde_json::json!({
                "success": true,
                "name": result.name,
                "path": result.path.to_string_lossy(),
                "source": result.source,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({ "error": e })).into_response(),
    }
}

/// POST /api/skills-hub/uninstall — 卸载技能
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn uninstall_skill(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UninstallRequest>,
) -> Response {
    let mut hub = state.skills_hub.write().await;
    match crate::skills_hub::install::uninstall(&req.name, &mut hub) {
        Ok(()) => {
            sync_loaded(&state, &mut hub).await;
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Err(e) => Json(serde_json::json!({ "error": e })).into_response(),
    }
}

/// POST /api/skills-hub/refresh — 刷新索引
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn refresh_hub(State(state): State<Arc<AppState>>) -> Response {
    let mut hub = state.skills_hub.write().await;
    hub.refresh();
    sync_loaded(&state, &mut hub).await;
    let count = hub.list().len();
    Json(serde_json::json!({ "success": true, "count": count })).into_response()
}

/// 同步已加载技能列表到 hub
async fn sync_loaded(state: &Arc<AppState>, hub: &mut crate::skills_hub::SkillsHub) {
    let loaded: Vec<String> = state
        .connection
        .agent
        .read(|a| a.skill_names().iter().map(|s| s.to_string()).collect())
        .await;
    hub.set_loaded_skills(loaded);
}
