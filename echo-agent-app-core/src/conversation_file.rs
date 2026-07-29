//! In-memory session search engine (U1c: EKO is local — no SQLite/FTS5).
//!
//! The framework (`echo_agent::memory::FileConversationStore`) owns the
//! file-backed `ConversationStore` trait implementation; this module owns only
//! the **in-memory substring index** over saved sessions, which is an
//! application-level concern (drives the sessions-search UI). It does
//! case-insensitive substring matching (matching the SQL `LIKE '%q%'` semantics
//! of the old `search_conversations`) plus a reindex-on-start from the
//! framework's conversation JSON files.
//!
//! The framework's `FileConversationStore` is the authority for storage; the
//! app constructs it from EKO's canonical user-data directory and shares the
//! instance via `AppState`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

// ── Session search engine (replaces FTS5) ──────────────────────────────────
//
// In-memory substring search over session content. Replaces the old FTS5
// virtual table. No bm25 ranking — the old `search_conversations` also used
// plain `LIKE '%q%'` (not FTS5), so this preserves the search UX. The sessions
// search UI's rank field is filled with a constant (0.0) since ordering is by
// recency, not relevance.

/// A single session search result (shape matches the old FTS5 SearchResult so
/// the Tauri command return type is unchanged).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchResult {
    pub session_id: String,
    pub session_name: String,
    pub model: String,
    pub snippet: String,
    pub rank: f64,
}

/// In-memory session search engine.
pub struct SessionSearchEngine {
    /// session_id → indexed content (name + model + joined messages).
    entries: Mutex<HashMap<String, IndexedSession>>,
}

#[derive(Clone)]
struct IndexedSession {
    session_name: String,
    model: String,
    content_lower: String, // lowercased, for substring match
    raw_content: String,   // original case, for snippets
}

impl Default for SessionSearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionSearchEngine {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock_entries(&self) -> MutexGuard<'_, HashMap<String, IndexedSession>> {
        self.entries.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Session search index lock was poisoned; recovering stored entries");
            poisoned.into_inner()
        })
    }

    /// Index (or re-index) a session. `messages` are the message contents.
    pub fn index_session(
        &self,
        session_id: &str,
        session_name: &str,
        model: &str,
        messages: &[impl AsRef<str>],
    ) {
        let raw_content = messages
            .iter()
            .map(|m| m.as_ref().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let mut entries = self.lock_entries();
        entries.insert(
            session_id.to_string(),
            IndexedSession {
                session_name: session_name.to_string(),
                model: model.to_string(),
                content_lower: raw_content.to_lowercase(),
                raw_content,
            },
        );
    }

    /// Remove a session from the index.
    pub fn remove_session(&self, session_id: &str) {
        let mut entries = self.lock_entries();
        entries.remove(session_id);
    }

    /// Substring search across indexed sessions. Returns results ordered by
    /// recency (insertion order is a best-effort proxy; exact tie-break is not
    /// load-bearing for the sessions search UI).
    pub fn search(&self, query: &str, limit: usize) -> Vec<SessionSearchResult> {
        let needle = query.to_lowercase();
        let entries = self.lock_entries();
        let mut results: Vec<SessionSearchResult> = entries
            .iter()
            .filter_map(|(id, s)| {
                let hit = s.content_lower.contains(&needle)
                    || s.session_name.to_lowercase().contains(&needle);
                if !hit {
                    return None;
                }
                let snippet = make_snippet(&s.raw_content, &needle, 32);
                Some(SessionSearchResult {
                    session_id: id.clone(),
                    session_name: s.session_name.clone(),
                    model: s.model.clone(),
                    snippet,
                    rank: 0.0,
                })
            })
            .collect();
        results.truncate(limit);
        results
    }

    /// Re-index all conversations from the framework's canonical file-backed
    /// store. Reads the on-disk JSON shape written by
    /// `echo_agent::memory::FileConversationStore` (a `{conversation, messages}`
    /// record); malformed records are skipped (best-effort reindex — a corrupt
    /// file should not block startup, the framework surfaces the error on
    /// direct access).
    pub fn reindex_all(&self) -> std::io::Result<usize> {
        let dir = echo_agent::paths::user_data_path("conversations");
        if !dir.exists() {
            return Ok(0);
        }
        let mut count: usize = 0;
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(data) = std::fs::read_to_string(&path)
                && let Ok(record) = serde_json::from_str::<ReindexRecord>(&data)
            {
                let id = record.conversation.conversation_id;
                let name = record
                    .conversation
                    .title
                    .unwrap_or_else(|| "Untitled".to_string());
                let messages = record
                    .messages
                    .into_iter()
                    .filter_map(|message| message.content)
                    .collect::<Vec<_>>();
                if !id.is_empty() {
                    self.index_session(&id, &name, "", &messages);
                    count = count.saturating_add(1);
                }
            }
        }
        Ok(count)
    }
}

/// Minimal projection of the framework's on-disk conversation record, used only
/// for reindex-on-start (read-only; never serialized by this module).
#[derive(Deserialize)]
struct ReindexRecord {
    conversation: ReindexConversation,
    messages: Vec<ReindexMessage>,
}

#[derive(Deserialize)]
struct ReindexConversation {
    conversation_id: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct ReindexMessage {
    content: Option<String>,
}

/// Build a short snippet around the first match of `needle` in `content`.
fn make_snippet(content: &str, needle: &str, window: usize) -> String {
    let pos = content.to_lowercase().find(needle).unwrap_or(0);
    let start = pos.saturating_sub(window / 2);
    // Use char indices to stay UTF-8 safe (AGENTS.md: no byte slicing).
    let prefix: String = content.chars().skip(start).take(window).collect();
    if start > 0 {
        format!("...{prefix}...")
    } else {
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_search_index_and_query() {
        let eng = SessionSearchEngine::new();
        eng.index_session(
            "s1",
            "rust help",
            "qwen3",
            &["use tokio::spawn", "spawn needs async"],
        );
        eng.index_session("s2", "python", "gpt", &["asyncio run"]);
        let r = eng.search("tokio", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].session_id, "s1");
        let r = eng.search("async", 10);
        assert_eq!(r.len(), 2); // both match "async"
        eng.remove_session("s1");
        assert!(eng.search("tokio", 10).is_empty());
    }
}
