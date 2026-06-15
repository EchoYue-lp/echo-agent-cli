//! Layered instruction-file loader.
//!
//! Loads four tiers of instruction Markdown files and
//! concatenates them as a system-prompt suffix:
//! - `~/.echo-agent/user.md`              — user-level (cross-project)
//! - `<project-root>/.echo-agent/project.md` — project-level
//! - `<project-root>/.echo-agent/AGENTS.md` — auto-promoted rules (evolution)
//! - `<cwd>/.echo-agent/local.md`         — local directory
//!
//! Also loads hot-layer memory from `.echo-agent/MEMORY.md`.
//!
//! Static, file-only loader: no DB, no embeddings, no recall. Agent-learned
//! dynamic memories are handled separately by `UnifiedMemory.memories`
//! (the `Store` backend).

use std::path::PathBuf;

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
        let project_level = Self::load_project_instructions();
        let user_level = Self::load_user_instructions();
        let local_level = Self::load_local_instructions();
        let agents_level = Self::load_agents_instructions();
        let hot_memory = Self::load_hot_memory();

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

    /// Load project-level instructions from `<project-root>/.echo-agent/project.md`.
    fn load_project_instructions() -> Option<String> {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| Self::find_project_root(&pwd))
            .map(|root| root.join(".echo-agent").join("project.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load user-level instructions from `~/.echo-agent/user.md`.
    fn load_user_instructions() -> Option<String> {
        dirs::home_dir()
            .map(|home| home.join(".echo-agent").join("user.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load local-directory instructions from `<cwd>/.echo-agent/local.md`.
    fn load_local_instructions() -> Option<String> {
        std::env::current_dir()
            .ok()
            .map(|pwd| pwd.join(".echo-agent").join("local.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Find the project root (first ancestor containing `.git` or `.echo-agent`).
    fn find_project_root(start: &std::path::Path) -> Option<PathBuf> {
        let mut current = Some(start);
        while let Some(dir) = current {
            if dir.join(".git").exists() || dir.join(".echo-agent").exists() {
                return Some(dir.to_path_buf());
            }
            current = dir.parent();
        }
        None
    }

    /// Save project-level instructions to `<cwd>/.echo-agent/project.md`.
    pub fn save_project_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::project_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// Save user-level instructions to `~/.echo-agent/user.md`.
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
            .join(".echo-agent")
            .join("project.md")
    }

    /// Path to the user-level instructions file.
    fn user_instructions_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".echo-agent")
            .join("user.md")
    }

    /// Load hot-layer memory content from `.echo-agent/MEMORY.md`.
    ///
    /// Returns the body (frontmatter stripped) so it can be included in the system prompt.
    fn load_hot_memory() -> Option<String> {
        let raw = std::env::current_dir()
            .ok()
            .and_then(|pwd| Self::find_project_root(&pwd))
            .map(|root| root.join(".echo-agent").join("MEMORY.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())?;

        Some(strip_yaml_frontmatter(&raw))
    }

    /// Load auto-promoted rules from `<project-root>/.echo-agent/AGENTS.md`.
    ///
    /// This is the fourth instruction tier — between project-level and local-level.
    /// Contains rules that were automatically promoted from high-confidence memories
    /// by the evolution system's `RulePromoter`.
    fn load_agents_instructions() -> Option<String> {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| Self::find_project_root(&pwd))
            .map(|root| root.join(".echo-agent").join("AGENTS.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|raw| strip_yaml_frontmatter(&raw))
    }

    /// Path to the AGENTS.md file.
    pub fn agents_instructions_path() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| Self::find_project_root(&pwd))
            .map(|root| root.join(".echo-agent").join("AGENTS.md"))
            .unwrap_or_else(|| std::path::PathBuf::from(".echo-agent/AGENTS.md"))
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

/// Strip YAML frontmatter (between --- markers) from a MEMORY.md file.
fn strip_yaml_frontmatter(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return raw.to_string();
    }

    let rest = &trimmed[3..]; // skip opening ---
    if let Some(pos) = rest.find("\n---") {
        let body = rest[pos + 4..]
            .trim_start_matches('\n')
            .trim_start_matches('\r');
        return body.to_string();
    }

    // No closing marker — return as-is
    raw.to_string()
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
