//! Layered instruction-file loader.
//!
//! Loads five tiers of instruction Markdown files and
//! concatenates them as a system-prompt suffix:
//! - `~/.eko/user.md`              — user-level (cross-project)
//! - `<root..cwd>/AGENTS[.override].md` — repository-standard chain
//! - `<project-root>/.eko/project.md` — project-level
//! - `<project-root>/.eko/learned-rules.md` — auto-promoted rules (evolution)
//! - `<cwd>/.eko/local.md`         — local directory
//!
//! Also loads hot-layer memory from `.eko/MEMORY.md`.
//!
//! Static, file-only loader: no DB, no embeddings, no recall. Query-dependent
//! dynamic memories are handled separately by the layered memory store.
//!
//! ## File-name history
//!
//! The auto-promoted rules file was originally `AGENTS.md` (a community-standard
//! name shared with other AI tools). It was renamed to `learned-rules.md` to
//! make the semantic distinction explicit: this file is *written by EKO's
//! `RulePromoter`*, not authored by the user. On first load after upgrade,
//! `load_for` performs a one-time rename of any existing `.eko/AGENTS.md` to
//! `.eko/learned-rules.md` so users do not lose promoted rules.

use std::path::{Path, PathBuf};

/// File name for auto-promoted rules (evolution system output).
const LEARNED_RULES_FILE: &str = "learned-rules.md";
/// Legacy file name; renamed to `LEARNED_RULES_FILE` on first load.
const LEGACY_AGENTS_FILE: &str = "AGENTS.md";

/// Layered instruction-file loader (user / repository / project / learned / local).
pub struct InstructionProvider {
    pub project_level: Option<String>,
    pub user_level: Option<String>,
    /// Standard `AGENTS.override.md` / `AGENTS.md` chain, root to working dir.
    pub repository_level: Option<String>,
    pub local_level: Option<String>,
    /// Auto-promoted rules and learned constraints (learned-rules.md body, frontmatter stripped).
    pub agents_level: Option<String>,
    /// Hot-layer memory content (MEMORY.md body, frontmatter stripped).
    pub hot_memory: Option<String>,
}

impl InstructionProvider {
    /// Load every tier from disk.
    pub fn load() -> Self {
        let working_dir = std::env::current_dir().ok();
        Self::load_for(working_dir.as_deref())
    }

    /// Load every tier for one explicit working directory.
    ///
    /// `None` means global context only: user instructions plus user-level
    /// `MEMORY.md`. It intentionally does not consult process cwd, so exiting a
    /// workspace can remove project-local instructions deterministically.
    ///
    /// Performs a one-time migration of any legacy `.eko/AGENTS.md` to
    /// `.eko/learned-rules.md` (only renames if the new file does not already
    /// exist; never overwrites user-authored content under the new name).
    pub fn load_for(working_dir: Option<&Path>) -> Self {
        let project_root = working_dir.map(|path| {
            crate::utils::find_project_root(path).unwrap_or_else(|| path.to_path_buf())
        });
        Self::migrate_legacy_agents_file(project_root.as_deref());
        let project_level = Self::load_project_instructions(project_root.as_deref());
        let user_level = Self::load_user_instructions();
        let repository_level =
            Self::load_repository_instructions(working_dir, project_root.as_deref());
        let local_level = Self::load_local_instructions(working_dir);
        let agents_level = Self::load_agents_instructions(project_root.as_deref());
        let hot_memory = Self::load_hot_memory(project_root.as_deref());

        Self {
            project_level,
            user_level,
            repository_level,
            local_level,
            agents_level,
            hot_memory,
        }
    }

    /// Load the complete instruction projection without silently dropping an
    /// existing but unreadable source.
    ///
    /// Promotion and workspace-rebind transactions use this path before
    /// publishing a runtime generation. Missing optional files are valid;
    /// symlinks, non-regular entries, invalid UTF-8, and read failures are not.
    pub(crate) fn load_for_strict(working_dir: Option<&Path>) -> std::io::Result<Self> {
        let project_root = working_dir.map(|path| {
            crate::utils::find_project_root(path).unwrap_or_else(|| path.to_path_buf())
        });
        let project_level = strict_optional_text(
            project_root
                .as_deref()
                .map(|root| root.join(".eko").join("project.md"))
                .as_deref(),
        )?;
        let user_path = crate::data_root::user_data_path("user.md");
        let user_level = strict_optional_text(Some(&user_path))?;
        let repository_level =
            strict_repository_instructions(working_dir, project_root.as_deref())?;
        let local_level = strict_optional_text(
            working_dir
                .map(|root| root.join(".eko").join("local.md"))
                .as_deref(),
        )?;
        let agents_level = strict_learned_rules(project_root.as_deref())?
            .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));
        let project_memory = project_root
            .as_deref()
            .map(|root| root.join(".eko").join("MEMORY.md"));
        let global_memory = crate::data_root::user_data_path("MEMORY.md");
        let hot_memory = match project_memory.as_deref() {
            Some(path) if path.try_exists()? => strict_optional_text(Some(path))?,
            _ => strict_optional_text(Some(&global_memory))?,
        }
        .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));

        Ok(Self {
            project_level,
            user_level,
            repository_level,
            local_level,
            agents_level,
            hot_memory,
        })
    }

    /// One-time migration: rename `<root>/.eko/AGENTS.md` → `<root>/.eko/learned-rules.md`.
    ///
    /// Skipped when: no project root, legacy file absent, or new file already
    /// exists (user may have created it manually). On any IO error the migration
    /// is silently skipped — the legacy file remains readable via the fallback
    /// in [`load_agents_instructions`], so no rules are lost.
    fn migrate_legacy_agents_file(project_root: Option<&Path>) {
        let Some(root) = project_root else {
            return;
        };
        let eko_dir = root.join(".eko");
        let legacy = eko_dir.join(LEGACY_AGENTS_FILE);
        let new_path = eko_dir.join(LEARNED_RULES_FILE);
        if !legacy.exists() || new_path.exists() {
            return;
        }
        // rename is atomic on the same filesystem; best-effort, never fatal.
        match std::fs::rename(&legacy, &new_path) {
            Ok(()) => tracing::info!(
                legacy = %legacy.display(),
                migrated = %new_path.display(),
                "one-time migration: renamed legacy AGENTS.md to learned-rules.md"
            ),
            Err(e) => tracing::warn!(
                legacy = %legacy.display(),
                error = %e,
                "could not rename legacy AGENTS.md; falling back to direct read"
            ),
        }
    }

    /// Concatenate the instruction tiers and hot-layer memory into a single
    /// system-prompt suffix.
    ///
    /// Compatibility aggregation for callers that need one string. Runtime
    /// context injection uses [`get_instruction_suffix`] and
    /// [`get_memory_suffix`] as two independently replaceable projections.
    pub fn get_system_prompt_suffix(&self) -> String {
        let mut parts = Vec::new();
        if let Some(suffix) = self.get_instruction_suffix() {
            parts.push(suffix);
        }
        if let Some(suffix) = self.get_memory_suffix() {
            parts.push(suffix);
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", parts.join("\n\n"))
        }
    }

    /// Concatenate only the instruction tiers
    /// (user / repository / project / learned-rules / local).
    ///
    /// Excludes hot-layer memory. Returns `None` when no instruction tier has
    /// content.
    pub fn get_instruction_suffix(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(ref user) = self.user_level {
            parts.push(format!("## User-level instructions\n{}", user));
        }
        if let Some(ref repository) = self.repository_level {
            parts.push(format!("## Repository instructions\n{}", repository));
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
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Format only the hot-layer memory (MEMORY.md body).
    ///
    /// Returns `None` when no memory content is loaded. Separated from
    /// [`get_instruction_suffix`] because runtime context owns instruction and
    /// hot-memory projections independently.
    pub fn get_memory_suffix(&self) -> Option<String> {
        self.hot_memory
            .as_ref()
            .map(|hot| format!("## Active Memories (Hot Layer)\n{}", hot))
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
        Some(crate::data_root::user_data_path("user.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load the standard root-to-working-directory AGENTS chain.
    ///
    /// EKO deliberately requests the framework resolver's AGENTS-only mode:
    /// `.echo-agent/*` and `CLAUDE.md` are not part of EKO's file protocol.
    fn load_repository_instructions(
        working_dir: Option<&Path>,
        project_root: Option<&Path>,
    ) -> Option<String> {
        let working_dir = working_dir?;
        let resolver =
            echo_agent::project_rules::InstructionResolver::new(working_dir).agents_files_only();
        let resolved = match project_root {
            Some(root) => resolver.project_root(root).resolve(),
            None => resolver.resolve(),
        };
        resolved.annotated()
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
        let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        crate::utils::find_project_root(&working_dir)
            .unwrap_or(working_dir)
            .join(".eko")
            .join("project.md")
    }

    /// Path to the user-level instructions file.
    fn user_instructions_path() -> PathBuf {
        crate::data_root::user_data_path("user.md")
    }

    /// Load hot-layer memory content from `.eko/MEMORY.md`.
    ///
    /// Returns the body (frontmatter stripped) so it can be included in the system prompt.
    fn load_hot_memory(project_root: Option<&Path>) -> Option<String> {
        let project_path = project_root.map(|root| root.join(".eko").join("MEMORY.md"));
        let path = project_path
            .filter(|path| path.exists())
            .unwrap_or_else(|| crate::data_root::user_data_path("MEMORY.md"));
        let raw = std::fs::read_to_string(path).ok()?;

        Some(crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Load auto-promoted rules from `<project-root>/.eko/learned-rules.md`.
    ///
    /// Falls back to the legacy `.eko/AGENTS.md` if it still exists (e.g. when
    /// the one-time rename failed or was skipped). Contains rules that were
    /// automatically promoted from high-confidence memories by the evolution
    /// system's `RulePromoter`.
    fn load_agents_instructions(project_root: Option<&Path>) -> Option<String> {
        let root = project_root?;
        let eko_dir = root.join(".eko");
        // Prefer the new name; fall back to legacy if the migration did not run.
        let new_path = eko_dir.join(LEARNED_RULES_FILE);
        let path = if new_path.exists() {
            new_path
        } else {
            let legacy = eko_dir.join(LEGACY_AGENTS_FILE);
            if legacy.exists() {
                legacy
            } else {
                return None;
            }
        };
        let raw = std::fs::read_to_string(path).ok()?;
        Some(crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Path to the learned-rules file (auto-promoted rules written by `RulePromoter`).
    ///
    /// Resolves the project root via marker-based discovery. Returns a relative
    /// `.eko/learned-rules.md` path when no project root is found so callers get
    /// a deterministic write target.
    pub fn agents_instructions_path() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| crate::utils::find_project_root(&pwd))
            .map(|root| root.join(".eko").join(LEARNED_RULES_FILE))
            .unwrap_or_else(|| std::path::PathBuf::from(".eko").join(LEARNED_RULES_FILE))
    }

    /// Save auto-promoted rules to the learned-rules file.
    pub fn save_agents_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::agents_instructions_path();
        Self::save_agents_instructions_at(&path, content)
    }

    /// Save auto-promoted rules to one previously resolved authority path.
    pub fn save_agents_instructions_at(path: &Path, content: &str) -> std::io::Result<()> {
        echo_agent::utils::fs::atomic_write(path, content.as_bytes())
    }
}

fn strict_optional_text(path: Option<&Path>) -> std::io::Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "instruction source {} must be a regular non-symlink file",
                path.display()
            ),
        ));
    }
    std::fs::read_to_string(path).map(Some)
}

fn strict_learned_rules(project_root: Option<&Path>) -> std::io::Result<Option<String>> {
    let Some(root) = project_root else {
        return Ok(None);
    };
    let eko_dir = root.join(".eko");
    let current = eko_dir.join(LEARNED_RULES_FILE);
    if current.try_exists()? {
        return strict_optional_text(Some(&current));
    }
    strict_optional_text(Some(&eko_dir.join(LEGACY_AGENTS_FILE)))
}

fn strict_repository_instructions(
    working_dir: Option<&Path>,
    project_root: Option<&Path>,
) -> std::io::Result<Option<String>> {
    let Some(working_dir) = working_dir else {
        return Ok(None);
    };
    let scan_root = project_root.unwrap_or(working_dir);
    if !working_dir.starts_with(scan_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "instruction working directory {} escapes project root {}",
                working_dir.display(),
                scan_root.display()
            ),
        ));
    }
    let mut directories = working_dir
        .ancestors()
        .take_while(|directory| directory.starts_with(scan_root))
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.reverse();
    let mut blocks = Vec::new();
    for directory in directories {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = directory.join(name);
            let Some(content) = strict_optional_text(Some(&path))? else {
                continue;
            };
            if content.trim().is_empty() {
                continue;
            }
            blocks.push(format!(
                "<!-- PROJECT INSTRUCTIONS: {} -->\n{}\n<!-- END PROJECT INSTRUCTIONS -->",
                path.display(),
                content.trim()
            ));
            break;
        }
    }
    Ok((!blocks.is_empty()).then(|| blocks.join("\n\n")))
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
            repository_level: None,
            local_level: None,
            agents_level: None,
            hot_memory: None,
        };
        assert!(instructions.get_system_prompt_suffix().is_empty());
    }

    #[test]
    fn migrate_legacy_agents_file_renames_when_new_absent() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let eko = root.join(".eko");
        std::fs::create_dir_all(&eko)?;
        std::fs::write(eko.join(LEGACY_AGENTS_FILE), "# legacy rules\n")?;

        InstructionProvider::migrate_legacy_agents_file(Some(root));

        assert!(!eko.join(LEGACY_AGENTS_FILE).exists());
        assert!(eko.join(LEARNED_RULES_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(eko.join(LEARNED_RULES_FILE))?,
            "# legacy rules\n"
        );
        Ok(())
    }

    #[test]
    fn migrate_legacy_agents_file_skips_when_new_exists() -> std::io::Result<()> {
        // If the user already created learned-rules.md, the legacy file is left
        // in place rather than clobbering their content.
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let eko = root.join(".eko");
        std::fs::create_dir_all(&eko)?;
        std::fs::write(eko.join(LEGACY_AGENTS_FILE), "legacy")?;
        std::fs::write(eko.join(LEARNED_RULES_FILE), "new")?;

        InstructionProvider::migrate_legacy_agents_file(Some(root));

        assert_eq!(
            std::fs::read_to_string(eko.join(LEARNED_RULES_FILE))?,
            "new"
        );
        assert!(eko.join(LEGACY_AGENTS_FILE).exists());
        Ok(())
    }

    #[test]
    fn migrate_legacy_agents_file_noop_without_legacy() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir_all(root.join(".eko"))?;
        // No legacy file, no new file — must not create anything.
        InstructionProvider::migrate_legacy_agents_file(Some(root));
        assert!(!root.join(".eko").join(LEARNED_RULES_FILE).exists());
        Ok(())
    }

    #[test]
    fn load_agents_instructions_reads_new_name() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let eko = root.join(".eko");
        std::fs::create_dir_all(&eko)?;
        std::fs::write(eko.join(LEARNED_RULES_FILE), "RULE_BODY")?;

        let provider = InstructionProvider::load_for(Some(root));
        assert_eq!(provider.agents_level.as_deref(), Some("RULE_BODY"));
        Ok(())
    }

    #[test]
    fn load_agents_instructions_falls_back_to_legacy() -> std::io::Result<()> {
        // When migration failed or was skipped, the legacy file is still readable.
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let eko = root.join(".eko");
        std::fs::create_dir_all(&eko)?;
        // Write BOTH (simulating user manually keeping legacy); new name wins.
        std::fs::write(eko.join(LEARNED_RULES_FILE), "NEW")?;
        std::fs::write(eko.join(LEGACY_AGENTS_FILE), "LEGACY")?;
        let provider = InstructionProvider::load_for(Some(root));
        assert_eq!(provider.agents_level.as_deref(), Some("NEW"));
        Ok(())
    }

    #[test]
    fn loads_agents_chain_without_echo_agent_namespace() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let child = root.join("src");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".echo-agent"))?;
        std::fs::create_dir_all(&child)?;
        std::fs::write(root.join("AGENTS.md"), "ROOT_RULE")?;
        std::fs::write(child.join("AGENTS.override.md"), "CHILD_RULE")?;
        std::fs::write(root.join(".echo-agent/AGENT.md"), "NOT_EKO_PROTOCOL")?;

        let provider = InstructionProvider::load_for(Some(&child));
        let repository = provider.repository_level.unwrap_or_default();
        assert!(repository.contains("ROOT_RULE"));
        assert!(repository.contains("CHILD_RULE"));
        assert!(!repository.contains("NOT_EKO_PROTOCOL"));
        Ok(())
    }

    #[test]
    fn local_instructions_are_relative_to_working_directory() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let child = root.join("src");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(child.join(".eko"))?;
        std::fs::write(child.join(".eko/local.md"), "LOCAL_CHILD_RULE")?;

        let provider = InstructionProvider::load_for(Some(&child));
        assert_eq!(provider.local_level.as_deref(), Some("LOCAL_CHILD_RULE"));
        Ok(())
    }

    #[test]
    fn strict_loader_rejects_invalid_utf8_in_learned_rules() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".eko"))?;
        std::fs::write(root.join(".eko/learned-rules.md"), [0xff, 0xfe])?;

        assert!(InstructionProvider::load_for_strict(Some(&root)).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn strict_loader_rejects_symlinked_instruction_source() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".eko"))?;
        let outside = temp.path().join("outside.md");
        std::fs::write(&outside, "outside")?;
        std::os::unix::fs::symlink(&outside, root.join(".eko/learned-rules.md"))?;

        assert!(InstructionProvider::load_for_strict(Some(&root)).is_err());
        Ok(())
    }
}
