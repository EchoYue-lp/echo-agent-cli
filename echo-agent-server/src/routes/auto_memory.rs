//! Auto-memory API routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/auto-memory/status | Get auto-memory status |
//! | POST | /api/auto-memory/toggle | Enable/disable auto-memory |
//! | POST | /api/auto-memory/extract | Trigger extraction now |
//! | GET | /api/auto-memory/observations | List observations |

use axum::{
    Json, Router, debug_handler,
    extract::State,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use echo_agent_app_core::state::AppState;

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AutoMemoryStatus {
    pub enabled: bool,
    pub observations_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct ToggleRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct Observation {
    pub category: String,
    pub text: String,
    pub confidence: f64,
}

// ── Handlers ───────────────────────────────────────────────────────

/// GET /api/auto-memory/status
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_status(
    State(_state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Try to read auto-memory config from project memory
    let enabled = std::fs::read_to_string(
        echo_agent_app_core::persistence::Persistence::base_dir().join("auto_memory_enabled"),
    )
    .ok()
    .and_then(|s| s.trim().parse::<bool>().ok())
    .unwrap_or(true);

    // Count observations from project memory file
    let observations_count = count_observations();

    Json(AutoMemoryStatus {
        enabled,
        observations_count,
    })
    .into_response()
}

/// POST /api/auto-memory/toggle
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn toggle(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<ToggleRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let path =
        echo_agent_app_core::persistence::Persistence::base_dir().join("auto_memory_enabled");
    let _ = std::fs::write(&path, req.enabled.to_string());

    Json(serde_json::json!({ "enabled": req.enabled })).into_response()
}

/// POST /api/auto-memory/extract
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn extract_now(
    State(_state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Read observations from the project memory file
    let observations = read_observations();

    Json(serde_json::json!({
        "success": true,
        "observations": observations,
    }))
    .into_response()
}

/// GET /api/auto-memory/observations
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn list_observations(
    State(_state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let observations = read_observations();
    Json(observations).into_response()
}

// ── Helpers ────────────────────────────────────────────────────────

fn project_memory_path() -> std::path::PathBuf {
    // Look for .echo-agent/project.md in current dir or home
    let cwd = std::env::current_dir().unwrap_or_default();
    let project_path = cwd.join(".echo-agent").join("project.md");
    if project_path.exists() {
        return project_path;
    }
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
    if let Some(home_dir) = home {
        let home_path = home_dir.join(".echo-agent").join("project.md");
        if home_path.exists() {
            return home_path;
        }
    }
    project_path
}

fn count_observations() -> usize {
    read_observations().len()
}

fn read_observations() -> Vec<Observation> {
    let path = project_memory_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut observations = Vec::new();
    let mut current_category = "Project".to_string();

    for line in content.lines() {
        let line = line.trim();

        // Detect category headers like "## Project Patterns" or "## User Preferences"
        if let Some(header) = line.strip_prefix("## ") {
            current_category = if header.to_lowercase().contains("user") {
                "User".to_string()
            } else if header.to_lowercase().contains("bug") || header.to_lowercase().contains("issue") {
                "Bug".to_string()
            } else if header.to_lowercase().contains("decision") {
                "Decision".to_string()
            } else if header.to_lowercase().contains("file") || header.to_lowercase().contains("path") {
                "FilePath".to_string()
            } else {
                "Project".to_string()
            };
            continue;
        }

        // Parse bullet points as observations
        if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                observations.push(Observation {
                    category: current_category.clone(),
                    text,
                    confidence: 0.85,
                });
            }
        }
    }

    observations
}

// ── Router ─────────────────────────────────────────────────────────

pub fn auto_memory_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auto-memory/status", get(get_status))
        .route("/api/auto-memory/toggle", post(toggle))
        .route("/api/auto-memory/extract", post(extract_now))
        .route("/api/auto-memory/observations", get(list_observations))
}
