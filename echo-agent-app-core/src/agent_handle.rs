//! AgentHandle — re-export from the framework.
//!
//! The framework's `AgentHandle` wraps `Arc<RwLock<ReactAgent>>` and provides
//! scoped access without requiring callers to manage locks directly.
//! Execution serialization is handled internally by `ReactAgent`'s
//! `execution_mutex` — no external mutex needed.

pub use echo_agent::agent::AgentHandle;
