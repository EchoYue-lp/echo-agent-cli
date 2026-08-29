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
use echo_agent::compression::ContextProjection;
use echo_agent::llm::types::Message;
use sha2::{Digest, Sha256};

const INSTRUCTION_CONTEXT_PROJECTION: &str = "eko:instruction-context";
pub(crate) const HOT_MEMORY_CONTEXT_PROJECTION: &str = "eko:hot-memory-context";

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

/// One immutable layered hot-memory generation. The revision is a deterministic
/// content hash; the generation-bound observer is used only to coalesce reads.
#[derive(Debug, Clone)]
pub(crate) struct HotMemoryProjectionSnapshot {
    revision: String,
    message: Option<Message>,
}

impl HotMemoryProjectionSnapshot {
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }

    pub(crate) fn same_content(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
}

/// Shared source consumed at every pre-model safe point. Publication is one
/// synchronous pointer/value replacement and never waits for a live Agent.
pub(crate) struct HotMemoryProjectionSource {
    snapshot: std::sync::RwLock<Option<HotMemoryProjectionSnapshot>>,
}

impl HotMemoryProjectionSource {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: std::sync::RwLock::new(None),
        }
    }

    pub(crate) fn publish(&self, snapshot: HotMemoryProjectionSnapshot) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
    }

    pub(crate) fn snapshot(&self) -> Option<HotMemoryProjectionSnapshot> {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub(crate) fn projection(&self) -> ContextProjection {
        ContextProjection {
            marker: HOT_MEMORY_CONTEXT_PROJECTION.to_string(),
            message: self.snapshot().and_then(|snapshot| snapshot.message),
        }
    }
}

impl Default for HotMemoryProjectionSource {
    fn default() -> Self {
        Self::new()
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
            InstructionTier::User => Ok(crate::data_root::user_data_path("user.md")),
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

/// Read one immutable hot-memory generation outside the async executor.
pub(crate) async fn load_hot_memory_projection_snapshot(
    echo_agent_dir: PathBuf,
) -> Result<HotMemoryProjectionSnapshot, String> {
    let content = tokio::task::spawn_blocking(move || {
        crate::instruction_provider::strict_optional_text(Some(&echo_agent_dir.join("MEMORY.md")))
            .map(|content| {
                content
                    .map(|raw| crate::utils::strip_yaml_frontmatter(&raw))
                    .unwrap_or_default()
            })
    })
    .await
    .map_err(|error| format!("hot-memory projection read task failed: {error}"))?
    .map_err(|error| format!("hot-memory projection read failed: {error}"))?;
    let rendered = content.trim();
    let mut hasher = Sha256::new();
    hasher.update(b"eko-hot-memory-projection-v1\0");
    hasher.update(rendered.as_bytes());
    let message = (!rendered.is_empty()).then(|| Message::system(rendered.to_string()));
    Ok(HotMemoryProjectionSnapshot {
        revision: format!("{:x}", hasher.finalize()),
        message,
    })
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
        let hot = load_hot_memory_projection_snapshot(root.join(".eko"))
            .await
            .map_err(anyhow::Error::msg)?;
        let source = HotMemoryProjectionSource::new();
        source.publish(hot);

        let context = agent.context().lock().await;
        assert!(context.has_projection(INSTRUCTION_CONTEXT_PROJECTION));
        drop(context);
        let projection = source.projection();
        assert_eq!(projection.marker, HOT_MEMORY_CONTEXT_PROJECTION);
        assert!(projection.message.is_some());
        Ok(())
    }
}
