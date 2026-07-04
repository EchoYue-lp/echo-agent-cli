//! HitlDispatcher — fan-out to multiple HumanLoopProviders.
//!
//! Routes approval/input requests to registered providers in order.
//! First responder wins. Supports dynamic registration/unregistration
//! of providers as interfaces connect and disconnect.

use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Named provider entry.
struct NamedProvider {
    name: String,
    provider: Arc<dyn HumanLoopProvider>,
}

/// Dispatcher that routes HITL requests to the first available provider.
///
/// Providers are tried in registration order. The first to respond wins.
/// If no provider responds within the timeout (5 minutes), returns Timeout.
pub struct HitlDispatcher {
    providers: RwLock<Vec<NamedProvider>>,
}

impl HitlDispatcher {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(Vec::new()),
        }
    }

    /// Register a new provider with a name.
    pub async fn register(&self, name: impl Into<String>, provider: Arc<dyn HumanLoopProvider>) {
        let name = name.into();
        let mut providers = self.providers.write().await;
        providers.push(NamedProvider {
            name: name.clone(),
            provider,
        });
        tracing::debug!(provider = %name, "HITL provider registered");
    }

    /// Unregister a provider by name.
    pub async fn unregister(&self, name: &str) {
        let mut providers = self.providers.write().await;
        providers.retain(|p| p.name != name);
        tracing::debug!(provider = %name, "HITL provider unregistered");
    }

    /// Get the number of registered providers.
    pub async fn provider_count(&self) -> usize {
        self.providers.read().await.len()
    }
}

impl Default for HitlDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HumanLoopProvider for HitlDispatcher {
    fn request(
        &self,
        req: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
        Box::pin(async move {
            let providers = self.providers.read().await;

            if providers.is_empty() {
                tracing::warn!("HITL request with no providers registered, auto-rejecting");
                return Ok(HumanLoopResponse::Rejected {
                    reason: Some("No HITL provider available".to_string()),
                });
            }

            // F5-2: 此前是串行 for 循环 + 每 provider 独立 5min 超时 → N 个 provider
            // 最坏 N×5min。文件头注释本就写"First responder wins", 现在补齐实现。
            //
            // 改为并行广播 + first-response-wins(对标 GitHub "any approve"、HITL
            // 多渠道冗余、Temporal first-input-wins):所有 provider 同时收到请求,
            // 第一个**实质性响应**(Ok, 含 approve/reject/modify)即采纳并取消其余;
            // 单个 provider 报 Err(连接断开等)不立即 reject, 继续等其余;
            // 全部 Err/超时才 reject(default deny)。总超时上限 = 1×5min 而非 N×5min。
            const TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

            use futures::stream::{FuturesUnordered, StreamExt};
            let mut pending: FuturesUnordered<_> = providers
                .iter()
                .map(|named| {
                    let fut = named.provider.request(req.clone());
                    let name = named.name.clone();
                    async move {
                        let result = tokio::time::timeout(TIMEOUT_DURATION, fut).await;
                        (name, result)
                    }
                })
                .collect();

            let mut failures: Vec<String> = Vec::new();
            while let Some((name, outcome)) = pending.next().await {
                match outcome {
                    Ok(Ok(response)) => {
                        tracing::info!(
                            provider = %name,
                            "HITL request resolved by first responder"
                        );
                        return Ok(response);
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(
                            provider = %name,
                            error = %e,
                            "HITL provider failed, waiting on others"
                        );
                        failures.push(format!("{name}: {e}"));
                    }
                    Err(_) => {
                        tracing::warn!(
                            provider = %name,
                            "HITL provider timed out after 5 minutes, waiting on others"
                        );
                        failures.push(format!("{name}: timeout"));
                    }
                }
            }

            // All providers failed or timed out — fail-closed (default deny).
            Ok(HumanLoopResponse::Rejected {
                reason: Some(format!(
                    "All HITL providers failed or timed out ({})",
                    failures.join("; ")
                )),
            })
        })
    }
}
