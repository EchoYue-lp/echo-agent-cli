//! Coding loop manager — orchestrates the coding workflow.
//!
//! Provides the understand → explore → plan → edit → test → fix cycle
//! for coding mode in the CLI.

use super::detector::ProjectType;
use std::path::{Path, PathBuf};

/// Manages the coding workflow for a project.
pub struct CodingLoop {
    pub project_root: PathBuf,
    pub project_type: ProjectType,
}

impl CodingLoop {
    /// Create a new coding loop for the given project root.
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_type: ProjectType::detect(project_root),
        }
    }

    /// Create a new coding loop with a known project type.
    pub fn with_type(project_root: &Path, project_type: ProjectType) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_type,
        }
    }

    /// Return the test command for the current project.
    pub fn test_command(&self) -> &str {
        self.project_type.test_command()
    }

    /// Return the lint command for the current project.
    pub fn lint_command(&self) -> &str {
        self.project_type.lint_command()
    }

    /// Get a summary of current state.
    pub fn status_summary(&self) -> String {
        format!(
            "Project: {} ({})",
            self.project_root.display(),
            self.project_type.name()
        )
    }
}
