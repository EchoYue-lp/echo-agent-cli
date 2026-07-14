//! 工作区路由 — 根据 WorkspaceKind 自动激活 Skills 和注入系统提示词
//!
//! 当用户切换到特定类型的工作区时，自动配置 Agent 以适应该工作区的专业需求。

use crate::workspace::WorkspaceKind;
use echo_agent::agent::ReactAgent;

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
    match kind {
        WorkspaceKind::Medical { .. } => {
            // 激活医学研究相关 Skills
            activate_skill_safe(agent, "evidence-medicine").await;
            activate_skill_safe(agent, "paper-search").await;
            activate_skill_safe(agent, "paper-reader").await;

            // 注入医学研究系统提示词
            append_system_prompt(agent, MEDICAL_RESEARCH_PROMPT).await;

            tracing::info!("Workspace routing: Medical research mode configured");
        }

        WorkspaceKind::Research { .. } => {
            // 激活学术研究相关 Skills
            activate_skill_safe(agent, "paper-search").await;
            activate_skill_safe(agent, "paper-reader").await;
            activate_skill_safe(agent, "doc-writing").await;

            // 注入学术研究系统提示词
            append_system_prompt(agent, RESEARCH_PROMPT).await;

            tracing::info!("Workspace routing: Research mode configured");
        }

        WorkspaceKind::DataAnalysis { .. } => {
            // 激活数据分析相关 Skills
            activate_skill_safe(agent, "data-wrangling").await;
            activate_skill_safe(agent, "statistical-analysis").await;
            activate_skill_safe(agent, "data-visualization").await;

            // 注入数据分析系统提示词
            append_system_prompt(agent, DATA_ANALYSIS_PROMPT).await;

            tracing::info!("Workspace routing: Data analysis mode configured");
        }

        WorkspaceKind::Code { .. } => {
            // 激活代码项目相关 Skills
            activate_skill_safe(agent, "coding").await;
            activate_skill_safe(agent, "git-workflow").await;

            // 注入代码项目系统提示词
            append_system_prompt(agent, CODE_PROMPT).await;

            tracing::info!("Workspace routing: Code project mode configured");
        }

        WorkspaceKind::General => {
            // 通用模式不自动激活特定 Skills
            tracing::info!("Workspace routing: General mode (no specific skills activated)");
        }
    }
}

/// 安全地激活 Skill（忽略错误）
async fn activate_skill_safe(agent: &mut ReactAgent, skill_name: &str) {
    match agent.skill_registry_mut().activate(skill_name).await {
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

/// 追加系统提示词
async fn append_system_prompt(agent: &mut ReactAgent, additional_prompt: &str) {
    let current_prompt = agent.config().get_system_prompt().to_string();
    let new_prompt = format!("{}{}", current_prompt, additional_prompt);
    agent.set_system_prompt(new_prompt).await;
}
