//! 权限管理 API

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::{AppState, PermissionBehavior, PermissionRuleConfig};

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

fn parse_behavior(s: &str) -> Result<PermissionBehavior, AppError> {
    match s {
        "allow" => Ok(PermissionBehavior::Allow),
        "deny" => Ok(PermissionBehavior::Deny),
        "ask" => Ok(PermissionBehavior::Ask),
        _ => Err(AppError::Validation(format!(
            "无效的 behavior '{}', 可选: allow, deny, ask",
            s
        ))),
    }
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/permissions/mode
pub async fn get_permission_mode(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PermissionModeResponse>, AppError> {
    let mode = state.config.permission_mode.read().await;
    Ok(Json(PermissionModeResponse { mode: mode.clone() }))
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
    let mut mode = state.config.permission_mode.write().await;
    *mode = req.mode.clone();
    Ok(Json(serde_json::json!({"success": true, "mode": req.mode})))
}

/// GET /api/permissions/rules
pub async fn list_permission_rules(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<PermissionRuleInfo>>, AppError> {
    let rules = state.config.permission_rules.read().await;
    let list: Vec<PermissionRuleInfo> = rules
        .iter()
        .map(|r| PermissionRuleInfo {
            matcher: r.matcher.clone(),
            behavior: r.behavior.to_string(),
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

    let behavior = parse_behavior(&req.behavior)?;

    let rule = PermissionRuleConfig {
        matcher: req.matcher.clone(),
        behavior,
        source: req.source.unwrap_or_else(|| "manual".to_string()),
    };

    let mut rules = state.config.permission_rules.write().await;
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
    let mut rules = state.config.permission_rules.write().await;
    let before = rules.len();
    rules.retain(|r| r.matcher != name);
    let removed = before - rules.len();

    if removed == 0 {
        return Err(AppError::NotFound(format!("权限规则 '{}' 未找到", name)));
    }

    Ok(Json(
        serde_json::json!({"success": true, "removed": removed}),
    ))
}
