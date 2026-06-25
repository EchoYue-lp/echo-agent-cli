//! Domain profile templates.
//!
//! Each profile customizes the universal task methodology (plan templates,
//! seed worker roles, plan-prompt suffix, review checklist) for a domain.
//! The profile does NOT replace the methodology — it specializes it.
//!
//! See the plan's "Domain Profiles" section. These templates are consumed by
//! the planner (`planner.rs`) when building the LLM prompt that generates a
//! structured `TaskPlan`.

use super::types::DomainProfile;

/// Static template for a domain profile.
pub struct ProfileTemplate {
    /// Stable identifier matching the [`DomainProfile`] variant.
    pub key: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Seed worker roles for this domain. These are defaults, not hard
    /// boundaries; real tasks may blend workers from every capability area.
    pub default_worker_roles: &'static [&'static str],
    /// Extra instructions appended to the plan-generation prompt to steer the
    /// LLM toward domain-appropriate tasks, artifacts, and verification.
    pub prompt_suffix: &'static str,
    /// Review checklist items surfaced to the reviewer gate (PR 4) and to the
    /// user in the plan-approval UI.
    pub review_checklist: &'static [&'static str],
    /// Universal Superpowers-style methodology workflows enabled for this
    /// profile. The planner prompt references these so the LLM structures
    /// plans accordingly (plan §46-99).
    pub workflows: &'static [&'static str],
}

/// Universal workflows available to every profile (plan §67-87).
pub const UNIVERSAL_WORKFLOWS: &[&str] = &[
    "brainstorming",
    "writing-plans",
    "executing-plans",
    "dispatching-parallel-agents",
    "subagent-driven-work",
    "systematic-debugging",
    "quality-review",
    "verification-before-completion",
    "finishing-a-development-branch",
];

/// Coding-specific workflows enabled only for the AI Coding profile.
pub const CODING_WORKFLOWS: &[&str] = &[
    "test-driven-development",
    "using-git-worktrees",
    "finishing-a-development-branch",
];

impl ProfileTemplate {
    /// Look up the template for a profile. Always succeeds — `General` is the
    /// universal fallback.
    pub fn for_profile(profile: DomainProfile) -> &'static ProfileTemplate {
        match profile {
            DomainProfile::General => &GENERAL,
            DomainProfile::AiCoding => &AI_CODING,
            DomainProfile::DataAnalysis => &DATA_ANALYSIS,
            DomainProfile::AcademicResearch => &ACADEMIC_RESEARCH,
            DomainProfile::MedicalResearch => &MEDICAL_RESEARCH,
        }
    }
}

pub const AI_CODING_WORKER_ROLES: &[&str] = &[
    "project_explorer",
    "code_reviewer",
    "test_planner",
    "summary_writer",
];
pub const DATA_ANALYSIS_WORKER_ROLES: &[&str] = &[
    "data_profiler",
    "analysis_reviewer",
    "reproducibility_planner",
    "summary_writer",
];
pub const ACADEMIC_RESEARCH_WORKER_ROLES: &[&str] = &[
    "literature_scout",
    "evidence_reviewer",
    "synthesis_planner",
    "summary_writer",
];
pub const MEDICAL_RESEARCH_WORKER_ROLES: &[&str] = &[
    "medical_literature_scout",
    "clinical_evidence_reviewer",
    "safety_reviewer",
    "summary_writer",
];
pub const GENERAL_WORKER_ROLES: &[&str] = &[
    "project_explorer",
    "literature_scout",
    "data_profiler",
    "summary_writer",
];

pub const ALL_WORKER_ROLES: &[&str] = &[
    "project_explorer",
    "code_reviewer",
    "test_planner",
    "data_profiler",
    "analysis_reviewer",
    "reproducibility_planner",
    "literature_scout",
    "evidence_reviewer",
    "synthesis_planner",
    "medical_literature_scout",
    "clinical_evidence_reviewer",
    "safety_reviewer",
    "summary_writer",
];

pub const WORKER_CAPABILITY_CATALOG: &[(&str, &str)] = &[
    (
        "project_explorer",
        "workspace/codebase discovery: files, configs, modules, docs",
    ),
    (
        "code_reviewer",
        "software correctness, architecture, duplication, edge cases",
    ),
    (
        "test_planner",
        "verification strategy for code, scripts, notebooks, builds",
    ),
    (
        "data_profiler",
        "data sources, schema, missing values, outliers, sample boundaries",
    ),
    (
        "analysis_reviewer",
        "metrics, statistical assumptions, plots, data-supported conclusions",
    ),
    (
        "reproducibility_planner",
        "rerunnable pipelines, notebooks, scripts, reports, audit trail",
    ),
    (
        "literature_scout",
        "academic search strategy, candidate papers, keywords, evidence gaps",
    ),
    (
        "evidence_reviewer",
        "evidence quality, study type, citation reliability, claim strength",
    ),
    (
        "synthesis_planner",
        "review structure, evidence tables, bibliography, final research artifact",
    ),
    (
        "medical_literature_scout",
        "medical guidelines, systematic reviews, clinical studies, PubMed-style search",
    ),
    (
        "clinical_evidence_reviewer",
        "clinical evidence level, applicability, guideline consistency",
    ),
    (
        "safety_reviewer",
        "medical safety boundaries, disclaimers, contraindication risk",
    ),
    (
        "summary_writer",
        "cross-worker synthesis into conclusions, plan, or delivery notes",
    ),
];

pub fn worker_catalog_prompt() -> String {
    WORKER_CAPABILITY_CATALOG
        .iter()
        .map(|(role, capability)| format!("- {role}: {capability}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub static GENERAL: ProfileTemplate = ProfileTemplate {
    key: "general",
    label: "General",
    default_worker_roles: GENERAL_WORKER_ROLES,
    prompt_suffix: "\
Use the universal task methodology: clarify the goal, split work into small \
verifiable tasks, mark read-only investigation tasks as parallelizable, and \
keep mutating/serial work separate. State assumptions and risks explicitly. \
Prefer concrete file paths, data sources, or discovery targets over vague \
phrasing like 'continue improving' or 'handle edge cases'.",
    workflows: UNIVERSAL_WORKFLOWS,
    review_checklist: &[
        "Is the goal understood and restated?",
        "Is the plan concrete (specific targets, not vague)?",
        "Are dependencies and parallel groups clear?",
        "Is the evidence sufficient for the conclusion?",
        "Is the final answer grounded in completed work?",
        "Are uncertainties and risks stated?",
    ],
};

pub static AI_CODING: ProfileTemplate = ProfileTemplate {
    key: "ai_coding",
    label: "AI Coding",
    default_worker_roles: AI_CODING_WORKER_ROLES,
    prompt_suffix: "\
This is a software-engineering workspace. Split work into read-only review / \
investigation tasks (parallelizable) and implementation tasks (serialized). \
Every implementation task must list concrete file paths and a verification \
step (cargo check / npm build / relevant tests). Read-only work is delegated \
to registered capability workers; implementation/debugging tasks are NOT \
delegated to a worker — the main agent performs writes directly, serially and \
approval-gated. So only assign read-only kinds (read_only_review, \
investigation, test_plan, review, summary) a worker role from the registered \
set. Coding tasks can still use data, research, or medical workers when the \
actual goal crosses those boundaries.",
    workflows: CODING_WORKFLOWS,
    review_checklist: &[
        "Architecture fit with existing code?",
        "File changes match the task scope (no drive-by edits)?",
        "Any duplicated logic introduced?",
        "Behavior regressions or removed edge-case handling?",
        "Concurrency / shared-state risks?",
        "Tests or checks run / specified?",
        "PR / commit readiness if requested?",
    ],
};

pub static DATA_ANALYSIS: ProfileTemplate = ProfileTemplate {
    key: "data_analysis",
    label: "Data Analysis",
    default_worker_roles: DATA_ANALYSIS_WORKER_ROLES,
    prompt_suffix: "\
This is a data-analysis task. Make data provenance explicit. Split work into \
profiling, cleaning, analysis, and reproducibility checks. Every \
transformation must be reproducible from a script or notebook artifact. \
Flag missing values, outliers, and metric-definition inconsistencies. Avoid \
conclusions overfit to a convenient subset of the data. Use coding workers \
when the analysis depends on scripts/notebooks/packages, and research or \
medical workers when the data question depends on external evidence.",
    workflows: UNIVERSAL_WORKFLOWS,
    review_checklist: &[
        "Data source and provenance are clear?",
        "Missing values and outliers inspected?",
        "Metric definitions consistent across steps?",
        "Transformations reproducible from artifacts?",
        "Analysis not overfit to a convenient conclusion?",
        "Charts not misleading?",
        "Notebook / pipeline rerunnable?",
    ],
};

pub static ACADEMIC_RESEARCH: ProfileTemplate = ProfileTemplate {
    key: "academic_research",
    label: "Academic Research",
    default_worker_roles: ACADEMIC_RESEARCH_WORKER_ROLES,
    prompt_suffix: "\
This is an academic-research task. Make the search strategy explicit. Every \
claim must cite a real, verifiable source. Distinguish study types and state \
evidence level where possible. Include disagreements and limitations. Do not \
let claims exceed the strength of the underlying evidence. Output an evidence \
table and a bibliography artifact. Use data-analysis workers when papers, \
datasets, statistics, or plots need scrutiny; use coding workers when the \
research includes reproducible code, tools, notebooks, or implementation.",
    workflows: UNIVERSAL_WORKFLOWS,
    review_checklist: &[
        "Search strategy explicit and reproducible?",
        "Papers and citations real and verifiable?",
        "Evidence supports the conclusion?",
        "Study type / evidence level stated?",
        "Disagreements and limitations included?",
        "Claims not stronger than the evidence?",
        "Bibliography complete enough for the task?",
    ],
};

pub static MEDICAL_RESEARCH: ProfileTemplate = ProfileTemplate {
    key: "medical_research",
    label: "Medical Research",
    default_worker_roles: MEDICAL_RESEARCH_WORKER_ROLES,
    prompt_suffix: "\
This is a medical-research task with strict safety and evidence boundaries. \
Prioritize authoritative sources (guidelines, systematic reviews). \
Distinguish guideline / systematic-review / trial types and state evidence \
level. Make uncertainty explicit. NEVER present a diagnosis or treatment as \
medical advice — include a non-diagnostic disclaimer where appropriate. \
Every clinical statement must be directly supported by a reliable citation. \
Use data-analysis workers for cohorts, datasets, statistics, or biomedical \
tables, and coding workers for notebooks, scripts, pipelines, or tool review.",
    workflows: UNIVERSAL_WORKFLOWS,
    review_checklist: &[
        "Authoritative sources prioritized?",
        "Guideline / systematic-review / trial type distinguished?",
        "Evidence level stated where possible?",
        "Uncertainty explicit?",
        "No diagnosis or treatment presented as medical advice?",
        "Clinical safety boundaries visible?",
        "Citations reliable and directly support each statement?",
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_has_a_template() {
        for p in [
            DomainProfile::General,
            DomainProfile::AiCoding,
            DomainProfile::DataAnalysis,
            DomainProfile::AcademicResearch,
            DomainProfile::MedicalResearch,
        ] {
            let t = ProfileTemplate::for_profile(p);
            assert_eq!(t.key, p.as_str());
            assert!(!t.default_worker_roles.is_empty());
            assert!(!t.prompt_suffix.is_empty());
            assert!(!t.review_checklist.is_empty());
        }
    }

    #[test]
    fn every_profile_uses_registered_subagent_roles() {
        for profile in [
            DomainProfile::General,
            DomainProfile::AiCoding,
            DomainProfile::DataAnalysis,
            DomainProfile::AcademicResearch,
            DomainProfile::MedicalResearch,
        ] {
            let t = ProfileTemplate::for_profile(profile);
            for required in t.default_worker_roles {
                assert!(
                    ALL_WORKER_ROLES.iter().any(|r| r == required),
                    "{profile:?} profile references unregistered worker {required}"
                );
            }
            assert!(
                t.default_worker_roles.contains(&"summary_writer"),
                "{profile:?} profile should keep summary_writer for synthesis"
            );
        }
    }

    #[test]
    fn catalog_lists_every_registered_worker_once() {
        for role in ALL_WORKER_ROLES {
            let matches = WORKER_CAPABILITY_CATALOG
                .iter()
                .filter(|(catalog_role, _)| catalog_role == role)
                .count();
            assert_eq!(matches, 1, "worker {role} must be described exactly once");
        }
    }
}
