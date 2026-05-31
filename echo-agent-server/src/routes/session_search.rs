//! 会话全文搜索 API

use axum::{
    Json, debug_handler,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

// ── Query params ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// FTS5 搜索关键词
    pub q: String,
    /// 返回结果数量上限 (默认 20)
    pub limit: Option<usize>,
}

// ── Response types ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionSearchResponse {
    pub results: Vec<SessionSearchItem>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionSearchItem {
    pub session_id: String,
    pub session_name: String,
    pub model: String,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Serialize)]
pub struct ReindexResponse {
    pub success: bool,
    pub indexed_count: usize,
}

// ── Handlers ──────────────────────────────────────────────────────

/// GET /api/sessions/search?q=<query>&limit=20
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn search_sessions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(20).min(100);

    match state.storage.search_engine.search(&params.q, limit) {
        Ok(results) => {
            let items: Vec<SessionSearchItem> = results
                .into_iter()
                .map(|r| SessionSearchItem {
                    session_id: r.session_id,
                    session_name: r.session_name,
                    model: r.model,
                    snippet: r.snippet,
                    rank: r.rank,
                })
                .collect();
            let total = items.len();
            Json(SessionSearchResponse {
                results: items,
                total,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!("Session search failed: {e}");
            Json(serde_json::json!({
                "error": format!("搜索失败: {e}")
            }))
            .into_response()
        }
    }
}

/// POST /api/sessions/reindex — 重建全文索引
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn reindex_sessions(State(state): State<Arc<AppState>>) -> Response {
    match state.storage.search_engine.reindex_all() {
        Ok(count) => Json(ReindexResponse {
            success: true,
            indexed_count: count,
        })
        .into_response(),
        Err(e) => {
            tracing::error!("Session reindex failed: {e}");
            Json(ReindexResponse {
                success: false,
                indexed_count: 0,
            })
            .into_response()
        }
    }
}
