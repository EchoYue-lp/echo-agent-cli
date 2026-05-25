//! Task TODO tracking for coding mode.
//!
//! Tracks coding tasks with status management (pending → in_progress → done).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Cancelled,
}

/// A single coding task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingTask {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub files: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Tracks coding tasks during a session.
#[derive(Debug)]
pub struct TaskTracker {
    tasks: Vec<CodingTask>,
    next_id: AtomicUsize,
}

impl TaskTracker {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: AtomicUsize::new(1),
        }
    }

    /// Add a new task.
    pub fn add(&mut self, description: &str) -> &CodingTask {
        let id = format!("task_{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let now = Utc::now();
        self.tasks.push(CodingTask {
            id,
            description: description.to_string(),
            status: TaskStatus::Pending,
            files: Vec::new(),
            created_at: now,
            updated_at: now,
        });
        self.tasks.last().unwrap()
    }

    /// Update task status.
    pub fn update_status(&mut self, task_id: &str, status: TaskStatus) -> bool {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            task.status = status;
            task.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Mark a task as done.
    pub fn mark_done(&mut self, task_id: &str) -> bool {
        self.update_status(task_id, TaskStatus::Done)
    }

    /// Mark a task as in progress.
    pub fn mark_in_progress(&mut self, task_id: &str) -> bool {
        self.update_status(task_id, TaskStatus::InProgress)
    }

    /// List all tasks.
    pub fn list(&self) -> &[CodingTask] {
        &self.tasks
    }

    /// List tasks filtered by status.
    pub fn list_by_status(&self, status: TaskStatus) -> Vec<&CodingTask> {
        self.tasks.iter().filter(|t| t.status == status).collect()
    }

    /// Get pending task count.
    pub fn pending_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.status == TaskStatus::Pending).count()
    }
}

impl Default for TaskTracker {
    fn default() -> Self {
        Self::new()
    }
}
