//! Skill 管理 API

use axum::{
    debug_handler,
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;
use crate::types::{SkillInfo, SkillSource};

/// GET /api/skills - 列出所有已安装的 Skill
#[debug_handler]
pub async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;

    let skills: Vec<SkillInfo> = agent
        .list_skills()
        .into_iter()
        .map(|s| SkillInfo {
            name: s.name.clone(),
            description: s.description.clone(),
            enabled: true,
            tool_names: s.tool_names.clone(),
            source: SkillSource::Builtin,
        })
        .collect();

    Json(skills).into_response()
}

/// GET /api/skills/:name - 获取指定 Skill 详情
#[debug_handler]
pub async fn get_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let agent = state.agent.lock().await;

    match agent.list_skills().into_iter().find(|s| s.name == name) {
        Some(s) => Json(SkillInfo {
            name: s.name.clone(),
            description: s.description.clone(),
            enabled: true,
            tool_names: s.tool_names.clone(),
            source: SkillSource::Builtin,
        }).into_response(),
        None => Json(serde_json::json!({
            "error": format!("Skill '{}' not found", name)
        })).into_response(),
    }
}

/// POST /api/skills/load - 从目录加载 Skill
#[debug_handler]
pub async fn load_skills_from_dir(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoadSkillsRequest>,
) -> Response {
    let mut agent = state.agent.lock().await;

    match agent.load_skills_from_dir(&req.path).await {
        Ok(loaded_skills) => {
            // 返回加载后的技能列表
            let skills: Vec<SkillInfo> = agent
                .list_skills()
                .into_iter()
                .map(|s| SkillInfo {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    enabled: true,
                    tool_names: s.tool_names.clone(),
                    source: SkillSource::External { path: req.path.clone() },
                })
                .collect();

            Json(serde_json::json!({
                "message": format!("成功加载 {} 个技能", loaded_skills.len()),
                "loaded": loaded_skills,
                "skills": skills
            })).into_response()
        }
        Err(e) => {
            WebError::Internal(format!("加载技能失败: {}", e)).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct LoadSkillsRequest {
    pub path: String,
}