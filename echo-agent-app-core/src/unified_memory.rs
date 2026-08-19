//! Instruction-tier memory API — loads the user/project/local `.md` files
//! and projects them into model context.
//!
//! Dynamic agent-learned memories are managed by the layered
//! `MemoryLayerManager` (written by accepted evidence candidates or the layered
//! `remember` tool), not by this type. BackgroundReviewer is proposal-only by
//! default. The earlier product-level
//! `remember` / `recall` helpers were半死 (never wired into the production
//! read path) and have been removed.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let memory = UnifiedMemory::load();
//!
//! // Get the current instruction context.
//! let suffix = memory.system_prompt_suffix();
//!
//! // Manage instructions
//! memory.get_instructions(InstructionTier::Project);
//! ```

use std::path::PathBuf;

use crate::instruction_provider::InstructionProvider;
use echo_agent::llm::types::Message;
use sha2::{Digest, Sha256};

const INSTRUCTION_CONTEXT_PROJECTION: &str = "eko:instruction-context";
const HOT_MEMORY_CONTEXT_PROJECTION: &str = "eko:hot-memory-context";

/// One strictly-read instruction generation shared by the primary agent and
/// every existing or future pooled agent.
#[derive(Debug, Clone)]
pub(crate) struct InstructionProjectionSnapshot {
    revision: String,
    message: Option<Message>,
}

impl InstructionProjectionSnapshot {
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }
}

// ── Instruction tiers ───────────────────────────────────────────────

/// Which tier of instructions to access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionTier {
    /// User-level instructions (`~/.eko/user.md`).
    User,
    /// Project-level instructions (`<project-root>/.eko/project.md`).
    Project,
    /// Local directory instructions (`<cwd>/.eko/local.md`).
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

// ── Unified Memory ──────────────────────────────────────────────────

/// Instructions manager — loads the user/project/local `.md` tiers and
/// aggregates them for system-prompt injection. Dynamic agent-learned
/// memories are managed by the layered `MemoryLayerManager`, not this type.
pub struct UnifiedMemory {
    /// Static file-based instructions (user/project/local .md files).
    instructions: InstructionProvider,
}

impl UnifiedMemory {
    /// Create a new unified memory with just instructions loaded.
    pub fn load() -> Self {
        Self {
            instructions: InstructionProvider::load(),
        }
    }

    /// Load instruction context for one explicit workspace root.
    pub fn load_for(root: Option<&std::path::Path>) -> Self {
        Self {
            instructions: InstructionProvider::load_for(root),
        }
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
            InstructionTier::User => Ok(echo_agent::paths::user_data_path("user.md")),
            InstructionTier::Project => {
                let pwd = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {e}"))?;
                let root =
                    crate::utils::find_project_root(&pwd).ok_or("Not in a project directory")?;
                Ok(root.join(".eko").join("project.md"))
            }
            InstructionTier::Local => {
                let pwd = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {e}"))?;
                Ok(pwd.join(".eko").join("local.md"))
            }
        }
    }

    // ── Aggregated context ───────────────────────────────────────────

    /// Get all instruction and hot-memory context for projection.
    pub fn system_prompt_suffix(&self) -> String {
        self.instructions.get_system_prompt_suffix()
    }

    pub fn instruction_prompt_suffix(&self) -> Option<String> {
        self.instructions.get_instruction_suffix()
    }

    pub fn memory_prompt_suffix(&self) -> Option<String> {
        self.instructions.get_memory_suffix()
    }
}

/// Build one fail-closed projection snapshot from current file authorities.
pub(crate) fn load_instruction_projection_strict(
    root: Option<&std::path::Path>,
) -> std::io::Result<InstructionProjectionSnapshot> {
    let suffix = InstructionProvider::load_for_strict(root)?
        .get_instruction_suffix()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    let mut hasher = Sha256::new();
    hasher.update(b"eko-instruction-projection-v1\0");
    if let Some(value) = suffix.as_deref() {
        hasher.update(value.as_bytes());
    }
    Ok(InstructionProjectionSnapshot {
        revision: format!("{:x}", hasher.finalize()),
        message: suffix.map(Message::system),
    })
}

/// Publish an already-read snapshot. This function never touches disk, so all
/// targets in one generation receive byte-identical content.
pub(crate) async fn apply_instruction_projection_snapshot(
    agent: &mut echo_agent::agent::ReactAgent,
    snapshot: &InstructionProjectionSnapshot,
) {
    agent
        .context()
        .lock()
        .await
        .replace_projection(INSTRUCTION_CONTEXT_PROJECTION, snapshot.message.clone());
}

/// Replace the independently-owned hot-memory projection for one agent.
pub async fn refresh_hot_memory_projection(
    agent: &mut echo_agent::agent::ReactAgent,
    root: Option<&std::path::Path>,
) {
    let suffix = UnifiedMemory::load_for(root).memory_prompt_suffix();
    let message = suffix
        .filter(|value| !value.trim().is_empty())
        .map(|value| Message::system(value.trim().to_string()));
    agent
        .context()
        .lock()
        .await
        .replace_projection(HOT_MEMORY_CONTEXT_PROJECTION, message);
}

/// Refresh both file-backed context domains from one filesystem snapshot.
pub async fn refresh_memory_projections(
    agent: &mut echo_agent::agent::ReactAgent,
    root: Option<&std::path::Path>,
) {
    let memory = UnifiedMemory::load_for(root);
    let instruction = memory
        .instruction_prompt_suffix()
        .filter(|value| !value.trim().is_empty())
        .map(|value| Message::system(value.trim().to_string()));
    let hot_memory = memory
        .memory_prompt_suffix()
        .filter(|value| !value.trim().is_empty())
        .map(|value| Message::system(value.trim().to_string()));
    let mut context = agent.context().lock().await;
    context.replace_projection(INSTRUCTION_CONTEXT_PROJECTION, instruction);
    context.replace_projection(HOT_MEMORY_CONTEXT_PROJECTION, hot_memory);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_instruction_tier_display() {
        assert_eq!(InstructionTier::User.to_string(), "user");
        assert_eq!(InstructionTier::Project.to_string(), "project");
        assert_eq!(InstructionTier::Local.to_string(), "local");
    }

    #[test]
    fn system_prompt_suffix_contains_loaded_instructions() {
        let memory = UnifiedMemory {
            instructions: InstructionProvider {
                project_level: Some("Use Rust".to_string()),
                user_level: None,
                repository_level: None,
                local_level: None,
                agents_level: None,
                hot_memory: None,
            },
        };
        let prompt = memory.system_prompt_suffix();
        assert!(prompt.contains("Project-level instructions"));
        assert!(prompt.contains("Use Rust"));
    }

    #[tokio::test]
    async fn instruction_projection_replaces_previous_workspace() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(first.join(".eko"))?;
        std::fs::create_dir_all(second.join(".eko"))?;
        std::fs::write(first.join(".eko/project.md"), "FIRST_WORKSPACE_RULE")?;
        std::fs::write(second.join(".eko/project.md"), "SECOND_WORKSPACE_RULE")?;

        let mut agent = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
            .system_prompt("test")
            .build()?;

        let first_snapshot = load_instruction_projection_strict(Some(&first))?;
        apply_instruction_projection_snapshot(&mut agent, &first_snapshot).await;
        let second_snapshot = load_instruction_projection_strict(Some(&second))?;
        apply_instruction_projection_snapshot(&mut agent, &second_snapshot).await;

        let context = agent.context().lock().await;
        let projected = context
            .messages()
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(projected.contains("SECOND_WORKSPACE_RULE"));
        assert!(!projected.contains("FIRST_WORKSPACE_RULE"));
        assert_eq!(projected.matches(INSTRUCTION_CONTEXT_PROJECTION).count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn instruction_and_hot_memory_use_distinct_projections() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir_all(root.join(".eko"))?;
        std::fs::write(root.join(".eko/project.md"), "PROJECT_RULE")?;
        std::fs::write(root.join(".eko/MEMORY.md"), "HOT_MEMORY")?;

        let mut agent = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
            .system_prompt("test")
            .build()?;
        let snapshot = load_instruction_projection_strict(Some(root))?;
        apply_instruction_projection_snapshot(&mut agent, &snapshot).await;
        refresh_hot_memory_projection(&mut agent, Some(root)).await;

        let context = agent.context().lock().await;
        assert!(context.has_projection(INSTRUCTION_CONTEXT_PROJECTION));
        assert!(context.has_projection(HOT_MEMORY_CONTEXT_PROJECTION));
        Ok(())
    }
}
