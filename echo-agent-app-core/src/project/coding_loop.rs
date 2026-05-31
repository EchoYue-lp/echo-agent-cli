//! Coding loop manager — orchestrates the coding workflow.
//!
//! Provides the understand → explore → plan → edit → test → fix cycle
//! for coding mode in the CLI.

use super::detector::ProjectType;
use super::file_tracker::FileChangeTracker;
use std::path::{Path, PathBuf};

/// Manages the coding workflow for a project.
pub struct CodingLoop {
    pub project_root: PathBuf,
    pub project_type: ProjectType,
    pub file_tracker: FileChangeTracker,
}

impl CodingLoop {
    /// Create a new coding loop for the given project root.
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_type: ProjectType::detect(project_root),
            file_tracker: FileChangeTracker::new(),
        }
    }

    /// Create a new coding loop with a known project type.
    pub fn with_type(project_root: &Path, project_type: ProjectType) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_type,
            file_tracker: FileChangeTracker::new(),
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

    /// Record a file write in the change tracker.
    pub fn record_file_write(&mut self, path: &str) {
        self.file_tracker.record_write(path);
    }

    /// Record a file deletion.
    pub fn record_file_delete(&mut self, path: &str) {
        self.file_tracker.record_delete(path);
    }

    /// Generate a diff summary of all changes.
    pub fn diff_summary(&self) -> String {
        self.file_tracker.diff_summary()
    }

    /// Get the number of tracked file changes.
    pub fn change_count(&self) -> usize {
        self.file_tracker.change_count()
    }

    /// Clear tracked file changes (e.g., after commit).
    pub fn clear_changes(&mut self) {
        self.file_tracker.clear();
    }

    /// Get a summary of current state.
    pub fn status_summary(&self) -> String {
        let mut lines = vec![
            format!(
                "Project: {} ({})",
                self.project_root.display(),
                self.project_type.name()
            ),
            format!("Files changed: {}", self.file_tracker.change_count()),
        ];
        if self.file_tracker.change_count() > 0 {
            lines.push(self.file_tracker.diff_summary());
        }
        lines.join("\n")
    }
}
