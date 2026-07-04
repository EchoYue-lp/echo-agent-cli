//! Subagent worker `.md` hot-loader (Sprint 6).
//!
//! Replaces the hardcoded `WORKER_DEFINITIONS` array in `infra.rs`. Worker
//! prompts now live in `.md` files (frontmatter + markdown body) and can be
//! edited without recompiling.
//!
//! ## Resolution order (highest priority first)
//!
//! 1. **Project scope**: `<project_root>/.echo-agent/subagents/**/*.md`
//! 2. **User scope**: `~/.echo-agent/subagents/**/*.md`
//! 3. **Builtin defaults**: compiled-in `.md` files via `include_str!`
//!
//! On name collisions, the higher-priority scope wins (mirrors the skills
//! `DiscoveryScope` pattern in `echo-execution/src/skills/external/loader.rs`).
//! If neither runtime scope contains any file, the builtin defaults are used
//! so the app is usable out of the box with no initialization step.
//!
//! ## Why application-layer (not framework)
//!
//! The framework's `SubagentKind::Custom { path }` is an inert placeholder
//! (kept as a published capability). Per deep-iteration-plan §六, the loader
//! lives in the app layer because the `.md` directory layout and EKO's
//! `.echo-agent/` convention are product-form-specific. The loader emits plain
//! `WorkerDefinition` values consumed by `register_default_subagents`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Builtin default worker definitions (compiled into the binary).
///
/// Sourced from `src/subagents/coding/*.md`. These are the fallback used when
/// no project/user `.md` files override them. Order matters: this defines the
/// default registration order.
const BUILTIN_WORKER_FILES: &[(&str, &str)] = &[
    ("explorer", include_str!("subagents/coding/explorer.md")),
    ("reviewer", include_str!("subagents/coding/reviewer.md")),
    ("planner", include_str!("subagents/coding/planner.md")),
    ("summarizer", include_str!("subagents/coding/summarizer.md")),
    // Sprint 9: writer worker — gets write tools + worktree isolation
    // (worktree:true && !readonly → isolate_worktree). Implementation/Debugging
    // tasks route here instead of running in-place on the primary agent.
    (
        "implementer",
        include_str!("subagents/coding/implementer.md"),
    ),
    // Sprint 10: data/research workers — per-worker tmpdir workspace
    // (workspace:true → isolate_workspace) for disjoint output artifacts.
    ("data-shaper", include_str!("subagents/data/data-shaper.md")),
    ("analyst", include_str!("subagents/data/analyst.md")),
];

/// Maximum recursion depth when scanning a scope directory for `.md` files.
const MAX_SCAN_DEPTH: usize = 4;

/// Directories skipped during scope scanning (avoid descending into these).
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".worktrees"];

/// Raw frontmatter deserialized from a worker `.md` file.
///
/// Field names mirror `SubagentDefinition` semantics. `name` is optional here
/// (a fallback name from the filename can fill it); `description` is required.
/// The rest fall back to sensible defaults so a minimal `.md` still loads.
#[derive(Debug, Clone, Deserialize)]
struct WorkerFrontmatter {
    #[serde(default)]
    name: Option<String>,
    description: String,
    #[serde(default)]
    readonly: bool,
    /// Sprint 8: request worktree isolation for this Fork-dispatched worker
    /// (Claude Code `isolation: worktree` equivalent). Only meaningful for
    /// writer workers; requires a `WorktreeFactory` configured on the agent.
    #[serde(default)]
    worktree: bool,
    /// Sprint 10: request a per-worker data workspace (tmpdir) for this
    /// Fork-dispatched data/research worker — disjoint output dir, no git
    /// coupling. Mutually exclusive with `worktree` (worktree wins if both).
    /// Requires a `DataWorkspaceFactory` configured on the agent.
    #[serde(default)]
    workspace: bool,
    /// Optional tags; merged with the default readonly/parallel tags when
    /// `readonly` is true. Empty if unset.
    #[serde(default)]
    tags: Vec<String>,
    /// Optional nested delegation capability. Defaults false: workers execute
    /// the assigned task and may suggest follow-up tasks, but cannot spawn
    /// child subagents unless explicitly granted this capability.
    #[serde(default)]
    can_delegate: bool,
    /// Sprint 11: declare this subagent as a team-mode dispatcher. Only
    /// `"manager-worker"` is supported via frontmatter (other strategies are
    /// programmatic-only — they carry inline agent-name data).
    #[serde(default)]
    team_strategy: Option<String>,
    /// Sprint 11: the manager/leader subagent name (must be separately
    /// registered). Required when `team_strategy` is set.
    #[serde(default)]
    team_manager: Option<String>,
    /// Sprint 11: worker subagent names (each must be separately registered).
    /// Required (non-empty) when `team_strategy` is set.
    #[serde(default)]
    team_workers: Vec<String>,
}

/// A resolved worker definition ready for registration.
#[derive(Debug, Clone)]
pub struct WorkerDefinition {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub readonly: bool,
    /// Sprint 8: whether Fork dispatch should isolate this worker in a git
    /// worktree. Mapped from frontmatter `worktree: true`. Only meaningful for
    /// writer workers (readonly workers don't mutate files).
    pub isolate_worktree: bool,
    /// Sprint 10: whether Fork dispatch should give this worker a per-worker
    /// data workspace (tmpdir). Mapped from frontmatter `workspace: true`.
    /// Mutually exclusive with isolate_worktree (worktree wins if both).
    pub isolate_workspace: bool,
    /// Sprint 11: if Some, this subagent is a team-mode dispatcher (not a
    /// normal worker). The registration path sets `execution_mode = Team` and
    /// attaches this TeamSpec. manager + workers are name-references.
    pub team: Option<echo_agent::agent::subagent::types::TeamSpec>,
    /// Whether this worker may receive the framework `agent_tool` and spawn
    /// child subagents. Defaults false.
    pub can_delegate: bool,
    pub tags: Vec<String>,
}

/// Discover worker definitions across scopes + builtin fallback.
///
/// `project_root` is the cwd or detected project root; `user_home` is the
/// user's home directory (`~`). Either may be `None` if undetectable, in which
/// case that scope is skipped.
///
/// Returns at least the builtin defaults (so the app always has the 4 default
/// workers), with project/user overrides layered on top by name.
pub fn discover_subagents(
    project_root: Option<&Path>,
    user_home: Option<&Path>,
) -> Vec<WorkerDefinition> {
    // name → definition, later inserts (lower priority) don't overwrite.
    let mut by_name: std::collections::HashMap<String, WorkerDefinition> =
        std::collections::HashMap::new();

    // 1. Builtin defaults (lowest priority — inserted first, never overwritten).
    for (builtin_name, content) in BUILTIN_WORKER_FILES {
        match parse_worker_md(content, Some(*builtin_name)) {
            Ok(def) => {
                by_name.entry(def.name.clone()).or_insert(def);
            }
            Err(e) => {
                tracing::error!(
                    worker = *builtin_name,
                    error = %e,
                    "Builtin subagent .md failed to parse (this is a bug — source file is corrupt)"
                );
            }
        }
    }

    // 2. User scope (~/.echo-agent/subagents/).
    if let Some(home) = user_home {
        let user_dir = home.join(".echo-agent").join("subagents");
        merge_scope(&mut by_name, &user_dir);
    }

    // 3. Project scope (<root>/.echo-agent/subagents/) — highest priority, last.
    if let Some(root) = project_root {
        let project_dir = root.join(".echo-agent").join("subagents");
        merge_scope(&mut by_name, &project_dir);
    }

    // Preserve builtin order for stable registration, then append any
    // extra (user/project-only) workers at the end.
    let mut result: Vec<WorkerDefinition> = Vec::with_capacity(by_name.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (builtin_name, _) in BUILTIN_WORKER_FILES {
        if let Some(def) = by_name.get(*builtin_name)
            && seen.insert(def.name.clone())
        {
            result.push(def.clone());
        }
    }
    // Any names not in the builtin set (user/project additions).
    for (_, def) in by_name {
        if seen.insert(def.name.clone()) {
            result.push(def);
        }
    }

    result
}

/// Scan a scope directory and merge its parsed workers into `by_name`,
/// **overwriting** any same-named entry. Higher-priority scopes are merged
/// later (builtins first → user → project), so the last write wins, giving
/// project > user > builtin precedence.
fn merge_scope(by_name: &mut std::collections::HashMap<String, WorkerDefinition>, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let mut found = Vec::new();
    scan_directory(dir, 0, &mut found);
    for (path, content) in found {
        match parse_worker_md(&content, None) {
            Ok(def) => {
                by_name.insert(def.name.clone(), def);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Skipping malformed subagent .md"
                );
            }
        }
    }
}

/// Recursively collect `*.md` files under `dir` up to `MAX_SCAN_DEPTH`.
fn scan_directory(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "Cannot read subagents scope dir");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Skip well-known non-config directories.
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && SKIP_DIRS.contains(&name)
            {
                continue;
            }
            scan_directory(&path, depth + 1, out);
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            match std::fs::read_to_string(&path) {
                Ok(content) => out.push((path, content)),
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "Cannot read subagent .md");
                }
            }
        }
    }
}

/// Parse a worker `.md` file into a [`WorkerDefinition`].
///
/// Format:
/// ```text
/// ---
/// name: explorer
/// description: "..."
/// readonly: true
/// tags: ["readonly", "parallel"]
/// ---
/// <markdown body = system_prompt>
/// ```
///
/// `fallback_name` (when `Some`) overrides an empty/missing `name` field —
/// used for builtin files where the filename is authoritative.
pub fn parse_worker_md(
    content: &str,
    fallback_name: Option<&str>,
) -> Result<WorkerDefinition, String> {
    let (fm_str, body) = split_frontmatter(content)?;
    let fm: WorkerFrontmatter = if fm_str.trim().is_empty() {
        return Err("frontmatter is empty (missing name/description)".into());
    } else {
        serde_yaml::from_str(fm_str).map_err(|e| format!("frontmatter parse error: {e}"))?
    };

    // Resolve the name: frontmatter `name` wins; otherwise the fallback (e.g.
    // builtin filename); otherwise it's an error.
    let name = fm
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| fallback_name.map(|s| s.to_string()))
        .ok_or_else(|| "frontmatter missing required `name` field".to_string())?;
    if fm.description.trim().is_empty() {
        return Err(format!("worker `{name}` missing `description`"));
    }

    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        return Err(format!(
            "worker `{name}` has empty system prompt (markdown body after frontmatter)"
        ));
    }

    // Ensure readonly workers carry the physical-enforcement tags the
    // registration path expects. Non-readonly workers keep their declared tags.
    let mut tags = fm.tags;
    if fm.readonly {
        if !tags.iter().any(|t| t == "readonly") {
            tags.push("readonly".into());
        }
        if !tags.iter().any(|t| t == "parallel") {
            tags.push("parallel".into());
        }
    }

    // Sprint 8: `worktree: true` only makes sense for writer workers; if a
    // readonly worker declares it, ignore (readonly workers don't mutate files).
    let isolate_worktree = fm.worktree && !fm.readonly;
    // Sprint 10: `workspace: true` requests a per-worker data tmpdir. It's
    // meaningful for ANY worker (data workers emit artifacts regardless of
    // readonly), but mutually exclusive with worktree — if both are set,
    // worktree wins (it also provides disjoint FS). Clear workspace when
    // worktree is active to avoid double-isolation at registration.
    let isolate_workspace = fm.workspace && !isolate_worktree;

    // Sprint 11: parse team frontmatter into a TeamSpec (only manager-worker
    // strategy is declarable). Validate that manager + non-empty workers given.
    let team = if let Some(strategy) = fm.team_strategy.as_deref() {
        if strategy != "manager-worker" {
            return Err(format!(
                "worker `{name}`: team_strategy '{strategy}' unsupported via frontmatter (only 'manager-worker')"
            ));
        }
        let manager = fm.team_manager.clone().ok_or_else(|| {
            format!("worker `{name}`: team_strategy set but team_manager missing")
        })?;
        if fm.team_workers.is_empty() {
            return Err(format!(
                "worker `{name}`: team_strategy set but team_workers empty"
            ));
        }
        Some(echo_agent::agent::subagent::types::TeamSpec {
            strategy: echo_agent::agent::subagent::team::strategy::TeamStrategy::ManagerWorker,
            manager,
            workers: fm.team_workers.clone(),
            config: echo_agent::agent::subagent::team::TeamConfig::default(),
        })
    } else {
        None
    };

    Ok(WorkerDefinition {
        name,
        description: fm.description,
        system_prompt,
        readonly: fm.readonly,
        isolate_worktree,
        isolate_workspace,
        team,
        can_delegate: fm.can_delegate,
        tags,
    })
}

/// Split a `.md` document into `(frontmatter_yaml, markdown_body)`.
///
/// Mirrors the skills loader's `parse_frontmatter`:
/// - Requires a leading `---` on the first line.
/// - The closing `\n---` (on its own line) ends the frontmatter.
/// - Returns `(frontmatter_str, body_str)`. If there is no frontmatter block,
///   returns an error (workers must declare name/description).
fn split_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let after_open = content
        .strip_prefix("---")
        .ok_or_else(|| "missing leading `---` frontmatter delimiter".to_string())?;

    // The line containing the opening `---` must be only `---` (plus optional
    // trailing newline). Reject `---foo` style.
    let after_open = after_open
        .strip_prefix(['\r', '\n'])
        .ok_or_else(|| "opening `---` must be on its own line".to_string())?;

    // Find the closing `---` on its own line.
    let close_idx = after_open
        .find("\n---")
        .ok_or_else(|| "missing closing `---` frontmatter delimiter".to_string())?;

    let yaml_str = &after_open[..close_idx];

    // Everything after the closing `---` line is the body.
    let rest = &after_open[close_idx + "\n---".len()..];
    // The closing `---` must also be alone on its line: ensure what follows
    // the `---` is a line break or end-of-string.
    let body = if rest.is_empty() {
        ""
    } else if rest.starts_with('\n') || rest.starts_with('\r') {
        rest.trim_start_matches(['\r', '\n'])
    } else {
        // Trailing content on the `---` line (e.g. `---  ` or `---x`) is malformed.
        return Err("closing `---` must be on its own line".into());
    };

    Ok((yaml_str, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parse_minimal_md_with_fallback_name() {
        let md = "---\ndescription: \"a worker\"\n---\nDo the thing.";
        let def = parse_worker_md(md, Some("worker1")).unwrap();
        assert_eq!(def.name, "worker1");
        assert_eq!(def.description, "a worker");
        assert_eq!(def.system_prompt, "Do the thing.");
        assert!(!def.readonly);
    }

    #[test]
    fn parse_full_frontmatter() {
        let md = "---\nname: explorer\ndescription: \"探索\"\nreadonly: true\ntags: [\"custom\"]\n---\n你是 explorer。";
        let def = parse_worker_md(md, None).unwrap();
        assert_eq!(def.name, "explorer");
        assert_eq!(def.description, "探索");
        assert!(def.readonly);
        // readonly → auto-ensured readonly + parallel tags, custom tag preserved.
        assert!(def.tags.contains(&"custom".to_string()));
        assert!(def.tags.contains(&"readonly".to_string()));
        assert!(def.tags.contains(&"parallel".to_string()));
        assert_eq!(def.system_prompt, "你是 explorer。");
    }

    #[test]
    fn parse_missing_name_without_fallback_errors() {
        let md = "---\ndescription: \"x\"\n---\nbody";
        let err = parse_worker_md(md, None).unwrap_err();
        assert!(err.contains("name"));
    }

    #[test]
    fn parse_missing_leading_delimiter_errors() {
        let md = "name: x\ndescription: y\n---\nbody";
        assert!(parse_worker_md(md, None).is_err());
    }

    #[test]
    fn parse_empty_body_errors() {
        let md = "---\nname: x\ndescription: y\n---\n";
        assert!(parse_worker_md(md, None).is_err());
    }

    #[test]
    fn parse_worktree_flag_for_writer_only() {
        // Sprint 8: `worktree: true` sets isolate_worktree on a writer.
        let md = "---\nname: refactorer\ndescription: \"writes code\"\nreadonly: false\nworktree: true\n---\nYou refactor code.";
        let def = parse_worker_md(md, None).unwrap();
        assert!(!def.readonly);
        assert!(def.isolate_worktree, "writer with worktree:true → isolate");
    }

    #[test]
    fn parse_worktree_flag_ignored_for_readonly() {
        // Sprint 8: a readonly worker declaring worktree:true is ignored —
        // readonly workers don't mutate files, so isolation is meaningless.
        let md = "---\nname: explorer\ndescription: \"reads\"\nreadonly: true\nworktree: true\n---\nYou explore.";
        let def = parse_worker_md(md, None).unwrap();
        assert!(def.readonly);
        assert!(
            !def.isolate_worktree,
            "readonly worker must not request worktree isolation"
        );
    }

    #[test]
    fn parse_worktree_defaults_false() {
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: false\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(!def.isolate_worktree);
    }

    #[test]
    fn builtin_defaults_parse_cleanly() {
        // The compiled-in defaults must all parse without error — guards
        // against a corrupt source .md slipping through. Sprint 9 added a
        // writer worker (implementer); Sprint 10 added data workers
        // (data-shaper, analyst).
        let defs = discover_subagents(None, None);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "explorer",
                "reviewer",
                "planner",
                "summarizer",
                "implementer",
                "data-shaper",
                "analyst"
            ]
        );
        for d in &defs {
            assert!(!d.system_prompt.is_empty());
            assert!(!d.description.is_empty());
        }
        // The 4 readonly roles are readonly + carry the readonly tag.
        for name in ["explorer", "reviewer", "planner", "summarizer"] {
            let d = defs.iter().find(|d| d.name == name).unwrap();
            assert!(d.readonly, "{name} should be readonly");
            assert!(d.tags.contains(&"readonly".to_string()));
            assert!(!d.isolate_worktree, "{name} must not request worktree");
            assert!(!d.isolate_workspace, "{name} must not request workspace");
        }
        // Sprint 9: the writer worker is non-readonly + requests worktree isolation.
        let implementer = defs.iter().find(|d| d.name == "implementer").unwrap();
        assert!(!implementer.readonly);
        assert!(
            implementer.isolate_worktree,
            "implementer must request worktree isolation (worktree:true && !readonly)"
        );
        // Sprint 10: data workers request a per-worker workspace (tmpdir).
        for name in ["data-shaper", "analyst"] {
            let d = defs.iter().find(|d| d.name == name).unwrap();
            assert!(
                d.isolate_workspace,
                "{name} must request a data workspace (workspace:true)"
            );
            // Worktree NOT requested (mutually exclusive; worktree is for writers).
            assert!(!d.isolate_worktree, "{name} must not request worktree");
        }
    }

    #[test]
    fn parse_workspace_flag() {
        // Sprint 10: `workspace: true` sets isolate_workspace.
        let md = "---\nname: data-shaper\ndescription: \"d\"\nworkspace: true\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(def.isolate_workspace);
        assert!(!def.isolate_worktree);
    }

    #[test]
    fn parse_workspace_cleared_when_worktree_active() {
        // Sprint 10: if BOTH worktree and workspace are set, worktree wins and
        // workspace is cleared (mutually exclusive — worktree also provides
        // disjoint FS, avoid double-isolation).
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: false\nworktree: true\nworkspace: true\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(def.isolate_worktree);
        assert!(
            !def.isolate_workspace,
            "workspace must be cleared when worktree is active"
        );
    }

    #[test]
    fn parse_team_frontmatter_builds_team_spec() {
        // Sprint 11: team_strategy + team_manager + team_workers → TeamSpec.
        let md = "---\n\
name: team-research\n\
description: \"team dispatcher\"\n\
team_strategy: manager-worker\n\
team_manager: planner\n\
team_workers: [\"explorer\", \"summarizer\"]\n\
---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        let spec = def.team.expect("team spec should be built");
        assert_eq!(spec.manager, "planner");
        assert_eq!(
            spec.workers,
            vec!["explorer".to_string(), "summarizer".to_string()]
        );
        assert_eq!(
            spec.strategy,
            echo_agent::agent::subagent::team::strategy::TeamStrategy::ManagerWorker
        );
    }

    #[test]
    fn parse_team_frontmatter_rejects_missing_manager() {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: manager-worker\nteam_workers: [\"w\"]\n---\nbody";
        let err = parse_worker_md(md, None).unwrap_err();
        assert!(err.contains("team_manager missing"), "got: {err}");
    }

    #[test]
    fn parse_team_frontmatter_rejects_empty_workers() {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: manager-worker\nteam_manager: m\n---\nbody";
        let err = parse_worker_md(md, None).unwrap_err();
        assert!(err.contains("team_workers empty"), "got: {err}");
    }

    #[test]
    fn parse_team_frontmatter_rejects_unsupported_strategy() {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: swarm\nteam_manager: m\nteam_workers: [\"w\"]\n---\nbody";
        let err = parse_worker_md(md, None).unwrap_err();
        assert!(err.contains("only 'manager-worker'"), "got: {err}");
    }

    #[test]
    fn parse_team_frontmatter_absent_yields_no_team() {
        // Normal worker without team_strategy → team is None.
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: true\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(def.team.is_none());
    }

    #[test]
    fn parse_can_delegate_defaults_false() {
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: true\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(!def.can_delegate);
    }

    #[test]
    fn parse_can_delegate_frontmatter() {
        let md = "---\nname: manager\ndescription: \"d\"\ncan_delegate: true\n---\nbody";
        let def = parse_worker_md(md, None).unwrap();
        assert!(def.can_delegate);
    }

    #[test]
    fn project_scope_overrides_builtin() {
        // A project-scoped .md with the same name as a builtin overrides it.
        let dir = tempdir().unwrap();
        let sub = dir.path().join(".echo-agent").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"override\"\nreadonly: true\n---\nOVERRIDDEN PROMPT",
        )
        .unwrap();

        let defs = discover_subagents(Some(dir.path()), None);
        let explorer = defs.iter().find(|d| d.name == "explorer").unwrap();
        assert_eq!(explorer.description, "override");
        assert_eq!(explorer.system_prompt, "OVERRIDDEN PROMPT");
        // Other builtins still present.
        assert!(defs.iter().any(|d| d.name == "reviewer"));
    }

    #[test]
    fn user_scope_adds_new_worker() {
        let home = tempdir().unwrap();
        let sub = home.path().join(".echo-agent").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("custom-worker.md"),
            "---\nname: custom-worker\ndescription: \"extra\"\nreadonly: false\n---\nCustom body",
        )
        .unwrap();

        let defs = discover_subagents(None, Some(home.path()));
        let custom = defs.iter().find(|d| d.name == "custom-worker").unwrap();
        assert_eq!(custom.system_prompt, "Custom body");
        assert!(!custom.readonly);
        // Builtins still there.
        assert_eq!(defs.iter().filter(|d| d.name == "explorer").count(), 1);
    }

    #[test]
    fn project_scope_beats_user_scope() {
        let home = tempdir().unwrap();
        let home_sub = home.path().join(".echo-agent").join("subagents");
        fs::create_dir_all(&home_sub).unwrap();
        fs::write(
            home_sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"user\"\nreadonly: true\n---\nUSER",
        )
        .unwrap();

        let proj = tempdir().unwrap();
        let proj_sub = proj.path().join(".echo-agent").join("subagents");
        fs::create_dir_all(&proj_sub).unwrap();
        fs::write(
            proj_sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"project\"\nreadonly: true\n---\nPROJECT",
        )
        .unwrap();

        let defs = discover_subagents(Some(proj.path()), Some(home.path()));
        let explorer = defs.iter().find(|d| d.name == "explorer").unwrap();
        assert_eq!(explorer.system_prompt, "PROJECT");
    }

    #[test]
    fn nonexistent_scope_dirs_are_silently_skipped() {
        // Neither scope dir exists → only builtins returned, no panic.
        // 4 readonly + 1 writer (Sprint 9) + 2 data (Sprint 10) = 7 builtins.
        let fake_root = PathBuf::from("/nonexistent/definitely/not/here");
        let defs = discover_subagents(Some(&fake_root), Some(&fake_root));
        assert_eq!(defs.len(), 7);
    }
}
