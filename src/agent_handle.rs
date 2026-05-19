//! AgentHandle — scoped access wrapper around the shared agent.
//!
//! Replaces `Arc<RwLock<ReactAgent>>` so that callers in the CLI never lock
//! or import `ReactAgent` directly.  The handle provides:
//!
//! | Method | Lock | Closure |
//! |--------|------|---------|
//! | [`read`](AgentHandle::read) | read (async) | sync |
//! | [`read_async`](AgentHandle::read_async) | read (async) | async (boxed) |
//! | [`write`](AgentHandle::write) | write (async) | sync |
//! | [`write_async`](AgentHandle::write_async) | write (async) | async (boxed) |
//! | [`try_write`](AgentHandle::try_write) | write (try) | sync |
//! | [`inner`](AgentHandle::inner) | none | escape hatch |

use echo_agent::prelude::ReactAgent;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Scoped-access handle for a shared [`ReactAgent`].
///
/// Clone is cheap (Arc clone).
#[derive(Clone)]
pub struct AgentHandle {
    agent: Arc<RwLock<ReactAgent>>,
}

impl AgentHandle {
    /// Wrap an existing `Arc<RwLock<ReactAgent>>`.
    pub fn from_arc(agent: Arc<RwLock<ReactAgent>>) -> Self {
        Self { agent }
    }

    /// Build from an owned `ReactAgent`.
    pub fn new(agent: ReactAgent) -> Self {
        Self {
            agent: Arc::new(RwLock::new(agent)),
        }
    }

    /// Escape hatch — return the inner `Arc<RwLock<ReactAgent>>`.
    pub fn inner(&self) -> &Arc<RwLock<ReactAgent>> {
        &self.agent
    }

    /// Acquire read lock, run a **sync** closure, return its result.
    pub async fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ReactAgent) -> R,
    {
        let guard = self.agent.read().await;
        f(&guard)
    }

    /// Acquire read lock, run a closure that returns a boxed future,
    /// and `.await` that future while still holding the lock.
    pub async fn read_async<F, R>(&self, f: F) -> R
    where
        F: for<'a> FnOnce(&'a ReactAgent) -> Pin<Box<dyn Future<Output = R> + Send + 'a>>,
    {
        let guard = self.agent.read().await;
        f(&guard).await
    }

    /// Acquire write lock, run a **sync** closure, return its result.
    pub async fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut ReactAgent) -> R,
    {
        let mut guard = self.agent.write().await;
        f(&mut guard)
    }

    /// Acquire write lock, run a closure that returns a boxed future,
    /// and `.await` that future while still holding the lock.
    pub async fn write_async<F, R>(&self, f: F) -> R
    where
        F: for<'a> FnOnce(&'a mut ReactAgent) -> Pin<Box<dyn Future<Output = R> + Send + 'a>>,
    {
        let mut guard = self.agent.write().await;
        f(&mut guard).await
    }

    /// Non-blocking write attempt — returns `None` if the lock is
    /// already held by another caller.
    pub fn try_write<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut ReactAgent) -> R,
    {
        let mut guard = self.agent.try_write().ok()?;
        Some(f(&mut guard))
    }
}
