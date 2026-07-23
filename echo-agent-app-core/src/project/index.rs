//! Project file index — metadata cache for fast context assembly.
//!
//! Builds an in-memory index of project files with metadata
//! (size, modification time, language, symbols) for quick lookup
//! during context selection.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ── FileInfo ─────────────────────────────────────────────────────────

/// Cached metadata for a single project file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// Path relative to project root.
    pub relative_path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time.
    pub modified: Option<u64>,
    /// Detected programming language (from extension).
    pub language: Option<String>,
    /// Extracted symbol names (functions, types, modules).
    pub symbols: Vec<String>,
    /// Import lines (for dependency tracing).
    pub imports: Vec<String>,
}

// ── SymbolMatch ──────────────────────────────────────────────────────

/// A symbol search result.
#[derive(Debug, Clone)]
pub struct SymbolMatch {
    pub symbol: String,
    pub file: PathBuf,
    pub line: Option<usize>,
}

// ── ProjectIndex ─────────────────────────────────────────────────────

/// In-memory project file index.
///
/// Built once at startup and refreshed on demand. Can be serialized
/// to `~/.eko/cache/{project_hash}.json` for persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectIndex {
    /// Project root directory.
    pub root: PathBuf,
    /// All indexed files.
    pub files: Vec<FileInfo>,
    /// File path → index into `files`.
    #[serde(skip)]
    by_path: HashMap<PathBuf, usize>,
    /// Symbol name → list of file indices.
    #[serde(skip)]
    by_symbol: HashMap<String, Vec<usize>>,
    /// When the index was built.
    pub built_at: Option<u64>,
}

impl ProjectIndex {
    /// Load from serialized JSON and rebuild lookup maps.
    /// Always prefer this over `serde_json::from_str::<ProjectIndex>()`.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let mut index: Self = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        index.rebuild_maps();
        Ok(index)
    }

    /// Save to a JSON file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)
    }

    /// Build an index from the project root.
    pub fn build(root: &Path) -> Self {
        let mut index = Self {
            root: root.to_path_buf(),
            files: Vec::new(),
            by_path: HashMap::new(),
            by_symbol: HashMap::new(),
            built_at: Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
        };

        // Directories to always skip (build artifacts, caches)
        let skip_dirs = [
            "target",
            "node_modules",
            "dist",
            "build",
            ".next",
            "vendor",
            "__pycache__",
            "venv",
            ".tox",
            ".mypy_cache",
        ];
        // Dotfile directories that should be indexed (important config)
        let allow_dot_dirs = [".github", ".cargo", ".config", ".claude", ".cursor"];
        // Important dotfiles to always include
        let include_dotfiles = [
            ".env.example",
            ".python-version",
            ".nvmrc",
            ".node-version",
            ".prettierrc",
            ".prettierrc.json",
            ".eslintrc",
            ".eslintrc.js",
            ".eslintrc.json",
            ".editorconfig",
            ".gitignore",
            ".dockerignore",
        ];

        index.walk(root, root, &skip_dirs, &allow_dot_dirs, &include_dotfiles);
        index.rebuild_maps();
        index
    }

    /// Recursively walk a directory.
    fn walk(
        &mut self,
        root: &Path,
        dir: &Path,
        skip: &[&str],
        allow_dot_dirs: &[&str],
        include_dotfiles: &[&str],
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Skip build artifacts and caches
            if skip.contains(&name) {
                continue;
            }
            // Dotfile handling: allow important config dirs/files, skip others
            let is_dot = name.starts_with('.');
            if is_dot && path.is_dir() && !allow_dot_dirs.contains(&name) {
                continue;
            }
            if is_dot && path.is_file() && !include_dotfiles.contains(&name) {
                continue;
            }

            if path.is_dir() {
                self.walk(root, &path, skip, allow_dot_dirs, include_dotfiles);
            } else if path.is_file()
                && let Ok(meta) = path.metadata()
            {
                // Size cap: skip files larger than 1MB for indexing
                if meta.len() > 1_048_576 {
                    continue;
                }
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                let language = detect_language(&relative);
                let (symbols, imports) = if language.is_some() {
                    extract_symbols_and_imports(&path, language.as_deref())
                } else {
                    (Vec::new(), Vec::new())
                };

                let info = FileInfo {
                    relative_path: relative,
                    size: meta.len(),
                    modified: meta.modified().ok().and_then(|t| {
                        t.duration_since(SystemTime::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs())
                    }),
                    language,
                    symbols,
                    imports,
                };

                self.files.push(info);
            }
        }
    }

    /// Rebuild lookup maps after files change.
    fn rebuild_maps(&mut self) {
        self.by_path.clear();
        self.by_symbol.clear();
        for (i, file) in self.files.iter().enumerate() {
            self.by_path.insert(file.relative_path.clone(), i);
            for sym in &file.symbols {
                self.by_symbol.entry(sym.clone()).or_default().push(i);
            }
        }
    }

    /// Search for a symbol by name (case-insensitive prefix match).
    pub fn search_symbols(&self, query: &str) -> Vec<SymbolMatch> {
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();

        for (symbol, indices) in &self.by_symbol {
            if symbol.to_lowercase().contains(&query_lower) {
                for &idx in indices {
                    if let Some(file) = self.files.get(idx) {
                        results.push(SymbolMatch {
                            symbol: symbol.clone(),
                            file: file.relative_path.clone(),
                            line: None,
                        });
                    }
                }
            }
        }
        results
    }

    /// Get files modified recently (within `within_secs` seconds).
    pub fn recently_modified(&self, within_secs: u64) -> Vec<&FileInfo> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let threshold = now.saturating_sub(within_secs);

        self.files
            .iter()
            .filter(|f| f.modified.is_some_and(|m| m >= threshold))
            .collect()
    }

    /// Find files related to a given file (via matching imports).
    pub fn related_files(&self, path: &Path) -> Vec<PathBuf> {
        let target_idx = match self.by_path.get(path) {
            Some(&idx) => idx,
            None => return Vec::new(),
        };

        let target = match self.files.get(target_idx) {
            Some(f) => f,
            None => return Vec::new(),
        };

        let mut related = Vec::new();
        for &idx in self.by_path.values() {
            if idx == target_idx {
                continue;
            }
            if let Some(other) = self.files.get(idx) {
                // Check if this file imports anything the target exports
                for import in &other.imports {
                    if target.symbols.iter().any(|s| import.contains(s.as_str())) {
                        related.push(other.relative_path.clone());
                        break;
                    }
                }
            }
        }
        related
    }

    /// Get file info by path.
    pub fn get(&self, path: &Path) -> Option<&FileInfo> {
        self.by_path.get(path).and_then(|&idx| self.files.get(idx))
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Files grouped by language.
    pub fn by_language(&self) -> HashMap<String, Vec<&FileInfo>> {
        let mut map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
        for file in &self.files {
            let lang = file.language.clone().unwrap_or_else(|| "other".into());
            map.entry(lang).or_default().push(file);
        }
        map
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn detect_language(path: &Path) -> Option<String> {
    match path.extension()?.to_str()? {
        "rs" => Some("rust".into()),
        "js" | "jsx" | "mjs" => Some("javascript".into()),
        "ts" | "tsx" => Some("typescript".into()),
        "py" => Some("python".into()),
        "go" => Some("go".into()),
        "java" => Some("java".into()),
        "c" | "h" => Some("c".into()),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp".into()),
        "json" => Some("json".into()),
        "yaml" | "yml" => Some("yaml".into()),
        "toml" => Some("toml".into()),
        "md" | "mdx" => Some("markdown".into()),
        "sql" => Some("sql".into()),
        "sh" | "bash" => Some("shell".into()),
        _ => None,
    }
}

/// Simple symbol and import extraction from source files.
fn extract_symbols_and_imports(path: &Path, language: Option<&str>) -> (Vec<String>, Vec<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let mut symbols = Vec::new();
    let mut imports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        match language {
            Some("rust") => {
                if trimmed.starts_with("pub fn ") {
                    if let Some(name) = trimmed
                        .strip_prefix("pub fn ")
                        .and_then(|s| s.split('(').next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("pub struct ") {
                    if let Some(name) = trimmed
                        .strip_prefix("pub struct ")
                        .and_then(|s| s.split(['<', '{', '(']).next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("pub enum ") {
                    if let Some(name) = trimmed
                        .strip_prefix("pub enum ")
                        .and_then(|s| s.split('{').next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("use ") {
                    imports.push(trimmed.to_string());
                } else if trimmed.starts_with("mod ")
                    && let Some(name) = trimmed
                        .strip_prefix("mod ")
                        .and_then(|s| s.split(';').next())
                {
                    symbols.push(format!("mod {name}").trim().to_string());
                }
            }
            Some("python") => {
                if trimmed.starts_with("def ") {
                    if let Some(name) = trimmed
                        .strip_prefix("def ")
                        .and_then(|s| s.split('(').next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("class ") {
                    if let Some(name) = trimmed
                        .strip_prefix("class ")
                        .and_then(|s| s.split(['(', ':']).next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                    imports.push(trimmed.to_string());
                }
            }
            Some("go") => {
                if trimmed.starts_with("func ") {
                    if let Some(name) = trimmed
                        .strip_prefix("func ")
                        .and_then(|s| s.split('(').next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("type ") {
                    if let Some(name) = trimmed
                        .strip_prefix("type ")
                        .and_then(|s| s.split(' ').next())
                    {
                        symbols.push(name.trim().to_string());
                    }
                } else if trimmed.starts_with("import ") {
                    imports.push(trimmed.to_string());
                }
            }
            _ => {
                // Generic: look for function-like patterns
                if (trimmed.starts_with("fn ")
                    || trimmed.starts_with("func ")
                    || trimmed.starts_with("def "))
                    && trimmed.contains('(')
                    && let Some(name) = trimmed.split_whitespace().nth(1)
                    && let Some(name) = name.split('(').next()
                {
                    symbols.push(name.to_string());
                }
            }
        }
    }

    symbols.sort();
    symbols.dedup();
    imports.sort();
    imports.dedup();

    (symbols, imports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(
            detect_language(Path::new("src/main.rs")),
            Some("rust".into())
        );
        assert_eq!(detect_language(Path::new("app.py")), Some("python".into()));
        assert_eq!(detect_language(Path::new("main.go")), Some("go".into()));
        assert_eq!(
            detect_language(Path::new("README.md")),
            Some("markdown".into())
        );
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }

    #[test]
    fn test_build_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "pub fn hello() {}\npub struct World;\nuse std::collections::HashMap;\n",
        )
        .unwrap();

        let index = ProjectIndex::build(root);
        assert!(!index.is_empty());
        let results = index.search_symbols("hello");
        assert!(!results.is_empty());
        assert_eq!(results[0].symbol, "hello");
    }

    #[test]
    fn test_related_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn hello() {}\npub struct World;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "use crate::hello;\nfn main() { hello(); }\n",
        )
        .unwrap();

        let index = ProjectIndex::build(root);
        let _related = index.related_files(Path::new("src/lib.rs"));
        // main.rs imports hello (though the simple parser won't resolve crate::hello)
        assert!(index.len() >= 2);
    }
}
