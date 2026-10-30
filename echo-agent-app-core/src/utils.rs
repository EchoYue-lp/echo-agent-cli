//! Shared utility functions used across echo-agent-app-core.
//!
//! Extracted from `unified_memory.rs` and `instruction_provider.rs` to eliminate
//! duplicate implementations (see review P1: "三处重复实现的项目根定位 / frontmatter 解析").

use std::path::{Path, PathBuf};

/// Find the project root by walking up from the given directory.
///
/// Uses the shared marker set for both instruction loading and structural
/// project context. The first matching ancestor is authoritative.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    find_project_root_with_file_system(start, &echo_agent::tools::files::LocalFileSystem)
        .ok()
        .flatten()
}

/// Use the same marker precedence in the selected project's namespace.
pub fn find_project_root_with_file_system(
    start: &Path,
    files: &dyn echo_agent::tools::filesystem::FileSystem,
) -> echo_agent::error::Result<Option<PathBuf>> {
    const VCS_MARKERS: &[&str] = &[".git", ".hg", ".svn"];
    const FALLBACK_MARKERS: &[&str] = &[
        ".eko",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "Makefile",
    ];
    // A repository boundary outranks nested package manifests. Otherwise a
    // workspace such as `repo/crate/Cargo.toml` would hide `repo/AGENTS.md`.
    for directory in start.ancestors() {
        for marker in VCS_MARKERS {
            if files.exists(&directory.join(marker))? {
                return Ok(Some(directory.to_path_buf()));
            }
        }
    }
    for directory in start.ancestors() {
        for marker in FALLBACK_MARKERS {
            if files.exists(&directory.join(marker))? {
                return Ok(Some(directory.to_path_buf()));
            }
        }
    }
    Ok(None)
}

/// Strip a leading YAML frontmatter block (delimited by `---` fences on their
/// own line) and return the body that follows the closing fence.
///
/// Used by `InstructionProvider` to extract the body of MEMORY.md and
/// learned-rules.md for system-prompt injection.
///
/// ## Rules (tightened from the previous loose version)
///
/// - The opening fence must be the first non-whitespace content and be `---`
///   alone on its line (not `------`, not `--- title`). This avoids misclassifying
///   a Markdown horizontal rule as frontmatter.
/// - The closing fence must be `---` on its own line. If no closing fence is
///   found, the input is returned unchanged (treated as not having frontmatter,
///   rather than silently stripping the opening fence and a partial body).
/// - Whitespace and CRLF line endings are tolerated; the returned body has any
///   leading newlines trimmed and trailing newlines preserved.
/// - All scanning uses the char-safe `str::lines()` iterator — never raw byte
///   slicing (see AGENTS.md UTF-8 rule).
pub fn strip_yaml_frontmatter(raw: &str) -> String {
    // `lines()` is char-safe (splits on `\n`, strips a trailing `\r`); it does
    // not preserve a trailing newline, so we re-add one if the original had one.
    let has_trailing_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.lines().collect();
    // Find the first non-blank line (opening fence may be preceded by blanks).
    let first_content = match lines.iter().position(|line| !line.trim().is_empty()) {
        Some(idx) => idx,
        None => return raw.to_string(),
    };
    // Opening fence must be exactly `---` (alone on its line).
    if lines.get(first_content).map(|line| line.trim_end()) != Some("---") {
        return raw.to_string();
    }
    // Find the closing fence: a line equal to `---` after the opening fence.
    let closing_fence = lines
        .iter()
        .enumerate()
        .skip(first_content.saturating_add(1))
        .find_map(|(index, line)| (line.trim_end() == "---").then_some(index));
    let Some(closing_fence) = closing_fence else {
        // No closing fence — return input unchanged rather than stripping a partial block.
        return raw.to_string();
    };
    let body_lines = lines
        .get(closing_fence.saturating_add(1)..)
        .unwrap_or_default();
    let mut body: String = body_lines.join("\n");
    // Trim leading blank lines but preserve a trailing newline if the
    // original input had one and the body is non-empty.
    body = body.chars().skip_while(|c| *c == '\n').collect();
    if has_trailing_newline && !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_root_outranks_nested_package_marker() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let nested = root.join("crates/member/src");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(&nested)?;
        std::fs::write(root.join("crates/member/Cargo.toml"), "[package]")?;

        assert_eq!(find_project_root(&nested).as_deref(), Some(root.as_path()));
        Ok(())
    }

    #[test]
    fn package_marker_is_used_without_repository_metadata() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(root.join("package.json"), "{}")?;

        use echo_agent::error::Result as FileResult;
        use echo_agent::tools::files::LocalFileSystem;
        use echo_agent::tools::filesystem::{FileEntry, FileMetadata, FileSystem};
        // This unit's real filesystem has no namespace above the fixture root.
        struct DiscoveryFiles(PathBuf);
        impl DiscoveryFiles {
            fn bound<'a>(&self, path: &'a Path) -> FileResult<&'a Path> {
                if path.starts_with(&self.0) {
                    Ok(path)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "outside discovery fixture",
                    )
                    .into())
                }
            }
        }
        impl FileSystem for DiscoveryFiles {
            fn current_dir(&self) -> FileResult<PathBuf> {
                Ok(self.0.clone())
            }
            fn canonicalize(&self, p: &Path) -> FileResult<PathBuf> {
                LocalFileSystem.canonicalize(self.bound(p)?)
            }
            fn metadata(&self, p: &Path) -> FileResult<FileMetadata> {
                LocalFileSystem.metadata(self.bound(p)?)
            }
            fn symlink_metadata(&self, p: &Path) -> FileResult<FileMetadata> {
                LocalFileSystem.symlink_metadata(self.bound(p)?)
            }
            fn read(&self, p: &Path) -> FileResult<Vec<u8>> {
                LocalFileSystem.read(self.bound(p)?)
            }
            fn read_dir(&self, p: &Path) -> FileResult<Vec<FileEntry>> {
                LocalFileSystem.read_dir(self.bound(p)?)
            }
            fn create_dir_all(&self, p: &Path) -> FileResult<()> {
                LocalFileSystem.create_dir_all(self.bound(p)?)
            }
            fn create_file(&self, p: &Path) -> FileResult<()> {
                LocalFileSystem.create_file(self.bound(p)?)
            }
            fn write(&self, p: &Path, bytes: &[u8]) -> FileResult<()> {
                LocalFileSystem.write(self.bound(p)?, bytes)
            }
            fn append(&self, p: &Path, bytes: &[u8]) -> FileResult<()> {
                LocalFileSystem.append(self.bound(p)?, bytes)
            }
            fn rename(&self, a: &Path, b: &Path) -> FileResult<()> {
                LocalFileSystem.rename(self.bound(a)?, self.bound(b)?)
            }
            fn remove_file(&self, p: &Path) -> FileResult<()> {
                LocalFileSystem.remove_file(self.bound(p)?)
            }
            fn remove_dir(&self, p: &Path) -> FileResult<()> {
                LocalFileSystem.remove_dir(self.bound(p)?)
            }
            fn remove_dir_all(&self, p: &Path) -> FileResult<()> {
                LocalFileSystem.remove_dir_all(self.bound(p)?)
            }
            fn set_mode(&self, p: &Path, mode: u32) -> FileResult<()> {
                LocalFileSystem.set_mode(self.bound(p)?, mode)
            }
            fn atomic_write(&self, p: &Path, bytes: &[u8]) -> FileResult<()> {
                LocalFileSystem.atomic_write(self.bound(p)?, bytes)
            }
            fn checkpoint(&self, p: &Path) -> FileResult<Option<String>> {
                LocalFileSystem.checkpoint(self.bound(p)?)
            }
        }
        assert_eq!(
            find_project_root_with_file_system(&nested, &DiscoveryFiles(root.clone()))?.as_deref(),
            Some(root.as_path())
        );
        Ok(())
    }

    #[test]
    fn strip_frontmatter_basic() {
        let input = "---\ntitle: Memory\n---\nbody line\n";
        assert_eq!(strip_yaml_frontmatter(input), "body line\n");
    }

    #[test]
    fn strip_frontmatter_crlf() {
        // Windows line endings are normalized to `\n` in the body (consistent
        // with `str::lines()`, which strips `\r`). Memory content is then
        // newline-normal regardless of the host OS that authored the file.
        let input = "---\r\ntitle: Memory\r\n---\r\nbody\r\n";
        assert_eq!(strip_yaml_frontmatter(input), "body\n");
    }

    #[test]
    fn strip_frontmatter_no_closing_fence_returns_input_unchanged() {
        // Tightened: previously this would strip the opening fence and return
        // the partial body; now we leave the input alone because a missing
        // closing fence means this is not valid frontmatter.
        let input = "---\nkey: value\nbody without closing fence";
        assert_eq!(strip_yaml_frontmatter(input), input);
    }

    #[test]
    fn strip_frontmatter_no_frontmatter_returns_input() {
        let input = "# Title\n\nbody";
        assert_eq!(strip_yaml_frontmatter(input), input);
    }

    #[test]
    fn strip_frontmatter_leading_blank_lines_before_fence() {
        let input = "\n\n---\nkey: v\n---\nbody";
        assert_eq!(strip_yaml_frontmatter(input), "body");
    }

    #[test]
    fn strip_frontmatter_horizontal_rule_not_treated_as_fence() {
        // `------` (long horizontal rule) is NOT an opening fence.
        let input = "------\nbody";
        assert_eq!(strip_yaml_frontmatter(input), input);
    }

    #[test]
    fn strip_frontmatter_empty_fence_pair() {
        // Empty frontmatter block still strips; body is the remainder.
        let input = "---\n---\nbody";
        assert_eq!(strip_yaml_frontmatter(input), "body");
    }

    #[test]
    fn strip_frontmatter_multibyte_body_preserved() {
        // UTF-8 safety: Chinese body content must not be corrupted.
        let input = "---\nkey: v\n---\n记忆内容\n";
        assert_eq!(strip_yaml_frontmatter(input), "记忆内容\n");
    }
}
