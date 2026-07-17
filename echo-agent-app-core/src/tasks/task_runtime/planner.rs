//! Lightweight DAG integrity checks for plan task graphs.
//!
//! Contains:
//! - `validate_plan_deps` — verify a plan has no dangling dependencies / cycles.
//! - `analyze_file_ownership` (Sprint 7) — plan-time file-overlap analysis for
//!   writer tasks. Pure function; reports which writer task pairs touch
//!   overlapping files. This is the **foundation** for Phase 2 parallel-write
//!   routing (Sprint 8 worktree isolation / Sprint 9 semaphore gating): those
//!   will consult this report to decide parallel vs serialized scheduling.
//!   Today it only emits a non-blocking advisory warning at plan mutation time
//!   (the write semaphore already serializes all writers, so the overlap is
//!   not a correctness hazard yet).
//!
//! Previous plan generation functions (`generate_parallel_readonly_plan`,
//! `generate_plan`) have been removed as part of the L1 path cleanup. Plans
//! are now produced by the main agent ReAct loop via `plan_create`.

use super::types::PlanTask;

/// Validate dependency integrity and acyclicity for a set of tasks.
///
/// This is a lightweight check used by dynamic plan operations (insert_task,
/// update_task) to ensure the DAG remains valid after mutation. Unlike
/// `validate_plan`, it skips structural quality checks (file lists, title
/// length, etc.) and only verifies:
///
/// 1. Every `depends_on` references an existing task id.
/// 2. The dependency graph has no cycles.
pub fn validate_plan_deps(tasks: &[PlanTask]) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    // 1. Dangling dependency check.
    let ids: std::collections::HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        for dep in &t.depends_on {
            if !ids.contains(dep.as_str()) {
                errors.push(format!(
                    "task '{}' depends on '{}' which does not exist",
                    t.id, dep
                ));
            }
        }
    }

    // 2. Cycle detection via DFS.
    {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stack: std::collections::HashSet<String> = std::collections::HashSet::new();
        let id_to_deps: std::collections::HashMap<String, Vec<String>> = tasks
            .iter()
            .map(|t| (t.id.clone(), t.depends_on.clone()))
            .collect();
        fn dfs(
            node: &str,
            id_to_deps: &std::collections::HashMap<String, Vec<String>>,
            visited: &mut std::collections::HashSet<String>,
            stack: &mut std::collections::HashSet<String>,
        ) -> bool {
            if stack.contains(node) {
                return true;
            }
            if visited.contains(node) {
                return false;
            }
            visited.insert(node.to_string());
            stack.insert(node.to_string());
            if let Some(deps) = id_to_deps.get(node) {
                for dep in deps {
                    if dfs(dep, id_to_deps, visited, stack) {
                        return true;
                    }
                }
            }
            stack.remove(node);
            false
        }
        for t in tasks {
            if visited.contains(&t.id) {
                continue;
            }
            if dfs(&t.id, &id_to_deps, &mut visited, &mut stack) {
                errors.push(format!("dependency cycle involving task '{}'", t.id));
                break;
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ── File Ownership Analysis (Sprint 7) ─────────────────────────────────────

/// A pair of writer tasks whose declared `files` overlap.
///
/// `shared` are the file paths both tasks claim (normalized, deduped, sorted
/// for stable output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOverlapPair {
    pub task_a: String,
    pub task_b: String,
    pub shared: Vec<String>,
}

/// Plan-time report on which writer tasks touch overlapping files.
///
/// Built by [`analyze_file_ownership`]. Only **writer** tasks
/// (`!PlanTaskKind::is_read_only()`) are considered — read-only tasks never
/// own files and can run concurrently without conflict.
///
/// Semantics for Sprint 7 (foundation):
/// - `overlap_pairs` is non-empty ⇒ those task pairs **must be serialized**
///   once parallel writes are enabled (Sprint 9). Today the write semaphore
///   already serializes every writer, so this is advisory only.
/// - Two tasks with disjoint (or empty) `files` are safe to parallelize in
///   separate worktrees (Sprint 8).
///
/// Precision caveat: path matching is string-based and normalized only by
/// trimming + collapsing repeated separators. Equivalent paths written
/// differently (`./a.rs` vs `a.rs`, absolute vs relative) may not match —
/// accepted imprecision (the worktree isolation in Sprint 8 is the physical
/// safety net; this analyzer is the scheduling hint).
#[derive(Debug, Clone, Default)]
pub struct OwnershipReport {
    /// Every pair of writer tasks sharing ≥1 file. Unordered; each pair
    /// appears once (`task_a` < `task_b` lexicographically by id).
    pub overlap_pairs: Vec<FileOverlapPair>,
}

impl OwnershipReport {
    /// `true` when at least one writer-task pair shares files.
    pub fn has_overlap(&self) -> bool {
        !self.overlap_pairs.is_empty()
    }
}

/// Normalize a declared file path for comparison: trim whitespace and collapse
/// repeated `/` (and treat `\` as `/` so Windows-style paths still compare).
/// Does **not** touch the filesystem — pure string normalization.
fn normalize_file(p: &str) -> String {
    let trimmed = p.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_sep = false;
    for ch in trimmed.chars() {
        let is_sep = ch == '/' || ch == '\\';
        if is_sep {
            if !prev_sep {
                out.push('/');
            }
            prev_sep = true;
        } else {
            out.push(ch);
            prev_sep = false;
        }
    }
    out
}

/// Analyze file ownership across a plan, returning the writer-task pairs that
/// declare overlapping files.
///
/// Read-only tasks are ignored (they don't claim ownership). Tasks with no
/// declared `files` are skipped too — a writer that declares no files can't
/// be analyzed and is left to the runtime advisory check.
pub fn analyze_file_ownership(tasks: &[PlanTask]) -> OwnershipReport {
    // Map writer task id → normalized, deduped file set.
    let mut writer_files: Vec<(&str, std::collections::BTreeSet<String>)> = Vec::new();
    for t in tasks {
        if t.kind.is_read_only() {
            continue;
        }
        let set: std::collections::BTreeSet<String> = t
            .files
            .iter()
            .map(|f| normalize_file(f))
            .filter(|f| !f.is_empty())
            .collect();
        if !set.is_empty() {
            writer_files.push((t.id.as_str(), set));
        }
    }

    // All-pairs intersection. n is small (plan task counts), O(n²) is fine.
    let mut overlap_pairs = Vec::new();
    for i in 0..writer_files.len() {
        for j in (i + 1)..writer_files.len() {
            let (id_a, set_a) = &writer_files[i];
            let (id_b, set_b) = &writer_files[j];
            let shared: Vec<String> = set_a.intersection(set_b).cloned().collect();
            if !shared.is_empty() {
                // Stable ordering: lex-smaller id first.
                let (a, b) = if id_a <= id_b {
                    (*id_a, *id_b)
                } else {
                    (*id_b, *id_a)
                };
                overlap_pairs.push(FileOverlapPair {
                    task_a: a.to_string(),
                    task_b: b.to_string(),
                    shared,
                });
            }
        }
    }

    OwnershipReport { overlap_pairs }
}

/// Convenience: `true` if any two writer tasks in the plan share a file.
/// Used by the store to gate the non-blocking overlap warning.
pub fn has_writer_file_overlap(tasks: &[PlanTask]) -> bool {
    analyze_file_ownership(tasks).has_overlap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::types::{DomainProfile, PlanTask, PlanTaskKind};

    fn writer(id: &str, files: &[&str]) -> PlanTask {
        PlanTask {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            kind: PlanTaskKind::Implementation,
            agent_role: "general".into(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            files: files.iter().map(|s| s.to_string()).collect(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            verification: Vec::new(),
            retry_count: 0,
            max_retries: 0,
            failure_fingerprint: None,
            status: crate::tasks::task_runtime::types::TodoStatus::Pending,
            sort_order: 0,
        }
    }

    fn reader(id: &str, files: &[&str]) -> PlanTask {
        let mut t = writer(id, files);
        t.kind = PlanTaskKind::ReadOnlyReview;
        t
    }

    #[test]
    fn disjoint_writers_have_no_overlap() {
        let plan = [writer("a", &["src/a.rs"]), writer("b", &["src/b.rs"])];
        let report = analyze_file_ownership(&plan);
        assert!(!report.has_overlap());
        assert!(report.overlap_pairs.is_empty());
    }

    #[test]
    fn overlapping_writers_reported() {
        let plan = [
            writer("a", &["src/a.rs", "src/shared.rs"]),
            writer("b", &["src/shared.rs", "src/b.rs"]),
        ];
        let report = analyze_file_ownership(&plan);
        assert!(report.has_overlap());
        assert_eq!(report.overlap_pairs.len(), 1);
        let pair = &report.overlap_pairs[0];
        assert_eq!(pair.task_a, "a");
        assert_eq!(pair.task_b, "b");
        assert_eq!(pair.shared, vec!["src/shared.rs".to_string()]);
    }

    #[test]
    fn readonly_tasks_ignored() {
        // A reader claiming the same file as a writer is NOT a conflict —
        // readers don't own files.
        let plan = [
            writer("w", &["src/x.rs"]),
            reader("r", &["src/x.rs"]),
            reader("r2", &["src/x.rs"]),
        ];
        let report = analyze_file_ownership(&plan);
        assert!(!report.has_overlap());
    }

    #[test]
    fn writers_without_files_skipped() {
        // A writer declaring no files can't be analyzed; doesn't pair up.
        let plan = [writer("a", &["src/a.rs"]), writer("b", &[])];
        let report = analyze_file_ownership(&plan);
        assert!(!report.has_overlap());
    }

    #[test]
    fn path_normalization_collapses_redundant_separators() {
        // `src//a.rs` and `src/a.rs` should be treated as the same file.
        let plan = [writer("a", &["src//a.rs"]), writer("b", &["src/a.rs"])];
        let report = analyze_file_ownership(&plan);
        assert!(report.has_overlap());
    }

    #[test]
    fn pair_ordering_is_lex_stable() {
        // Ensure (task_a, task_b) has task_a < task_b regardless of input order.
        let plan = [writer("zzz", &["f.rs"]), writer("aaa", &["f.rs"])];
        let report = analyze_file_ownership(&plan);
        assert_eq!(report.overlap_pairs.len(), 1);
        assert_eq!(report.overlap_pairs[0].task_a, "aaa");
        assert_eq!(report.overlap_pairs[0].task_b, "zzz");
    }

    #[test]
    fn empty_plan_no_overlap() {
        let report = analyze_file_ownership(&[]);
        assert!(!report.has_overlap());
    }

    #[test]
    fn has_writer_file_overlap_helper() {
        let clean = [writer("a", &["x.rs"]), writer("b", &["y.rs"])];
        let dirty = [writer("a", &["x.rs"]), writer("b", &["x.rs"])];
        assert!(!has_writer_file_overlap(&clean));
        assert!(has_writer_file_overlap(&dirty));
    }
}
