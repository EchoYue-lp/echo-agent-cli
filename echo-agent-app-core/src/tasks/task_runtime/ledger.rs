//! Progress ledger export.
//!
//! Generates a human-readable `progress.md` from the canonical SQLite state.
//! Per the plan (§866-901): SQLite is the source of truth; the markdown
//! export is derived and may be shown to agents as compact recovery context.
//! If the two ever disagree, SQLite wins.
//!
//! The export is written to `.eko/runtime/{run_id}/progress.md` and is also
//! returned as a string so the GUI / IPC can render it without touching disk.

use std::path::PathBuf;

use super::store::{StoreError, TaskRuntimeStore};
use super::types::*;

/// Render the progress ledger for a run as markdown. Pure function over the
/// store — no side effects.
pub fn render_progress(store: &TaskRuntimeStore, run_id: &str) -> Result<String, StoreError> {
    let run = store
        .get_run(run_id)?
        .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
    let plan = store.get_plan(run_id)?;
    let todos = store.list_todos(run_id)?;

    let mut s = String::new();
    s.push_str("# Progress\n\n");
    s.push_str(&format!("## Goal\n{}\n\n", run.goal));
    s.push_str(&format!(
        "- **Run**: `{}`\n- **Status**: `{}`\n- **Profile**: `{}`\n\n",
        run.run_id,
        run.status.as_str(),
        run.domain_profile.as_str(),
    ));

    if let Some(plan) = &plan {
        if !plan.assumptions.is_empty() {
            s.push_str("## Assumptions\n");
            for a in &plan.assumptions {
                s.push_str(&format!("- {a}\n"));
            }
            s.push('\n');
        }
        if !plan.risks.is_empty() {
            s.push_str("## Risks\n");
            for r in &plan.risks {
                s.push_str(&format!("- {r}\n"));
            }
            s.push('\n');
        }
    }

    // Group todos by status for at-a-glance progress.
    let (mut completed, mut in_progress, mut blocked, mut pending, mut failed) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for t in &todos {
        match t.status {
            TodoStatus::Completed => completed.push(t),
            TodoStatus::Running => in_progress.push(t),
            TodoStatus::Blocked => blocked.push(t),
            TodoStatus::Failed => failed.push(t),
            TodoStatus::Pending => pending.push(t),
            TodoStatus::Skipped => {} // skipped tasks omitted for brevity
        }
    }

    if !in_progress.is_empty() {
        s.push_str("## In Progress\n");
        for t in &in_progress {
            s.push_str(&format!(
                "- `{}` — {}{}\n",
                t.task_id,
                t.title,
                t.owner_agent
                    .as_deref()
                    .map(|o| format!("  _[{o}]_"))
                    .unwrap_or_default()
            ));
        }
        s.push('\n');
    }
    if !completed.is_empty() {
        s.push_str("## Completed\n");
        for t in &completed {
            let summary = t
                .summary
                .as_deref()
                .map(|s| format!(": {s}"))
                .unwrap_or_default();
            s.push_str(&format!("- `{}` — {}{summary}\n", t.task_id, t.title));
        }
        s.push('\n');
    }
    if !failed.is_empty() {
        s.push_str("## Failed\n");
        for t in &failed {
            s.push_str(&format!("- `{}` — {}\n", t.task_id, t.title));
        }
        s.push('\n');
    }
    if !blocked.is_empty() {
        s.push_str("## Blocked\n");
        for t in &blocked {
            s.push_str(&format!("- `{}` — {}\n", t.task_id, t.title));
        }
        s.push('\n');
    }
    if !pending.is_empty() {
        s.push_str("## Next\n");
        for t in &pending {
            s.push_str(&format!("- `{}` — {}\n", t.task_id, t.title));
        }
        s.push('\n');
    }

    // One-line health summary.
    let total = todos.len();
    let done = completed.len();
    s.push_str(&format!("## Health\n{done}/{total} tasks completed.\n",));

    Ok(s)
}

/// Where the export file lives for a run: `{base}/.eko/runtime/{run_id}/progress.md`.
pub fn export_path(run_id: &str, base: Option<&std::path::Path>) -> PathBuf {
    let root = base
        .map(|b| b.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    root.join(".eko/runtime").join(run_id).join("progress.md")
}

/// Export all todos as JSON for debugging/recovery (plan §860).
pub fn export_todos_json(store: &TaskRuntimeStore, run_id: &str) -> Result<String, StoreError> {
    let todos = store.list_todos(run_id)?;
    Ok(serde_json::to_string_pretty(&todos).unwrap_or_else(|_| "[]".to_string()))
}

/// Archive raw worker output as a trace artifact (plan §1057-1061).
/// Writes to `{base}/.eko/runtime/{run_id}/artifacts/traces/{task_id}.txt`.
pub fn archive_trace(run_id: &str, task_id: &str, output: &str, base: Option<&std::path::Path>) {
    let root = base
        .map(|b| b.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = root
        .join(".eko/runtime")
        .join(run_id)
        .join("artifacts/traces");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "failed to create trace dir");
        return;
    }
    let path = dir.join(format!("{task_id}.txt"));
    if let Err(e) = std::fs::write(&path, output) {
        tracing::warn!(path = %path.display(), error = %e, "failed to archive trace");
    }
}

/// Render AND write the progress ledger to disk. Returns the rendered text
/// so the caller can also surface it without re-reading. Best-effort: a write
/// failure is logged but does NOT fail the call, because the export is a
/// debug/recovery aid, not canonical state (plan §865).
///
/// `base` is the workspace root for the export path. Pass `None` to use CWD
/// (fine for CLI/agents; NOT reliable in Tauri — the caller should pass the
/// workspace path).
pub fn write_progress(
    store: &TaskRuntimeStore,
    run_id: &str,
    base: Option<&std::path::Path>,
) -> Result<String, StoreError> {
    let markdown = render_progress(store, run_id)?;
    let path = export_path(run_id, base);
    if let Some(parent) = path.parent() {
        #[allow(clippy::collapsible_if)]
        // nested let-Some/let-Err reads clearer than a let-chain here
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %path.display(), error = %e, "failed to create progress dir; export stays in-memory");
            return Ok(markdown);
        }
    }
    if let Err(e) = std::fs::write(&path, &markdown) {
        tracing::warn!(path = %path.display(), error = %e, "failed to write progress.md; export stays in-memory");
    } else {
        tracing::debug!(path = %path.display(), run_id = run_id, "progress.md written");
    }
    Ok(markdown)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn seeded_store() -> Arc<TaskRuntimeStore> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "Build real runtime",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::AiCoding,
            goal: "Build real runtime".into(),
            assumptions: vec!["runtime exists".into()],
            risks: vec!["LLM cost".into()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                PlanTask {
                    id: "t1".into(),
                    title: "Review runtime".into(),
                    kind: PlanTaskKind::ReadOnlyReview,
                    agent_role: "code_reviewer".into(),
                    ..Default::default()
                },
                PlanTask {
                    id: "t2".into(),
                    title: "Implement fix".into(),
                    kind: PlanTaskKind::Implementation,
                    agent_role: "implementer".into(),
                    depends_on: vec!["t1".into()],
                    ..Default::default()
                },
            ],
        };
        store.attach_plan(&plan).unwrap();
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("code_reviewer"),
                Some("found router gap"),
            )
            .unwrap();
        store
    }

    #[test]
    fn render_progress_groups_by_status() {
        let store = seeded_store();
        let md = render_progress(&store, "r1").unwrap();
        assert!(md.contains("## Goal\nBuild real runtime"));
        assert!(md.contains("## Completed"));
        assert!(md.contains("`t1` — Review runtime: found router gap"));
        assert!(md.contains("## Next"));
        assert!(md.contains("`t2` — Implement fix"));
        assert!(md.contains("## Assumptions"));
        assert!(md.contains("## Risks"));
        assert!(md.contains("1/2 tasks completed"));
    }

    #[test]
    fn render_progress_errors_on_unknown_run() {
        let store = TaskRuntimeStore::new_in_memory().unwrap();
        assert!(matches!(
            render_progress(&store, "nope"),
            Err(StoreError::RunNotFound(_))
        ));
    }

    #[test]
    fn write_progress_creates_file_and_returns_markdown() {
        // Use a temp CWD so the test doesn't litter the repo.
        let tmp = std::env::temp_dir().join(format!("eko-progress-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let store = seeded_store();
        let md = write_progress(&store, "r1", Some(&tmp)).unwrap();
        let written = std::fs::read_to_string(export_path("r1", Some(&tmp))).unwrap();
        assert_eq!(md, written);
        assert!(written.contains("## Goal"));

        std::env::set_current_dir(prev).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
