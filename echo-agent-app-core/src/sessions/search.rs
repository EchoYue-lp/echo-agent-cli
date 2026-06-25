//! Session search — delegates to the in-memory engine in [`crate::conversation_file`].
//!
//! U1c: EKO is local — no SQLite/FTS5. The actual implementation lives in
//! `conversation_file::SessionSearchEngine` (an in-memory substring index that
//! replaces the old FTS5 virtual table). This module re-exports it under the
//! historical `sessions::SessionSearchEngine` path so existing call sites
//! (`AppState.search_engine`, Tauri commands) stay unchanged.

pub use crate::conversation_file::{SessionSearchEngine, SessionSearchResult as SearchResult};
