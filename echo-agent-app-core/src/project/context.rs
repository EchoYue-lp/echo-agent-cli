use std::path::{Path, PathBuf};
use tracing;

use super::gitignore::GitIgnore;

const PROJECT_CONTEXT_FILES: &[&str] = &[
    // Cross-tool community standard (Codex / Aider / Cursor partial).
    "AGENTS.md",
    // echo-agent's own private namespace, for users who want instructions
    // that apply only to this product and not to other AI tools.
    "ECHO_AGENT.md",
];

const PROJECT_DIR_MARKERS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "pom.xml",
    "Makefile",
];

#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub name: String,
    pub instructions: Vec<LoadedInstruction>,
    pub file_tree_summary: String,
    /// Parsed .gitignore rules (empty if no .gitignore exists).
    pub gitignore: GitIgnore,
}

#[derive(Debug, Clone)]
pub struct LoadedInstruction {
    pub source: String,
    pub content: String,
}

pub fn discover_project_root(start: Option<&Path>) -> Option<PathBuf> {
    let start = start.unwrap_or_else(|| Path::new("."));
    let mut dir = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };

    loop {
        for marker in PROJECT_DIR_MARKERS {
            if dir.join(marker).exists() {
                return Some(dir);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn load_project_context(project_root: &Path) -> ProjectContext {
    let name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut instructions = Vec::new();

    for filename in PROJECT_CONTEXT_FILES {
        let path = project_root.join(filename);
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if !content.trim().is_empty() {
                        tracing::info!("加载项目指令: {}", path.display());
                        instructions.push(LoadedInstruction {
                            source: filename.to_string(),
                            content,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("无法读取 {}: {}", path.display(), e);
                }
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let home_instructions = [
        format!("{}/.echo-agent/AGENTS.md", home),
        format!("{}/.echo-agent/instructions.md", home),
    ];
    for path_str in &home_instructions {
        let path = Path::new(path_str);
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    if !content.trim().is_empty() {
                        tracing::info!("加载全局指令: {}", path.display());
                        instructions.push(LoadedInstruction {
                            source: format!(
                                "~/.echo-agent/{}",
                                path.file_name().unwrap().to_string_lossy()
                            ),
                            content,
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read global instruction file"
                    );
                }
            }
        }
    }

    let file_tree_summary = generate_file_tree_summary(project_root);
    let gitignore = GitIgnore::load(project_root);

    ProjectContext {
        root: project_root.to_path_buf(),
        name,
        instructions,
        file_tree_summary,
        gitignore,
    }
}

impl ProjectContext {
    /// Check whether a path (relative to the project root) should be ignored
    /// by file tools. The check combines:
    ///
    /// 1. Built-in skip directories (`target`, `node_modules`, `.git`, etc.)
    /// 2. `.gitignore` rules loaded from the project root
    pub fn should_ignore_path(&self, relative_path: &str, is_dir: bool) -> bool {
        // Check built-in skip dirs first
        let first_component = relative_path.split('/').next().unwrap_or(relative_path);
        if SKIP_DIRS.contains(&first_component) {
            return true;
        }
        // Then check gitignore rules
        self.gitignore.is_ignored(relative_path, is_dir)
    }
}

pub fn build_system_prompt_with_context(base_prompt: &str, context: &ProjectContext) -> String {
    let mut prompt = base_prompt.to_string();

    if !context.instructions.is_empty() {
        prompt.push_str("\n\n## Project Instructions\n\n");
        for inst in &context.instructions {
            prompt.push_str(&format!(
                "### From: {}\n\n{}\n\n",
                inst.source, inst.content
            ));
        }
    }

    if !context.file_tree_summary.is_empty() {
        prompt.push_str(&format!(
            "\n## Project Structure ({})\n\n```\n{}\n```\n",
            context.name, context.file_tree_summary
        ));
    }

    // Inject git context if available
    if let Some(git_ctx) = load_git_context(&context.root) {
        prompt.push_str("\n## Git Status\n\n");
        prompt.push_str(&git_ctx);
        prompt.push('\n');
    }

    prompt
}

/// Load git status and diff summary from the project root.
pub fn load_git_context(project_root: &std::path::Path) -> Option<String> {
    let git_dir = project_root.join(".git");
    if !git_dir.exists() {
        return None;
    }

    let status = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(project_root)
        .output()
        .ok()?;
    let status_str = String::from_utf8_lossy(&status.stdout).to_string();

    let diff = std::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(project_root)
        .output()
        .ok()?;
    let diff_str = String::from_utf8_lossy(&diff.stdout).to_string();

    let mut result = String::new();
    if !status_str.trim().is_empty() {
        result.push_str(&format!("```\n{}\n```\n", status_str.trim()));
    }
    if !diff_str.trim().is_empty() {
        result.push_str(&format!("```\n{}\n```\n", diff_str.trim()));
    }
    if result.is_empty() {
        return None;
    }
    Some(result)
}

fn generate_file_tree_summary(root: &Path) -> String {
    let mut entries = Vec::new();
    collect_dir_entries(root, &mut entries, 0, 3);
    entries.join("\n")
}

fn collect_dir_entries(dir: &Path, out: &mut Vec<String>, depth: usize, max_depth: usize) {
    if depth > max_depth {
        return;
    }

    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".env.example" {
            continue;
        }
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }

        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }

    dirs.sort();
    files.sort();

    let indent = "  ".repeat(depth);

    for name in &files {
        if files.len() > 20 && out.len() > 50 {
            out.push(format!("{}  ... ({} more files)", indent, files.len() - 20));
            break;
        }
        out.push(format!("{}{}", indent, name));
    }

    for dir_name in &dirs {
        if out.len() > 80 {
            out.push(format!("{}  ... (truncated)", indent));
            return;
        }
        out.push(format!("{}{}/", indent, dir_name));
        collect_dir_entries(&dir.join(dir_name), out, depth + 1, max_depth);
    }
}

const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    ".DS_Store",
];
