//! 论文管理 API
//!
//! 提供学术论文的 CRUD 操作、PDF 管理和笔记功能。
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/papers | List papers (with optional tag/search filter) |
//! | POST | /api/papers | Add a new paper |
//! | GET | /api/papers/:id | Get paper details |
//! | DELETE | /api/papers/:id | Delete a paper |
//! | PUT | /api/papers/:id/notes | Update paper notes |
//! | POST | /api/papers/:id/tags | Add tags to a paper |

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use echo_agent_app_core::state::AppState;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arxiv_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_path: Option<String>,
    pub added_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreatePaperRequest {
    pub title: String,
    pub authors: Option<Vec<String>>,
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub year: Option<u32>,
    pub venue: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateNotesRequest {
    pub notes: String,
}

#[derive(Debug, Deserialize)]
pub struct AddTagsRequest {
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListPapersParams {
    pub tag: Option<String>,
    pub search: Option<String>,
}

// ── In-memory store (replace with SQLite in production) ────────────────

lazy_static::lazy_static! {
    static ref PAPER_STORE: RwLock<HashMap<String, Paper>> = RwLock::new(HashMap::new());
}

// ── Handlers ───────────────────────────────────────────────────────────

/// GET /api/papers — list papers with optional filtering
async fn list_papers(
    State(_state): State<Arc<AppState>>,
    Query(params): Query<ListPapersParams>,
) -> Json<Vec<Paper>> {
    let store = PAPER_STORE.read().await;
    let mut papers: Vec<Paper> = store.values().cloned().collect();

    // Filter by tag
    if let Some(ref tag) = params.tag {
        papers.retain(|p| p.tags.contains(tag));
    }

    // Filter by search term (title, authors, abstract)
    if let Some(ref search) = params.search {
        let needle = search.to_lowercase();
        papers.retain(|p| {
            p.title.to_lowercase().contains(&needle)
                || p.authors
                    .iter()
                    .any(|a| a.to_lowercase().contains(&needle))
                || p.abstract_text
                    .as_ref()
                    .map(|a| a.to_lowercase().contains(&needle))
                    .unwrap_or(false)
        });
    }

    // Sort by added_at descending (newest first)
    papers.sort_by(|a, b| b.added_at.cmp(&a.added_at));
    Json(papers)
}

/// POST /api/papers — add a new paper
async fn create_paper(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<CreatePaperRequest>,
) -> (StatusCode, Json<Paper>) {
    let paper = Paper {
        id: uuid::Uuid::new_v4().to_string(),
        title: req.title,
        authors: req.authors.unwrap_or_default(),
        abstract_text: req.abstract_text,
        doi: req.doi,
        arxiv_id: req.arxiv_id,
        year: req.year,
        venue: req.venue,
        tags: req.tags.unwrap_or_default(),
        notes: None,
        pdf_path: None,
        added_at: chrono::Utc::now().to_rfc3339(),
    };

    let mut store = PAPER_STORE.write().await;
    store.insert(paper.id.clone(), paper.clone());

    (StatusCode::CREATED, Json(paper))
}

/// GET /api/papers/:id — get a single paper
async fn get_paper(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Paper>, (StatusCode, String)> {
    let store = PAPER_STORE.read().await;
    store
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "Paper not found".into()))
}

/// PUT /api/papers/:id/notes — update notes for a paper
async fn update_notes(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNotesRequest>,
) -> Result<Json<Paper>, (StatusCode, String)> {
    let mut store = PAPER_STORE.write().await;
    let paper = store
        .get_mut(&id)
        .ok_or((StatusCode::NOT_FOUND, "Paper not found".to_string()))?;
    paper.notes = Some(req.notes);
    Ok(Json(paper.clone()))
}

/// POST /api/papers/:id/tags — add tags to a paper
async fn add_tags(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<AddTagsRequest>,
) -> Result<Json<Paper>, (StatusCode, String)> {
    let mut store = PAPER_STORE.write().await;
    let paper = store
        .get_mut(&id)
        .ok_or((StatusCode::NOT_FOUND, "Paper not found".to_string()))?;
    for tag in req.tags {
        if !paper.tags.contains(&tag) {
            paper.tags.push(tag);
        }
    }
    Ok(Json(paper.clone()))
}

/// DELETE /api/papers/:id — delete a paper
async fn delete_paper(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut store = PAPER_STORE.write().await;
    store
        .remove(&id)
        .map(|_| Json(serde_json::json!({ "deleted": id })))
        .ok_or((StatusCode::NOT_FOUND, "Paper not found".into()))
}

// ── Router ─────────────────────────────────────────────────────────────

pub fn paper_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/papers", get(list_papers).post(create_paper))
        .route(
            "/api/papers/:id",
            get(get_paper).delete(delete_paper),
        )
        .route("/api/papers/:id/notes", put(update_notes))
        .route("/api/papers/:id/tags", post(add_tags))
}
