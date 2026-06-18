//! Unified memory API — bridges instructions and agent-learned memories.
//!
//! ## Two memory concepts
//!
//! | Concept | Source | Writer | Storage | Purpose |
//! |---------|--------|--------|---------|---------|
//! | **Instructions** | User-curated files | User | `user.md` / `project.md` / `local.md` | Tell the agent how to behave |
//! | **Memories** | Agent-learned | Agent (BackgroundReviewer) | Store (KV) | Knowledge the agent has accumulated |
//!
//! ## Usage
//!
//! ```rust,ignore
//! let memory = UnifiedMemory::load();
//!
//! // Get system prompt context
//! let ctx = memory.system_prompt_context();
//!
//! // Manage instructions
//! memory.get_instructions(InstructionTier::Project);
//!
//! // Manage memories (async)
//! memory.remember("User prefers Rust over Python", 0.8).await;
//! memory.recall("language preference").await;
//! ```

use std::path::PathBuf;

use echo_agent::memory::{Store, StoreItem};
use std::sync::Arc;

use crate::instruction_provider::InstructionProvider;

// ── Instruction tiers ───────────────────────────────────────────────

/// Which tier of instructions to access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionTier {
    /// User-level instructions (`~/.echo-agent/user.md`).
    User,
    /// Project-level instructions (`<project-root>/.echo-agent/project.md`).
    Project,
    /// Local directory instructions (`<cwd>/.echo-agent/local.md`).
    Local,
}

impl std::fmt::Display for InstructionTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
        }
    }
}

// ── Memory entry ────────────────────────────────────────────────────

/// A single memory entry retrieved from the Store.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    /// Memory key.
    pub key: String,
    /// Memory content.
    pub content: String,
    /// Importance score.
    pub importance: f32,
    /// Namespace the memory belongs to.
    pub namespace: String,
}

// ── Memory context ──────────────────────────────────────────────────

/// Aggregated context for system prompt injection.
#[derive(Debug, Clone)]
pub struct MemoryContext {
    /// Merged instructions from all tiers.
    pub instructions: String,
    /// Relevant memory summaries.
    pub memories: Vec<String>,
}

impl MemoryContext {
    /// Whether there is any context to inject.
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty() && self.memories.is_empty()
    }

    /// Format as a system prompt suffix.
    pub fn to_prompt_suffix(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();

        if !self.instructions.is_empty() {
            parts.push(self.instructions.clone());
        }

        if !self.memories.is_empty() {
            parts.push(format!(
                "## Relevant memories\n{}",
                self.memories
                    .iter()
                    .map(|m| format!("- {m}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        format!("\n\n{}", parts.join("\n\n"))
    }
}

// ── Unified Memory ──────────────────────────────────────────────────

/// Unified memory manager bridging instructions and dynamic memories.
pub struct UnifiedMemory {
    /// Static file-based instructions (user/project/local .md files).
    instructions: InstructionProvider,
    /// Dynamic KV store for agent-learned memories.
    memories: Option<Arc<dyn Store>>,
    /// Hot layer content cached at load time (MEMORY.md body, frontmatter stripped).
    hot_content: Option<String>,
}

impl UnifiedMemory {
    /// Create a new unified memory with just instructions loaded.
    pub fn load() -> Self {
        let instructions = InstructionProvider::load();
        let hot_content = load_hot_content();
        Self {
            instructions,
            memories: None,
            hot_content,
        }
    }

    /// Attach a Store for dynamic memories.
    pub fn with_store(mut self, store: Arc<dyn Store>) -> Self {
        self.memories = Some(store);
        self
    }

    // ── Instructions ─────────────────────────────────────────────────

    /// Get instructions for a specific tier.
    pub fn get_instructions(&self, tier: InstructionTier) -> Option<&str> {
        match tier {
            InstructionTier::User => self.instructions.user_level.as_deref(),
            InstructionTier::Project => self.instructions.project_level.as_deref(),
            InstructionTier::Local => self.instructions.local_level.as_deref(),
        }
    }

    /// Set instructions for a specific tier (writes to disk).
    pub fn set_instructions(&self, tier: InstructionTier, content: &str) -> Result<(), String> {
        let path = self.instruction_path(tier)?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

        std::fs::write(&path, content)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))
    }

    /// Get the file path for a given instruction tier.
    fn instruction_path(&self, tier: InstructionTier) -> Result<PathBuf, String> {
        match tier {
            InstructionTier::User => {
                let home = dirs::home_dir().ok_or("Could not determine home directory")?;
                Ok(home.join(".echo-agent").join("user.md"))
            }
            InstructionTier::Project => {
                let pwd = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {e}"))?;
                let root = crate::utils::find_project_root(&pwd).ok_or("Not in a project directory")?;
                Ok(root.join(".echo-agent").join("project.md"))
            }
            InstructionTier::Local => {
                let pwd = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {e}"))?;
                Ok(pwd.join(".echo-agent").join("local.md"))
            }
        }
    }

    // ── Memories (async) ─────────────────────────────────────────────

    /// Store a legacy app-level memory in the raw Store namespace.
    ///
    /// Runtime-recallable agent memories should use the layered memory path
    /// (`MemoryLayerManager::write_memory`) rather than this product API.
    pub async fn remember(&self, content: &str, importance: f32) -> Result<String, String> {
        let store = self.memories.as_ref().ok_or("No memory store configured")?;

        let key = format!("memory_{}", chrono::Utc::now().timestamp_millis());

        let value = serde_json::json!({
            "content": content,
            "importance": importance,
            "created_at": chrono::Utc::now().to_rfc3339(),
        });

        let ns: &[&str] = &["agent", "memories"];
        store
            .put(ns, &key, value)
            .await
            .map_err(|e| format!("Failed to store memory: {e}"))?;

        Ok(key)
    }

    /// Search memories by keyword.
    pub async fn recall(&self, query: &str) -> Result<Vec<MemoryEntry>, String> {
        let store = self.memories.as_ref().ok_or("No memory store configured")?;

        let ns: &[&str] = &["agent", "memories"];
        let results: Vec<StoreItem> = store
            .search(ns, query, 10)
            .await
            .map_err(|e| format!("Failed to search memories: {e}"))?;

        Ok(results
            .into_iter()
            .filter_map(|item| {
                let content = item.value.get("content")?.as_str()?.to_string();
                Some(MemoryEntry {
                    key: item.key,
                    content,
                    importance: item.importance,
                    namespace: item.namespace.join("."),
                })
            })
            .collect())
    }

    /// Delete a memory by key.
    pub async fn forget(&self, key: &str) -> Result<bool, String> {
        let store = self.memories.as_ref().ok_or("No memory store configured")?;

        let ns: &[&str] = &["agent", "memories"];
        store
            .delete(ns, key)
            .await
            .map_err(|e| format!("Failed to delete memory: {e}"))
    }

    /// List all memories.
    pub async fn list_memories(&self) -> Result<Vec<MemoryEntry>, String> {
        let store = self.memories.as_ref().ok_or("No memory store configured")?;

        let ns: &[&str] = &["agent", "memories"];
        let results: Vec<StoreItem> = store
            .list(ns)
            .await
            .map_err(|e| format!("Failed to list memories: {e}"))?;

        Ok(results
            .into_iter()
            .filter_map(|item| {
                let content = item.value.get("content")?.as_str()?.to_string();
                Some(MemoryEntry {
                    key: item.key,
                    content,
                    importance: item.importance,
                    namespace: item.namespace.join("."),
                })
            })
            .collect())
    }

    // ── Aggregated context ───────────────────────────────────────────

    /// Get all context needed for system prompt injection.
    ///
    /// The instructions suffix (via `InstructionProvider`) already includes the
    /// MEMORY.md hot layer. We do NOT re-read it here — that would duplicate
    /// the same content under two different section headers (P0-5).
    pub fn system_prompt_context(&self) -> MemoryContext {
        MemoryContext {
            instructions: self.instructions.get_system_prompt_suffix(),
            memories: Vec::new(),
        }
    }

    /// Refresh cached hot layer content (call after a review cycle or
    /// explicit promotion/demotion).
    pub fn refresh_hot(&mut self) {
        self.hot_content = load_hot_content();
    }
}

// ── Helpers ─────────────────────────────────────────────────────────


/// Load the hot layer content from MEMORY.md (body only, frontmatter stripped).
///
/// Tries project-level `.echo-agent/MEMORY.md` first, then user-level `~/.echo-agent/MEMORY.md`.
fn load_hot_content() -> Option<String> {
    // Project-level
    if let Ok(pwd) = std::env::current_dir()
        && let Some(root) = crate::utils::find_project_root(&pwd)
    {
        let path = root.join(".echo-agent").join("MEMORY.md");
        if path.exists()
            && let Ok(raw) = std::fs::read_to_string(&path)
        {
            return Some(crate::utils::strip_yaml_frontmatter(&raw));
        }
    }

    // User-level
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".echo-agent").join("MEMORY.md");
        if path.exists()
            && let Ok(raw) = std::fs::read_to_string(&path)
        {
            return Some(crate::utils::strip_yaml_frontmatter(&raw));
        }
    }

    None
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instruction_tier_display() {
        assert_eq!(InstructionTier::User.to_string(), "user");
        assert_eq!(InstructionTier::Project.to_string(), "project");
        assert_eq!(InstructionTier::Local.to_string(), "local");
    }

    #[test]
    fn test_memory_context_empty() {
        let ctx = MemoryContext {
            instructions: String::new(),
            memories: Vec::new(),
        };
        assert!(ctx.is_empty());
        assert_eq!(ctx.to_prompt_suffix(), "");
    }

    #[test]
    fn test_memory_context_with_instructions() {
        let ctx = MemoryContext {
            instructions: "## User-level instructions\nBe helpful".to_string(),
            memories: vec!["User prefers Rust".to_string()],
        };
        assert!(!ctx.is_empty());
        let prompt = ctx.to_prompt_suffix();
        assert!(prompt.contains("User-level instructions"));
        assert!(prompt.contains("User prefers Rust"));
    }
}
