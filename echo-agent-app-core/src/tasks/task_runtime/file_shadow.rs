//! File-based authoritative store (U1c phase-0/0bc).
//!
//! The file system (`events.jsonl` + `plan.json`) is the read/write authority
//! for all task data. SQL was retired in 0bc step 5 (except tr_usage_records
//! and tr_conversation_events).
//!
//! Layout: `{root}/{run_id}/events.jsonl` (append-only) + `plan.json` (snapshot).
//! `root` defaults to `~/.echo-agent/tasks/` (global, spec §2 path A).

use std::path::{Path, PathBuf};

use super::event_rebuild::{RebuiltPlan, rebuild_plan_from_events};
use super::types::RuntimeTaskEvent;

/// Shadow writer for one root directory. Cheap to clone (wraps a root path; the
/// append lock is per-run via the `events.jsonl` file handle being held briefly).
#[derive(Clone)]
pub struct FileTaskShadow {
    root: PathBuf,
    /// In-memory seq cache (run_id → last assigned seq) to avoid re-reading
    /// the whole `events.jsonl` on every append (O(n) per write → O(n²) per
    /// run). Seeded lazily from the file's line count on first append per run,
    /// so it self-heals across restarts. Shared across clones via `Arc` so all
    /// clones of this shadow agree on seq. Contention is low: the store holds
    /// a single `Mutex` serializing all writes (the in-memory usage/conv-event
    /// mutexes, not a DB connection).
    seq_cache: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>>,
}

impl FileTaskShadow {
    /// Create a shadow rooted at `root`. The directory is created lazily on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            seq_cache: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
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

    // ── 0bc step-2: file-authority write path (replaces SQL INSERT + flush) ──

    /// Append one enriched event to `events.jsonl` for `run_id`, assigning it
    /// the next seq (1-based, = current line count + 1). Returns the fully
    /// formed `RuntimeTaskEvent` (with seq + timestamp) that was written.
    ///
    /// This is the file-authority write primitive: store write methods call
    /// this instead of `INSERT INTO tr_events`, then call [`rewrite_plan`] to
    /// refresh the `plan.json` snapshot. seq is per-run (each run has its own
    /// `events.jsonl`), so appending to run B does not advance run A's seq.
    ///
    /// Atomicity: the append is serialized by the store mutex (single writer),
    /// and the line is written with a trailing newline. A crash mid-append can
    /// at worst lose the last partial line — `read_events` skips empty lines
    /// and a future hardening pass (gate 2) will truncate a partial tail.
    pub fn append_event_line(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        step_id: Option<&str>,
        event_type: super::types::RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Result<RuntimeTaskEvent, ShadowError> {
        let dir = self.run_dir(run_id);
        std::fs::create_dir_all(&dir).map_err(|e| ShadowError::Io(e.to_string()))?;

        // seq = last assigned + 1 (1-based). Cached in memory per run to avoid
        // re-reading events.jsonl on every append; seeded from the file's line
        // count on first touch per run so it self-heals across restarts.
        let next_seq = self.next_seq(run_id)?;

        let event = RuntimeTaskEvent {
            seq: next_seq,
            run_id: run_id.to_string(),
            task_id: task_id.map(str::to_string),
            step_id: step_id.map(str::to_string),
            event_type,
            payload,
            timestamp: chrono::Utc::now(),
        };

        // Append one JSON line. Ensure a trailing newline so the file is a
        // well-formed JSONL stream (each event on its own line).
        let mut line =
            serde_json::to_string(&event).map_err(|e| ShadowError::Encode(e.to_string()))?;
        line.push('\n');
        append_line(&self.events_path(run_id), line.as_bytes())
            .map_err(|e| ShadowError::Io(e.to_string()))?;

        // Advance the in-memory cache so the next append doesn't re-read the file.
        if let Ok(mut cache) = self.seq_cache.lock() {
            cache.insert(run_id.to_string(), next_seq);
        }
        Ok(event)
    }

    /// Rebuild `plan.json` for `run_id` from its full `events.jsonl` stream.
    /// Called by store write methods after `append_event_line` to refresh the
    /// snapshot. Uses tmp+rename (atomic on the same filesystem).
    ///
    /// If the event stream has no `RunCreated` yet (e.g. an orphan review
    /// written before the run was created — SQL tolerated this), there is no
    /// plan to snapshot, so this is a no-op rather than an error. The events
    /// themselves are still durably appended to `events.jsonl`.
    pub fn rewrite_plan(&self, run_id: &str) -> Result<(), ShadowError> {
        let events = self.read_events(run_id)?;
        if events.is_empty() {
            return Ok(());
        }
        let rebuilt = match rebuild_plan_from_events(&events) {
            Ok(r) => r,
            Err(super::event_rebuild::RebuildError::NoRunCreated) => {
                // No RunCreated in the stream. On the live file path every
                // write that reaches here has either just appended RunCreated
                // (create_run) or operates on an existing run — so hitting this
                // branch means an orphan write (e.g. add_review before
                // create_run, which SQL tolerated) or a corrupted/partial
                // events.jsonl. Events are still authoritative on disk; we skip
                // the plan.json snapshot but log so this stays diagnosable
                // rather than a silent no-op.
                tracing::warn!(
                    run_id = %run_id,
                    event_count = events.len(),
                    "rewrite_plan: no RunCreated in event stream, skipping plan.json snapshot \
                     (orphan write or corrupted events.jsonl)"
                );
                return Ok(());
            }
        };
        let plan_json = serde_json::to_string_pretty(&rebuilt)
            .map_err(|e| ShadowError::Encode(e.to_string()))?;
        std::fs::create_dir_all(self.run_dir(run_id))
            .map_err(|e| ShadowError::Io(e.to_string()))?;
        atomic_write(&self.plan_path(run_id), plan_json.as_bytes())
            .map_err(|e| ShadowError::Io(e.to_string()))?;
        Ok(())
    }

    /// Compute the next seq for `run_id`: last assigned + 1 (1-based).
    ///
    /// Uses the in-memory `seq_cache` to avoid re-reading `events.jsonl` on
    /// every append. On first touch per run (cache miss) it seeds from the
    /// file's line count, so the cache self-heals across restarts or when a
    /// different process wrote to the same file. Returns 1 if the file does
    /// not exist yet.
    fn next_seq(&self, run_id: &str) -> Result<i64, ShadowError> {
        // Fast path: cached.
        if let Ok(cache) = self.seq_cache.lock()
            && let Some(&last) = cache.get(run_id)
        {
            return Ok(last + 1);
        }
        // Slow path: seed from file line count (self-healing).
        let file_len = self.read_events(run_id)?.len() as i64;
        Ok(file_len + 1)
    }

    /// Enumerate every run_id known to the file store: the directory names
    /// under `root` that contain an `events.jsonl` (or `plan.json`). Used by
    /// the collection-query read API (`list_runs` / `list_runs_in` / etc.) that
    /// replaces SQL `SELECT ... FROM tr_runs WHERE ...`.
    pub fn list_run_ids(&self) -> Result<Vec<String>, ShadowError> {
        let mut ids = Vec::new();
        let read_dir = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ShadowError::Io(e.to_string())),
        };
        for entry in read_dir {
            let entry = entry.map_err(|e| ShadowError::Io(e.to_string()))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // A run dir is one that has events.jsonl or plan.json.
            let has_events = path.join("events.jsonl").exists();
            let has_plan = path.join("plan.json").exists();
            if !has_events && !has_plan {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }

    /// The root path this shadow writes under (used by FileTaskStore to share
    /// the same root for enumeration).
    pub fn root_path(&self) -> &Path {
        &self.root
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

/// Append `bytes` (one JSONL line, including trailing newline) to `path`.
/// Creates the file if it does not exist. Uses `O_APPEND` so concurrent
/// appends (if any) do not interleave within a single write(2) call.
fn append_line(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RuntimeEventKind, TaskPatch,
        TaskPlan, TaskRunStatus, TodoStatus,
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
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).expect("store");

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

    /// Helper: assert file shadow plan matches SQL-rebuilt plan on the fields the
    /// rebuilder tracks (run header, plan envelope, task count + per-task identity).
    fn assert_parity(store: &TaskRuntimeStore, shadow: &FileTaskShadow, run_id: &str) {
        let sql_events = store.list_events(run_id, 0).unwrap();
        let file_events = shadow.read_events(run_id).unwrap();
        assert_eq!(
            sql_events.len(),
            file_events.len(),
            "[{run_id}] event count parity: sql={} file={}",
            sql_events.len(),
            file_events.len()
        );
        for (s, f) in sql_events.iter().zip(file_events.iter()) {
            assert_eq!(s.seq, f.seq, "[{run_id}] seq parity");
            assert_eq!(
                s.event_type, f.event_type,
                "[{run_id}] kind parity at seq {}",
                s.seq
            );
        }
        let file_plan = shadow.read_plan(run_id).unwrap();
        let rebuilt = rebuild_plan_from_events(&sql_events).unwrap();
        let file_plan = file_plan.expect("plan.json written");
        assert_eq!(
            file_plan.run.run_id, rebuilt.run.run_id,
            "[{run_id}] run_id"
        );
        assert_eq!(file_plan.run.goal, rebuilt.run.goal, "[{run_id}] goal");
        assert_eq!(file_plan.run.route, rebuilt.run.route, "[{run_id}] route");
        assert_eq!(
            file_plan.run.status, rebuilt.run.status,
            "[{run_id}] run status"
        );
        assert_eq!(
            file_plan.plan.plan_id, rebuilt.plan.plan_id,
            "[{run_id}] plan_id"
        );
        assert_eq!(
            file_plan.tasks.len(),
            rebuilt.tasks.len(),
            "[{run_id}] task count"
        );
        for (ft, rt) in file_plan.tasks.iter().zip(rebuilt.tasks.iter()) {
            assert_eq!(ft.id, rt.id, "[{run_id}] task id");
            assert_eq!(ft.title, rt.title, "[{run_id}] task {} title", ft.id);
            assert_eq!(ft.kind, rt.kind, "[{run_id}] task {} kind", ft.id);
            assert_eq!(ft.status, rt.status, "[{run_id}] task {} status", ft.id);
            assert_eq!(
                ft.sort_order, rt.sort_order,
                "[{run_id}] task {} sort_order",
                ft.id
            );
        }
    }

    /// Parity across reorder + remove: ordering changes and deletions must be
    /// reflected identically in the file shadow and the SQL rebuild.
    #[test]
    fn shadow_parity_reorder_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g", "")
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            domain_profile: DomainProfile::General,
            goal: "g".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::Investigation),
                task("t2", PlanTaskKind::Investigation),
                task("t3", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan(&plan).unwrap();
        // Reorder: move t3 to front.
        store
            .reorder_tasks(
                "r1",
                vec!["t3".to_string(), "t1".to_string(), "t2".to_string()],
            )
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        // Remove t2 — soft delete: the task stays in the plan but is marked
        // Skipped (matching SQL `remove_task`, which sets status=Skipped and
        // keeps the row). Hard-deleting would diverge from `list_todos`.
        store.remove_task("r1", "t2").unwrap();
        assert_parity(&store, &shadow, "r1");
        let file_plan = shadow.read_plan("r1").unwrap().unwrap();
        assert_eq!(file_plan.tasks.len(), 3, "soft delete keeps the task");
        let t2 = file_plan
            .tasks
            .iter()
            .find(|t| t.id == "t2")
            .expect("t2 still present after soft delete");
        assert_eq!(t2.status, TodoStatus::Skipped);
    }

    /// Parity across multiple runs: events and plans must not cross-contaminate.
    #[test]
    fn shadow_parity_multiple_runs_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        for rid in ["r1", "r2"] {
            store
                .create_run(
                    rid,
                    "ws",
                    rid,
                    "m",
                    DomainProfile::AiCoding,
                    &format!("goal {rid}"),
                    "complex",
                )
                .unwrap();
            let plan = TaskPlan {
                plan_id: format!("p_{rid}"),
                run_id: rid.to_string(),
                domain_profile: DomainProfile::AiCoding,
                goal: format!("goal {rid}"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Parallel,
                tasks: vec![task(&format!("{rid}_t1"), PlanTaskKind::Summary)],
            };
            store.attach_plan(&plan).unwrap();
        }
        assert_parity(&store, &shadow, "r1");
        assert_parity(&store, &shadow, "r2");
        // r1's file plan must not contain r2's task.
        let p1 = shadow.read_plan("r1").unwrap().unwrap();
        assert!(p1.tasks.iter().all(|t| t.id == "r1_t1"));
    }

    /// Parity across run status transitions (Pending → Running → Paused → Completed).
    #[test]
    fn shadow_parity_run_status_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g", "")
            .unwrap();
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        assert_parity(&store, &shadow, "r1");
        store.transition_run("r1", TaskRunStatus::Paused).unwrap();
        assert_parity(&store, &shadow, "r1");
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        // Final status in file plan must be Completed.
        let p = shadow.read_plan("r1").unwrap().unwrap();
        assert_eq!(p.run.status, TaskRunStatus::Completed);
    }

    /// Parity with a bootstrap plan (insert_task without attach_plan) — the
    /// lazy-bootstrap path that creates a PlanGenerated event with empty tasks.
    #[test]
    fn shadow_parity_bootstrap_plan_via_insert_task() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g", "")
            .unwrap();
        // insert_task triggers lazy bootstrap (no prior attach_plan).
        store
            .insert_task("r1", None, task("t1", PlanTaskKind::Investigation))
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        // File plan should have the 1 task from insert.
        let p = shadow.read_plan("r1").unwrap().unwrap();
        assert_eq!(p.tasks.len(), 1);
        assert_eq!(p.tasks[0].id, "t1");
    }

    // ── 0bc step-2: incremental append API (file becomes write authority) ──

    /// `append_event` writes one event line to events.jsonl with a seq derived
    /// from the current line count (1-based, monotonically increasing), and
    /// `rewrite_plan` rebuilds plan.json from the full event stream. This is
    /// the file-authority path that replaces SQL INSERT + flush_shadow.
    #[test]
    fn append_event_assigns_incremental_seq_and_rewinds_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = FileTaskShadow::new(tmp.path());

        // No RunCreated yet — append three events and check seq + plan rebuild.
        let e1 = shadow
            .append_event_line(
                "r1",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "g", "domain_profile": "general",
                    "workspace_id": "ws", "conversation_id": "c1",
                    "root_message_id": "m1", "route": "", "created_at": "2026-06-25T00:00:00Z",
                }),
            )
            .unwrap();
        let e2 = shadow
            .append_event_line(
                "r1",
                None,
                None,
                RuntimeEventKind::PlanGenerated,
                serde_json::json!({
                    "plan_id": "p1", "task_count": 0,
                    "domain_profile": "general", "goal": "g",
                    "assumptions": [], "risks": [],
                    "execution_mode": "parallel", "tasks": [],
                }),
            )
            .unwrap();
        let e3 = shadow
            .append_event_line(
                "r1",
                Some("t1"),
                None,
                RuntimeEventKind::TaskStarted,
                serde_json::json!({ "status": "running", "owner_agent": "explorer" }),
            )
            .unwrap();

        // seq is 1-based, monotonically increasing per run.
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        assert_eq!(e3.seq, 3);

        // read_events returns all three in seq order.
        let read_back = shadow.read_events("r1").unwrap();
        assert_eq!(read_back.len(), 3);
        assert_eq!(read_back[0].seq, 1);
        assert_eq!(read_back[2].event_type, RuntimeEventKind::TaskStarted);

        // rewrite_plan produces a plan.json reflecting the event stream.
        shadow.rewrite_plan("r1").unwrap();
        let plan = shadow.read_plan("r1").unwrap().unwrap();
        assert_eq!(plan.run.run_id, "r1");
        assert_eq!(plan.run.goal, "g");
        assert_eq!(plan.plan.plan_id, "p1");
    }

    /// A second run's events must not perturb the first run's seq or plan —
    /// seq is per-run (each run has its own events.jsonl).
    #[test]
    fn append_event_seq_is_per_run() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = FileTaskShadow::new(tmp.path());

        let a1 = shadow
            .append_event_line(
                "rA",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({}),
            )
            .unwrap();
        let b1 = shadow
            .append_event_line(
                "rB",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({}),
            )
            .unwrap();
        let a2 = shadow
            .append_event_line(
                "rA",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({}),
            )
            .unwrap();

        assert_eq!(a1.seq, 1);
        assert_eq!(a2.seq, 2); // rA's second event, not affected by rB
        assert_eq!(b1.seq, 1); // rB starts at 1
    }
}
