//! Project type auto-detection for coding mode.
//!
//! Detects the project type (Rust/Node/Python/Go/Java) from the project root
//! and provides appropriate test/lint/format commands.

use std::path::Path;

/// Detected project type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Rust,
    Node,
    Python,
    Go,
    Java,
    Unknown,
}

impl ProjectType {
    /// Detect project type from the given root directory.
    pub fn detect(root: &Path) -> Self {
        if root.join("Cargo.toml").exists() {
            return Self::Rust;
        }
        if root.join("package.json").exists() {
            return Self::Node;
        }
        if root.join("pyproject.toml").exists()
            || root.join("setup.py").exists()
            || root.join("requirements.txt").exists()
        {
            return Self::Python;
        }
        if root.join("go.mod").exists() {
            return Self::Go;
        }
        if root.join("pom.xml").exists() || root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
            return Self::Java;
        }
        Self::Unknown
    }

    /// Return the default test command for this project type.
    pub fn test_command(&self) -> &str {
        match self {
            Self::Rust => "cargo test",
            Self::Node => "npm test",
            Self::Python => "pytest",
            Self::Go => "go test ./...",
            Self::Java => "mvn test",
            Self::Unknown => "",
        }
    }

    /// Return the default lint command.
    pub fn lint_command(&self) -> &str {
        match self {
            Self::Rust => "cargo clippy -- -D warnings",
            Self::Node => "npm run lint",
            Self::Python => "ruff check .",
            Self::Go => "golangci-lint run",
            Self::Java => "mvn checkstyle:check",
            Self::Unknown => "",
        }
    }

    /// Human-readable name.
    pub fn name(&self) -> &str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "Node.js",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::Unknown => "Unknown",
        }
    }
}
