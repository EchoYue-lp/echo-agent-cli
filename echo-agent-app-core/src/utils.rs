//! Shared utility functions used across echo-agent-app-core.
//!
//! Extracted from `unified_memory.rs` and `instruction_provider.rs` to eliminate
//! duplicate implementations (see review P1: "三处重复实现的项目根定位 / frontmatter 解析").

use std::path::{Path, PathBuf};

/// Find the project root by walking up from the given directory.
///
/// Looks for `.eko` or `.git` markers — the same logic used by
/// by `InstructionProvider` and project-context discovery.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".eko").exists() || dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Strip YAML frontmatter (between `---` markers) from a MEMORY.md file.
///
/// `InstructionProvider::load` parses MEMORY.md content and needs the body
/// without the frontmatter block.
pub fn strip_yaml_frontmatter(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return raw.to_string();
    }

    // Strip the opening `---` fence using character-aware APIs (never byte
    // slicing — see AGENTS.md UTF-8 rule). `---` is ASCII so the boundary is
    // safe, but we keep the code character-safe by construction.
    let rest = trimmed.trim_start_matches("---");
    if let Some((_fence, body)) = rest.split_once("\n---") {
        return body
            .trim_start_matches('\n')
            .trim_start_matches('\r')
            .to_string();
    }

    // No closing marker — return as-is
    raw.to_string()
}
