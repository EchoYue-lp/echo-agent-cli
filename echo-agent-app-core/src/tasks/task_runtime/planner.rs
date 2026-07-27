//! EKO file-ownership policy for plan task graphs.
//!
//! `file_ownership` / `analyze_file_ownership` provide deterministic ownership
//!   classification for writer tasks. Exact workspace-relative files may run
//!   in parallel when disjoint; broad or invalid scopes are `Unknown` and must
//!   serialize with every writer.
//!
//! Generic task identity, dependency, cycle, depth, and retry validation lives
//! in `echo_orchestration::planning::PlanValidator`.

use super::types::PlanTask;
use std::collections::BTreeSet;

// ── File Ownership Analysis ────────────────────────────────────────────────

/// Runtime meaning of a task's declared `files` list.
///
/// Writer parallelism is only allowed for exact, normalized workspace-relative
/// paths. Anything broader or ambiguous remains executable, but is classified
/// as `Unknown` and serialized with every other writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOwnership {
    /// Read-only tasks never claim write ownership.
    ReadOnly,
    /// Exact exclusive file paths, normalized and deduplicated.
    Known(BTreeSet<String>),
    /// Empty, broad, absolute, or otherwise unsafe-to-compare scope.
    Unknown { reason: &'static str },
}

impl FileOwnership {
    pub fn conflicts_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::ReadOnly, _) | (_, Self::ReadOnly) => false,
            (Self::Unknown { .. }, _) | (_, Self::Unknown { .. }) => true,
            (Self::Known(left), Self::Known(right)) => left.intersection(right).next().is_some(),
        }
    }

    pub fn known_files(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::Known(files) => Some(files),
            Self::ReadOnly | Self::Unknown { .. } => None,
        }
    }
}

/// A pair of writer tasks whose declared `files` overlap.
///
/// `shared` are normalized overlapping paths, or `<unknown ownership>` when a
/// broad/empty scope forces serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOverlapPair {
    pub task_a: String,
    pub task_b: String,
    pub shared: Vec<String>,
}

/// Plan-time report on which writer tasks cannot run in the same write wave.
///
/// Built by [`analyze_file_ownership`]. Only **writer** tasks
/// (`!PlanTaskKind::is_read_only()`) are considered — read-only tasks never
/// own files and can run concurrently without conflict.
///
/// Exact disjoint ownership can run in parallel. Exact overlap, empty scope,
/// glob/directory-like scope, absolute paths, and parent traversal serialize.
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

/// Normalize one exact workspace-relative file path.
///
/// This intentionally rejects globs and directory-like declarations. Those
/// are valid plan hints, but not precise enough to unlock parallel writers.
pub fn normalize_owned_file(p: &str) -> Option<String> {
    let trimmed = p.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.ends_with('/')
        || trimmed.ends_with('\\')
        || trimmed
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return None;
    }
    let mut prefix = trimmed.chars();
    if prefix.next().is_some_and(|ch| ch.is_ascii_alphabetic()) && prefix.next() == Some(':') {
        return None;
    }

    let slashed: String = trimmed
        .chars()
        .map(|ch| if ch == '\\' { '/' } else { ch })
        .collect();
    let mut segments = Vec::new();
    for segment in slashed.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// Classify one task's file ownership declaration.
pub fn file_ownership(task: &PlanTask) -> FileOwnership {
    if task.kind.is_read_only() {
        return FileOwnership::ReadOnly;
    }
    if task.files.is_empty() {
        return FileOwnership::Unknown {
            reason: "no files declared",
        };
    }

    let mut files = BTreeSet::new();
    for file in &task.files {
        let Some(normalized) = normalize_owned_file(file) else {
            return FileOwnership::Unknown {
                reason: "scope is not an exact workspace-relative file",
            };
        };
        files.insert(normalized);
    }
    if files.is_empty() {
        FileOwnership::Unknown {
            reason: "no exact files declared",
        }
    } else {
        FileOwnership::Known(files)
    }
}

/// Analyze file ownership across a plan, returning the writer-task pairs that
/// declare overlapping files.
///
/// Read-only tasks are ignored. Unknown writers conflict with every writer.
pub fn analyze_file_ownership(tasks: &[PlanTask]) -> OwnershipReport {
    let writers: Vec<(&str, FileOwnership)> = tasks
        .iter()
        .filter(|task| !task.kind.is_read_only())
        .map(|task| (task.id.as_str(), file_ownership(task)))
        .collect();

    let mut overlap_pairs = Vec::new();
    for i in 0..writers.len() {
        for j in (i + 1)..writers.len() {
            let Some((id_a, ownership_a)) = writers.get(i) else {
                continue;
            };
            let Some((id_b, ownership_b)) = writers.get(j) else {
                continue;
            };
            if !ownership_a.conflicts_with(ownership_b) {
                continue;
            }
            let shared = match (ownership_a, ownership_b) {
                (FileOwnership::Known(left), FileOwnership::Known(right)) => {
                    left.intersection(right).cloned().collect()
                }
                _ => vec!["<unknown ownership>".to_string()],
            };
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
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
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
    fn writer_without_files_conflicts_with_every_writer() {
        let plan = [writer("a", &["src/a.rs"]), writer("b", &[])];
        let report = analyze_file_ownership(&plan);
        assert!(report.has_overlap());
        assert_eq!(
            report.overlap_pairs.first().map(|pair| pair.shared.clone()),
            Some(vec!["<unknown ownership>".to_string()])
        );
    }

    #[test]
    fn path_normalization_collapses_redundant_separators() {
        // `src//a.rs` and `src/a.rs` should be treated as the same file.
        let plan = [writer("a", &["src//a.rs"]), writer("b", &["src/a.rs"])];
        let report = analyze_file_ownership(&plan);
        assert!(report.has_overlap());
    }

    #[test]
    fn dot_prefix_normalizes_to_exact_file() {
        let plan = [writer("a", &["./src/a.rs"]), writer("b", &["src/a.rs"])];
        assert!(analyze_file_ownership(&plan).has_overlap());
    }

    #[test]
    fn glob_scope_is_unknown_and_serializes() {
        let plan = [writer("a", &["src/*.rs"]), writer("b", &["tests/b.rs"])];
        assert!(analyze_file_ownership(&plan).has_overlap());
    }

    #[test]
    fn parent_traversal_scope_is_unknown() {
        let task = writer("a", &["../outside.rs"]);
        assert!(matches!(
            file_ownership(&task),
            FileOwnership::Unknown { .. }
        ));
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
