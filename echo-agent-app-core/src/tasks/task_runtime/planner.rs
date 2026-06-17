//! Structured plan generator.
//!
//! Turns a complex user goal into a structured [`TaskPlan`] via a single
//! JSON-mode LLM call, then validates plan quality (rejects vague wording,
//! ensures every task has concrete targets + verification).
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
use super::profiles::ProfileTemplate;
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

/// Generate a structured plan for a run.
///
/// `run_id` becomes the plan's `run_id`; `plan_id` is minted here. The
/// classification (profile + reason) steers the prompt.
pub async fn generate_plan(
    llm: &Arc<dyn LlmClient>,
    run_id: &str,
    goal: &str,
    classification: &Classification,
) -> Result<GeneratedPlan, PlanError> {
    let profile = classification.inferred_profile;
    let template = ProfileTemplate::for_profile(profile);
    let prompt = build_prompt(goal, template);

    // JSON-mode request. We use JsonObject rather than JsonSchema so we work
    // across all providers (schema-enforced mode is opt-in per provider; see
    // ProviderCapabilities). Validation is done on our side instead.
    let request = ChatRequest {
        messages: vec![Message::system(system_preamble(template)), Message::user(prompt)],
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
        Workers available for read-only roles: {workers}.",
        label = template.label,
        suffix = template.prompt_suffix,
        workers = template.default_worker_roles.join(", "),
    )
}

fn build_prompt(goal: &str, template: &ProfileTemplate) -> String {
    let checklist = template
        .review_checklist
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Goal:\n{goal}\n\n\
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
            \"agent_role\": string (one of the available workers for read-only kinds),\n    \
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
    let mut title_to_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (i, d) in drafts.iter().enumerate() {
        let id = slug_id(i, &d.title);
        title_to_id.insert(d.title.trim().to_lowercase(), id);
    }

    let default_role = template.default_worker_roles.first().copied().unwrap_or("general");
    let mut out = Vec::with_capacity(drafts.len());
    for (i, d) in drafts.iter().enumerate() {
        let id = slug_id(i, &d.title);
        let kind = d
            .kind
            .as_deref()
            .and_then(PlanTaskKind::from_str)
            .unwrap_or(PlanTaskKind::ReadOnlyReview);
        let role = d.agent_role.clone().unwrap_or_else(|| default_role.to_string());

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
        let parallel_group = if kind.is_read_only() { parallel_group } else { None };

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
            title: d.title.trim().to_string(),
            description: d.description.trim().to_string(),
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

fn validate_plan(goal: &str, tasks: &[PlanTask], warnings: &mut Vec<String>) -> Result<(), PlanError> {
    if goal.trim().is_empty() {
        return Err(PlanError::Quality("plan goal is empty".into()));
    }

    let mut errors: Vec<String> = Vec::new();
    for t in tasks {
        // Forbidden vague wording anywhere in title/description.
        let hay = format!("{} {}", t.title, t.description).to_lowercase();
        for bad in FORBIDDEN_PHRASES {
            if hay.contains(&bad.to_lowercase()) {
                errors.push(format!("task '{}' uses forbidden phrase '{bad}'", t.title));
            }
        }
        // Implementation / verification tasks must list concrete files or a
        // concrete verification step — otherwise they are not actionable.
        if matches!(t.kind, PlanTaskKind::Implementation | PlanTaskKind::Verification)
            && t.files.is_empty()
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
    let slug: String = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    let slug = if slug.len() > 32 { slug[..32].to_string() } else { slug };
    if slug.is_empty() {
        format!("task-{index}")
    } else {
        slug
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
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
        let tasks = normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "review-runtime");
        // Implementation task's depends_on rewritten from title → id.
        assert_eq!(tasks[1].depends_on, vec!["review-runtime".to_string()]);
        // Implementation task's parallel_group stripped (mutating work serializes).
        assert!(tasks[1].parallel_group.is_none());
        assert!(warnings.iter().any(|w| w.contains("serializing it")));
    }

    #[test]
    fn validation_rejects_vague_phrasing() {
        let template = ProfileTemplate::for_profile(DomainProfile::General);
        let drafts = vec![PlanTaskDraft {
            title: "继续完善相关代码".into(),
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
        let tasks = normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        let err = validate_plan("g", &tasks, &mut warnings).unwrap_err();
        match err {
            PlanError::Quality(msg) => {
                assert!(msg.contains("继续完善"), "{msg}");
                assert!(msg.contains("处理边界情况"), "{msg}");
                assert!(msg.contains("后续补测试"), "{msg}");
            }
            _ => panic!("expected Quality error, got {err:?}"),
        }
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
        let tasks = normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        let err = validate_plan("g", &tasks, &mut warnings).unwrap_err();
        assert!(matches!(err, PlanError::Quality(_)));
    }

    #[test]
    fn validation_accepts_concrete_plan() {
        let template = ProfileTemplate::for_profile(DomainProfile::AiCoding);
        let drafts = vec![
            draft("Review chat.rs", "read_only_review", &["chat.rs"], &["report root cause"]),
            draft("Apply fix", "implementation", &["chat.rs"], &["cargo check"]),
        ];
        let mut warnings = Vec::new();
        let tasks = normalize_tasks(&drafts, &mut warnings, template, DomainProfile::AiCoding).unwrap();
        validate_plan("Build real runtime", &tasks, &mut warnings).unwrap();
    }

    #[test]
    fn execution_mode_parses_leniently() {
        assert!(matches!(parse_execution_mode(Some("sequential")), ExecutionMode::Sequential));
        assert!(matches!(parse_execution_mode(Some("PARALLEL")), ExecutionMode::Parallel));
        assert!(matches!(parse_execution_mode(Some("plan_only")), ExecutionMode::PlanOnly));
        assert!(matches!(parse_execution_mode(None), ExecutionMode::Parallel));
        assert!(matches!(parse_execution_mode(Some("garbage")), ExecutionMode::Parallel));
    }

    #[test]
    fn slug_id_handles_unicode_and_collapses_separators() {
        assert_eq!(slug_id(0, "Review HITL approval chain"), "review-hitl-approval-chain");
        assert_eq!(slug_id(3, ""), "task-3");
        // Unicode becomes separators, then collapsed.
        let s = slug_id(0, "审查 GUI 主运行时");
        assert!(!s.is_empty());
    }
}
