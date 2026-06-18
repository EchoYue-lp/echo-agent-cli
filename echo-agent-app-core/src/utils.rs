//! Shared utility functions used across echo-agent-app-core.
//!
//! Extracted from `unified_memory.rs` and `instruction_provider.rs` to eliminate
//! duplicate implementations (see review P1: "三处重复实现的项目根定位 / frontmatter 解析").

use std::path::{Path, PathBuf};

/// Find the project root by walking up from the given directory.
///
/// Looks for `.echo-agent` or `.git` markers — the same logic used by
/// both `UnifiedMemory` and `InstructionProvider`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".echo-agent").exists() || dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Strip YAML frontmatter (between `---` markers) from a MEMORY.md file.
///
/// Both `UnifiedMemory::load_hot_content` and `InstructionProvider::load`
/// parse MEMORY.md content and need the body without the frontmatter block.
pub fn strip_yaml_frontmatter(raw: &str) -> String {
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
