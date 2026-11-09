//! General host methodology and built-in routing only.
//!
//! Professional methods, evidence rules and role choices come from the
//! immutable domain plugin binding. Historical DomainProfile discriminators
//! are display data and never authorize a new professional execution.

use super::types::PlanTaskKind;
use crate::subagent_loader::SubagentCatalogSnapshot;

pub struct ProfileTemplate {
    pub key: &'static str,
    pub label: &'static str,
    pub default_subagent_roles: &'static [&'static str],
    pub prompt_suffix: &'static str,
    pub execution_guidance: &'static str,
    pub review_checklist: &'static [&'static str],
}

impl ProfileTemplate {
    pub fn general() -> &'static Self {
        &GENERAL
    }
}

pub const GENERAL_SUBAGENT_ROLES: &[&str] = &[
    "explorer",
    "reviewer",
    "planner",
    "summarizer",
    "implementer",
    "general-purpose",
];

pub fn default_subagent_for(kind: PlanTaskKind) -> &'static str {
    match kind {
        PlanTaskKind::ReadOnlyReview | PlanTaskKind::Investigation | PlanTaskKind::TestPlan => {
            "explorer"
        }
        PlanTaskKind::Review => "reviewer",
        PlanTaskKind::Summary => "summarizer",
        PlanTaskKind::Implementation | PlanTaskKind::Debugging => "implementer",
        PlanTaskKind::Verification => "primary",
    }
}

pub fn validate_default_subagent_routes(catalog: &SubagentCatalogSnapshot) -> Result<(), String> {
    for kind in [
        PlanTaskKind::ReadOnlyReview,
        PlanTaskKind::Investigation,
        PlanTaskKind::TestPlan,
        PlanTaskKind::Implementation,
        PlanTaskKind::Debugging,
        PlanTaskKind::Review,
        PlanTaskKind::Summary,
        PlanTaskKind::Verification,
    ] {
        let role = default_subagent_for(kind);
        if role != "primary" && !catalog.contains(role) {
            return Err(format!(
                "General Subagent route {kind:?} references unregistered role {role}"
            ));
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
    review_checklist: &[
        "Is the goal understood and restated?",
        "Is the plan concrete (specific targets, not vague)?",
        "Are dependencies and parallel groups clear?",
        "Is the evidence sufficient for the conclusion?",
        "Is the final answer grounded in completed work?",
        "Are uncertainties and risks stated?",
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn general_host_routes_remain_registered() -> Result<(), String> {
        let definitions = crate::subagent_loader::discover_subagents(None, None);
        let catalog = SubagentCatalogSnapshot::from_definitions(&definitions);
        validate_default_subagent_routes(&catalog)?;
        assert_eq!(
            default_subagent_for(PlanTaskKind::Implementation),
            "implementer"
        );
        assert_eq!(default_subagent_for(PlanTaskKind::Verification), "primary");
        assert_eq!(ProfileTemplate::general().key, "general");
        Ok(())
    }
}
