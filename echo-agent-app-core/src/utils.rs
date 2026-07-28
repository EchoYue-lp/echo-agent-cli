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
    for idx in (first_content + 1)..lines.len() {
        if lines[idx].trim_end() == "---" {
            // Body = everything after the closing fence.
            let body_lines = &lines[idx + 1..];
            let mut body: String = body_lines.join("\n");
            // Trim leading blank lines but preserve a trailing newline if the
            // original input had one and the body is non-empty.
            body = body.chars().skip_while(|c| *c == '\n').collect();
            if has_trailing_newline && !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            return body;
        }
    }
    // No closing fence — return input unchanged rather than stripping a partial block.
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
