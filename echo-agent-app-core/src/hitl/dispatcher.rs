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

            // Try each provider in order — first to respond wins
            // with a 5-minute timeout per provider
            const TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

            for named in providers.iter() {
                let provider_result =
                    tokio::time::timeout(TIMEOUT_DURATION, named.provider.request(req.clone()))
                        .await;

                match provider_result {
                    Ok(Ok(response)) => return Ok(response),
                    Ok(Err(e)) => {
                        tracing::debug!(
                            provider = %named.name,
                            error = %e,
                            "HITL provider failed, trying next"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            provider = %named.name,
                            "HITL provider timed out after 5 minutes, trying next"
                        );
                    }
                }
            }

            // All providers failed or timed out
            Ok(HumanLoopResponse::Rejected {
                reason: Some("All HITL providers failed or timed out".to_string()),
            })
        })
    }
}
