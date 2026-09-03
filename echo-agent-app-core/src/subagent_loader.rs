//! Subagent `.md` hot-loader (Sprint 6).
//!
//! Replaces the hardcoded `SUBAGENT_DEFINITIONS` array in `infra/factory.rs`. Subagent
//! prompts now live in `.md` files (frontmatter + markdown body) and can be
//! edited without recompiling.
//!
//! ## Resolution order (highest priority first)
//!
//! 1. **Project scope**: `<project_root>/.eko/subagents/**/*.md`
//! 2. **User scope**: `~/.eko/subagents/**/*.md`
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
//! `.eko/` convention are product-form-specific. The loader emits plain
//! `SubagentDefinition` values consumed by `register_default_subagents`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Builtin default subagent definitions (compiled into the binary).
///
/// Sourced from `src/subagents/coding/*.md`. These are the fallback used when
/// no project/user `.md` files override them. Order matters: this defines the
/// default registration order.
const BUILTIN_SUBAGENT_FILES: &[(&str, &str)] = &[
    ("explorer", include_str!("subagents/coding/explorer.md")),
    ("reviewer", include_str!("subagents/coding/reviewer.md")),
    ("planner", include_str!("subagents/coding/planner.md")),
    ("summarizer", include_str!("subagents/coding/summarizer.md")),
    // Sprint 9: writer subagent — gets write tools + worktree isolation
    // (worktree:true && !readonly → isolate_worktree). Implementation/Debugging
    // tasks route here instead of running in-place on the primary agent.
    (
        "implementer",
        include_str!("subagents/coding/implementer.md"),
    ),
    (
        "general-purpose",
        include_str!("subagents/coding/general-purpose.md"),
    ),
    // Sprint 10: data/research subagents — per-subagent tmpdir workspace
    // (workspace:true → isolate_workspace) for disjoint output artifacts.
    ("data-shaper", include_str!("subagents/data/data-shaper.md")),
    ("analyst", include_str!("subagents/data/analyst.md")),
];

/// Maximum recursion depth when scanning a scope directory for `.md` files.
const MAX_SCAN_DEPTH: usize = 4;

/// Directories skipped during scope scanning (avoid descending into these).
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".worktrees"];

/// Raw frontmatter deserialized from a subagent `.md` file.
///
/// Field names mirror `SubagentDefinition` semantics. `name` is optional here
/// (a fallback name from the filename can fill it); `description` is required.
/// The rest fall back to sensible defaults so a minimal `.md` still loads.
#[derive(Debug, Clone, Deserialize)]
struct SubagentFrontmatter {
    #[serde(default)]
    name: Option<String>,
    description: String,
    #[serde(default)]
    readonly: bool,
    /// Sprint 8: request worktree isolation for this Fork-dispatched subagent
    /// (Claude Code `isolation: worktree` equivalent). Only meaningful for
    /// writer subagents; resolved by EKO's worktree isolation policy.
    #[serde(default)]
    worktree: bool,
    /// Sprint 10: request a per-subagent data workspace (tmpdir) for this
    /// Fork-dispatched data/research subagent — disjoint output dir, no git
    /// coupling. Mutually exclusive with `worktree` (worktree wins if both).
    /// Resolved by EKO's data-workspace isolation policy.
    #[serde(default)]
    workspace: bool,
    /// Optional tags; merged with the default readonly/parallel tags when
    /// `readonly` is true. Empty if unset.
    #[serde(default)]
    tags: Vec<String>,
    /// Optional nested delegation capability. Defaults false: subagents execute
    /// the assigned task and may suggest follow-up tasks, but cannot spawn
    /// child subagents unless explicitly granted this capability.
    #[serde(default)]
    can_delegate: bool,
    /// Model override. Omitted values track the current parent generation.
    /// Explicit `inherit` / `fast` values resolve once at registration and then
    /// remain fixed; any other string is a concrete model id.
    #[serde(default)]
    model: Option<String>,
    /// Max ReAct turns for this role (`None` = unlimited / builder default).
    #[serde(default)]
    max_turns: Option<usize>,
    /// Prefer background dispatch (Phase 1: parse+store only; Phase 2 schedules).
    #[serde(default)]
    is_background: bool,
    /// Sprint 11: declare this subagent as a team-mode dispatcher. Only
    /// `"manager-subagent"` is the supported wire value via frontmatter (other strategies are
    /// programmatic-only — they carry inline agent-name data).
    #[serde(default)]
    team_strategy: Option<String>,
    /// Sprint 11: the manager/leader subagent name (must be separately
    /// registered). Required when `team_strategy` is set.
    #[serde(default)]
    team_manager: Option<String>,
    /// Sprint 11: subagent team member names (each must be separately registered).
    /// Required (non-empty) when `team_strategy` is set.
    #[serde(default)]
    team_subagents: Vec<String>,
    /// Per-subagent execution timeout in seconds (0/None = framework default).
    /// Used by long-running custom roles.
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Per-subagent reasoning-depth spec (`ThinkingConfig::parse_spec` syntax:
    /// `low`/`medium`/`high`/`disabled`/`auto`/budget number). `None` = inherit
    /// the parent generation's thinking.
    #[serde(default)]
    thinking: Option<String>,
}

/// A resolved subagent definition ready for registration.
#[derive(Debug, Clone)]
pub struct SubagentDefinition {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    /// Where the subagent prompt came from. Stable diagnostic value:
    /// `builtin:<name>`, `user:<path>`, or `project:<path>`.
    pub source: String,
    pub readonly: bool,
    /// Sprint 8: whether Fork dispatch should isolate this subagent in a git
    /// worktree. Mapped from frontmatter `worktree: true`. Only meaningful for
    /// writer subagents (readonly subagents don't mutate files).
    pub isolate_worktree: bool,
    /// Sprint 10: whether Fork dispatch should give this subagent a per-subagent
    /// data workspace (tmpdir). Mapped from frontmatter `workspace: true`.
    /// Mutually exclusive with isolate_worktree (worktree wins if both).
    pub isolate_workspace: bool,
    /// Sprint 11: if Some, this subagent is a team-mode dispatcher (not a
    /// normal subagent). The registration path sets `execution_mode = Team` and
    /// attaches this TeamSpec. manager + subagent team members are name-references.
    pub team: Option<echo_agent::subagent::TeamSpec>,
    /// Whether this subagent may receive the framework `agent_tool` and spawn
    /// child subagents. Defaults false.
    pub can_delegate: bool,
    /// Model override after frontmatter normalize (`None` = inherit parent).
    pub model: Option<String>,
    /// Max ReAct turns (`None` = unlimited / builder default of 0).
    pub max_turns: Option<usize>,
    /// Prefer background dispatch (stored for Phase 2).
    pub is_background: bool,
    /// Per-subagent execution timeout in seconds (`None` = framework default).
    pub timeout_secs: Option<u64>,
    /// Per-subagent reasoning-depth spec, parsed via
    /// `ThinkingConfig::parse_spec` at registration (`None` = inherit the
    /// parent generation's thinking).
    pub thinking: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentCatalogEntry {
    pub name: String,
    pub description: String,
    pub readonly: bool,
    pub can_delegate: bool,
    pub isolation: String,
}

/// Immutable catalog derived from the same definitions used for registration.
#[derive(Debug, Clone, Default)]
pub struct SubagentCatalogSnapshot {
    entries: Vec<SubagentCatalogEntry>,
}

impl SubagentCatalogSnapshot {
    pub fn from_definitions(definitions: &[SubagentDefinition]) -> Self {
        Self {
            entries: definitions
                .iter()
                .map(|definition| SubagentCatalogEntry {
                    name: definition.name.clone(),
                    description: definition.description.clone(),
                    readonly: definition.readonly,
                    can_delegate: definition.can_delegate,
                    isolation: subagent_isolation(definition).to_string(),
                })
                .collect(),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    pub fn prompt(&self) -> String {
        let mut output = String::from(
            "\n## Available Subagents (agent_tool)\n\
             Use agent_tool for bounded side work and task_execute for formal DAG execution.\n\
             Default context is fresh; use mode=fork only when recent conversation turns are required.\n",
        );
        for entry in &self.entries {
            let access = if entry.readonly { "readonly" } else { "writer" };
            let delegation = if entry.can_delegate {
                "delegation=enabled"
            } else {
                "delegation=disabled"
            };
            output.push_str(&format!(
                "- `{}`: {} [access={access}, isolation={}, {delegation}]\n",
                entry.name, entry.description, entry.isolation
            ));
        }
        output
    }
}

pub fn subagent_isolation(definition: &SubagentDefinition) -> &'static str {
    if definition.isolate_worktree {
        "worktree"
    } else if definition.isolate_workspace {
        "workspace"
    } else {
        "context"
    }
}

/// Discover subagent definitions across scopes + builtin fallback.
///
/// `project_root` is the cwd or detected project root; `user_home` is the
/// user's home directory (`~`). Either may be `None` if undetectable, in which
/// case that scope is skipped.
///
/// Returns at least the builtin defaults (so the app always has the 4 default
/// subagents), with project/user overrides layered on top by name.
pub fn discover_subagents(
    project_root: Option<&Path>,
    user_home: Option<&Path>,
) -> Vec<SubagentDefinition> {
    // name → definition, later inserts (lower priority) don't overwrite.
    let mut by_name: std::collections::HashMap<String, SubagentDefinition> =
        std::collections::HashMap::new();

    // 1. Builtin defaults (lowest priority — inserted first, never overwritten).
    for (builtin_name, content) in BUILTIN_SUBAGENT_FILES {
        match parse_subagent_md(content, Some(*builtin_name)) {
            Ok(def) => {
                by_name.entry(def.name.clone()).or_insert(def);
            }
            Err(e) => {
                tracing::error!(
                    subagent = *builtin_name,
                    error = %e,
                    "Builtin subagent .md failed to parse (this is a bug — source file is corrupt)"
                );
            }
        }
    }

    // 2. User scope (~/.eko/subagents/).
    if let Some(home) = user_home {
        let user_dir = home.join(".eko").join("subagents");
        merge_scope(&mut by_name, "user", &user_dir);
    }

    // 3. Project scope (<root>/.eko/subagents/) — highest priority, last.
    if let Some(root) = project_root {
        let project_dir = root.join(".eko").join("subagents");
        merge_scope(&mut by_name, "project", &project_dir);
    }

    // Preserve builtin order for stable registration, then append any
    // extra (user/project-only) subagents at the end.
    let mut result: Vec<SubagentDefinition> = Vec::with_capacity(by_name.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (builtin_name, _) in BUILTIN_SUBAGENT_FILES {
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

/// Scan a scope directory and merge its parsed subagents into `by_name`,
/// **overwriting** any same-named entry. Higher-priority scopes are merged
/// later (builtins first → user → project), so the last write wins, giving
/// project > user > builtin precedence.
fn merge_scope(
    by_name: &mut std::collections::HashMap<String, SubagentDefinition>,
    scope: &str,
    dir: &Path,
) {
    if !dir.is_dir() {
        return;
    }
    let mut found = Vec::new();
    scan_directory(dir, 0, &mut found);
    for (path, content) in found {
        match parse_subagent_md(&content, None) {
            Ok(mut def) => {
                def.source = format!("{scope}:{}", path.display());
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

/// Parse a subagent `.md` file into a [`SubagentDefinition`].
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
pub fn parse_subagent_md(
    content: &str,
    fallback_name: Option<&str>,
) -> Result<SubagentDefinition, String> {
    let (fm_str, body) = split_frontmatter(content)?;
    let fm: SubagentFrontmatter = if fm_str.trim().is_empty() {
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
        return Err(format!("subagent `{name}` missing `description`"));
    }

    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        return Err(format!(
            "subagent `{name}` has empty system prompt (markdown body after frontmatter)"
        ));
    }

    // Ensure readonly subagents carry the physical-enforcement tags the
    // registration path expects. Non-readonly subagents keep their declared tags.
    let mut tags = fm.tags;
    if fm.readonly {
        if !tags.iter().any(|t| t == "readonly") {
            tags.push("readonly".into());
        }
        if !tags.iter().any(|t| t == "parallel") {
            tags.push("parallel".into());
        }
    }

    // Sprint 8: `worktree: true` only makes sense for writer subagents; if a
    // readonly subagent declares it, ignore (readonly subagents don't mutate files).
    let isolate_worktree = fm.worktree && !fm.readonly;
    // Sprint 10: `workspace: true` requests a per-subagent data tmpdir. It's
    // meaningful for ANY subagent (data subagents emit artifacts regardless of
    // readonly), but mutually exclusive with worktree — if both are set,
    // worktree wins (it also provides disjoint FS). Clear workspace when
    // worktree is active to avoid double-isolation at registration.
    let isolate_workspace = fm.workspace && !isolate_worktree;

    // Sprint 11: parse team frontmatter into a TeamSpec (the wire value
    // uses `manager-subagent`. Validate that manager +
    // non-empty subagent team members are given.
    let team = if let Some(strategy) = fm.team_strategy.as_deref() {
        if strategy != "manager-subagent" {
            return Err(format!(
                "subagent `{name}`: team_strategy '{strategy}' unsupported via frontmatter (only 'manager-subagent')"
            ));
        }
        let manager = fm.team_manager.clone().ok_or_else(|| {
            format!("subagent `{name}`: team_strategy set but team_manager missing")
        })?;
        if fm.team_subagents.is_empty() {
            return Err(format!(
                "subagent `{name}`: team_strategy set but team_subagents empty"
            ));
        }
        Some(echo_agent::subagent::TeamSpec {
            strategy: echo_agent::subagent::TeamStrategy::ManagerSubagent,
            manager,
            subagents: fm.team_subagents.clone(),
            config: echo_agent::subagent::TeamConfig::default(),
        })
    } else {
        None
    };

    // Only an omitted/empty value dynamically inherits the parent generation.
    // Explicit aliases stay present so the runtime can resolve them once and
    // preserve the resulting fixed model across parent hot-swaps.
    let model = fm
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());

    // Same normalization as `model`: only an omitted/empty value inherits the
    // parent generation's thinking; any other string is an explicit spec
    // resolved once at registration.
    let thinking = fm
        .thinking
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    Ok(SubagentDefinition {
        source: fallback_name
            .map(|name| format!("builtin:{name}"))
            .unwrap_or_else(|| "file:<unknown>".to_string()),
        name,
        description: fm.description,
        system_prompt,
        readonly: fm.readonly,
        isolate_worktree,
        isolate_workspace,
        team,
        can_delegate: fm.can_delegate,
        model,
        max_turns: fm.max_turns,
        is_background: fm.is_background,
        timeout_secs: fm.timeout_secs,
        thinking,
        tags,
    })
}

/// Split a `.md` document into `(frontmatter_yaml, markdown_body)`.
///
/// Mirrors the skills loader's `parse_frontmatter`:
/// - Requires a leading `---` on the first line.
/// - The closing `\n---` (on its own line) ends the frontmatter.
/// - Returns `(frontmatter_str, body_str)`. If there is no frontmatter block,
///   returns an error (subagents must declare name/description).
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

    type TestResult = Result<(), String>;

    #[test]
    fn parse_minimal_md_with_fallback_name() -> TestResult {
        let md = "---\ndescription: \"a subagent\"\n---\nDo the thing.";
        let def = parse_subagent_md(md, Some("subagent1"))?;
        assert_eq!(def.name, "subagent1");
        assert_eq!(def.description, "a subagent");
        assert_eq!(def.system_prompt, "Do the thing.");
        assert!(!def.readonly);
        Ok(())
    }

    #[test]
    fn parse_full_frontmatter() -> TestResult {
        let md = "---\nname: explorer\ndescription: \"探索\"\nreadonly: true\ntags: [\"custom\"]\n---\n你是 explorer。";
        let def = parse_subagent_md(md, None)?;
        assert_eq!(def.name, "explorer");
        assert_eq!(def.description, "探索");
        assert!(def.readonly);
        // readonly → auto-ensured readonly + parallel tags, custom tag preserved.
        assert!(def.tags.contains(&"custom".to_string()));
        assert!(def.tags.contains(&"readonly".to_string()));
        assert!(def.tags.contains(&"parallel".to_string()));
        assert_eq!(def.system_prompt, "你是 explorer。");
        Ok(())
    }

    #[test]
    fn parse_model_max_turns_is_background() -> TestResult {
        let md = "---\nname: explorer\ndescription: \"x\"\nreadonly: true\nmodel: fast\nmax_turns: 30\nis_background: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert_eq!(def.model.as_deref(), Some("fast"));
        assert_eq!(def.max_turns, Some(30));
        assert!(def.is_background);
        Ok(())
    }

    #[test]
    fn parse_timeout_secs_and_thinking() -> TestResult {
        // Per-subagent thinking/timeout frontmatter: explicit values pass
        // through as specs (parsed by infra at registration); empty/whitespace
        // thinking normalizes to None (inherit parent generation).
        let md = "---\nname: long-review\ndescription: \"x\"\nreadonly: true\nmodel: fast\nmax_turns: 64\ntimeout_secs: 90000\nthinking: low\nis_background: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert_eq!(def.timeout_secs, Some(90000));
        assert_eq!(def.thinking.as_deref(), Some("low"));

        let unset = parse_subagent_md("---\nname: w\ndescription: \"d\"\n---\nbody", None)?;
        assert_eq!(unset.timeout_secs, None);
        assert_eq!(unset.thinking, None);

        let blank = parse_subagent_md(
            "---\nname: w\ndescription: \"d\"\nthinking: \"  \"\n---\nbody",
            None,
        )?;
        assert_eq!(blank.thinking, None, "whitespace-only thinking → None");
        Ok(())
    }

    #[test]
    fn parse_explicit_model_inherit_remains_an_override() -> Result<(), String> {
        let md = "---\nname: explorer\ndescription: \"x\"\nmodel: inherit\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert_eq!(def.model.as_deref(), Some("inherit"));
        assert!(!def.is_background);
        assert!(def.max_turns.is_none());
        Ok(())
    }

    #[test]
    fn parse_missing_name_without_fallback_errors() -> TestResult {
        let md = "---\ndescription: \"x\"\n---\nbody";
        let err = parse_subagent_md(md, None)
            .err()
            .ok_or_else(|| "expected missing-name parse error".to_string())?;
        assert!(err.contains("name"));
        Ok(())
    }

    #[test]
    fn parse_missing_leading_delimiter_errors() {
        let md = "name: x\ndescription: y\n---\nbody";
        assert!(parse_subagent_md(md, None).is_err());
    }

    #[test]
    fn parse_empty_body_errors() {
        let md = "---\nname: x\ndescription: y\n---\n";
        assert!(parse_subagent_md(md, None).is_err());
    }

    #[test]
    fn parse_worktree_flag_for_writer_only() -> TestResult {
        // Sprint 8: `worktree: true` sets isolate_worktree on a writer.
        let md = "---\nname: refactorer\ndescription: \"writes code\"\nreadonly: false\nworktree: true\n---\nYou refactor code.";
        let def = parse_subagent_md(md, None)?;
        assert!(!def.readonly);
        assert!(def.isolate_worktree, "writer with worktree:true → isolate");
        Ok(())
    }

    #[test]
    fn parse_worktree_flag_ignored_for_readonly() -> TestResult {
        // Sprint 8: a readonly subagent declaring worktree:true is ignored —
        // readonly subagents don't mutate files, so isolation is meaningless.
        let md = "---\nname: explorer\ndescription: \"reads\"\nreadonly: true\nworktree: true\n---\nYou explore.";
        let def = parse_subagent_md(md, None)?;
        assert!(def.readonly);
        assert!(
            !def.isolate_worktree,
            "readonly subagent must not request worktree isolation"
        );
        Ok(())
    }

    #[test]
    fn parse_worktree_defaults_false() -> TestResult {
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: false\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(!def.isolate_worktree);
        Ok(())
    }

    #[test]
    fn builtin_defaults_parse_cleanly() -> TestResult {
        // The compiled-in defaults must all parse without error — guards
        // against a corrupt source .md slipping through. Sprint 9 added a
        // writer subagent (implementer); Sprint 10 added data subagents
        // (data-shaper, analyst); Phase 1 added general-purpose.
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
                "general-purpose",
                "data-shaper",
                "analyst"
            ]
        );
        for d in &defs {
            assert!(!d.system_prompt.is_empty());
            assert!(!d.description.is_empty());
        }
        let explorer = defs
            .iter()
            .find(|d| d.name == "explorer")
            .ok_or_else(|| "builtin explorer must load".to_string())?;
        assert_eq!(explorer.model.as_deref(), Some("fast"));
        // The 4 readonly roles are readonly + carry the readonly tag.
        for name in ["explorer", "reviewer", "planner", "summarizer"] {
            let d = defs
                .iter()
                .find(|d| d.name == name)
                .ok_or_else(|| format!("builtin {name} must load"))?;
            assert!(d.readonly, "{name} should be readonly");
            assert!(d.tags.contains(&"readonly".to_string()));
            assert!(!d.isolate_worktree, "{name} must not request worktree");
            assert!(!d.isolate_workspace, "{name} must not request workspace");
        }
        // Sprint 9: the writer subagent is non-readonly + requests worktree isolation.
        // Phase 2 Task 13: hard-gate — builtin implementer.md must keep worktree: true
        // so multi-implementer dispatches never share the main tree.
        let implementer_md = BUILTIN_SUBAGENT_FILES
            .iter()
            .find(|(name, _)| *name == "implementer")
            .map(|(_, content)| *content)
            .ok_or_else(|| "builtin implementer.md".to_string())?;
        assert!(
            implementer_md.contains("worktree: true"),
            "builtin implementer.md must declare worktree: true"
        );
        let implementer = defs
            .iter()
            .find(|d| d.name == "implementer")
            .ok_or_else(|| "builtin implementer must load".to_string())?;
        assert!(!implementer.readonly);
        assert!(
            implementer.isolate_worktree,
            "implementer must request worktree isolation (worktree:true && !readonly)"
        );
        let gp = defs
            .iter()
            .find(|d| d.name == "general-purpose")
            .ok_or_else(|| "builtin general-purpose must load".to_string())?;
        assert!(!gp.readonly);
        assert!(
            !gp.isolate_worktree,
            "general-purpose stays in-workspace by default; use implementer for worktree"
        );
        // Sprint 10: data subagents request a per-subagent workspace (tmpdir).
        for name in ["data-shaper", "analyst"] {
            let d = defs
                .iter()
                .find(|d| d.name == name)
                .ok_or_else(|| format!("builtin {name} must load"))?;
            assert!(
                d.isolate_workspace,
                "{name} must request a data workspace (workspace:true)"
            );
            // Worktree NOT requested (mutually exclusive; worktree is for writers).
            assert!(!d.isolate_worktree, "{name} must not request worktree");
        }
        Ok(())
    }

    #[test]
    fn parse_workspace_flag() -> TestResult {
        // Sprint 10: `workspace: true` sets isolate_workspace.
        let md = "---\nname: data-shaper\ndescription: \"d\"\nworkspace: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(def.isolate_workspace);
        assert!(!def.isolate_worktree);
        Ok(())
    }

    #[test]
    fn parse_workspace_cleared_when_worktree_active() -> TestResult {
        // Sprint 10: if BOTH worktree and workspace are set, worktree wins and
        // workspace is cleared (mutually exclusive — worktree also provides
        // disjoint FS, avoid double-isolation).
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: false\nworktree: true\nworkspace: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(def.isolate_worktree);
        assert!(
            !def.isolate_workspace,
            "workspace must be cleared when worktree is active"
        );
        Ok(())
    }

    #[test]
    fn parse_team_frontmatter_builds_team_spec() -> TestResult {
        // Sprint 11: team_strategy + team_manager + team_subagents → TeamSpec.
        let md = "---\n\
name: team-research\n\
description: \"team dispatcher\"\n\
team_strategy: manager-subagent\n\
team_manager: planner\n\
team_subagents: [\"explorer\", \"summarizer\"]\n\
---\nbody";
        let def = parse_subagent_md(md, None)?;
        let spec = def
            .team
            .ok_or_else(|| "team spec should be built".to_string())?;
        assert_eq!(spec.manager, "planner");
        assert_eq!(
            spec.subagents,
            vec!["explorer".to_string(), "summarizer".to_string()]
        );
        assert_eq!(
            spec.strategy,
            echo_agent::subagent::TeamStrategy::ManagerSubagent
        );
        Ok(())
    }

    #[test]
    fn parse_team_frontmatter_rejects_missing_manager() -> TestResult {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: manager-subagent\nteam_subagents: [\"w\"]\n---\nbody";
        let err = parse_subagent_md(md, None)
            .err()
            .ok_or_else(|| "expected missing-manager parse error".to_string())?;
        assert!(err.contains("team_manager missing"), "got: {err}");
        Ok(())
    }

    #[test]
    fn parse_team_frontmatter_rejects_empty_subagents() -> TestResult {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: manager-subagent\nteam_manager: m\n---\nbody";
        let err = parse_subagent_md(md, None)
            .err()
            .ok_or_else(|| "expected empty-subagents parse error".to_string())?;
        assert!(err.contains("team_subagents empty"), "got: {err}");
        Ok(())
    }

    #[test]
    fn parse_team_frontmatter_rejects_unsupported_strategy() -> TestResult {
        let md = "---\nname: t\ndescription: \"d\"\nteam_strategy: swarm\nteam_manager: m\nteam_subagents: [\"w\"]\n---\nbody";
        let err = parse_subagent_md(md, None)
            .err()
            .ok_or_else(|| "expected unsupported-strategy parse error".to_string())?;
        assert!(err.contains("only 'manager-subagent'"), "got: {err}");
        Ok(())
    }

    #[test]
    fn parse_team_frontmatter_absent_yields_no_team() -> TestResult {
        // Normal subagent without team_strategy → team is None.
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(def.team.is_none());
        Ok(())
    }

    #[test]
    fn parse_can_delegate_defaults_false() -> TestResult {
        let md = "---\nname: w\ndescription: \"d\"\nreadonly: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(!def.can_delegate);
        Ok(())
    }

    #[test]
    fn builtin_roles_all_delegate_capable() -> TestResult {
        // 全方向通信矩阵:每个内置角色都可派发自己的子智能体(NestedDelegation
        // 深度由框架 policy 兜底),不再有只能被动接受派发的角色。
        for (name, md) in BUILTIN_SUBAGENT_FILES {
            let def = parse_subagent_md(md, None)?;
            assert!(
                def.can_delegate,
                "builtin role {name} must declare can_delegate"
            );
        }
        Ok(())
    }

    #[test]
    fn parse_can_delegate_frontmatter() -> TestResult {
        let md = "---\nname: manager\ndescription: \"d\"\ncan_delegate: true\n---\nbody";
        let def = parse_subagent_md(md, None)?;
        assert!(def.can_delegate);
        Ok(())
    }

    #[test]
    fn project_scope_overrides_builtin() -> TestResult {
        // A project-scoped .md with the same name as a builtin overrides it.
        let dir = tempdir().map_err(|e| e.to_string())?;
        let sub = dir.path().join(".eko").join("subagents");
        fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
        fs::write(
            sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"override\"\nreadonly: true\n---\nOVERRIDDEN PROMPT",
        )
        .map_err(|e| e.to_string())?;

        let defs = discover_subagents(Some(dir.path()), None);
        let explorer = defs
            .iter()
            .find(|d| d.name == "explorer")
            .ok_or_else(|| "project explorer must load".to_string())?;
        assert_eq!(explorer.description, "override");
        assert_eq!(explorer.system_prompt, "OVERRIDDEN PROMPT");
        // Other builtins still present.
        assert!(defs.iter().any(|d| d.name == "reviewer"));
        Ok(())
    }

    #[test]
    fn user_scope_adds_new_subagent() -> TestResult {
        let home = tempdir().map_err(|e| e.to_string())?;
        let sub = home.path().join(".eko").join("subagents");
        fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
        fs::write(
            sub.join("custom-subagent.md"),
            "---\nname: custom-subagent\ndescription: \"extra\"\nreadonly: false\n---\nCustom body",
        )
        .map_err(|e| e.to_string())?;

        let defs = discover_subagents(None, Some(home.path()));
        let custom = defs
            .iter()
            .find(|d| d.name == "custom-subagent")
            .ok_or_else(|| "custom subagent must load".to_string())?;
        assert_eq!(custom.system_prompt, "Custom body");
        assert!(!custom.readonly);
        // Builtins still there.
        assert_eq!(defs.iter().filter(|d| d.name == "explorer").count(), 1);
        Ok(())
    }

    #[test]
    fn project_scope_beats_user_scope() -> TestResult {
        let home = tempdir().map_err(|e| e.to_string())?;
        let home_sub = home.path().join(".eko").join("subagents");
        fs::create_dir_all(&home_sub).map_err(|e| e.to_string())?;
        fs::write(
            home_sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"user\"\nreadonly: true\n---\nUSER",
        )
        .map_err(|e| e.to_string())?;

        let proj = tempdir().map_err(|e| e.to_string())?;
        let proj_sub = proj.path().join(".eko").join("subagents");
        fs::create_dir_all(&proj_sub).map_err(|e| e.to_string())?;
        fs::write(
            proj_sub.join("explorer.md"),
            "---\nname: explorer\ndescription: \"project\"\nreadonly: true\n---\nPROJECT",
        )
        .map_err(|e| e.to_string())?;

        let defs = discover_subagents(Some(proj.path()), Some(home.path()));
        let explorer = defs
            .iter()
            .find(|d| d.name == "explorer")
            .ok_or_else(|| "project explorer must load".to_string())?;
        assert_eq!(explorer.system_prompt, "PROJECT");
        Ok(())
    }

    #[test]
    fn nonexistent_scope_dirs_are_silently_skipped() {
        // Neither scope dir exists → only builtins returned, no panic.
        // 4 readonly + 1 writer + 1 general-purpose + 2 data = 8 builtins.
        let fake_root = PathBuf::from("/nonexistent/definitely/not/here");
        let defs = discover_subagents(Some(&fake_root), Some(&fake_root));
        assert_eq!(defs.len(), 8);
    }

    #[test]
    fn catalog_lists_builtin_names_and_descriptions() {
        let defs = discover_subagents(None, None);
        let catalog = SubagentCatalogSnapshot::from_definitions(&defs);
        let text = catalog.prompt();
        assert!(text.contains("`explorer`"));
        assert!(text.contains("`implementer`"));
        assert!(text.contains("`general-purpose`"));
        assert!(text.contains("agent_tool"));
        assert!(
            text.contains("探索") || text.contains("只读") || text.contains("Read"),
            "catalog should include explorer description text, got: {text}"
        );
        assert!(text.contains("access=readonly"));
        assert!(text.contains("isolation=worktree"));
    }
}
