//! Static prompt contract auditing for EKO-owned prompt surfaces.

use echo_core::tokenizer::{HeuristicTokenizer, Tokenizer};

pub const DEMO_PHRASES: &[&str] = &[
    "you are a helpful assistant",
    "do your best",
    "just answer",
    "demo only",
    "for demonstration",
    "lorem ipsum",
    "pretend you",
];

#[derive(Debug, Clone)]
pub struct PromptContractSpec<'a> {
    pub name: &'a str,
    pub max_tokens: usize,
    pub required_phrases: &'a [&'a str],
    pub forbidden_phrases: &'a [&'a str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAuditReport {
    pub name: String,
    pub estimated_tokens: usize,
    pub missing_phrases: Vec<String>,
    pub forbidden_phrases: Vec<String>,
    pub over_budget: bool,
}

impl PromptAuditReport {
    pub fn is_compliant(&self) -> bool {
        !self.over_budget && self.missing_phrases.is_empty() && self.forbidden_phrases.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "{}: tokens={}, over_budget={}, missing=[{}], forbidden=[{}]",
            self.name,
            self.estimated_tokens,
            self.over_budget,
            self.missing_phrases.join(", "),
            self.forbidden_phrases.join(", ")
        )
    }
}

pub fn audit_prompt(spec: &PromptContractSpec<'_>, content: &str) -> PromptAuditReport {
    let tokenizer = HeuristicTokenizer;
    let normalized = content.to_lowercase();
    let missing_phrases = spec
        .required_phrases
        .iter()
        .filter(|phrase| !content.contains(**phrase))
        .map(|phrase| (*phrase).to_string())
        .collect();
    let forbidden_phrases = spec
        .forbidden_phrases
        .iter()
        .filter(|phrase| normalized.contains(&phrase.to_lowercase()))
        .map(|phrase| (*phrase).to_string())
        .collect();
    let estimated_tokens = tokenizer.count_tokens(content);

    PromptAuditReport {
        name: spec.name.to_string(),
        estimated_tokens,
        missing_phrases,
        forbidden_phrases,
        over_budget: estimated_tokens > spec.max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::TASK_MANAGEMENT_GUIDE;
    use crate::project::prompt::CORE_ASSISTANT_PROMPT;
    use crate::subagent_loader::discover_subagents;
    use crate::tasks::task_runtime::profiles::ProfileTemplate;
    use crate::tasks::task_runtime::types::DomainProfile;
    use std::path::{Path, PathBuf};

    fn assert_compliant(report: PromptAuditReport) -> Result<(), String> {
        if report.is_compliant() {
            Ok(())
        } else {
            Err(report.summary())
        }
    }

    #[test]
    fn core_prompt_has_a_bounded_production_contract() -> Result<(), String> {
        assert_compliant(audit_prompt(
            &PromptContractSpec {
                name: "core",
                max_tokens: 1_800,
                required_phrases: &[
                    "## Collaboration",
                    "## Execution",
                    "## Evidence And Verification",
                    "## Local Safety And Side Effects",
                    "## Domain Baselines",
                    "## Delivery",
                ],
                forbidden_phrases: DEMO_PHRASES,
            },
            CORE_ASSISTANT_PROMPT,
        ))?;
        assert_compliant(audit_prompt(
            &PromptContractSpec {
                name: "task_management",
                max_tokens: 900,
                required_phrases: &[
                    "## Task And Delegation Tools",
                    "### Formal Plan Contract",
                    "### Complex Run Contract",
                    "verification",
                ],
                forbidden_phrases: DEMO_PHRASES,
            },
            TASK_MANAGEMENT_GUIDE,
        ))
    }

    #[test]
    fn builtin_subagents_leave_shared_sections_to_the_compiler() -> Result<(), String> {
        for subagent in discover_subagents(None, None) {
            assert_compliant(audit_prompt(
                &PromptContractSpec {
                    name: subagent.name.as_str(),
                    max_tokens: 700,
                    required_phrases: &["# Role"],
                    forbidden_phrases: DEMO_PHRASES,
                },
                subagent.system_prompt.as_str(),
            ))?;
            for compiler_owned in [
                "# Delivery",
                "# Boundary",
                "## Response Language",
                "## Result",
                "suggested_tasks",
            ] {
                if subagent.system_prompt.contains(compiler_owned) {
                    return Err(format!(
                        "{}: role markdown duplicates compiler-owned section {compiler_owned}",
                        subagent.name
                    ));
                }
            }
            if !subagent.system_prompt.contains("# Method")
                && !subagent.system_prompt.contains("# Execution")
                && !subagent.system_prompt.contains("# Review Standard")
            {
                return Err(format!(
                    "{}: role markdown does not define a role-specific method",
                    subagent.name
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn domain_profiles_cover_evidence_and_completion_dimensions() -> Result<(), String> {
        let cases = [
            (DomainProfile::General, &["evidence", "complete"] as &[&str]),
            (DomainProfile::AiCoding, &["verification", "repository"]),
            (
                DomainProfile::DataAnalysis,
                &["reproducible", "uncertainty"],
            ),
            (DomainProfile::AcademicResearch, &["evidence", "citation"]),
            (DomainProfile::MedicalResearch, &["evidence", "uncertainty"]),
        ];
        for (profile, required) in cases {
            let template = ProfileTemplate::for_profile(profile);
            assert_compliant(audit_prompt(
                &PromptContractSpec {
                    name: template.key,
                    max_tokens: 320,
                    required_phrases: required,
                    forbidden_phrases: DEMO_PHRASES,
                },
                template.prompt_suffix,
            ))?;
        }
        Ok(())
    }

    #[test]
    fn bundled_skills_are_bounded_and_not_demo_prompts() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills");
        let mut files = Vec::new();
        collect_skill_files(root.as_path(), &mut files)?;
        files.sort();
        if files.is_empty() {
            return Err("no bundled SKILL.md files discovered".to_string());
        }

        for path in files {
            let content = std::fs::read_to_string(&path)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            assert_compliant(audit_prompt(
                &PromptContractSpec {
                    name: path.to_string_lossy().as_ref(),
                    max_tokens: 1_800,
                    required_phrases: &["name:", "description:", "#"],
                    forbidden_phrases: DEMO_PHRASES,
                },
                content.as_str(),
            ))?;
        }
        Ok(())
    }

    fn collect_skill_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|error| format!("read skill directory {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("read file type {}: {error}", path.display()))?;
            if file_type.is_dir() {
                collect_skill_files(path.as_path(), files)?;
            } else if file_type.is_file()
                && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
            {
                files.push(path);
            }
        }
        Ok(())
    }
}
