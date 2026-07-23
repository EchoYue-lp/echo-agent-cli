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

const INSTRUCTION_CONTEXT_PROJECTION: &str = "eko:instruction-context";

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
}

/// Replace the instruction/hot-memory projection for one agent.
pub async fn refresh_instruction_projection(
    agent: &mut echo_agent::agent::ReactAgent,
    root: Option<&std::path::Path>,
) {
    let suffix = UnifiedMemory::load_for(root).system_prompt_suffix();
    let message = (!suffix.trim().is_empty()).then(|| Message::system(suffix.trim().to_string()));
    agent
        .context()
        .lock()
        .await
        .replace_projection(INSTRUCTION_CONTEXT_PROJECTION, message);
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

        refresh_instruction_projection(&mut agent, Some(&first)).await;
        refresh_instruction_projection(&mut agent, Some(&second)).await;

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
}
