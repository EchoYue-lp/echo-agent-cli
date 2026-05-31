//! 终端管理 API
//!
//! 提供 PTY 终端会话的创建、管理和 WebSocket 连接。
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | POST | /api/terminal | Create a new terminal session |
//! | GET | /api/terminal | List active terminal sessions |
//! | DELETE | /api/terminal/:id | Close a terminal session |
//! | GET | /api/terminal/:id/ws | WebSocket for terminal I/O |

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{delete, get, post},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use echo_agent_app_core::state::AppState;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TerminalSession {
    pub id: String,
    pub cwd: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateTerminalRequest {
    pub cwd: Option<String>,
}

// ── State ──────────────────────────────────────────────────────────────

/// Terminal session manager — tracks active sessions.
pub struct TerminalManager {
    sessions: RwLock<HashMap<String, TerminalInfo>>,
}

struct TerminalInfo {
    id: String,
    cwd: String,
    created_at: String,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create(&self, cwd: Option<String>) -> TerminalSession {
        let id = uuid::Uuid::new_v4().to_string();
        let cwd = cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        let session = TerminalSession {
            id: id.clone(),
            cwd: cwd.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let mut sessions = self.sessions.write().await;
        sessions.insert(
            id.clone(),
            TerminalInfo {
                id,
                cwd,
                created_at: session.created_at.clone(),
            },
        );

        session
    }

    pub async fn list(&self) -> Vec<TerminalSession> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .map(|info| TerminalSession {
                id: info.id.clone(),
                cwd: info.cwd.clone(),
                created_at: info.created_at.clone(),
            })
            .collect()
    }

    pub async fn remove(&self, id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        sessions.remove(id).is_some()
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static::lazy_static! {
    static ref TERMINAL_MANAGER: TerminalManager = TerminalManager::new();
}

// ── Handlers ───────────────────────────────────────────────────────────

/// POST /api/terminal — create a new terminal session
async fn create_terminal(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<CreateTerminalRequest>,
) -> Json<TerminalSession> {
    let session = TERMINAL_MANAGER.create(req.cwd).await;
    Json(session)
}

/// GET /api/terminal — list active terminal sessions
async fn list_terminals(State(_state): State<Arc<AppState>>) -> Json<Vec<TerminalSession>> {
    let sessions = TERMINAL_MANAGER.list().await;
    Json(sessions)
}

/// DELETE /api/terminal/:id — close a terminal session
async fn delete_terminal(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    TERMINAL_MANAGER.remove(&id).await;
    Json(serde_json::json!({ "closed": id }))
}

/// WebSocket endpoint for terminal I/O.
///
/// Protocol:
/// - Client sends: `{ "type": "input", "data": "..." }` or `{ "type": "resize", "cols": N, "rows": N }`
/// - Server sends: `{ "type": "output", "data": "..." }`
async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> impl IntoResponse {
    ws.on_upgrade(|mut socket| async move {
        use axum::extract::ws::Message;

        // Send welcome banner
        let welcome = serde_json::json!({
            "type": "output",
            "data": "Welcome to Echo Agent Terminal\r\n$ "
        });
        let _ = socket.send(Message::Text(welcome.to_string())).await;

        while let Some(Ok(msg)) = socket.next().await {
            if let Message::Text(text) = msg {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    match parsed["type"].as_str() {
                        Some("input") => {
                            let input = parsed["data"].as_str().unwrap_or("");
                            // Echo back the input (in production, connect to a real PTY)
                            let echo = serde_json::json!({
                                "type": "output",
                                "data": input
                            });
                            if socket.send(Message::Text(echo.to_string())).await.is_err() {
                                break;
                            }
                        }
                        Some("resize") => {
                            // Acknowledge resize; no-op without a real PTY
                            tracing::debug!(
                                cols = parsed["cols"].as_u64(),
                                rows = parsed["rows"].as_u64(),
                                "terminal resize request"
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    })
}

// ── Router ─────────────────────────────────────────────────────────────

pub fn terminal_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/terminal", post(create_terminal).get(list_terminals))
        .route("/api/terminal/:id", delete(delete_terminal))
        .route("/api/terminal/:id/ws", get(terminal_ws))
}
