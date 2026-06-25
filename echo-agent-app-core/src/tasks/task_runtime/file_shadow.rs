//! File-based shadow store (U1c phase-0/0a step 9).
//!
//! Writes a non-authoritative file mirror (`events.jsonl` + `plan.json`) of the
//! SQLite `TaskRuntimeStore`, so that 0b can switch the read path to files and
//! 0c can retire SQL. In 0a the SQL store remains the read/write authority;
//! this is a shadow that must stay in parity (verified by the parity test).
//!
//! Layout: `{root}/{run_id}/events.jsonl` (append-only) + `plan.json` (snapshot).
//! `root` defaults to `~/.echo-agent/tasks/` (global, spec §2 path A).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::event_rebuild::{RebuiltPlan, rebuild_plan_from_events};
use super::store::TaskRuntimeStore;
use super::types::RuntimeTaskEvent;

/// Shadow writer for one root directory. Cheap to clone (wraps a root path; the
/// append lock is per-run via the `events.jsonl` file handle being held briefly).
#[derive(Clone)]
pub struct FileTaskShadow {
    root: PathBuf,
}

impl FileTaskShadow {
    /// Create a shadow rooted at `root`. The directory is created lazily on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default shadow root: `~/.echo-agent/tasks/`.
    pub fn default_root() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".echo-agent")
            .join("tasks")
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.join(run_id)
    }

    fn events_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("events.jsonl")
    }

    fn plan_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("plan.json")
    }

    /// Re-read all events for `run_id` via the caller's already-held `conn`
    /// guard (the SQL authority), and rewrite `events.jsonl` + `plan.json` from
    /// the rebuilt snapshot.
    ///
    /// Taking `&Connection` (not `&TaskRuntimeStore`) avoids re-locking the store
    /// mutex — callers hold the guard for the whole write method, so re-locking
    /// would deadlock. O(events) per flush; acceptable for a non-authoritative
    /// shadow in 0a (0b optimizes to incremental append).
    pub fn flush_from_conn(
        &self,
        conn: &rusqlite::Connection,
        run_id: &str,
    ) -> Result<(), ShadowError> {
        let events = TaskRuntimeStore::list_events_conn(conn, run_id, 0)
            .map_err(|e| ShadowError::Read(e.to_string()))?;
        if events.is_empty() {
            return Ok(());
        }

        // Ensure run dir exists.
        let dir = self.run_dir(run_id);
        std::fs::create_dir_all(&dir).map_err(|e| ShadowError::Io(e.to_string()))?;

        // Rewrite events.jsonl (full — shadow is non-authoritative; simplest correct form).
        let events_path = self.events_path(run_id);
        let mut text = String::new();
        for ev in &events {
            // Each line: one RuntimeTaskEvent as JSON. (seq + payload + kind.)
            let line = serde_json::to_string(ev).map_err(|e| ShadowError::Encode(e.to_string()))?;
            text.push_str(&line);
            text.push('\n');
        }
        atomic_write(&events_path, text.as_bytes()).map_err(|e| ShadowError::Io(e.to_string()))?;

        // Rebuild plan.json from the events (gate 1 proved this is faithful).
        let rebuilt =
            rebuild_plan_from_events(&events).map_err(|e| ShadowError::Rebuild(e.to_string()))?;
        let plan_json = serde_json::to_string_pretty(&rebuilt)
            .map_err(|e| ShadowError::Encode(e.to_string()))?;
        atomic_write(&self.plan_path(run_id), plan_json.as_bytes())
            .map_err(|e| ShadowError::Io(e.to_string()))?;

        Ok(())
    }

    /// Read the shadow plan.json for parity comparison. Returns None if not yet written.
    pub fn read_plan(&self, run_id: &str) -> Result<Option<RebuiltPlan>, ShadowError> {
        let path = self.plan_path(run_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ShadowError::Io(e.to_string()))?;
        let plan: RebuiltPlan =
            serde_json::from_str(&text).map_err(|e| ShadowError::Decode(e.to_string()))?;
        Ok(Some(plan))
    }

    /// Read the shadow events.jsonl for parity comparison.
    pub fn read_events(&self, run_id: &str) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        let path = self.events_path(run_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ShadowError::Io(e.to_string()))?;
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let ev: RuntimeTaskEvent = serde_json::from_str(line)
                .map_err(|e| ShadowError::Decode(format!("line {}: {}", i + 1, e)))?;
            out.push(ev);
        }
        Ok(out)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error("shadow read failed: {0}")]
    Read(String),
    #[error("shadow io: {0}")]
    Io(String),
    #[error("shadow encode: {0}")]
    Encode(String),
    #[error("shadow decode: {0}")]
    Decode(String),
    #[error("shadow rebuild: {0}")]
    Rebuild(String),
}

/// Write `bytes` to `path` atomically: write to `path.tmp`, fsync, rename over `path`.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// `Mutex` import retained for future per-run append-lock; the current shadow uses
// full-rewrite which is already serialized by the single `TaskRuntimeStore` mutex.
#[allow(dead_code)]
fn _lock_type_anchor() -> Mutex<()> {
    Mutex::new(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPatch, TaskPlan, TodoStatus,
    };
    use std::sync::Arc;

    fn task(id: &str, kind: PlanTaskKind) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind,
            agent_role: "code_reviewer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: Vec::new(),
            parallel_group: None,
            files: vec!["src/a.rs".to_string()],
            allowed_tools: vec!["read_file".to_string()],
            verification: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            sort_order: 0,
        }
    }

    /// Parity: after attaching a shadow and driving a full lifecycle, the file
    /// mirror (events.jsonl + plan.json) must agree with the SQL store on events
    /// and on the rebuilt plan. This is the 0a acceptance gate for the shadow.
    #[test]
    fn shadow_parity_after_full_lifecycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let mut store = TaskRuntimeStore::new_in_memory().expect("store");
        store.attach_shadow(shadow.clone());

        // Drive a lifecycle.
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review runtime",
                "complex_runtime",
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            domain_profile: DomainProfile::AiCoding,
            goal: "review runtime".to_string(),
            assumptions: vec!["small repo".to_string()],
            risks: vec!["flaky tests".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::ReadOnlyReview),
                task("t2", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan(&plan).unwrap();
        store
            .update_task(
                "r1",
                "t1",
                TaskPatch {
                    title: Some("renamed t1".to_string()),
                    description: None,
                    kind: None,
                    agent_role: None,
                    depends_on: None,
                    files: None,
                    allowed_tools: None,
                },
            )
            .unwrap();
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("explorer"), None)
            .unwrap();

        // Parity 1: event count matches.
        let sql_events = store.list_events("r1", 0).unwrap();
        let file_events = shadow.read_events("r1").unwrap();
        assert_eq!(
            sql_events.len(),
            file_events.len(),
            "event count parity: sql={} file={}",
            sql_events.len(),
            file_events.len()
        );
        // Parity 2: event seqs + kinds match (payloads are enriched identically).
        for (s, f) in sql_events.iter().zip(file_events.iter()) {
            assert_eq!(s.seq, f.seq, "seq parity");
            assert_eq!(s.event_type, f.event_type, "kind parity at seq {}", s.seq);
        }

        // Parity 3: file plan.json matches SQL-rebuilt plan (via the shadow's own rebuild).
        let file_plan = shadow.read_plan("r1").unwrap().expect("plan.json written");
        let rebuilt = rebuild_plan_from_events(&sql_events).unwrap();
        assert_eq!(file_plan.run.run_id, rebuilt.run.run_id);
        assert_eq!(file_plan.run.goal, rebuilt.run.goal);
        assert_eq!(file_plan.run.route, rebuilt.run.route);
        assert_eq!(file_plan.plan.plan_id, rebuilt.plan.plan_id);
        assert_eq!(file_plan.plan.execution_mode, rebuilt.plan.execution_mode);
        assert_eq!(file_plan.tasks.len(), rebuilt.tasks.len());
        let ft1 = file_plan.tasks.iter().find(|t| t.id == "t1").unwrap();
        let rt1 = rebuilt.tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(ft1.title, rt1.title);
        assert_eq!(ft1.title, "renamed t1");
        assert_eq!(ft1.status, rt1.status);
    }
}
