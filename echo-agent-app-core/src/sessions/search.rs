//! Session FTS5 full-text search engine.
//!
//! Indexes session content (name + message content) into an SQLite FTS5
//! virtual table for fast full-text search across all sessions.
//!
//! The design uses a separate content table (`sessions_meta`) for structured
//! metadata and a standalone FTS5 virtual table for full-text indexing.
//! FTS5's `content=` (content-sync) mode was found to be unreliable with
//! TEXT PRIMARY KEY tables, so we use a standalone FTS5 table and manually
//! keep it in sync with the meta table.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// A single search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Session ID (UUID).
    pub session_id: String,
    /// Session name.
    pub session_name: String,
    /// Model used.
    pub model: String,
    /// FTS5 snippet (matched context).
    pub snippet: String,
    /// Match rank (lower = better).
    pub rank: f64,
}

/// FTS5-backed session search engine.
pub struct SessionSearchEngine {
    conn: Mutex<Connection>,
}

impl SessionSearchEngine {
    /// Create or open the search index at the default path.
    ///
    /// The SQLite database is stored at `~/.echo-agent/search.db`.
    pub fn new() -> anyhow::Result<Self> {
        let db_path = Self::db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        let engine = Self {
            conn: Mutex::new(conn),
        };
        engine.init_schema()?;
        Ok(engine)
    }

    /// Create an in-memory search engine (for testing or fallback).
    pub fn new_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let engine = Self {
            conn: Mutex::new(conn),
        };
        engine.init_schema()?;
        Ok(engine)
    }

    fn db_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".echo-agent").join("search.db")
    }

    /// Create the FTS5 virtual table if it doesn't exist.
    fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions_meta (
                session_id  TEXT PRIMARY KEY,
                session_name TEXT NOT NULL,
                model       TEXT NOT NULL DEFAULT ''
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
                session_id,
                session_name,
                model,
                content
            );",
        )?;
        Ok(())
    }

    /// Index a session: upsert meta row and sync the FTS index.
    pub fn index_session(
        &self,
        session_id: &str,
        session_name: &str,
        model: &str,
        messages: &[impl AsRef<str>],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // Build full content string: all message content
        let mut content_parts = Vec::with_capacity(messages.len());
        for msg in messages {
            content_parts.push(msg.as_ref().to_string());
        }
        let full_content = content_parts.join(" ");

        // Upsert the meta row
        conn.execute(
            "INSERT OR REPLACE INTO sessions_meta (session_id, session_name, model) VALUES (?1, ?2, ?3)",
            params![session_id, session_name, model],
        )?;

        // Sync FTS: delete old entry (if any) then insert new one
        // FTS5 requires the 'delete' command format to remove by rowid.
        // Since we don't track rowid, we first query for it.
        let existing_rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM sessions_fts WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .ok();
        if let Some(rid) = existing_rowid {
            conn.execute(
                "INSERT INTO sessions_fts(sessions_fts, rowid, session_id, session_name, model, content) VALUES ('delete', ?1, ?2, ?3, ?4, '')",
                params![rid, session_id, session_name, model],
            )?;
        }
        conn.execute(
            "INSERT INTO sessions_fts (session_id, session_name, model, content) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, session_name, model, full_content],
        )?;

        Ok(())
    }

    /// Remove a session from the index.
    pub fn remove_session(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // Remove FTS entry via 'delete' command.
        // FTS5 virtual tables can only be queried via MATCH, so we use
        // the special 'delete' command format with session_id as the lookup key.
        conn.execute(
            "INSERT INTO sessions_fts(sessions_fts, rowid, session_id, session_name, model, content) VALUES ('delete', 0, ?1, '', '', '')",
            params![session_id],
        ).ok(); // may fail if not indexed, that's fine

        conn.execute(
            "DELETE FROM sessions_meta WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Search sessions by query string using FTS5.
    ///
    /// Returns results sorted by rank (best match first).
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // FTS5 columns: session_id(0), session_name(1), model(2), content(3)
        let sql = "SELECT
                m.session_id,
                m.session_name,
                m.model,
                snippet(sessions_fts, 3, '<<', '>>', '...', 32) AS snippet,
                bm25(sessions_fts) AS rank
             FROM sessions_fts f
             JOIN sessions_meta m ON f.session_id = m.session_id
             WHERE sessions_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
            .to_string();

        let mut stmt = conn.prepare(&sql)?;
        let results = stmt
            .query_map(params![query, limit as i64], |row| {
                Ok(SearchResult {
                    session_id: row.get(0)?,
                    session_name: row.get(1)?,
                    model: row.get(2)?,
                    snippet: row.get(3)?,
                    rank: row.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Re-index all sessions from the file system.
    ///
    /// Scans both `~/.echo-agent/sessions/` (v1) and `~/.echo-agent/sessions_v2/` (v2).
    pub fn reindex_all(&self) -> anyhow::Result<usize> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let mut count = 0;

        // Index v1 sessions (~/.echo-agent/sessions/)
        let v1_dir = PathBuf::from(&home).join(".echo-agent").join("sessions");
        if v1_dir.exists() {
            count += self.index_directory(&v1_dir, /* is_v2 */ false)?;
        }

        // Index v2 sessions (~/.echo-agent/sessions_v2/)
        let v2_dir = PathBuf::from(&home).join(".echo-agent").join("sessions_v2");
        if v2_dir.exists() {
            count += self.index_directory(&v2_dir, /* is_v2 */ true)?;
        }

        Ok(count)
    }

    fn index_directory(&self, dir: &PathBuf, is_v2: bool) -> anyhow::Result<usize> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(0),
        };

        let mut count = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Ok(data) = std::fs::read_to_string(&path)
            {
                if is_v2 {
                    if let Ok(session) = serde_json::from_str::<serde_json::Value>(&data) {
                        let id = session["id"].as_str().unwrap_or("");
                        let name = session["name"].as_str().unwrap_or("");
                        let model = session["model"].as_str().unwrap_or("");
                        let messages: Vec<String> = session["messages"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|m| m["content"].as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();

                        if !id.is_empty() {
                            self.index_session(id, name, model, &messages)?;
                            count += 1;
                        }
                    }
                } else {
                    // v1 format: { name, model, messages: [{ content }], ... }
                    if let Ok(session) = serde_json::from_str::<serde_json::Value>(&data) {
                        let name = session["name"].as_str().unwrap_or("");
                        let model = session["model"].as_str().unwrap_or("");
                        let messages: Vec<String> = session["messages"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|m| m["content"].as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();

                        if !name.is_empty() {
                            // v1 uses name as identifier
                            self.index_session(name, name, model, &messages)?;
                            count += 1;
                        }
                    }
                }
            }
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_engine_basic() {
        let conn = Connection::open_in_memory().unwrap();
        let engine = SessionSearchEngine {
            conn: Mutex::new(conn),
        };
        engine.init_schema().unwrap();

        engine
            .index_session(
                "sess-1",
                "rust help",
                "qwen-plus",
                &["How do I use tokio?", "You can use tokio::spawn"],
            )
            .unwrap();

        engine
            .index_session(
                "sess-2",
                "python help",
                "gpt-4o",
                &["How do I use asyncio?", "Use async/await syntax"],
            )
            .unwrap();

        let results = engine.search("tokio", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "sess-1");

        let results = engine.search("python OR rust", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_engine_remove() {
        let conn = Connection::open_in_memory().unwrap();
        let engine = SessionSearchEngine {
            conn: Mutex::new(conn),
        };
        engine.init_schema().unwrap();

        engine
            .index_session("sess-rm", "test", "qwen-plus", &["hello world"])
            .unwrap();

        let results = engine.search("hello", 10).unwrap();
        assert_eq!(results.len(), 1);

        engine.remove_session("sess-rm").unwrap();

        let results = engine.search("hello", 10).unwrap();
        assert!(results.is_empty());
    }
}
