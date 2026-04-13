//! 权限管理 API

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::{AppState, PermissionRule};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PermissionModeResponse {
    pub mode: String,
}

#[derive(Debug, Deserialize)]
pub struct SetPermissionModeRequest {
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct PermissionRuleInfo {
    pub matcher: String,
    pub behavior: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct AddPermissionRuleRequest {
    pub matcher: String,
    pub behavior: String,
    pub source: Option<String>,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/permissions/mode
pub async fn get_permission_mode(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PermissionModeResponse>, AppError> {
    let mode = state.permission_mode.read().unwrap();
    Ok(Json(PermissionModeResponse {
        mode: mode.clone(),
    }))
}

/// PUT /api/permissions/mode
pub async fn set_permission_mode(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetPermissionModeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let valid_modes = ["default", "auto-approve", "strict"];
    if !valid_modes.contains(&req.mode.as_str()) {
        return Err(AppError::Internal(format!(
            "无效的权限模式 '{}', 可选: {:?}",
            req.mode, valid_modes
        )));
    }

    tracing::info!("设置权限模式: {}", req.mode);
    let mut mode = state.permission_mode.write().unwrap();
    *mode = req.mode.clone();
    Ok(Json(serde_json::json!({"success": true, "mode": req.mode})))
}

/// GET /api/permissions/rules
pub async fn list_permission_rules(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<PermissionRuleInfo>>, AppError> {
    let rules = state.permission_rules.read().unwrap();
    let list: Vec<PermissionRuleInfo> = rules
        .iter()
        .map(|r| PermissionRuleInfo {
            matcher: r.matcher.clone(),
            behavior: r.behavior.clone(),
            source: r.source.clone(),
        })
        .collect();
    Ok(Json(list))
}

/// POST /api/permissions/rules
pub async fn add_permission_rule(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddPermissionRuleRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("添加权限规则: {:?} -> {:?}", req.matcher, req.behavior);

    let valid_behaviors = ["allow", "deny", "ask"];
    if !valid_behaviors.contains(&req.behavior.as_str()) {
        return Err(AppError::Internal(format!(
            "无效的 behavior '{}', 可选: {:?}",
            req.behavior, valid_behaviors
        )));
    }

    let rule = PermissionRule {
        matcher: req.matcher.clone(),
        behavior: req.behavior.clone(),
        source: req.source.unwrap_or_else(|| "manual".to_string()),
    };

    let mut rules = state.permission_rules.write().unwrap();
    // 如果已存在同 matcher 的规则，替换
    if let Some(existing) = rules.iter_mut().find(|r| r.matcher == req.matcher) {
        *existing = rule;
    } else {
        rules.push(rule);
    }

    Ok(Json(serde_json::json!({"success": true})))
}

/// DELETE /api/permissions/rules/:name
pub async fn remove_permission_rule(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("删除权限规则: {}", name);
    let mut rules = state.permission_rules.write().unwrap();
    let before = rules.len();
    rules.retain(|r| r.matcher != name);
    let removed = before - rules.len();

    if removed == 0 {
        return Err(AppError::NotFound(format!("权限规则 '{}' 未找到", name)));
    }

    Ok(Json(serde_json::json!({"success": true, "removed": removed})))
}
