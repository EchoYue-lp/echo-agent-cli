//! Webhook 管理 API

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;
use crate::webhook::emitter::WebhookEndpoint;

// ── Request/Response types ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddWebhookRequest {
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    pub secret: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WebhookEndpointResponse {
    pub url: String,
    pub events: Vec<String>,
    pub has_secret: bool,
}

#[derive(Debug, Deserialize)]
pub struct RemoveWebhookRequest {
    pub url: String,
}

// ── Handlers ───────────────────────────────────────────────────────

/// GET /api/webhooks — 列出所有 Webhook 端点
#[debug_handler]
pub async fn list_webhooks(
    State(state): State<Arc<AppState>>,
) -> Response {
    let endpoints = state.webhook.emitter.list_endpoints().await;
    let items: Vec<WebhookEndpointResponse> = endpoints
        .into_iter()
        .map(|e| WebhookEndpointResponse {
            url: e.url,
            events: e.events,
            has_secret: e.secret.is_some(),
        })
        .collect();
    Json(items).into_response()
}

/// POST /api/webhooks — 添加 Webhook 端点
#[debug_handler]
pub async fn add_webhook(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddWebhookRequest>,
) -> Response {
    if req.url.is_empty() {
        return Json(serde_json::json!({ "error": "url is required" })).into_response();
    }

    let endpoint = WebhookEndpoint {
        url: req.url,
        events: req.events,
        secret: req.secret,
    };
    state.webhook.emitter.add_endpoint(endpoint).await;
    Json(serde_json::json!({ "success": true })).into_response()
}

/// POST /api/webhooks/remove — 移除 Webhook 端点
#[debug_handler]
pub async fn remove_webhook(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoveWebhookRequest>,
) -> Response {
    let removed = state.webhook.emitter.remove_endpoint(&req.url).await;
    Json(serde_json::json!({ "success": removed })).into_response()
}

/// POST /api/webhooks/test — 发送测试事件
#[debug_handler]
pub async fn test_webhook(
    State(state): State<Arc<AppState>>,
) -> Response {
    state.webhook.emitter.emit(crate::webhook::WebhookEvent::AgentError {
        error: "test event from /api/webhooks/test".to_string(),
    });
    Json(serde_json::json!({ "success": true, "message": "test event emitted" })).into_response()
}
