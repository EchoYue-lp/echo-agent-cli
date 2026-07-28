//! Webhook 事件发射器
//!
//! 非阻塞 HTTP POST，失败仅 warn，1 次重试，支持 HMAC-SHA256 签名。
//!
//! ## 单例策略
//!
//! 没有 global singleton。`WebhookEmitter` 通过 `AppState.webhook.emitter`
//! (GUI/TUI/channel) 或 `ReplConfig.webhook_emitter` (CLI) 注入。一个进程
//! 内可以有多个独立 emitter，但通常只有一个，从 `AppConfig.webhooks` 构建。
//! 之前的 `init_global` / `emit_global` / `global_emitter` 被移除，因为
//! `init_global` 从未被调用 → 全局 emitter 永远没有 endpoints → 之前的
//! `emit_global(...)` 调用全是 no-op，掩盖了"webhook 实际上没生效"的真实状态。

use std::sync::Arc;
use tokio::sync::RwLock;

use super::events::{WebhookEvent, WebhookPayload};

/// 单个 Webhook 端点配置
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    /// 回调 URL
    pub url: String,
    /// 订阅的事件类型列表（空 = 所有事件）
    pub events: Vec<String>,
    /// HMAC-SHA256 签名密钥（可选）
    pub secret: Option<String>,
}

/// Webhook 发射器
#[derive(Debug, Clone)]
pub struct WebhookEmitter {
    endpoints: Arc<RwLock<Vec<WebhookEndpoint>>>,
    client: reqwest::Client,
}

impl Default for WebhookEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookEmitter {
    /// 创建空的发射器
    pub fn new() -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(Vec::new())),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 从端点列表创建
    pub fn with_endpoints(endpoints: Vec<WebhookEndpoint>) -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(endpoints)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// True when at least one endpoint is registered.
    pub async fn has_endpoints(&self) -> bool {
        !self.endpoints.read().await.is_empty()
    }

    /// 添加端点（运行时动态添加）
    pub async fn add_endpoint(&self, endpoint: WebhookEndpoint) {
        self.endpoints.write().await.push(endpoint);
    }

    /// 移除端点
    pub async fn remove_endpoint(&self, url: &str) -> bool {
        let mut guard = self.endpoints.write().await;
        let before = guard.len();
        guard.retain(|e| e.url != url);
        guard.len() != before
    }

    /// 列出所有端点
    pub async fn list_endpoints(&self) -> Vec<WebhookEndpoint> {
        self.endpoints.read().await.clone()
    }

    /// 发射事件（非阻塞 fire-and-forget）。
    ///
    /// 没有注册端点时立即返回，避免无谓的 spawn。
    pub fn emit(&self, event: WebhookEvent) {
        let endpoints = self.endpoints.clone();
        let client = self.client.clone();
        let event_name = event.event_name().to_string();

        tokio::spawn(async move {
            let guard = endpoints.read().await;
            if guard.is_empty() {
                return;
            }
            let payload = WebhookPayload {
                event: event_name,
                timestamp: chrono::Utc::now(),
                data: event,
            };
            let body = match serde_json::to_vec(&payload) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("Webhook: failed to serialize payload: {e}");
                    return;
                }
            };

            for endpoint in guard.iter() {
                // Filter: skip if events list is non-empty and doesn't include this event
                if !endpoint.events.is_empty()
                    && !endpoint.events.iter().any(|e| e == &payload.event)
                {
                    continue;
                }

                let url = endpoint.url.clone();
                let secret = endpoint.secret.clone();
                let client = client.clone();
                let body = body.clone();

                tokio::spawn(async move {
                    if let Err(e) = deliver(&client, &url, &secret, &body).await {
                        tracing::warn!("Webhook delivery failed to {url}: {e}, retrying...");
                        // 1 重试
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        if let Err(e) = deliver(&client, &url, &secret, &body).await {
                            tracing::warn!("Webhook retry failed to {url}: {e}");
                        }
                    }
                });
            }
        });
    }
}

/// 执行一次 HTTP POST 投递
async fn deliver(
    client: &reqwest::Client,
    url: &str,
    secret: &Option<String>,
    body: &[u8],
) -> Result<(), String> {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_vec());

    // HMAC-SHA256 签名
    if let Some(secret) = secret {
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<sha2::Sha256>;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| format!("HMAC init failed: {e}"))?;
        mac.update(body);
        let sig = mac.finalize().into_bytes();
        let sig_hex = hex::encode(sig);
        req = req.header("X-Webhook-Signature", format!("sha256={}", sig_hex));
    }

    let resp = req.send().await.map_err(|e| format!("{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(())
}
