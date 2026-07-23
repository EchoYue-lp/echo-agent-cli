//! Layered instruction-file loader.
//!
//! Loads four tiers of instruction Markdown files and
//! concatenates them as a system-prompt suffix:
//! - `~/.eko/user.md`              — user-level (cross-project)
//! - `<project-root>/.eko/project.md` — project-level
//! - `<project-root>/.eko/AGENTS.md` — auto-promoted rules (evolution)
//! - `<cwd>/.eko/local.md`         — local directory
//!
//! Also loads hot-layer memory from `.eko/MEMORY.md`.
//!
//! Static, file-only loader: no DB, no embeddings, no recall. Query-dependent
//! dynamic memories are handled separately by the layered memory store.

use std::path::{Path, PathBuf};

/// Layered instruction-file loader (user / project / agents / local `.md`).
pub struct InstructionProvider {
    pub project_level: Option<String>,
    pub user_level: Option<String>,
    pub local_level: Option<String>,
    /// Auto-promoted rules and learned constraints (AGENTS.md body, frontmatter stripped).
    pub agents_level: Option<String>,
    /// Hot-layer memory content (MEMORY.md body, frontmatter stripped).
    pub hot_memory: Option<String>,
}

impl InstructionProvider {
    /// Load every tier from disk.
    pub fn load() -> Self {
        let root = std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::utils::find_project_root(&cwd));
        Self::load_for(root.as_deref())
    }

    /// Load every tier for one explicit workspace/project root.
    ///
    /// `None` means global context only: user instructions plus user-level
    /// `MEMORY.md`. It intentionally does not consult process cwd, so exiting a
    /// workspace can remove project-local instructions deterministically.
    pub fn load_for(root: Option<&Path>) -> Self {
        let project_root = root.map(|path| {
            crate::utils::find_project_root(path).unwrap_or_else(|| path.to_path_buf())
        });
        let project_level = Self::load_project_instructions(project_root.as_deref());
        let user_level = Self::load_user_instructions();
        let local_level = Self::load_local_instructions(root);
        let agents_level = Self::load_agents_instructions(project_root.as_deref());
        let hot_memory = Self::load_hot_memory(project_root.as_deref());

        Self {
            project_level,
            user_level,
            local_level,
            agents_level,
            hot_memory,
        }
    }

    /// Concatenate the four tiers into a system-prompt suffix.
    pub fn get_system_prompt_suffix(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref user) = self.user_level {
            parts.push(format!("## User-level instructions\n{}", user));
        }
        if let Some(ref project) = self.project_level {
            parts.push(format!("## Project-level instructions\n{}", project));
        }
        if let Some(ref agents) = self.agents_level {
            parts.push(format!("## Auto-promoted rules\n{}", agents));
        }
        if let Some(ref local) = self.local_level {
            parts.push(format!("## Local directory instructions\n{}", local));
        }
        if let Some(ref hot) = self.hot_memory {
            parts.push(format!("## Active Memories (Hot Layer)\n{}", hot));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", parts.join("\n\n"))
        }
    }

    /// Load project-level instructions from `<project-root>/.eko/project.md`.
    fn load_project_instructions(project_root: Option<&Path>) -> Option<String> {
        project_root
            .map(|root| root.join(".eko").join("project.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load user-level instructions from `~/.eko/user.md`.
    fn load_user_instructions() -> Option<String> {
        Some(echo_agent::paths::user_data_path("user.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load local-directory instructions from `<cwd>/.eko/local.md`.
    fn load_local_instructions(root: Option<&Path>) -> Option<String> {
        root.map(|path| path.join(".eko").join("local.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Save project-level instructions to `<cwd>/.eko/project.md`.
    pub fn save_project_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::project_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// Save user-level instructions to `~/.eko/user.md`.
    pub fn save_user_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::user_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// Path to the project-level instructions file.
    fn project_instructions_path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".eko")
            .join("project.md")
    }

    /// Path to the user-level instructions file.
    fn user_instructions_path() -> PathBuf {
        echo_agent::paths::user_data_path("user.md")
    }

    /// Load hot-layer memory content from `.eko/MEMORY.md`.
    ///
    /// Returns the body (frontmatter stripped) so it can be included in the system prompt.
    fn load_hot_memory(project_root: Option<&Path>) -> Option<String> {
        let project_path = project_root.map(|root| root.join(".eko").join("MEMORY.md"));
        let path = project_path
            .filter(|path| path.exists())
            .unwrap_or_else(|| echo_agent::paths::user_data_path("MEMORY.md"));
        let raw = std::fs::read_to_string(path).ok()?;

        Some(crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Load auto-promoted rules from `<project-root>/.eko/AGENTS.md`.
    ///
    /// This is the fourth instruction tier — between project-level and local-level.
    /// Contains rules that were automatically promoted from high-confidence memories
    /// by the evolution system's `RulePromoter`.
    fn load_agents_instructions(project_root: Option<&Path>) -> Option<String> {
        project_root
            .map(|root| root.join(".eko").join("AGENTS.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|raw| crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Path to the AGENTS.md file.
    pub fn agents_instructions_path() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| crate::utils::find_project_root(&pwd))
            .map(|root| root.join(".eko").join("AGENTS.md"))
            .unwrap_or_else(|| std::path::PathBuf::from(".eko/AGENTS.md"))
    }

    /// Save content to the AGENTS.md file.
    pub fn save_agents_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::agents_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }
}

impl Default for InstructionProvider {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_empty_instructions() {
        // This test assumes no instruction files exist
        let instructions = InstructionProvider {
            project_level: None,
            user_level: None,
            local_level: None,
            agents_level: None,
            hot_memory: None,
        };
        assert!(instructions.get_system_prompt_suffix().is_empty());
    }
}
