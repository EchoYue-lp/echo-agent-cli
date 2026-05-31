//! 决策日志 API
//!
//! Agent 自动从对话中提取关键决策，存入结构化日志。
//! 存储在 {workspace}/decisions.jsonl。

use axum::{
    extract::{State, Query},
    Json, Router,
    routing::{get, post, delete},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use chrono::Utc;
use uuid::Uuid;

use echo_agent_app_core::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub decision: String,
    pub rationale: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateDecisionRequest {
    pub decision: String,
    pub rationale: String,
    pub alternatives: Option<Vec<String>>,
    pub context: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListDecisionsParams {
    pub limit: Option<usize>,
}

/// GET /api/decisions — list decisions
async fn list_decisions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListDecisionsParams>,
) -> Json<Vec<Decision>> {
    let path = get_decisions_path(&state).await;
    let limit = params.limit.unwrap_or(50);

    let decisions = read_decisions(&path);
    let recent: Vec<Decision> = decisions.into_iter().rev().take(limit).collect();
    Json(recent)
}

/// POST /api/decisions — add a decision
async fn create_decision(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateDecisionRequest>,
) -> Json<Decision> {
    let decision = Decision {
        id: Uuid::new_v4().to_string(),
        decision: req.decision,
        rationale: req.rationale,
        alternatives: req.alternatives.unwrap_or_default(),
        context: req.context,
        timestamp: Utc::now().to_rfc3339(),
    };

    let path = get_decisions_path(&state).await;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Append as JSONL
    if let Ok(json) = serde_json::to_string(&decision) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{}", json);
        }
    }

    Json(decision)
}

/// DELETE /api/decisions — clear all decisions
async fn clear_decisions(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let path = get_decisions_path(&state).await;
    std::fs::remove_file(&path).ok();
    Json(serde_json::json!({ "cleared": true }))
}

fn read_decisions(path: &std::path::Path) -> Vec<Decision> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn get_decisions_path(state: &AppState) -> std::path::PathBuf {
    if let Some(ws) = state.current_workspace().await {
        echo_agent_app_core::workspace::layout::WorkspaceLayout::decisions(&ws.root)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home).join(".echo-agent").join("decisions.jsonl")
    }
}

pub fn decision_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/decisions", get(list_decisions).post(create_decision).delete(clear_decisions))
}
