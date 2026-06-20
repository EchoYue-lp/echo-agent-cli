//! Structured plan generator.
//!
//! Turns a complex user goal into a structured [`TaskPlan`] via a single
//! JSON-mode LLM call, then validates plan quality (rejects vague wording,
//! ensures every task has concrete targets + verification).
//! Broad read-only fanout is the exception: the router has already selected
//! workers, so we generate that DAG deterministically instead of asking the
//! model for a plan that may fail on wording or JSON shape.
//!
//! This is the structured-output counterpart to the plan's "writing-plans
//! integration" section (lines 587-633). The LLM is asked to return strict
//! JSON matching a schema derived from `PlanTask`; we never accept free-form
//! markdown plans from the model.
//!
//! All LLM I/O is colocated here. The classifier (`classify.rs`) is
//! heuristic-only by default; if a future LLM complexity classifier is
//! needed it also lives in this module's style.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use echo_agent::llm::{ChatRequest, LlmClient, ResponseFormat};
use echo_agent::prelude::Message;

use super::classify::Classification;
use super::profiles::{
    ALL_WORKER_ROLES, ProfileTemplate, WORKER_CAPABILITY_CATALOG, worker_catalog_prompt,
};
use super::types::*;

/// Error returned by plan generation. Distinguishes infrastructure failures
/// (LLM unreachable, malformed JSON) from quality rejections (the model
/// produced a parseable but low-quality plan).
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("no LLM client available; cannot generate a structured plan")]
    NoLlmClient,
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("LLM returned malformed JSON: {0}")]
    Json(String),
    #[error("plan rejected: {0}")]
    Quality(String),
}

/// The raw shape the LLM is asked to return. Mirrors `TaskPlan`/`PlanTask`
/// but is lenient about IDs and enum casing so the model has room to be
/// useful; `PlanDraft::into_plan` fills in canonical ids and normalizes.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct PlanDraft {
    goal: String,
    #[serde(default)]
    assumptions: Vec<String>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    execution_mode: Option<String>,
    tasks: Vec<PlanTaskDraft>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PlanTaskDraft {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    agent_role: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    parallel_group: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    verification: Vec<String>,
}

/// Wording that signals a vague, non-actionable task. The plan's
/// "Forbidden plan wording" list (lines 605-610), plus a few common cousins.
const FORBIDDEN_PHRASES: &[&str] = &[
    "继续完善",
    "优化相关代码",
    "处理边界情况",
    "后续补测试",
    "视情况修改",
    "TBD",
    "TODO",
    "待定",
    "etc.",
    "and so on",
    "进一步优化",
    "完善相关",
];

/// Result of a successful generation: the canonical plan plus the ids that
/// were assigned (so the caller can log / surface them).
pub struct GeneratedPlan {
    pub plan: TaskPlan,
    pub warnings: Vec<String>,
}

/// Build a deterministic parallel read-only plan from the router's worker
/// decision. This is the stable runtime path for broad analysis/review tasks:
/// once routing says "fan out", execution must not depend on a second LLM call
/// producing perfect planning JSON.
pub fn generate_parallel_readonly_plan(
    run_id: &str,
    goal: &str,
    classification: &Classification,
    suggested_workers: &[String],
) -> GeneratedPlan {
    let profile = classification.inferred_profile;
    let template = ProfileTemplate::for_profile(profile);
    let mut warnings = Vec::new();
    let mut workers = Vec::new();

    for worker in suggested_workers {
        if ALL_WORKER_ROLES
            .iter()
            .any(|allowed| allowed == &worker.as_str())
            && !workers.iter().any(|existing| existing == worker)
        {
            workers.push(worker.clone());
        }
    }

    if workers.is_empty() {
        for worker in template.default_worker_roles {
            if !workers.iter().any(|existing| existing == worker) {
                workers.push((*worker).to_string());
            }
        }
        warnings.push("router returned no usable workers; used profile defaults".to_string());
    }

    let has_synthesis = workers.iter().any(|worker| worker == "summary_writer");
    let mut discovery_workers = workers
        .iter()
        .filter(|worker| worker.as_str() != "summary_writer")
        .cloned()
        .collect::<Vec<_>>();

    if discovery_workers.is_empty() {
        discovery_workers.push("project_explorer".to_string());
        warnings.push("read-only plan needed at least one discovery worker".to_string());
    }

    let parallel_group = "readonly-fanout".to_string();
    let mut tasks = Vec::new();
    let mut fanout_ids = Vec::new();
    for (i, worker) in discovery_workers.iter().enumerate() {
        let id = format!("readonly-{}-{}", i.saturating_add(1), role_slug(worker));
        fanout_ids.push(id.clone());
        tasks.push(PlanTask {
            id,
            title: worker_title(worker),
            description: format!(
                "{} Focus on this goal: {}",
                worker_description(worker),
                goal.trim()
            ),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: worker.clone(),
            domain_profile: profile,
            depends_on: Vec::new(),
            parallel_group: Some(parallel_group.clone()),
            files: vec!["workspace".to_string()],
            allowed_tools: vec![
                "repo_map".to_string(),
                "glob".to_string(),
                "list_dir".to_string(),
                "read_file".to_string(),
                "rg".to_string(),
            ],
            verification: vec![format!(
                "Return concrete findings from {} with file paths, evidence, and uncertainty.",
                worker
            )],
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
        });
    }

    if has_synthesis || tasks.len() > 1 {
        tasks.push(PlanTask {
            id: "readonly-synthesis-summary_writer".to_string(),
            title: "Synthesize worker findings".to_string(),
            description: format!(
                "Combine parallel read-only findings into an actionable answer for: {}",
                goal.trim()
            ),
            kind: PlanTaskKind::Summary,
            agent_role: "summary_writer".to_string(),
            domain_profile: profile,
            depends_on: fanout_ids,
            parallel_group: None,
            files: Vec::new(),
            allowed_tools: vec!["read_file".to_string()],
            verification: vec![
                "Summarize agreements, conflicts, evidence gaps, and recommended next actions."
                    .to_string(),
            ],
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
        });
    }

    let plan = TaskPlan {
        plan_id: uuid::Uuid::new_v4().to_string(),
        run_id: run_id.to_string(),
        domain_profile: profile,
        goal: goal.trim().to_string(),
        assumptions: vec![
            "This is a read-only analysis/review request; workers must not modify files."
                .to_string(),
        ],
        risks: vec![
            "Worker findings may overlap; synthesis should reconcile conflicts and cite evidence."
                .to_string(),
        ],
        execution_mode: ExecutionMode::Parallel,
        tasks,
    };

    GeneratedPlan { plan, warnings }
}

/// Generate a structured plan for a run.
///
/// `run_id` becomes the plan's `run_id`; `plan_id` is minted here. The
/// classification (profile + reason) steers the prompt.
pub async fn generate_plan(
    llm: &Arc<dyn LlmClient>,
    run_id: &str,
    goal: &str,
    classification: &Classification,
    suggested_workers: &[String],
) -> Result<GeneratedPlan, PlanError> {
    let profile = classification.inferred_profile;
    let template = ProfileTemplate::for_profile(profile);
    let prompt = build_prompt(goal, template, suggested_workers);

    // JSON-mode request. We use JsonObject rather than JsonSchema so we work
    // across all providers (schema-enforced mode is opt-in per provider; see
    // ProviderCapabilities). Validation is done on our side instead.
    let request = ChatRequest {
        messages: vec![
            Message::system(system_preamble(template)),
            Message::user(prompt),
        ],
        response_format: Some(ResponseFormat::JsonObject),
        ..Default::default()
    };
    let response = llm
        .chat(request)
        .await
        .map_err(|e| PlanError::Llm(e.to_string()))?;
    let content = response.content().unwrap_or_default();
    let draft: PlanDraft = serde_json::from_str(content.trim())
        .map_err(|e| PlanError::Json(format!("{e}; raw head: {}", head(&content, 200))))?;

    let plan_id = uuid::Uuid::new_v4().to_string();
    let mut warnings = Vec::new();
    let tasks = normalize_tasks(&draft.tasks, &mut warnings, template, profile)?;
    validate_plan(&draft.goal, &tasks, &mut warnings)?;

    let plan = TaskPlan {
        plan_id,
        run_id: run_id.to_string(),
        domain_profile: profile,
        goal: draft.goal,
        assumptions: draft.assumptions,
        risks: draft.risks,
        execution_mode: parse_execution_mode(draft.execution_mode.as_deref()),
        tasks,
    };
    Ok(GeneratedPlan { plan, warnings })
}

fn system_preamble(template: &ProfileTemplate) -> String {
    format!(
        "You are a task planner for the {label} domain. \
        Convert the user's goal into a structured execution plan. \
        Return ONLY valid JSON matching the requested schema — no markdown, \
        no prose before or after.\n\n\
        Domain guidance: {suffix}\n\n\
        Registered read-only worker capability catalog:
        {workers}

        Domain profiles are guidance and review context, not hard worker
        boundaries. Pick worker roles by the actual capability needed for each
        task; cross-domain plans may mix coding, data, research, and medical
        workers.",
        label = template.label,
        suffix = template.prompt_suffix,
        workers = worker_catalog_prompt(),
    )
}

fn build_prompt(goal: &str, template: &ProfileTemplate, suggested_workers: &[String]) -> String {
    let checklist = template
        .review_checklist
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n");
    let worker_hint = if suggested_workers.is_empty() {
        "Router suggested workers: none. Select workers from the capability catalog.".to_string()
    } else {
        format!(
            "Router suggested workers: {}.\n\
            If the goal is read-only investigation/review, split the plan into \
            independent parallel tasks that use these worker roles where useful. \
            Do not collapse a broad read-only request into one generic task.",
            suggested_workers.join(", ")
        )
    };
    format!(
        "Goal:\n{goal}\n\n\
        {worker_hint}\n\n\
        Return JSON with this exact shape:\n\
        {{\n  \
          \"goal\": string (restate the user's goal concretely),\n  \
          \"assumptions\": string[],\n  \
          \"risks\": string[],\n  \
          \"execution_mode\": \"sequential\" | \"parallel\" | \"plan_only\",\n  \
          \"tasks\": [{{\n    \
            \"title\": string,\n    \
            \"description\": string (concrete, not vague),\n    \
            \"kind\": \"read_only_review\" | \"investigation\" | \"test_plan\" | \"implementation\" | \"debugging\" | \"review\" | \"summary\" | \"verification\",\n    \
            \"agent_role\": string (one of the registered workers for read-only kinds),\n    \
            \"depends_on\": string[] (titles or ids of tasks this one waits for),\n    \
            \"parallel_group\": string | null,\n    \
            \"files\": string[] (concrete paths or discovery targets; empty for non-coding),\n    \
            \"allowed_tools\": string[],\n    \
            \"verification\": string[] (how to verify this task is done)\n  \
          }}]\n\
        }}\n\n\
        Rules:\n\
        - Every task MUST have a concrete target (file path, data source, module name) and a verification step.\n\
        - Read-only kinds (read_only_review, investigation, test_plan, review, summary) may share a parallel_group.\n\
        - Implementation / verification kinds must NOT claim to parallelize with reads.\n\
        - Do NOT use vague wording: {forbidden}.\n\
        - Split large goals into small, independently verifiable tasks.\n\n\
        The reviewer will check:\n{checklist}",
        forbidden = FORBIDDEN_PHRASES.join(" | "),
    )
}

fn parse_execution_mode(s: Option<&str>) -> ExecutionMode {
    match s.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("sequential") => ExecutionMode::Sequential,
        Some("plan_only") => ExecutionMode::PlanOnly,
        _ => ExecutionMode::Parallel,
    }
}

/// Assign canonical ids to draft tasks and normalize enum fields. Rewrites
/// `depends_on` from titles → ids so the DAG is well-formed regardless of
/// how the model referenced siblings.
fn normalize_tasks(
    drafts: &[PlanTaskDraft],
    warnings: &mut Vec<String>,
    template: &ProfileTemplate,
    domain_profile: DomainProfile,
) -> Result<Vec<PlanTask>, PlanError> {
    if drafts.is_empty() {
        return Err(PlanError::Quality("plan has zero tasks".into()));
    }

    // title → id map for dependency rewriting.
    let mut title_to_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (i, d) in drafts.iter().enumerate() {
        let id = slug_id(i, &d.title);
        title_to_id.insert(d.title.trim().to_lowercase(), id);
    }

    let default_role = template
        .default_worker_roles
        .first()
        .copied()
        .unwrap_or("project_explorer");
    let mut out = Vec::with_capacity(drafts.len());
    for (i, d) in drafts.iter().enumerate() {
        let id = slug_id(i, &d.title);
        let fallback_title = format!("Task {}", i.saturating_add(1));
        let title = sanitize_plan_text(&d.title, &fallback_title, warnings);
        let description = sanitize_plan_text(&d.description, &title, warnings);
        let kind = d
            .kind
            .as_deref()
            .and_then(PlanTaskKind::from_str)
            .unwrap_or(PlanTaskKind::ReadOnlyReview);
        let requested_role = d
            .agent_role
            .clone()
            .unwrap_or_else(|| default_role.to_string());
        let role = if ALL_WORKER_ROLES
            .iter()
            .any(|allowed| allowed == &requested_role.as_str())
        {
            requested_role
        } else {
            warnings.push(format!(
                "task '{}' used unknown worker '{}'; falling back to '{}'",
                d.title, requested_role, default_role
            ));
            default_role.to_string()
        };

        // If the model claimed an implementation/verification task is parallel
        // with reads, downgrade the parallel_group and warn — mutating work
        // must serialize (see plan DAG section).
        let parallel_group = d.parallel_group.clone();
        if !kind.is_read_only() && parallel_group.is_some() {
            warnings.push(format!(
                "task '{}' is a {:?} but declared a parallel_group; serializing it",
                d.title, kind
            ));
        }
        let parallel_group = if kind.is_read_only() {
            parallel_group
        } else {
            None
        };

        let depends_on: Vec<String> = d
            .depends_on
            .iter()
            .map(|dep| {
                title_to_id
                    .get(&dep.trim().to_lowercase())
                    .cloned()
                    .unwrap_or_else(|| dep.clone())
            })
            .collect();

        out.push(PlanTask {
            id,
            title,
            description,
            kind,
            agent_role: role,
            domain_profile,
            depends_on,
            parallel_group,
            files: d.files.clone(),
            allowed_tools: d.allowed_tools.clone(),
            verification: d.verification.clone(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
        });
    }
    Ok(out)
}

fn sanitize_plan_text(raw: &str, fallback: &str, warnings: &mut Vec<String>) -> String {
    let mut text = raw.trim().to_string();
    for bad in FORBIDDEN_PHRASES {
        if text.to_lowercase().contains(&bad.to_lowercase()) {
            warnings.push(format!(
                "removed vague phrase '{}' from plan text '{}'",
                bad, raw
            ));
            text = text.replace(bad, "");
            text = text.replace(&bad.to_ascii_uppercase(), "");
            text = text.replace(&bad.to_ascii_lowercase(), "");
        }
    }
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, ',' | '，' | '.' | '。' | ';' | '；' | ':' | '：')
    });
    if trimmed.is_empty() {
        fallback.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn role_slug(role: &str) -> String {
    role.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn worker_title(worker: &str) -> String {
    match worker {
        "project_explorer" => "Explore project structure and architecture".to_string(),
        "code_reviewer" => "Review code architecture and correctness risks".to_string(),
        "test_planner" => "Map verification and test strategy".to_string(),
        "data_profiler" => "Profile data sources and analysis shape".to_string(),
        "analysis_reviewer" => "Review metrics and analytical assumptions".to_string(),
        "reproducibility_planner" => "Plan reproducibility checks".to_string(),
        "literature_scout" => "Scout relevant literature and sources".to_string(),
        "evidence_reviewer" => "Review evidence quality and claim strength".to_string(),
        "synthesis_planner" => "Plan research synthesis structure".to_string(),
        "medical_literature_scout" => "Scout medical literature and guidelines".to_string(),
        "clinical_evidence_reviewer" => "Review clinical evidence and applicability".to_string(),
        "safety_reviewer" => "Review safety boundaries and risk notes".to_string(),
        "summary_writer" => "Synthesize worker findings".to_string(),
        _ => format!("Run read-only worker {}", worker),
    }
}

fn worker_description(worker: &str) -> String {
    WORKER_CAPABILITY_CATALOG
        .iter()
        .find(|(role, _)| role == &worker)
        .map(|(_, capability)| format!("Use {} capability: {}.", worker, capability))
        .unwrap_or_else(|| format!("Use {} for read-only investigation.", worker))
}

fn validate_plan(
    goal: &str,
    tasks: &[PlanTask],
    warnings: &mut Vec<String>,
) -> Result<(), PlanError> {
    if goal.trim().is_empty() {
        return Err(PlanError::Quality("plan goal is empty".into()));
    }

    let mut errors: Vec<String> = Vec::new();
    for t in tasks {
        // Vague wording is sanitized in normalize_tasks and surfaced as a
        // warning. Do not reject the whole runtime path for a removable phrase
        // such as "etc."; reserve hard errors for structural plan defects.
        // Implementation / verification tasks must list concrete files or a
        // concrete verification step — otherwise they are not actionable.
        if matches!(
            t.kind,
            PlanTaskKind::Implementation | PlanTaskKind::Verification
        ) && t.files.is_empty()
            && t.verification.is_empty()
        {
            errors.push(format!(
                "task '{}' is a {:?} but lists no files and no verification",
                t.title, t.kind
            ));
        }
        // Titles must be non-trivial.
        if t.title.trim().len() < 3 {
            errors.push(format!("task title too short: '{}'", t.title));
        }
    }

    // Dependency integrity: every depends_on must reference a task that exists
    // in the plan. A dangling reference produces a task that can never become
    // ready → the DAG stalls and the user sees a misleading "cycle or blocked"
    // error in run_dag.
    let ids: std::collections::HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        for dep in &t.depends_on {
            if !ids.contains(dep.as_str()) {
                errors.push(format!(
                    "task '{}' depends on '{}' which does not exist in the plan",
                    t.title, dep
                ));
            }
        }
    }

    // Cycle detection via DFS. A cycle makes the DAG unschedulable.
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
                return true; // cycle found
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
                errors.push(format!(
                    "plan contains a dependency cycle involving task '{}'",
                    t.title
                ));
                break;
            }
        }
    }

    // Drain soft warnings for non-blocking issues.
    if !warnings.is_empty() {
        // already populated by normalize_tasks; nothing to do
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(PlanError::Quality(format!(
            "{} quality issue(s): {}",
            errors.len(),
            errors.join("; ")
        )))
    }
}

fn slug_id(index: usize, title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else if c.is_whitespace() || c == '_' || c == '/' || c == '-' {
                '-'
            } else {
                '-'
            }
        })
        .collect();
    let slug: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.chars().count() > 32 {
        slug.chars().take(32).collect()
    } else {
        slug
    };
    // Always append index to guarantee uniqueness — two titles that normalise
    // to the same slug (e.g. "A/B" and "A B" → "a-b") would otherwise collide
    // on the PRIMARY KEY in tr_plan_tasks.
    if slug.is_empty() {
        format!("task-{index}")
    } else {
        format!("{slug}-{index}")
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::super::classify::ComplexityLabel;
    use super::*;

    fn draft(title: &str, kind: &str, files: &[&str], verification: &[&str]) -> PlanTaskDraft {
        PlanTaskDraft {
            title: title.into(),
            description: format!("desc for {title}"),
            kind: Some(kind.into()),
            agent_role: Some("code_reviewer".into()),
            depends_on: vec![],
            parallel_group: None,
            files: files.iter().map(|s| s.to_string()).collect(),
            allowed_tools: vec![],
            verification: verification.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn normalize_assigns_ids_and_rewrites_deps_by_title() {
        let drafts = vec![
            draft("Review runtime", "read_only_review", &["a.rs"], &["report"]),
            PlanTaskDraft {
                title: "Implement fix".into(),
                description: "fix it".into(),
                kind: Some("implementation".into()),
                agent_role: Some("implementer".into()),
                depends_on: vec!["Review runtime".into()],
                parallel_group: Some("g1".into()),
                files: vec!["b.rs".into()],
                allowed_tools: vec![],
                verification: vec!["cargo check".into()],
            },
        ];
        let template = ProfileTemplate::for_profile(DomainProfile::AiCoding);
        let mut warnings = Vec::new();
        let tasks =
            normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        assert_eq!(tasks.len(), 2);
        // slug_id always appends the index to guarantee uniqueness
        // (see slug_id): "Review runtime" at index 0 → "review-runtime-0".
        assert_eq!(tasks[0].id, "review-runtime-0");
        // Implementation task's depends_on rewritten from title → id.
        assert_eq!(tasks[1].depends_on, vec!["review-runtime-0".to_string()]);
        // Implementation task's parallel_group stripped (mutating work serializes).
        assert!(tasks[1].parallel_group.is_none());
        assert!(warnings.iter().any(|w| w.contains("serializing it")));
    }

    #[test]
    fn normalize_sanitizes_vague_phrasing() {
        let template = ProfileTemplate::for_profile(DomainProfile::General);
        let drafts = vec![PlanTaskDraft {
            title: "Project Structure Discovery etc.".into(),
            description: "处理边界情况，后续补测试".into(),
            kind: Some("implementation".into()),
            agent_role: Some("implementer".into()),
            depends_on: vec![],
            parallel_group: None,
            files: vec!["x.rs".into()],
            allowed_tools: vec![],
            verification: vec!["cargo check".into()],
        }];
        let mut warnings = Vec::new();
        let tasks =
            normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        let task = tasks.first();
        assert!(task.is_some());
        if let Some(task) = task {
            assert_eq!(task.title, "Project Structure Discovery");
            assert!(!task.description.contains("处理边界情况"));
            assert!(!task.description.contains("后续补测试"));
        }
        assert!(
            warnings.iter().any(|w| w.contains("removed vague phrase")),
            "{warnings:?}"
        );
        validate_plan("g", &tasks, &mut warnings).unwrap();
    }

    #[test]
    fn validation_rejects_implementation_without_files_or_verification() {
        let template = ProfileTemplate::for_profile(DomainProfile::AiCoding);
        let drafts = vec![PlanTaskDraft {
            title: "Do the thing".into(),
            description: "make it work".into(),
            kind: Some("implementation".into()),
            agent_role: Some("implementer".into()),
            depends_on: vec![],
            parallel_group: None,
            files: vec![],
            allowed_tools: vec![],
            verification: vec![],
        }];
        let mut warnings = Vec::new();
        let tasks =
            normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        let err = validate_plan("g", &tasks, &mut warnings).unwrap_err();
        assert!(matches!(err, PlanError::Quality(_)));
    }

    #[test]
    fn validation_accepts_concrete_plan() {
        let template = ProfileTemplate::for_profile(DomainProfile::AiCoding);
        let drafts = vec![
            draft(
                "Review chat.rs",
                "read_only_review",
                &["chat.rs"],
                &["report root cause"],
            ),
            draft(
                "Apply fix",
                "implementation",
                &["chat.rs"],
                &["cargo check"],
            ),
        ];
        let mut warnings = Vec::new();
        let tasks =
            normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        validate_plan("Build real runtime", &tasks, &mut warnings).unwrap();
    }

    #[test]
    fn deterministic_readonly_plan_builds_parallel_fanout_without_llm() {
        let classification = Classification {
            complexity: ComplexityLabel::Complex,
            inferred_profile: DomainProfile::AiCoding,
            reason: "test".to_string(),
            signals: vec!["analysis".to_string()],
        };
        let generated = generate_parallel_readonly_plan(
            "run-1",
            "请分析这个项目架构",
            &classification,
            &[
                "project_explorer".to_string(),
                "code_reviewer".to_string(),
                "test_planner".to_string(),
                "summary_writer".to_string(),
            ],
        );

        assert_eq!(generated.plan.run_id, "run-1");
        assert!(matches!(
            generated.plan.execution_mode,
            ExecutionMode::Parallel
        ));
        assert_eq!(generated.plan.tasks.len(), 4);
        let fanout = generated
            .plan
            .tasks
            .iter()
            .filter(|task| task.parallel_group.as_deref() == Some("readonly-fanout"))
            .collect::<Vec<_>>();
        assert_eq!(fanout.len(), 3);
        assert!(fanout.iter().all(|task| task.depends_on.is_empty()));

        let summary = generated
            .plan
            .tasks
            .iter()
            .find(|task| task.agent_role == "summary_writer");
        assert!(summary.is_some());
        if let Some(summary) = summary {
            assert_eq!(summary.depends_on.len(), fanout.len());
        }
        assert!(
            generated
                .plan
                .tasks
                .iter()
                .all(|task| task.kind.is_read_only())
        );
        assert!(generated.warnings.is_empty());
    }

    #[test]
    fn normalize_allows_cross_domain_worker_roles() {
        let template = ProfileTemplate::for_profile(DomainProfile::AcademicResearch);
        let drafts = vec![PlanTaskDraft {
            title: "Profile paper datasets".into(),
            description: "Inspect dataset schema and metric definitions from selected papers"
                .into(),
            kind: Some("read_only_review".into()),
            agent_role: Some("data_profiler".into()),
            depends_on: vec![],
            parallel_group: Some("evidence".into()),
            files: vec!["paper datasets".into()],
            allowed_tools: vec![],
            verification: vec!["dataset profile findings".into()],
        }];
        let mut warnings = Vec::new();
        let tasks = normalize_tasks(
            &drafts,
            &mut warnings,
            template,
            DomainProfile::AcademicResearch,
        )
        .unwrap();
        assert_eq!(tasks[0].agent_role, "data_profiler");
        assert!(
            warnings.is_empty(),
            "cross-domain workers should not be downgraded: {warnings:?}"
        );
    }

    #[test]
    fn execution_mode_parses_leniently() {
        assert!(matches!(
            parse_execution_mode(Some("sequential")),
            ExecutionMode::Sequential
        ));
        assert!(matches!(
            parse_execution_mode(Some("PARALLEL")),
            ExecutionMode::Parallel
        ));
        assert!(matches!(
            parse_execution_mode(Some("plan_only")),
            ExecutionMode::PlanOnly
        ));
        assert!(matches!(
            parse_execution_mode(None),
            ExecutionMode::Parallel
        ));
        assert!(matches!(
            parse_execution_mode(Some("garbage")),
            ExecutionMode::Parallel
        ));
    }

    #[test]
    fn slug_id_handles_unicode_and_collapses_separators() {
        assert_eq!(
            slug_id(0, "Review HITL approval chain"),
            "review-hitl-approval-chain-0"
        );
        assert_eq!(slug_id(3, ""), "task-3");
        // Unicode becomes separators, then collapsed.
        let s = slug_id(0, "审查 GUI 主运行时");
        assert!(!s.is_empty());
    }
}
