//! 自进化 / 自我改善 API
//!
//! 提供轨迹管理、后台审查、技能策展等功能。
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/evolution/trajectories | List trajectory entries |
//! | GET | /api/evolution/trajectories/stats | Get trajectory statistics |
//! | POST | /api/evolution/review | Run background review on a run |
//! | POST | /api/evolution/curator | Curator action (status/run/pin/unpin) |

use axum::{
    Json, Router, debug_handler,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::Arc;

use echo_agent_app_core::state::AppState;

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TrajectoryListParams {
    pub date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    pub run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CuratorActionRequest {
    pub action: String,
    pub skill_name: Option<String>,
}

// ── Trajectory Endpoints ──────────────────────────────────────────

#[debug_handler]
pub async fn list_trajectories(
    State(_state): State<Arc<AppState>>,
    Query(params): Query<TrajectoryListParams>,
) -> Response {
    match echo_agent::improve::TrajectorySaver::default_dir() {
        Ok(saver) => {
            let entries = saver.list(params.date.as_deref()).await.unwrap_or_default();
            Json(serde_json::json!({
                "trajectories": entries,
                "count": entries.len(),
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to init trajectory saver: {e}")
        }))
        .into_response(),
    }
}

#[debug_handler]
pub async fn trajectory_stats(
    State(_state): State<Arc<AppState>>,
) -> Response {
    match echo_agent::improve::TrajectorySaver::default_dir() {
        Ok(saver) => {
            let stats = saver.stats().await.unwrap_or_else(|_| {
                echo_agent::improve::TrajectoryStats {
                    total: 0,
                    completed: 0,
                    failed: 0,
                    total_tokens: 0,
                    total_tool_calls: 0,
                    avg_duration_ms: 0,
                }
            });
            Json(serde_json::json!({ "stats": stats })).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to get stats: {e}")
        }))
        .into_response(),
    }
}

// ── Background Review ─────────────────────────────────────────────

#[debug_handler]
pub async fn run_review(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReviewRequest>,
) -> Response {
    let (llm_client, run_store, memory_store) = state
        .connection
        .agent
        .read(|a| {
            let llm: Option<Arc<dyn echo_agent::llm::LlmClient>> =
                a.llm_client().cloned();
            let rs = a.run_store.clone();
            let ms = a.store().cloned();
            (llm, rs, ms)
        })
        .await;

    let Some(llm) = llm_client else {
        return Json(serde_json::json!({
            "error": "No LLM client available"
        }))
        .into_response();
    };

    let Some(run_store) = run_store else {
        return Json(serde_json::json!({
            "error": "No run store configured. Enable run tracing first."
        }))
        .into_response();
    };

    let config = echo_agent::improve::BackgroundReviewConfig {
        enabled: true,
        max_iterations: 8,
        review_memory: true,
        review_skills: true,
    };

    let reviewer = echo_agent::improve::BackgroundReviewer::new(
        config,
        llm,
        memory_store,
        Some(run_store.clone()),
    );

    // Determine which run to review
    let run_id = if let Some(id) = req.run_id {
        id
    } else {
        // Get latest run
        let runs = match run_store.list_all(1).await {
            Ok(r) => r,
            Err(e) => {
                return Json(serde_json::json!({
                    "error": format!("Failed to list runs: {e}")
                }))
                .into_response();
            }
        };
        match runs.first() {
            Some(summary) => summary.run_id.clone(),
            None => {
                return Json(serde_json::json!({
                    "error": "No runs available for review. Start a conversation first."
                }))
                .into_response();
            }
        }
    };

    match reviewer.review_by_run_id(&run_id).await {
        Ok(outcome) => Json(serde_json::json!({
            "success": true,
            "run_id": outcome.run_id,
            "actions": outcome.actions,
            "nothing_to_save": outcome.nothing_to_save,
            "error": outcome.error,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "error": format!("Review failed: {e}")
        }))
        .into_response(),
    }
}

// ── Curator ───────────────────────────────────────────────────────

#[debug_handler]
pub async fn curator_action(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<CuratorActionRequest>,
) -> Response {
    let config = echo_agent::improve::CuratorConfig {
        stale_days: 30,
        archive_days: 90,
        enabled: true,
    };
    let curator = echo_agent::improve::Curator::default_path(config);

    match req.action.as_str() {
        "status" => {
            let status = curator.status();
            Json(serde_json::json!({
                "success": true,
                "status": {
                    "total": status.total,
                    "active": status.active,
                    "stale": status.stale,
                    "archived": status.archived,
                    "pinned": status.pinned,
                    "last_run_at": status.last_run_at.map(|t| t.to_rfc3339()),
                },
            }))
            .into_response()
        }
        "run" => match curator.apply_transitions() {
            Ok(transitions) => {
                let changes: Vec<_> = transitions
                    .iter()
                    .map(|(name, from, to)| {
                        serde_json::json!({
                            "skill": name,
                            "from": format!("{:?}", from),
                            "to": format!("{:?}", to),
                        })
                    })
                    .collect();
                Json(serde_json::json!({
                    "success": true,
                    "transitions": changes,
                    "count": changes.len(),
                }))
                .into_response()
            }
            Err(e) => Json(serde_json::json!({
                "error": format!("Curator run failed: {e}")
            }))
            .into_response(),
        },
        "pin" => {
            if let Some(ref name) = req.skill_name {
                match curator.pin_skill(name) {
                    Ok(()) => Json(serde_json::json!({ "success": true, "pinned": name }))
                        .into_response(),
                    Err(e) => {
                        Json(serde_json::json!({ "error": e.to_string() })).into_response()
                    }
                }
            } else {
                Json(serde_json::json!({ "error": "skill_name required" })).into_response()
            }
        }
        "unpin" => {
            if let Some(ref name) = req.skill_name {
                match curator.unpin_skill(name) {
                    Ok(()) => {
                        Json(serde_json::json!({ "success": true, "unpinned": name }))
                            .into_response()
                    }
                    Err(e) => {
                        Json(serde_json::json!({ "error": e.to_string() })).into_response()
                    }
                }
            } else {
                Json(serde_json::json!({ "error": "skill_name required" })).into_response()
            }
        }
        _ => Json(serde_json::json!({ "error": "Unknown action" })).into_response(),
    }
}

// ── Router ────────────────────────────────────────────────────────

pub fn evolution_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/evolution/trajectories",
            get(list_trajectories),
        )
        .route(
            "/api/evolution/trajectories/stats",
            get(trajectory_stats),
        )
        .route("/api/evolution/review", post(run_review))
        .route("/api/evolution/curator", post(curator_action))
}
