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
            // Snapshot providers and drop the read guard immediately so that
            // provider registration/unregistration is not blocked for the entire
            // (up to 5-minute) request duration. The previous implementation
            // held `self.providers.read().await` across the whole `while` loop,
            // deadlocking any concurrent `register`/`unregister` call.
            let providers: Vec<(String, Arc<dyn HumanLoopProvider>)> = {
                let guard = self.providers.read().await;
                guard
                    .iter()
                    .map(|named| (named.name.clone(), named.provider.clone()))
                    .collect()
            };

            if providers.is_empty() {
                tracing::warn!("HITL request with no providers registered, auto-rejecting");
                return Ok(HumanLoopResponse::Rejected {
                    reason: Some("No HITL provider available".to_string()),
                });
            }

            // Parallel broadcast + first-response-wins. All providers receive
            // the request concurrently; the first Ok (approve/reject/modify)
            // wins and the remaining futures are cancelled by dropping
            // `pending`. A single Err (connection drop) does not reject
            // immediately — we keep waiting on the others. Only when all have
            // failed/timed out does the dispatcher fail-closed (default deny).
            //
            // Single shared deadline: every provider's individual timeout is
            // computed against ONE start instant, so the total wall-clock wait
            // is bounded by `TIMEOUT_DURATION` regardless of how many providers
            // are registered (the previous per-provider timeout was roughly
            // equivalent but not strict — this version is).
            const TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);
            let deadline = tokio::time::Instant::now() + TIMEOUT_DURATION;

            // Give each provider its own clone of the request so the dispatch
            // futures can each be `'static` and driven independently inside a
            // `FuturesUnordered` (the per-iteration `FnMut` cannot move `req`
            // once, so we pre-clone here).
            let prepared: Vec<(String, Arc<dyn HumanLoopProvider>, HumanLoopRequest)> = providers
                .into_iter()
                .map(|(name, provider)| (name, provider, req.clone()))
                .collect();

            use futures::stream::{FuturesUnordered, StreamExt};
            let mut pending: FuturesUnordered<_> = prepared
                .into_iter()
                .map(|(name, provider, req_for_provider)| async move {
                    let fut = provider.request(req_for_provider);
                    // `timeout_at` returns `Err(Elapsed)` immediately if the
                    // deadline is already in the past — consistent timeout
                    // reporting without a separate per-future branch.
                    let result = tokio::time::timeout_at(deadline, fut).await;
                    (name, result)
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
                        // Returning drops `pending`, which cancels the remaining
                        // provider futures (their `JoinHandle`-like semantics
                        // come from the `Future` being polled inside the stream).
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
                            "HITL provider hit the shared deadline, waiting on others"
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
