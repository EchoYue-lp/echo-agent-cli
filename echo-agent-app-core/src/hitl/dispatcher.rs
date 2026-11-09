//! HitlDispatcher — fan-out to multiple HumanLoopProviders.
//!
//! Routes approval/input requests to registered providers in order.
//! First responder wins. Supports dynamic registration/unregistration
//! of providers as interfaces connect and disconnect.

use super::{HitlRequestScope, PairedHumanLoopProvider, PairedHumanLoopRequest, pair_request};
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use std::sync::{Arc, RwLock, Weak};

#[derive(Clone)]
enum ProviderEntry {
    Legacy(Arc<dyn HumanLoopProvider>),
    Paired(Arc<dyn PairedHumanLoopProvider>),
}

impl ProviderEntry {
    fn request(
        &self,
        request: HumanLoopRequest,
        identity: Option<super::HitlRequestIdentity>,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
        match self {
            Self::Legacy(provider) => provider.request(request),
            Self::Paired(provider) => match identity {
                Some(identity) => {
                    provider.request_paired(PairedHumanLoopRequest { request, identity })
                }
                None => Box::pin(async {
                    Err(echo_agent::error::ReactError::Other(
                        "Paired HITL provider requires its original identity".to_string(),
                    ))
                }),
            },
        }
    }
}

/// Named provider entry.
struct NamedProvider {
    registration_id: uuid::Uuid,
    name: String,
    provider: ProviderEntry,
}

/// Exact ownership receipt for one dynamically registered HITL provider.
///
/// Dropping the receipt synchronously removes only its own registration, so an
/// aborted surface cannot leave a closed provider behind or unregister a newer
/// provider that reused the same display name.
#[must_use]
pub struct HitlProviderRegistration {
    dispatcher: Weak<HitlDispatcher>,
    registration_id: Option<uuid::Uuid>,
}

impl HitlProviderRegistration {
    pub fn unregister(mut self) {
        self.unregister_inner();
    }

    fn unregister_inner(&mut self) {
        let Some(registration_id) = self.registration_id.take() else {
            return;
        };
        if let Some(dispatcher) = self.dispatcher.upgrade() {
            dispatcher.unregister_exact(registration_id);
        }
    }
}

impl Drop for HitlProviderRegistration {
    fn drop(&mut self) {
        self.unregister_inner();
    }
}

/// Dispatcher that routes HITL requests to the first available provider.
///
/// Providers are tried in registration order. The first to respond wins.
/// If no provider responds within the timeout (5 minutes), returns Timeout.
pub struct HitlDispatcher {
    providers: RwLock<Vec<NamedProvider>>,
    scope: HitlRequestScope,
    scope_resolver: Option<super::HitlScopeResolver>,
}

impl HitlDispatcher {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(Vec::new()),
            scope: HitlRequestScope::default(),
            scope_resolver: None,
        }
    }

    /// Use one invocation's original owner scope for a GUI+IM combination.
    pub fn for_scope(scope: HitlRequestScope) -> Self {
        Self {
            providers: RwLock::new(Vec::new()),
            scope,
            scope_resolver: None,
        }
    }

    pub fn with_scope_resolver(mut self, resolver: super::HitlScopeResolver) -> Self {
        self.scope_resolver = Some(resolver);
        self
    }

    pub async fn register_paired_owned(
        self: &Arc<Self>,
        name: impl Into<String>,
        provider: Arc<dyn PairedHumanLoopProvider>,
    ) -> HitlProviderRegistration {
        let registration_id = uuid::Uuid::new_v4();
        self.providers
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .push(NamedProvider {
                registration_id,
                name: name.into(),
                provider: ProviderEntry::Paired(provider),
            });
        HitlProviderRegistration {
            dispatcher: Arc::downgrade(self),
            registration_id: Some(registration_id),
        }
    }

    /// Register a new provider with a name.
    pub async fn register(&self, name: impl Into<String>, provider: Arc<dyn HumanLoopProvider>) {
        let name = name.into();
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        providers.push(NamedProvider {
            registration_id: uuid::Uuid::new_v4(),
            name: name.clone(),
            provider: ProviderEntry::Legacy(provider),
        });
        tracing::debug!(provider = %name, "HITL provider registered");
    }

    /// Register a provider whose lifetime is controlled by an exact receipt.
    pub async fn register_owned(
        self: &Arc<Self>,
        name: impl Into<String>,
        provider: Arc<dyn HumanLoopProvider>,
    ) -> HitlProviderRegistration {
        let name = name.into();
        let registration_id = uuid::Uuid::new_v4();
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        providers.push(NamedProvider {
            registration_id,
            name: name.clone(),
            provider: ProviderEntry::Legacy(provider),
        });
        drop(providers);
        tracing::debug!(provider = %name, %registration_id, "owned HITL provider registered");
        HitlProviderRegistration {
            dispatcher: Arc::downgrade(self),
            registration_id: Some(registration_id),
        }
    }

    /// Unregister a provider by name.
    pub async fn unregister(&self, name: &str) {
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        providers.retain(|p| p.name != name);
        tracing::debug!(provider = %name, "HITL provider unregistered");
    }

    fn unregister_exact(&self, registration_id: uuid::Uuid) {
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_len = providers.len();
        providers.retain(|provider| provider.registration_id != registration_id);
        if providers.len() != previous_len {
            tracing::debug!(%registration_id, "owned HITL provider unregistered");
        }
    }

    /// Get the number of registered providers.
    pub async fn provider_count(&self) -> usize {
        self.providers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
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
            let providers: Vec<(String, ProviderEntry)> = {
                let guard = self
                    .providers
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            let request_kind = req.kind.clone();
            let uses_pair = providers
                .iter()
                .any(|(_, provider)| matches!(provider, ProviderEntry::Paired(_)));
            let duration = if uses_pair {
                req.timeout
                    .unwrap_or(TIMEOUT_DURATION)
                    .min(TIMEOUT_DURATION)
            } else {
                TIMEOUT_DURATION
            };
            let deadline = tokio::time::Instant::now()
                .checked_add(duration)
                .ok_or_else(|| {
                    echo_agent::error::ReactError::Other("HITL deadline unavailable".to_string())
                })?;
            // Generic-only legacy registration keeps the original request
            // bytes and behavior. Only the EKO paired route creates a target.
            let (request, identity) = if uses_pair {
                let scope = match self.scope_resolver.as_ref() {
                    Some(resolve) => resolve()?,
                    None => self.scope.clone().with_current_runtime(),
                };
                let paired = pair_request(req, scope)?;
                (paired.request, Some(paired.identity))
            } else {
                (req, None)
            };

            // Give each provider its own clone of the request so the dispatch
            // futures can each be `'static` and driven independently inside a
            // `FuturesUnordered` (the per-iteration `FnMut` cannot move `req`
            // once, so we pre-clone here).
            let prepared: Vec<_> = providers
                .into_iter()
                .map(|(name, provider)| (name, provider, request.clone(), identity.clone()))
                .collect();

            use futures::stream::{FuturesUnordered, StreamExt};
            let mut pending: FuturesUnordered<_> = prepared
                .into_iter()
                .map(|(name, provider, req_for_provider, identity)| async move {
                    let fut = provider.request(req_for_provider, identity);
                    // `timeout_at` returns `Err(Elapsed)` immediately if the
                    // deadline is already in the past — consistent timeout
                    // reporting without a separate per-future branch.
                    let result = tokio::time::timeout_at(deadline, fut).await;
                    (name, result)
                })
                .collect();

            let mut failures: Vec<String> = Vec::new();
            let mut unanswered = false;
            while let Some((name, outcome)) = pending.next().await {
                match outcome {
                    Ok(Ok(HumanLoopResponse::Timeout | HumanLoopResponse::Deferred))
                        if uses_pair =>
                    {
                        unanswered = uses_pair;
                    }
                    Ok(Ok(response))
                        if uses_pair
                            && !matches!(
                                (&request_kind, &response),
                                (
                                    echo_agent::human_loop::HumanLoopKind::Approval,
                                    HumanLoopResponse::Approved
                                        | HumanLoopResponse::ApprovedWithScope { .. }
                                        | HumanLoopResponse::ModifiedArgs { .. }
                                        | HumanLoopResponse::Rejected { .. }
                                ) | (
                                    echo_agent::human_loop::HumanLoopKind::Input,
                                    HumanLoopResponse::Text(_) | HumanLoopResponse::Rejected { .. }
                                ) | (
                                    echo_agent::human_loop::HumanLoopKind::Selection,
                                    HumanLoopResponse::Selection { .. }
                                        | HumanLoopResponse::Rejected { .. }
                                )
                            ) =>
                    {
                        failures.push(format!("{name}: response kind mismatch"));
                    }
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
                        unanswered = uses_pair;
                        tracing::warn!(
                            provider = %name,
                            "HITL provider hit the shared deadline, waiting on others"
                        );
                        failures.push(format!("{name}: timeout"));
                    }
                }
            }

            if unanswered {
                tokio::time::sleep_until(deadline).await;
                return Ok(HumanLoopResponse::Timeout);
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

#[cfg(test)]
mod paired_tests {
    use super::*;
    use std::sync::Mutex;

    struct PairedFixture {
        observed: Arc<Mutex<Vec<super::super::HitlRequestIdentity>>>,
        response: HumanLoopResponse,
        delay: std::time::Duration,
    }
    impl PairedHumanLoopProvider for PairedFixture {
        fn request_paired(
            &self,
            request: PairedHumanLoopRequest,
        ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
            Box::pin(async move {
                request.validate()?;
                self.observed
                    .lock()
                    .map_err(|_| {
                        echo_agent::error::ReactError::Other("fixture lock poisoned".to_string())
                    })?
                    .push(request.identity);
                tokio::time::sleep(self.delay).await;
                Ok(self.response.clone())
            })
        }
    }

    #[tokio::test]
    async fn paired_timeout_or_deferred_cannot_beat_one_effective_human_answer()
    -> Result<(), Box<dyn std::error::Error>> {
        for ignored in [
            HumanLoopResponse::Timeout,
            HumanLoopResponse::Deferred,
            HumanLoopResponse::Text("wrong kind".to_string()),
        ] {
            let dispatcher = Arc::new(HitlDispatcher::for_scope(HitlRequestScope::default()));
            let observed = Arc::new(Mutex::new(Vec::new()));
            let _first = dispatcher
                .register_paired_owned(
                    "first",
                    Arc::new(PairedFixture {
                        observed: observed.clone(),
                        response: ignored,
                        delay: std::time::Duration::ZERO,
                    }),
                )
                .await;
            let _second = dispatcher
                .register_paired_owned(
                    "second",
                    Arc::new(PairedFixture {
                        observed: observed.clone(),
                        response: HumanLoopResponse::Approved,
                        delay: std::time::Duration::from_millis(1),
                    }),
                )
                .await;
            let mut request =
                HumanLoopRequest::approval("read_file", serde_json::json!({"path":"x"}));
            request.request_id = Some("original-id".to_string());
            request.timeout = Some(std::time::Duration::from_secs(1));
            assert!(matches!(
                dispatcher.request(request).await?,
                HumanLoopResponse::Approved
            ));
            let pairs = observed.lock().map_err(|_| "fixture lock poisoned")?;
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs.first(), pairs.get(1));
            assert_eq!(
                pairs.first().ok_or("missing pair")?.request_id,
                "original-id"
            );
        }
        Ok(())
    }

    struct LegacyFixture;
    impl HumanLoopProvider for LegacyFixture {
        fn request(
            &self,
            request: HumanLoopRequest,
        ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
            Box::pin(async move {
                assert!(request.request_id.is_none());
                assert_eq!(request.context, Some(serde_json::json!({"original":true})));
                Ok(HumanLoopResponse::Deferred)
            })
        }
    }
    #[tokio::test]
    async fn generic_only_registration_does_not_pair_or_change_legacy_request()
    -> Result<(), echo_agent::error::ReactError> {
        let dispatcher = HitlDispatcher::new();
        dispatcher.register("legacy", Arc::new(LegacyFixture)).await;
        let mut request = HumanLoopRequest::input("input");
        request.context = Some(serde_json::json!({"original":true}));
        assert!(matches!(
            dispatcher.request(request).await?,
            HumanLoopResponse::Deferred
        ));
        Ok(())
    }
}
