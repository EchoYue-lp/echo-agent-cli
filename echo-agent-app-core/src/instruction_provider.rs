//! Layered instruction-file loader.
//!
//! Loads three tiers of user-curated instruction Markdown files and
//! concatenates them as a system-prompt suffix:
//! - `~/.echo-agent/user.md`              — user-level (cross-project)
//! - `<project-root>/.echo-agent/project.md` — project-level
//! - `<cwd>/.echo-agent/local.md`         — local directory
//!
//! Static, file-only loader: no DB, no embeddings, no recall. Agent-learned
//! dynamic memories are handled separately by `UnifiedMemory.memories`
//! (the `Store` backend).

use std::path::PathBuf;

/// Layered instruction-file loader (user / project / local `.md`).
pub struct InstructionProvider {
    pub project_level: Option<String>,
    pub user_level: Option<String>,
    pub local_level: Option<String>,
}

impl InstructionProvider {
    /// Load every tier from disk.
    pub fn load() -> Self {
        let project_level = Self::load_project_instructions();
        let user_level = Self::load_user_instructions();
        let local_level = Self::load_local_instructions();

        Self {
            project_level,
            user_level,
            local_level,
        }
    }

    /// Concatenate the three tiers into a system-prompt suffix.
    pub fn get_system_prompt_suffix(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref user) = self.user_level {
            parts.push(format!("## User-level instructions\n{}", user));
        }
        if let Some(ref project) = self.project_level {
            parts.push(format!("## Project-level instructions\n{}", project));
        }
        if let Some(ref local) = self.local_level {
            parts.push(format!("## Local directory instructions\n{}", local));
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
        };
        assert!(instructions.get_system_prompt_suffix().is_empty());
    }
}
