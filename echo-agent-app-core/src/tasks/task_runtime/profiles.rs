//! Domain profile templates.
//!
//! Each profile customizes the universal task methodology (plan templates,
//! seed subagent roles, plan-prompt suffix, review checklist) for a domain.
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
    /// Seed subagent roles for this domain. These are defaults, not hard
    /// boundaries; real tasks may blend subagents from every capability area.
    pub default_worker_roles: &'static [&'static str],
    /// Extra instructions appended to the plan-generation prompt to steer the
    /// LLM toward domain-appropriate tasks, artifacts, and verification.
    pub prompt_suffix: &'static str,
    /// Review checklist items surfaced to the reviewer and plan UI.
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

// SA-4: All domain profiles now use the same 4 generic roles.
// Domain specialization is achieved via skill/profile prompt injection,
// not via separate agent definitions.
pub const AI_CODING_WORKER_ROLES: &[&str] = &["explorer", "reviewer", "planner", "summarizer"];
pub const DATA_ANALYSIS_WORKER_ROLES: &[&str] = &["explorer", "reviewer", "planner", "summarizer"];
pub const ACADEMIC_RESEARCH_WORKER_ROLES: &[&str] =
    &["explorer", "reviewer", "planner", "summarizer"];
pub const MEDICAL_RESEARCH_WORKER_ROLES: &[&str] =
    &["explorer", "reviewer", "planner", "summarizer"];
pub const GENERAL_WORKER_ROLES: &[&str] = &["explorer", "reviewer", "planner", "summarizer"];

pub const ALL_WORKER_ROLES: &[&str] = &["explorer", "reviewer", "planner", "summarizer"];

pub const WORKER_CAPABILITY_CATALOG: &[(&str, &str)] = &[
    (
        "explorer",
        "read-only discovery: codebase structure, data sources, literature, configs, docs",
    ),
    (
        "reviewer",
        "read-only review: code bugs, analysis methods, evidence quality, safety boundaries",
    ),
    (
        "planner",
        "read-only planning: verification strategy, reproducibility paths, review structure",
    ),
    (
        "summarizer",
        "cross-subagent synthesis into conclusions, plan, or delivery notes",
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
Build the smallest plan that fully satisfies the user's outcome. Each task must \
produce a decision, evidence set, change, artifact, or verification result. Use \
dependencies only for real information or mutation ordering; independent work \
should be parallelizable. Assign a role by capability, name concrete targets, \
and state what evidence will prove the task complete. Keep assumptions, external \
side effects, and unresolved risks explicit.",
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
This is a software-engineering workspace. Plan from repository evidence, not \
from an imagined greenfield design. Separate discovery, implementation, and \
verification outcomes. Read-only work can run in parallel; writer tasks should \
declare owned files and use a writer-capable role so the runtime can isolate \
their changes. Every behavior-changing task needs a concrete verification path \
(targeted tests plus the repository-required build/type/format checks). Include \
dirty-worktree preservation, state ownership, failure handling, and rollback \
considerations when relevant. Cross-domain tasks may use data or research roles.",
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
This is a data-analysis task. Make the analytical question and decision metric \
explicit. Plan provenance/schema profiling before transformations, and plan \
transformations before inference. Preserve raw inputs; require reproducible \
artifacts, row-count/quality checks, assumption tests, uncertainty, and at least \
one sensitivity or reconciliation check for material conclusions. Separate \
descriptive, predictive, and causal claims. Add research or coding tasks when \
external evidence, scripts, notebooks, or packages are necessary.",
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
This is an academic-research task. Plan the question, source strategy, screening \
criteria, extraction fields, critical appraisal, synthesis, and citation audit. \
Central claims need directly supporting, verifiable sources. Distinguish study \
type, peer-review status, methods, population/data, effect or result, and \
limitations. Preserve disagreement and null results. Require a search trail and \
evidence table when the scope is a review; add data or coding tasks for \
statistical, dataset, notebook, or reproducibility work.",
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
This is a medical-research task. Frame the question with PICO/PECO or an \
equivalent structure and prioritize current guidelines, systematic reviews, and \
pivotal studies. Plan direct citation support for material clinical claims, \
evidence-quality and applicability assessment, harms/contraindications, \
conflicting guidance, and uncertainty. Do not convert population evidence into \
individual diagnosis or treatment. Add data-analysis or coding tasks for \
cohorts, statistics, tables, notebooks, pipelines, or reproducibility work.",
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
                    "{profile:?} profile references unregistered subagent {required}"
                );
            }
            assert!(
                t.default_worker_roles.contains(&"summarizer"),
                "{profile:?} profile should keep summarizer for synthesis"
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
            assert_eq!(matches, 1, "subagent {role} must be described exactly once");
        }
    }
}
