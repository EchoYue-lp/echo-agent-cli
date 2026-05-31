//! Webhook 管理 API

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
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
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn list_webhooks(State(state): State<Arc<AppState>>) -> Response {
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
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn add_webhook(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddWebhookRequest>,
) -> Response {
    if req.url.is_empty() {
        return Json(serde_json::json!({ "error": "url is required" })).into_response();
    }

    // Security: SSRF protection — reject internal/private URLs
    if let Err(e) = validate_webhook_url(&req.url) {
        return Json(serde_json::json!({ "error": e })).into_response();
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
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn remove_webhook(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoveWebhookRequest>,
) -> Response {
    let removed = state.webhook.emitter.remove_endpoint(&req.url).await;
    Json(serde_json::json!({ "success": removed })).into_response()
}

/// POST /api/webhooks/test — 发送测试事件
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn test_webhook(State(state): State<Arc<AppState>>) -> Response {
    state
        .webhook
        .emitter
        .emit(crate::webhook::WebhookEvent::AgentError {
            error: "test event from /api/webhooks/test".to_string(),
        });
    Json(serde_json::json!({ "success": true, "message": "test event emitted" })).into_response()
}

/// Validate webhook URL to prevent SSRF attacks.
/// Rejects internal/private IP ranges, metadata endpoints, and localhost.
fn validate_webhook_url(url_str: &str) -> Result<(), String> {
    let url = url::Url::parse(url_str).map_err(|e| format!("Invalid URL: {}", e))?;

    // Only allow http and https schemes
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Unsupported scheme: {}", other)),
    }

    let host = url.host_str().ok_or("URL has no host")?;

    // Reject localhost variants
    let host_lower = host.to_lowercase();
    if matches!(
        host_lower.as_str(),
        "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "[::1]"
    ) {
        return Err("Webhook URL cannot point to localhost".to_string());
    }

    // Reject AWS/GCP/Azure metadata endpoints
    if matches!(
        host_lower.as_str(),
        "169.254.169.254" | "metadata.google.internal" | "metadata.azure.com" | "100.100.100.200"
    ) {
        return Err("Webhook URL cannot point to cloud metadata service".to_string());
    }

    // Reject private IP ranges (basic check)
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(ipv4) => {
                if ipv4.is_private()
                    || ipv4.is_loopback()
                    || ipv4.is_link_local()
                    || ipv4.is_broadcast()
                    || ipv4.is_unspecified()
                {
                    return Err("Webhook URL cannot point to a private/internal IP".to_string());
                }
            }
            std::net::IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() || ipv6.is_unspecified() {
                    return Err("Webhook URL cannot point to a private/internal IP".to_string());
                }
            }
        }
    }

    Ok(())
}
