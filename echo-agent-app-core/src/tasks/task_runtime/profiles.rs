//! Domain profile templates.
//!
//! Each profile customizes the universal task methodology (plan templates,
//! seed subagent roles, plan-prompt suffix, review checklist) for a domain.
//! The profile does NOT replace the methodology — it specializes it.
//!
//! See the plan's "Domain Profiles" section. These templates are consumed by
//! the planner (`planner.rs`) when building the LLM prompt that generates a
//! structured `TaskPlan`.

use super::types::{DomainProfile, PlanTaskKind};
use crate::subagent_loader::SubagentCatalogSnapshot;

const DOMAIN_PROFILES: &[DomainProfile] = &[
    DomainProfile::General,
    DomainProfile::AiCoding,
    DomainProfile::DataAnalysis,
    DomainProfile::AcademicResearch,
    DomainProfile::MedicalResearch,
];

const PLAN_TASK_KINDS: &[PlanTaskKind] = &[
    PlanTaskKind::ReadOnlyReview,
    PlanTaskKind::Investigation,
    PlanTaskKind::TestPlan,
    PlanTaskKind::Implementation,
    PlanTaskKind::Debugging,
    PlanTaskKind::Review,
    PlanTaskKind::Summary,
    PlanTaskKind::Verification,
];

/// Static template for a domain profile.
pub struct ProfileTemplate {
    /// Stable identifier matching the [`DomainProfile`] variant.
    pub key: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Seed subagent roles for this domain. These are defaults, not hard
    /// boundaries; real tasks may blend subagents from every capability area.
    pub default_subagent_roles: &'static [&'static str],
    /// Extra instructions appended to the plan-generation prompt to steer the
    /// LLM toward domain-appropriate tasks, artifacts, and verification.
    pub prompt_suffix: &'static str,
    /// Domain-specific guidance injected into each PlanTask's Subagent prompt.
    pub execution_guidance: &'static str,
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

pub const AI_CODING_SUBAGENT_ROLES: &[&str] = &[
    "explorer",
    "reviewer",
    "planner",
    "summarizer",
    "implementer",
    "general-purpose",
];
pub const DATA_ANALYSIS_SUBAGENT_ROLES: &[&str] = &[
    "explorer",
    "reviewer",
    "planner",
    "summarizer",
    "data-shaper",
    "analyst",
];
pub const ACADEMIC_RESEARCH_SUBAGENT_ROLES: &[&str] =
    &["explorer", "reviewer", "planner", "summarizer"];
pub const MEDICAL_RESEARCH_SUBAGENT_ROLES: &[&str] =
    &["explorer", "reviewer", "planner", "summarizer"];
pub const GENERAL_SUBAGENT_ROLES: &[&str] = &[
    "explorer",
    "reviewer",
    "planner",
    "summarizer",
    "implementer",
    "general-purpose",
];

/// Resolve the default Subagent for a PlanTask. An explicit `subagent`
/// parameter may select any registered project, user, or builtin role.
pub fn default_subagent_for(profile: DomainProfile, kind: PlanTaskKind) -> &'static str {
    match kind {
        PlanTaskKind::ReadOnlyReview | PlanTaskKind::Investigation | PlanTaskKind::TestPlan => {
            "explorer"
        }
        PlanTaskKind::Review => "reviewer",
        PlanTaskKind::Summary => "summarizer",
        PlanTaskKind::Implementation | PlanTaskKind::Debugging => {
            if profile == DomainProfile::DataAnalysis {
                "analyst"
            } else {
                "implementer"
            }
        }
        PlanTaskKind::Verification => "primary",
    }
}

/// Validate every built-in routing target against the effective catalog.
pub fn validate_default_subagent_routes(catalog: &SubagentCatalogSnapshot) -> Result<(), String> {
    for profile in DOMAIN_PROFILES {
        for kind in PLAN_TASK_KINDS {
            let role = default_subagent_for(*profile, *kind);
            if role != "primary" && !catalog.contains(role) {
                return Err(format!(
                    "default Subagent route {profile:?}/{kind:?} references unregistered role {role}"
                ));
            }
        }
    }
    Ok(())
}

pub static GENERAL: ProfileTemplate = ProfileTemplate {
    key: "general",
    label: "General",
    default_subagent_roles: GENERAL_SUBAGENT_ROLES,
    prompt_suffix: "\
Build the smallest plan that fully satisfies the user's outcome. Each task must \
produce a decision, evidence set, change, artifact, or verification result. Use \
dependencies only for real information or mutation ordering; independent work \
should be parallelizable. Assign a role by capability, name concrete targets, \
and state what evidence will prove the task complete. Keep assumptions, external \
side effects, and unresolved risks explicit.",
    execution_guidance: "Apply the evidence, artifact, and verification standard stated by the task. Keep observed facts, assumptions, and unresolved work distinct.",
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
    default_subagent_roles: AI_CODING_SUBAGENT_ROLES,
    prompt_suffix: "\
This is a software-engineering workspace. Plan from repository evidence, not \
from an imagined greenfield design. Separate discovery, implementation, and \
verification outcomes. Read-only work can run in parallel; writer tasks should \
declare owned files and use a writer-capable role so the runtime can isolate \
their changes. Every behavior-changing task needs a concrete verification path \
(targeted tests plus the repository-required build/type/format checks). Include \
dirty-worktree preservation, state ownership, failure handling, and rollback \
considerations when relevant. Cross-domain tasks may use data or research roles.",
    execution_guidance: "Work from the real repository and its local instructions. Preserve unrelated changes, keep edits scoped, and report exact build, test, type, and format evidence actually observed.",
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
    default_subagent_roles: DATA_ANALYSIS_SUBAGENT_ROLES,
    prompt_suffix: "\
This is a data-analysis task. Make the analytical question and decision metric \
explicit. Plan provenance/schema profiling before transformations, and plan \
transformations before inference. Preserve raw inputs; require reproducible \
artifacts, row-count/quality checks, assumption tests, uncertainty, and at least \
one sensitivity or reconciliation check for material conclusions. Separate \
descriptive, predictive, and causal claims. Add research or coding tasks when \
external evidence, scripts, notebooks, or packages are necessary. Treat \
`exploratory_statistics` as descriptive only; formal inference must use a \
persisted SciPy/statsmodels/R script executed through `run_code.script_path`. \
Durable user-facing analyses live under `analysis/<analysis-id>/` with a \
versioned manifest whose id matches the directory; do not make \
an in-memory-only notebook the source of truth.",
    execution_guidance: "Preserve raw inputs and provenance. Make transformations reproducible, state metric definitions and assumptions, and validate material results with reconciliation or sensitivity evidence. For formal inference, persist the exact Python/R script and record input hashes, package versions, seeds, missing-data handling, diagnostics, warnings, and result artifacts; do not hand-write statistical distributions or p-value approximations.",
    workflows: UNIVERSAL_WORKFLOWS,
    review_checklist: &[
        "Data source and provenance are clear?",
        "Missing values and outliers inspected?",
        "Metric definitions consistent across steps?",
        "Transformations reproducible from artifacts?",
        "Analysis not overfit to a convenient conclusion?",
        "Formal inference uses a persisted mature-library script rather than an exploratory tool or hand-written approximation?",
        "Charts not misleading?",
        "Script / notebook / pipeline rerunnable with input hash, package versions, seed, and result artifacts?",
        "The executed script is the same persisted file the user reviewed?",
    ],
};

pub static ACADEMIC_RESEARCH: ProfileTemplate = ProfileTemplate {
    key: "academic_research",
    label: "Academic Research",
    default_subagent_roles: ACADEMIC_RESEARCH_SUBAGENT_ROLES,
    prompt_suffix: "\
This is an academic-research task. Plan the question, source strategy, screening \
criteria, extraction fields, critical appraisal, synthesis, and citation audit. \
Central claims need directly supporting, verifiable sources. Distinguish study \
type, peer-review status, methods, population/data, effect or result, and \
limitations. Preserve disagreement and null results. Require a search trail and \
evidence table when the scope is a review; add data or coding tasks for \
statistical, dataset, notebook, or reproducibility work.",
    execution_guidance: "Keep a reproducible source trail. Record study type, methods, population or data, result, limitations, and direct citation support; preserve disagreement, null results, and evidence gaps.",
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
    default_subagent_roles: MEDICAL_RESEARCH_SUBAGENT_ROLES,
    prompt_suffix: "\
This is a medical-research task. Frame the question with PICO/PECO or an \
equivalent structure and prioritize current guidelines, systematic reviews, and \
pivotal studies. Plan direct citation support for material clinical claims, \
evidence-quality and applicability assessment, harms/contraindications, \
conflicting guidance, and uncertainty. Do not convert population evidence into \
individual diagnosis or treatment. Add data-analysis or coding tasks for \
cohorts, statistics, tables, notebooks, pipelines, or reproducibility work.",
    execution_guidance: "Frame clinical evidence with PICO, PECO, or an equivalent structure. Prioritize authoritative sources, assess evidence quality, applicability, harms, contraindications, disagreement, and uncertainty, and do not turn population evidence into individual medical advice.",
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
        for profile in DOMAIN_PROFILES {
            let t = ProfileTemplate::for_profile(*profile);
            assert_eq!(t.key, profile.as_str());
            assert!(!t.default_subagent_roles.is_empty());
            assert!(!t.prompt_suffix.is_empty());
            assert!(!t.execution_guidance.is_empty());
            assert!(!t.review_checklist.is_empty());
        }
    }

    #[test]
    fn every_profile_uses_registered_subagent_roles() {
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        let catalog =
            crate::subagent_loader::SubagentCatalogSnapshot::from_definitions(&definitions);
        for profile in DOMAIN_PROFILES {
            let t = ProfileTemplate::for_profile(*profile);
            for required in t.default_subagent_roles {
                assert!(
                    catalog.contains(required),
                    "{profile:?} profile references unregistered subagent {required}"
                );
            }
            assert!(
                t.default_subagent_roles.contains(&"summarizer"),
                "{profile:?} profile should keep summarizer for synthesis"
            );
        }
    }

    #[test]
    fn every_default_route_resolves_in_the_effective_catalog() -> Result<(), String> {
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        let catalog =
            crate::subagent_loader::SubagentCatalogSnapshot::from_definitions(&definitions);
        validate_default_subagent_routes(&catalog)
    }

    #[test]
    fn data_writer_defaults_to_analyst() {
        assert_eq!(
            default_subagent_for(DomainProfile::DataAnalysis, PlanTaskKind::Implementation),
            "analyst"
        );
        assert_eq!(
            default_subagent_for(DomainProfile::AiCoding, PlanTaskKind::Implementation),
            "implementer"
        );
        assert_eq!(
            default_subagent_for(DomainProfile::MedicalResearch, PlanTaskKind::Review),
            "reviewer"
        );
    }

    #[test]
    fn data_profile_separates_exploration_from_formal_inference() {
        let template = ProfileTemplate::for_profile(DomainProfile::DataAnalysis);
        assert!(template.prompt_suffix.contains("exploratory_statistics"));
        assert!(template.prompt_suffix.contains("SciPy/statsmodels/R"));
        assert!(template.execution_guidance.contains("input hashes"));
        assert!(
            template
                .execution_guidance
                .contains("p-value approximations")
        );
        assert!(
            template
                .review_checklist
                .iter()
                .any(|item| item.contains("mature-library script"))
        );
    }
}
