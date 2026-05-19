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

use serde::Deserialize;

/// GET /api/skills - 列出所有已安装的 Skill
#[debug_handler]
pub async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Response {
    let skills = state.connection.agent.read(|agent| {
        agent.list_skills()
            .into_iter()
            .map(|s| SkillInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                enabled: true,
                tool_names: s.tool_names.clone(),
                source: SkillSource::Builtin,
            })
            .collect::<Vec<_>>()
    }).await;

    Json(skills).into_response()
}

/// GET /api/skills/:name - 获取指定 Skill 详情
#[debug_handler]
pub async fn get_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let skill_opt = state.connection.agent.read(|agent| {
        agent.list_skills().into_iter().find(|s| s.name == name).cloned()
    }).await;

    match skill_opt {
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
    let path = req.path.clone();
    let path_clone = path.clone();
    match state.connection.agent.write_async(|agent| Box::pin(async move {
        agent.load_skills_from_dir(&path).await
    })).await {
        Ok(loaded_skills) => {
            // 返回加载后的技能列表
            let skills: Vec<SkillInfo> = state.connection.agent.read(|agent| {
                agent.list_skills()
                    .into_iter()
                    .map(|s| SkillInfo {
                        name: s.name.clone(),
                        description: s.description.clone(),
                        enabled: true,
                        tool_names: s.tool_names.clone(),
                        source: SkillSource::External { path: path_clone.clone() },
                    })
                    .collect()
            }).await;

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

#[derive(Debug, Deserialize)]
pub struct LoadSkillsRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct UploadSkillsRequest {
    pub root_dir: String,
    pub files: Vec<UploadedFile>,
}

#[derive(Debug, Deserialize)]
pub struct UploadedFile {
    pub path: String,
    pub content: String,
}

/// Check if a file path represents a flat .md/.markdown file that is NOT already named SKILL.md.
/// The skill loader expects `skill-name/SKILL.md` directory structure, so flat files need
/// to be reorganized.
fn is_flat_md_file(path: &str) -> bool {
    let p = std::path::Path::new(path);
    let filename = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if filename.eq_ignore_ascii_case("SKILL.md") {
        return false; // Already named SKILL.md, keep as-is
    }
    filename.ends_with(".md") || filename.ends_with(".markdown")
}

/// POST /api/skills/upload - 从浏览器上传的目录文件加载 Skill
#[debug_handler]
pub async fn upload_skills(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UploadSkillsRequest>,
) -> Response {
    // 在临时目录下创建唯一子目录
    let temp_root = std::env::temp_dir().join("echo-agent-skills");
    let dir_id = uuid::Uuid::new_v4().to_string();
    let upload_dir = temp_root.join(&dir_id);

    // 写入文件（保留子目录结构，自动将扁平 .md 文件组织为 SKILL.md 子目录）
    for file in &req.files {
        let file_path = upload_dir.join(&file.path);
        // If the file is a flat .md file (not already named SKILL.md inside a subdirectory),
        // reorganize it as <name>/SKILL.md to match the skill loader's expected structure.
        let target_path = if is_flat_md_file(&file.path) {
            let skill_name = std::path::Path::new(&file.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            upload_dir.join(skill_name).join("SKILL.md")
        } else {
            file_path
        };
        if let Some(parent) = target_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent) {
                return WebError::Internal(format!("创建目录失败: {e}")).into_response();
            }
        if let Err(e) = std::fs::write(&target_path, &file.content) {
            return WebError::Internal(format!("写入文件失败: {e}")).into_response();
        }
    }

    // 加载技能
    let root_dir = req.root_dir.clone();
    match state.connection.agent.write_async(|agent| Box::pin(async move {
        agent.load_skills_from_dir(&upload_dir).await
    })).await {
        Ok(loaded_skills) => {
            let skills: Vec<SkillInfo> = state.connection.agent.read(|agent| {
                agent.list_skills()
                    .into_iter()
                    .map(|s| SkillInfo {
                        name: s.name.clone(),
                        description: s.description.clone(),
                        enabled: true,
                        tool_names: s.tool_names.clone(),
                        source: SkillSource::External {
                            path: root_dir.clone(),
                        },
                    })
                    .collect()
            }).await;

            Json(serde_json::json!({
                "message": format!("成功上传并加载 {} 个技能", loaded_skills.len()),
                "loaded": loaded_skills,
                "skills": skills
            }))
            .into_response()
        }
        Err(e) => WebError::Internal(format!("加载技能失败: {e}")).into_response(),
    }
}