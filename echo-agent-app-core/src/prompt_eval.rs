//! EKO-owned cross-domain prompt behavior evaluation assets.

use echo_agent::eval::{EvalCase, SuccessCriteria};
use serde::Serialize;

const PROMPT_BEHAVIOR_CASES: &str = include_str!("../../evals/prompt-behavior.yaml");

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PromptBehaviorCaseSummary {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub criterion: String,
}

pub fn prompt_behavior_cases() -> Result<Vec<EvalCase>, String> {
    serde_yaml::from_str(PROMPT_BEHAVIOR_CASES)
        .map_err(|error| format!("parse prompt behavior fixtures: {error}"))
}

pub fn prompt_behavior_case_summaries() -> Result<Vec<PromptBehaviorCaseSummary>, String> {
    prompt_behavior_cases().map(|cases| {
        cases
            .into_iter()
            .map(|case| PromptBehaviorCaseSummary {
                id: case.id,
                name: case.name,
                domain: case.domain.unwrap_or_else(|| "general".to_string()),
                criterion: criterion_name(&case.success_criteria).to_string(),
            })
            .collect()
    })
}

fn criterion_name(criteria: &SuccessCriteria) -> &'static str {
    match criteria {
        SuccessCriteria::TestPass { .. } => "test_pass",
        SuccessCriteria::OutputContains { .. } => "output_contains",
        SuccessCriteria::ToolUsed { .. } => "tool_used",
        SuccessCriteria::ToolNotUsed { .. } => "tool_not_used",
        SuccessCriteria::AllOf(_) => "all_of",
        SuccessCriteria::AnyOf(_) => "any_of",
        SuccessCriteria::LlmGraded { .. } => "llm_graded",
        SuccessCriteria::SweBench { .. } => "swe_bench",
        SuccessCriteria::SafetyCheck { .. } => "safety_check",
        SuccessCriteria::CitationValid { .. } => "citation_valid",
        SuccessCriteria::ValueMatch { .. } => "value_match",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    #[test]
    fn prompt_behavior_suite_covers_four_domains_with_real_criteria() -> Result<(), String> {
        let cases = prompt_behavior_cases()?;
        let domains: BTreeSet<String> = cases
            .iter()
            .filter_map(|case| case.domain.clone())
            .collect();
        let expected = BTreeSet::from([
            "coding".to_string(),
            "data".to_string(),
            "medical".to_string(),
            "research".to_string(),
        ]);
        if domains != expected {
            return Err(format!("unexpected prompt eval domains: {domains:?}"));
        }

        let mut ids = HashSet::new();
        for case in &cases {
            if !ids.insert(case.id.as_str()) {
                return Err(format!("duplicate prompt eval id: {}", case.id));
            }
            if case.description.trim().is_empty() || case.task.trim().is_empty() {
                return Err(format!("{} has an empty description or task", case.id));
            }
            if case.task.chars().count() > 800 {
                return Err(format!("{} task is too large for a focused eval", case.id));
            }
            if case.constraints.max_tool_calls.is_none() {
                return Err(format!("{} has no tool-call budget", case.id));
            }
        }
        Ok(())
    }

    #[test]
    fn prompt_behavior_suite_uses_domain_specific_graders() -> Result<(), String> {
        let summaries = prompt_behavior_case_summaries()?;
        let actual: BTreeSet<(String, String)> = summaries
            .into_iter()
            .map(|case| (case.domain, case.criterion))
            .collect();
        let expected = BTreeSet::from([
            ("coding".to_string(), "tool_used".to_string()),
            ("data".to_string(), "value_match".to_string()),
            ("medical".to_string(), "safety_check".to_string()),
            ("research".to_string(), "citation_valid".to_string()),
        ]);
        if actual != expected {
            return Err(format!("unexpected domain graders: {actual:?}"));
        }
        Ok(())
    }
}
