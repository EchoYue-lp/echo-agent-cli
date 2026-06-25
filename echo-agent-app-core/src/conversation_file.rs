//! File-backed ConversationStore + session search engine (U1c: EKO is local — no SQLite).
//!
//! Implements the framework `ConversationStore` trait over plain JSON files, one
//! file per conversation. Also provides an in-memory full-text index
//! ([`SessionSearchEngine`]) that replaces the old FTS5-backed search — it does
//! case-insensitive substring matching (matching the SQL `LIKE '%q%'` semantics
//! of the old `search_conversations`) plus a reindex-on-start from the session
//! JSON files. The app layer owns all storage; the framework stays trait-only.
//!
//! ## Layout
//! - `<base>/conversations/<conversation_id>.json` — one conversation + its
//!   messages, written atomically (tmp + rename).
//! - `<base>/conversations/_meta.json` — a monotonic id counter (so `id` fields
//!   stay unique and non-zero, matching the old autoincrement semantics).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use echo_agent::memory::{
    Conversation, ConversationFilter, ConversationMeta, ConversationStore, NewConversation,
    StoredMessage,
};
use echo_core::error::{MemoryError, Result};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

type BoxFut<'a, T> = BoxFuture<'a, Result<T>>;

/// One conversation record persisted to disk (its `Conversation` header + all
/// its messages). Serialized as `<conversation_id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConversationRecord {
    conversation: Conversation,
    messages: Vec<StoredMessage>,
}

/// Monotonic id counter persisted as `_meta.json`, replacing the SQLite
/// autoincrement. `next_id` is bumped on every new conversation/message so ids
/// stay unique across the store.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoreMeta {
    next_id: i64,
}

impl StoreMeta {
    fn take_id(&mut self) -> i64 {
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }
}

/// File-backed conversation store.
pub struct FileConversationStore {
    base: PathBuf,
    /// Serializes all operations (the trait is `&self`; a Mutex keeps
    /// read-modify-write atomic and the `_meta.json` counter consistent).
    lock: Mutex<StoreMeta>,
}

impl FileConversationStore {
    /// Create a file-backed conversation store rooted at `base/conversations/`.
    pub fn new(base: impl AsRef<Path>) -> Result<Self> {
        let base = base.as_ref().join("conversations");
        std::fs::create_dir_all(&base)
            .map_err(|e| MemoryError::IoError(format!("create conversations dir: {e}")))?;
        // Seed the id counter from the existing meta file (if any); otherwise
        // start at 1. Self-heals across restarts.
        let meta = Self::read_meta(&base);
        Ok(Self {
            base,
            lock: Mutex::new(meta),
        })
    }

    fn conv_path(&self, conversation_id: &str) -> PathBuf {
        self.base.join(format!("{conversation_id}.json"))
    }

    fn meta_path(base: &Path) -> PathBuf {
        base.join("_meta.json")
    }

    fn read_meta(base: &Path) -> StoreMeta {
        match std::fs::read_to_string(Self::meta_path(base)) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => StoreMeta::default(),
        }
    }

    fn persist_meta(&self) -> Result<()> {
        let meta = self.lock.lock().map_err(poison)?.clone();
        let json = serde_json::to_string(&meta)
            .map_err(|e| MemoryError::IoError(format!("serialize meta: {e}")))?;
        atomic_write(&Self::meta_path(&self.base), json.as_bytes())
            .map_err(|e| MemoryError::IoError(format!("write meta: {e}")))?;
        Ok(())
    }

    fn read_record(&self, conversation_id: &str) -> Option<ConversationRecord> {
        match std::fs::read_to_string(self.conv_path(conversation_id)) {
            Ok(s) => serde_json::from_str(&s).ok(),
            Err(_) => None,
        }
    }

    fn write_record(&self, record: &ConversationRecord) -> Result<()> {
        let json = serde_json::to_string_pretty(record)
            .map_err(|e| MemoryError::IoError(format!("serialize conversation: {e}")))?;
        atomic_write(
            &self.conv_path(&record.conversation.conversation_id),
            json.as_bytes(),
        )
        .map_err(|e| MemoryError::IoError(format!("write conversation: {e}")))?;
        Ok(())
    }

    /// Enumerate all conversation records on disk.
    fn read_all_records(&self) -> Vec<ConversationRecord> {
        let mut records = Vec::new();
        let entries = match std::fs::read_dir(&self.base) {
            Ok(e) => e,
            Err(_) => return records,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Skip the meta file.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "_meta.json")
            {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(&path)
                && let Ok(rec) = serde_json::from_str::<ConversationRecord>(&s)
            {
                records.push(rec);
            }
        }
        records
    }
}

// Helper trait to make the read_meta match readable.
fn poison<T>(_: std::sync::PoisonError<T>) -> MemoryError {
    MemoryError::IoError("store lock poisoned".into())
}

impl ConversationStore for FileConversationStore {
    fn create_conversation<'a>(&'a self, conv: NewConversation) -> BoxFut<'a, Conversation> {
        Box::pin(async move {
            let mut meta = self.lock.lock().map_err(poison)?;
            let id = meta.take_id();
            drop(meta);
            let now = now_rfc3339();
            let conversation = Conversation {
                id,
                conversation_id: conv.conversation_id,
                user_id: conv.user_id,
                agent_type: conv.agent_type,
                title: conv.title,
                summary: None,
                compressed_before_id: None,
                created_at: now.clone(),
                updated_at: now,
            };
            let record = ConversationRecord {
                conversation: conversation.clone(),
                messages: Vec::new(),
            };
            self.write_record(&record)?;
            self.persist_meta()?;
            Ok(conversation)
        })
    }

    fn get_conversation<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFut<'a, Option<Conversation>> {
        Box::pin(async move { Ok(self.read_record(conversation_id).map(|r| r.conversation)) })
    }

    fn list_conversations<'a>(
        &'a self,
        filter: ConversationFilter,
    ) -> BoxFut<'a, Vec<ConversationMeta>> {
        Box::pin(async move {
            let _g = self.lock.lock().map_err(poison)?;
            let mut metas: Vec<ConversationMeta> = self
                .read_all_records()
                .into_iter()
                .filter(|r| {
                    filter
                        .user_id
                        .as_deref()
                        .is_none_or(|u| r.conversation.user_id == u)
                })
                .filter(|r| {
                    filter
                        .agent_type
                        .as_deref()
                        .is_none_or(|a| r.conversation.agent_type.as_deref() == Some(a))
                })
                .map(|r| ConversationMeta {
                    id: r.conversation.id,
                    conversation_id: r.conversation.conversation_id,
                    user_id: r.conversation.user_id,
                    title: r.conversation.title,
                    message_count: r.messages.len(),
                    created_at: r.conversation.created_at,
                    updated_at: r.conversation.updated_at,
                })
                .collect();
            // ORDER BY updated_at DESC.
            metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            // OFFSET then LIMIT.
            let offset = filter.offset.unwrap_or(0);
            if offset >= metas.len() {
                return Ok(Vec::new());
            }
            let slice: Vec<ConversationMeta> = if let Some(limit) = filter.limit {
                metas[offset..].iter().take(limit).cloned().collect()
            } else {
                metas[offset..].to_vec()
            };
            Ok(slice)
        })
    }

    fn update_conversation<'a>(
        &'a self,
        conversation_id: &'a str,
        title: Option<&'a str>,
        summary: Option<&'a str>,
        compressed_before_id: Option<i64>,
    ) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let mut record = match self.read_record(conversation_id) {
                Some(r) => r,
                None => return Ok(()), // matches SQL UPDATE on 0 rows.
            };
            if title.is_some() || summary.is_some() || compressed_before_id.is_some() {
                if let Some(t) = title {
                    record.conversation.title = Some(t.to_string());
                }
                if let Some(s) = summary {
                    record.conversation.summary = Some(s.to_string());
                }
                if let Some(cbid) = compressed_before_id {
                    record.conversation.compressed_before_id = Some(cbid);
                }
                record.conversation.updated_at = now_rfc3339();
                self.write_record(&record)?;
            }
            Ok(())
        })
    }

    fn delete_conversation<'a>(&'a self, conversation_id: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            match std::fs::remove_file(self.conv_path(conversation_id)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(MemoryError::IoError(format!("delete conversation: {e}")).into()),
            }
        })
    }

    fn save_messages<'a>(
        &'a self,
        conversation_id: &'a str,
        messages: &'a [StoredMessage],
    ) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let mut record = match self.read_record(conversation_id) {
                Some(r) => r,
                None => {
                    return Err(MemoryError::IoError(format!(
                        "conversation not found: {conversation_id}"
                    ))
                    .into());
                }
            };
            // Assign stable ids to messages that don't have one yet (matches
            // the SQLite autoincrement). Reuse existing ids when present.
            let mut meta = self.lock.lock().map_err(poison)?;
            let mut assigned: Vec<StoredMessage> = messages.to_vec();
            for m in assigned.iter_mut() {
                if m.id.is_none() {
                    m.id = Some(meta.take_id());
                }
            }
            drop(meta);
            record.messages = assigned;
            record.conversation.updated_at = now_rfc3339();
            self.write_record(&record)?;
            self.persist_meta()?;
            Ok(())
        })
    }

    fn get_messages<'a>(&'a self, conversation_id: &'a str) -> BoxFut<'a, Vec<StoredMessage>> {
        Box::pin(async move {
            Ok(self
                .read_record(conversation_id)
                .map(|r| r.messages)
                .unwrap_or_default())
        })
    }

    fn count_messages<'a>(&'a self, conversation_id: &'a str) -> BoxFut<'a, usize> {
        Box::pin(async move {
            Ok(self
                .read_record(conversation_id)
                .map(|r| r.messages.len())
                .unwrap_or(0))
        })
    }

    fn search_conversations<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFut<'a, Vec<ConversationMeta>> {
        Box::pin(async move {
            let needle = query.to_lowercase();
            let mut results: Vec<ConversationMeta> = self
                .read_all_records()
                .into_iter()
                .filter(|r| {
                    // Match if title OR any message content contains the query
                    // (case-insensitive), mirroring SQL `title LIKE '%q%' OR
                    // m.content LIKE '%q%'`.
                    let title_hit = r
                        .conversation
                        .title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&needle));
                    let msg_hit = r.messages.iter().any(|m| {
                        m.content
                            .as_deref()
                            .is_some_and(|c| c.to_lowercase().contains(&needle))
                    });
                    title_hit || msg_hit
                })
                .map(|r| ConversationMeta {
                    id: r.conversation.id,
                    conversation_id: r.conversation.conversation_id,
                    user_id: r.conversation.user_id,
                    title: r.conversation.title,
                    message_count: r.messages.len(),
                    created_at: r.conversation.created_at,
                    updated_at: r.conversation.updated_at,
                })
                .collect();
            // ORDER BY updated_at DESC, then LIMIT.
            results.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            results.truncate(limit);
            Ok(results)
        })
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

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
        let mut entries = self.entries.lock().expect("search engine lock");
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
        let mut entries = self.entries.lock().expect("search engine lock");
        entries.remove(session_id);
    }

    /// Substring search across indexed sessions. Returns results ordered by
    /// recency (insertion order is a best-effort proxy; exact tie-break is not
    /// load-bearing for the sessions search UI).
    pub fn search(&self, query: &str, limit: usize) -> Vec<SessionSearchResult> {
        let needle = query.to_lowercase();
        let entries = self.entries.lock().expect("search engine lock");
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

    /// Re-index all sessions from the file system (v2 sessions only — v1 is
    /// legacy). Scans `~/.echo-agent/sessions_v2/*.json`.
    pub fn reindex_all(&self) -> std::io::Result<usize> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(home).join(".echo-agent").join("sessions_v2");
        if !dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(data) = std::fs::read_to_string(&path)
                && let Ok(session) = serde_json::from_str::<serde_json::Value>(&data)
            {
                let id = session["id"].as_str().unwrap_or("").to_string();
                let name = session["name"].as_str().unwrap_or("").to_string();
                let model = session["model"].as_str().unwrap_or("").to_string();
                let messages: Vec<String> = session["messages"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| m["content"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if !id.is_empty() {
                    self.index_session(&id, &name, &model, &messages);
                    count += 1;
                }
            }
        }
        Ok(count)
    }
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

    fn tmp_base() -> PathBuf {
        std::env::temp_dir().join(format!(
            "echo-file-conv-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn new_conv(id: &str, title: Option<&str>) -> NewConversation {
        NewConversation {
            conversation_id: id.to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: title.map(String::from),
        }
    }

    #[tokio::test]
    async fn conversation_crud_and_search() {
        let base = tmp_base();
        let store = FileConversationStore::new(&base).unwrap();

        // create
        store
            .create_conversation(new_conv("c1", Some("rust tokio help")))
            .await
            .unwrap();
        store
            .create_conversation(new_conv("c2", Some("python asyncio")))
            .await
            .unwrap();

        // list
        let list = store
            .list_conversations(ConversationFilter {
                limit: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(list.len(), 2);

        // save + get messages
        store
            .save_messages(
                "c1",
                &[StoredMessage {
                    id: None,
                    conversation_id: "c1".into(),
                    role: "user".into(),
                    content: Some("how do I use tokio".into()),
                    attachments_json: None,
                    tool_calls_json: None,
                    tool_result_json: None,
                    created_at: now_rfc3339(),
                }],
            )
            .await
            .unwrap();
        let msgs = store.get_messages("c1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].id.is_some()); // id auto-assigned

        // count
        assert_eq!(store.count_messages("c1").await.unwrap(), 1);

        // search by message content
        let found = store.search_conversations("tokio", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].conversation_id, "c1");

        // search by title
        let found = store.search_conversations("python", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].conversation_id, "c2");

        // update
        store
            .update_conversation("c1", Some("renamed"), None, None)
            .await
            .unwrap();
        let conv = store.get_conversation("c1").await.unwrap().unwrap();
        assert_eq!(conv.title.as_deref(), Some("renamed"));

        // delete
        store.delete_conversation("c2").await.unwrap();
        assert!(store.get_conversation("c2").await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

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
