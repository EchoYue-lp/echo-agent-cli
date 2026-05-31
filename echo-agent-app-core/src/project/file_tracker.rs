//! File change tracking for coding mode.
//!
//! Accumulates file changes made during a coding session and generates
//! unified diffs for review.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;

/// Type of file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

/// A single file change record.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub timestamp: DateTime<Utc>,
}

/// Accumulates file changes and generates diffs.
#[derive(Debug, Default)]
pub struct FileChangeTracker {
    changes: HashMap<PathBuf, FileChange>,
}

impl FileChangeTracker {
    pub fn new() -> Self {
        Self {
            changes: HashMap::new(),
        }
    }

    /// Record a file modification.
    pub fn record_change(&mut self, path: PathBuf, change_type: ChangeType) {
        self.changes.insert(
            path.clone(),
            FileChange {
                path,
                change_type,
                timestamp: Utc::now(),
            },
        );
    }

    /// Record a file write/edit.
    pub fn record_write(&mut self, path: &str) {
        let pb = PathBuf::from(path);
        let change_type = if pb.exists() {
            ChangeType::Modified
        } else {
            ChangeType::Created
        };
        self.record_change(pb, change_type);
    }

    /// Record a file deletion.
    pub fn record_delete(&mut self, path: &str) {
        self.record_change(PathBuf::from(path), ChangeType::Deleted);
    }

    /// List all tracked changes.
    pub fn list_changes(&self) -> Vec<&FileChange> {
        let mut changes: Vec<&FileChange> = self.changes.values().collect();
        changes.sort_by_key(|c| c.timestamp);
        changes
    }

    /// Return the count of tracked changes.
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    /// Check if a specific file has been changed.
    pub fn has_changed(&self, path: &str) -> bool {
        self.changes.contains_key(&PathBuf::from(path))
    }

    /// Clear all tracked changes.
    pub fn clear(&mut self) {
        self.changes.clear();
    }

    /// Generate a summary of all changes.
    pub fn diff_summary(&self) -> String {
        let changes = self.list_changes();
        if changes.is_empty() {
            return "No file changes".to_string();
        }
        let mut lines = vec![format!("{} files changed:", changes.len())];
        for change in &changes {
            let icon = match change.change_type {
                ChangeType::Created => "+",
                ChangeType::Modified => "M",
                ChangeType::Deleted => "-",
            };
            lines.push(format!("  {} {}", icon, change.path.display()));
        }
        lines.join("\n")
    }
}
