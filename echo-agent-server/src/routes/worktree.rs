//! Git worktree management API
//!
//! Provides endpoints for listing, creating, and removing git worktrees.
//! Uses `git worktree` CLI commands under the hood.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/worktrees | List all worktrees |
//! | POST | /api/worktrees | Create a new worktree |
//! | DELETE | /api/worktrees?branch=<branch> | Remove a worktree |

use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use echo_agent_app_core::state::AppState;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub managed: bool,
    pub head: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub base: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveWorktreeQuery {
    pub branch: String,
}

// ── Git worktree helpers ───────────────────────────────────────────────

/// Resolve the workspace root from AppState, falling back to cwd.
async fn workspace_root(state: &AppState) -> PathBuf {
    if let Some(ws) = state.current_workspace().await {
        ws.project_root.unwrap_or(ws.root)
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

/// Parse `git worktree list --porcelain` output into WorktreeInfo records.
fn parse_worktree_list(output: &str, repo_root: &Path) -> Vec<WorktreeInfo> {
    let mut trees = Vec::new();
    let mut current_path = String::new();
    let mut current_head = String::new();
    let mut current_branch = String::new();

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Flush previous entry
            if !current_path.is_empty() {
                let managed = is_managed_path(Path::new(&current_path), repo_root);
                trees.push(WorktreeInfo {
                    path: current_path.clone(),
                    branch: current_branch.clone(),
                    managed,
                    head: current_head.clone(),
                });
            }
            current_path = path.to_string();
            current_head = String::new();
            current_branch = String::new();
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head[..std::cmp::min(head.len(), 8)].to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            // branch refs/heads/xxx → xxx
            current_branch = branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch)
                .to_string();
        } else if line == "detached" {
            current_branch = "(detached)".to_string();
        } else if line.is_empty() {
            // end of entry
        }
    }

    // Flush last entry
    if !current_path.is_empty() {
        let managed = is_managed_path(Path::new(&current_path), repo_root);
        trees.push(WorktreeInfo {
            path: current_path,
            branch: current_branch,
            managed,
            head: current_head,
        });
    }

    trees
}

/// A worktree is "managed" if it lives inside a `.worktrees/` directory
/// adjacent to the repo root (the convention used by `git worktree add`).
fn is_managed_path(path: &Path, repo_root: &Path) -> bool {
    if let Some(parent) = path.parent() {
        if parent.file_name().map(|n| n == ".worktrees").unwrap_or(false) {
            if let Some(grandparent) = parent.parent() {
                return grandparent == repo_root
                    || parent.parent().map(|p| p == repo_root).unwrap_or(false);
            }
        }
        // Also consider paths containing "worktrees" as managed
        if parent.file_name().map(|n| n == "worktrees").unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Run `git worktree list --porcelain` in the given directory.
fn git_worktree_list(repo_root: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree list: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git worktree list failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── Handlers ───────────────────────────────────────────────────────────

/// GET /api/worktrees — list all worktrees
async fn list_worktrees(State(state): State<Arc<AppState>>) -> Response {
    let root = workspace_root(&state).await;

    match git_worktree_list(&root) {
        Ok(output) => {
            let trees = parse_worktree_list(&output, &root);
            Json(trees).into_response()
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// POST /api/worktrees — create a new worktree
async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorktreeRequest>,
) -> Response {
    if req.branch.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "branch name must not be empty" })),
        )
            .into_response();
    }

    let root = workspace_root(&state).await;

    // Place worktrees in .worktrees/ next to repo root
    let worktree_dir = root.join(".worktrees").join(&req.branch);

    // Build the git command:
    //   With base:    git worktree add -b <branch> <path> <base>
    //   Without base: git worktree add -b <branch> <path>
    let mut cmd = Command::new("git");
    cmd.args(["worktree", "add", "-b"])
        .arg(&req.branch)
        .arg(&worktree_dir);

    if let Some(ref base) = req.base {
        cmd.arg(base);
    }

    cmd.current_dir(&root);

    match cmd.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("git worktree add failed: {stderr}") })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to run git: {e}") })),
            )
                .into_response();
        }
    }

    let managed = is_managed_path(&worktree_dir, &root);
    Json(WorktreeInfo {
        path: worktree_dir.to_string_lossy().to_string(),
        branch: req.branch,
        managed,
        head: String::new(),
    })
    .into_response()
}

/// DELETE /api/worktrees?branch=<branch> — remove a worktree
async fn remove_worktree(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RemoveWorktreeQuery>,
) -> Response {
    let root = workspace_root(&state).await;

    // Find the worktree path for the given branch
    let list_output = match git_worktree_list(&root) {
        Ok(o) => o,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    let trees = parse_worktree_list(&list_output, &root);
    let target = trees.iter().find(|t| t.branch == params.branch);

    let target_path = match target {
        Some(t) => PathBuf::from(&t.path),
        None => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Worktree with branch '{}' not found", params.branch)
                })),
            )
                .into_response();
        }
    };

    // Don't allow removing the main worktree
    if target_path == root {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Cannot remove the main worktree"
            })),
        )
            .into_response();
    }

    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&target_path)
        .current_dir(&root)
        .output();

    match output {
        Ok(o) if o.status.success() => Json(serde_json::json!({ "success": true })).into_response(),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("git worktree remove failed: {stderr}") })),
            )
                .into_response()
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to run git: {e}") })),
        )
            .into_response(),
    }
}

// ── Router ─────────────────────────────────────────────────────────────

pub fn worktree_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/worktrees",
            get(list_worktrees).post(create_worktree).delete(remove_worktree),
        )
}
