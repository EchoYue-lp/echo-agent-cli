//! 共享草稿 API
//!
//! 工作区级别的 Markdown 文档，人类和 Agent 可以同时编辑。
//! 存储在 {workspace}/scratchpad.md。

use axum::{
    Json, Router,
    extract::State,
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use echo_agent_app_core::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct ScratchpadContent {
    pub content: String,
    pub modified_at: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateScratchpadRequest {
    pub content: String,
}

/// GET /api/scratchpad — get scratchpad content
async fn get_scratchpad(
    State(state): State<Arc<AppState>>,
) -> Json<ScratchpadContent> {
    let path = get_scratchpad_path(&state).await;
    let content = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# Scratchpad\n\nShared workspace notes.\n".to_string()
    });
    let modified_at = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_default();

    Json(ScratchpadContent { content, modified_at })
}

/// PUT /api/scratchpad — update scratchpad content
async fn update_scratchpad(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateScratchpadRequest>,
) -> Result<Json<ScratchpadContent>, (axum::http::StatusCode, String)> {
    let path = get_scratchpad_path(&state).await;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            tracing::error!(path = %path.display(), error = %e, "Failed to create scratchpad directory");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create scratchpad directory: {e}"),
            )
        })?;
    }
    std::fs::write(&path, &req.content).map_err(|e| {
        tracing::error!(path = %path.display(), error = %e, "Failed to write scratchpad");
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write scratchpad: {e}"),
        )
    })?;

    Ok(Json(ScratchpadContent {
        content: req.content,
        modified_at: chrono::Utc::now().to_rfc3339(),
    }))
}

async fn get_scratchpad_path(state: &AppState) -> std::path::PathBuf {
    if let Some(ws) = state.current_workspace().await {
        echo_agent_app_core::workspace::layout::WorkspaceLayout::scratchpad(&ws.root)
    } else {
        // Fallback to global scratchpad
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home).join(".echo-agent").join("scratchpad.md")
    }
}

pub fn scratchpad_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/scratchpad", get(get_scratchpad).put(update_scratchpad))
}
