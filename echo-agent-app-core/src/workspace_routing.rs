//! 工作区路由 — 根据 WorkspaceKind 自动激活 Skills 和注入系统提示词
//!
//! 当用户切换到特定类型的工作区时，自动配置 Agent 以适应该工作区的专业需求。

use crate::workspace::WorkspaceKind;
use echo_agent::agent::ReactAgent;
use echo_agent::llm::types::Message;

const WORKSPACE_PROFILE_PROJECTION: &str = "eko:workspace-profile";
const MEDICAL_SKILLS: &[&str] = &["evidence-medicine", "paper-search", "paper-reader"];
const RESEARCH_SKILLS: &[&str] = &["paper-search", "paper-reader"];
const DATA_SKILLS: &[&str] = &[
    "data-wrangling",
    "statistical-analysis",
    "data-visualization",
];
/// coding skill 已删(行为准则由基础 prompt contract 承担),Code 工作区
/// 保留 git-workflow;CODE_PROMPT 本身承担编程工作区的行为注入。
const CODE_SKILLS: &[&str] = &["git-workflow"];

/// 医学研究系统提示词增强
const MEDICAL_RESEARCH_PROMPT: &str = r#"

## Workspace Profile: Medical Research

Outcome: produce a traceable evidence synthesis that a researcher can inspect and reproduce.
- Frame answerable questions with PICO/PECO or an equivalent structure, including population, intervention or exposure, comparator, outcomes, setting, and time horizon when relevant.
- Prefer current guidelines and systematic reviews, then pivotal trials and high-quality observational evidence. Record search date, source, study design, population, effect estimate, uncertainty, and applicability.
- Separate evidence quality, recommendation strength, clinical importance, contraindications, and uncertainty. Conflicting evidence must remain visible.
- Never invent a citation or turn population evidence into an individualized diagnosis or treatment instruction. Escalate urgent safety concerns to qualified clinical care.
- Completion requires sources that directly support the material clinical claims, explicit evidence gaps, and a clear statement of applicability limits. Use only tools actually available in the current context."#;

/// 学术研究系统提示词增强
const RESEARCH_PROMPT: &str = r#"

## Workspace Profile: Academic Research

Outcome: produce a reproducible, citable answer whose claims do not exceed the evidence.
- Define the research question, scope, date range, terminology, source types, and inclusion/exclusion criteria before broad retrieval.
- Verify titles, authors, venues, dates, identifiers, and quoted findings from actual sources. Never fabricate bibliographic details or treat a search snippet as full-text evidence.
- Compare methods, datasets, baselines, statistical support, external validity, conflicting results, and limitations. Distinguish peer-reviewed work from preprints and secondary commentary.
- Label source-backed fact, synthesis, and inference separately. Preserve uncertainty and negative evidence.
- Completion requires a transparent search trail, direct support for central claims, and enough citation detail for the user to locate the sources. Use only tools actually available in the current context."#;

/// 数据分析系统提示词增强
const DATA_ANALYSIS_PROMPT: &str = r#"

## Workspace Profile: Data Analysis

Outcome: produce an auditable analysis that can be rerun from the original inputs.
- Establish provenance, schema, semantics, units, population, time grain, missingness, duplicates, outliers, joins, and sampling before modeling or charting.
- Define the question and metric before choosing a method. Check assumptions, leakage, denominator changes, multiple comparisons, and sensitivity to reasonable alternatives.
- Distinguish descriptive results, association, prediction, and causal claims. Report uncertainty and practical significance, not only a point estimate or p-value.
- Preserve raw inputs; make transformations explicit in code or a durable artifact; record parameters, row-count changes, and generated outputs.
- Completion requires reproducibility evidence, validation of key calculations, and a limitations section. Use only tools actually available in the current context."#;

/// 代码项目系统提示词增强
const CODE_PROMPT: &str = r#"

## Workspace Profile: AI Coding

Outcome: deliver a repository-native change that solves the requested behavior and is supported by verification.
- Inspect repository instructions, current diffs, architecture, call paths, tests, and local conventions before editing. Confirm whether the capability already exists.
- Prefer root-cause fixes and focused diffs. Reuse established abstractions and dependency choices; do not hide uncertainty behind broad refactors or compatibility shims.
- Protect uncommitted work. Treat concurrency, state ownership, persistence, cancellation, permissions, and failure recovery as explicit design concerns when they are in scope.
- Add or update tests in proportion to behavioral risk, then run the relevant formatter, type/build checks, tests, and feature targets required by the repository.
- Completion requires a clear change summary, exact verification evidence, and disclosure of any unverified path. Use only tools actually available in the current context."#;

/// 根据工作区类型配置 Agent
///
/// 自动激活相关 Skills 并注入专业系统提示词。
pub async fn configure_agent_for_workspace(agent: &mut ReactAgent, kind: &WorkspaceKind) {
    let (skills, profile, label): (&[&str], Option<&str>, &str) = match kind {
        WorkspaceKind::Medical { .. } => (
            MEDICAL_SKILLS,
            Some(MEDICAL_RESEARCH_PROMPT),
            "medical_research",
        ),
        WorkspaceKind::Research { .. } => {
            (RESEARCH_SKILLS, Some(RESEARCH_PROMPT), "academic_research")
        }
        WorkspaceKind::DataAnalysis { .. } => {
            (DATA_SKILLS, Some(DATA_ANALYSIS_PROMPT), "data_analysis")
        }
        WorkspaceKind::Code { .. } => (CODE_SKILLS, Some(CODE_PROMPT), "ai_coding"),
        WorkspaceKind::General => (&[], None, "general"),
    };

    for skill in skills {
        activate_skill_safe(agent, skill).await;
    }

    agent.context().lock().await.replace_projection(
        WORKSPACE_PROFILE_PROJECTION,
        profile.map(|prompt| Message::system(prompt.trim().to_string())),
    );
    tracing::info!(profile = label, "Workspace routing configured");
}

/// 安全地激活 Skill（忽略错误）
async fn activate_skill_safe(agent: &ReactAgent, skill_name: &str) {
    match agent.activate_skill(skill_name).await {
        Ok(_) => {
            tracing::debug!("Skill '{}' activated successfully", skill_name);
        }
        Err(e) => {
            tracing::warn!(
                skill = skill_name,
                error = %e,
                "Failed to activate skill (skill may not exist)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::AgentConfig;

    #[test]
    fn workspace_profiles_are_bounded_behavior_contracts() -> Result<(), String> {
        for (name, prompt) in [
            ("medical", MEDICAL_RESEARCH_PROMPT),
            ("research", RESEARCH_PROMPT),
            ("data", DATA_ANALYSIS_PROMPT),
            ("coding", CODE_PROMPT),
        ] {
            let report = crate::prompt_contract::audit_prompt(
                &crate::prompt_contract::PromptContractSpec {
                    name,
                    max_tokens: 360,
                    required_phrases: &["Outcome:", "Completion requires", "evidence"],
                    forbidden_phrases: crate::prompt_contract::DEMO_PHRASES,
                },
                prompt,
            );
            if !report.is_compliant() {
                return Err(report.summary());
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn workspace_profile_replaces_without_mutating_root_prompt() {
        let mut agent = ReactAgent::new(AgentConfig::minimal("model", "agent"));
        let root_prompt = agent.config().get_system_prompt().to_string();

        configure_agent_for_workspace(&mut agent, &WorkspaceKind::Code { repo_url: None }).await;
        configure_agent_for_workspace(
            &mut agent,
            &WorkspaceKind::DataAnalysis { datasets: vec![] },
        )
        .await;

        assert_eq!(agent.config().get_system_prompt(), root_prompt);
        let context = agent.context().lock().await;
        assert!(context.has_projection(WORKSPACE_PROFILE_PROJECTION));
        let profiles: Vec<_> = context
            .messages()
            .iter()
            .filter(|message| {
                message
                    .content
                    .as_text_ref()
                    .is_some_and(|text| text.contains("## Workspace Profile:"))
            })
            .collect();
        assert_eq!(profiles.len(), 1);
        assert!(profiles.first().is_some_and(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|text| text.contains("Data Analysis"))
        }));
        drop(context);

        configure_agent_for_workspace(&mut agent, &WorkspaceKind::General).await;
        assert!(
            !agent
                .context()
                .lock()
                .await
                .has_projection(WORKSPACE_PROFILE_PROJECTION)
        );
    }
}
