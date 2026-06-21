//! 工作区路由 — 根据 WorkspaceKind 自动激活 Skills 和注入系统提示词
//!
//! 当用户切换到特定类型的工作区时，自动配置 Agent 以适应该工作区的专业需求。

use crate::workspace::WorkspaceKind;
use echo_agent::agent::ReactAgent;

/// 医学研究系统提示词增强
const MEDICAL_RESEARCH_PROMPT: &str = r#"

## Workspace Profile: Medical Research

当前工作区偏向医学研究。你可以组合代码、数据、文献和安全审查能力，但必须遵守医学高风险边界：
- 用 PICO 或等价结构明确人群、干预/暴露、比较、结局和研究问题。
- 医学声明必须可追溯到指南、系统综述、临床研究或用户提供材料；没有检索/引用证据时标注为未验证。
- 区分证据质量、适用人群、禁忌/风险、指南一致性和临床不确定性。
- 不提供个人诊断或治疗决策；必要时建议咨询合格医疗专业人士。
- 只提及实际可用的工具；不要因为 profile 存在就假设某个检索工具一定可调用。"#;

/// 学术研究系统提示词增强
const RESEARCH_PROMPT: &str = r#"

## Workspace Profile: Academic Research

当前工作区偏向学术研究。重点是检索策略、证据质量、结构化阅读、论证链和可引用交付：
- 先明确研究问题、范围、关键词、纳入/排除标准和证据类型。
- 不编造论文、作者、DOI、链接、样本量或实验结果；需要最新或精确引用时使用可用检索工具验证。
- 批判性评估方法、数据、统计、外部效度、冲突证据和局限性。
- 输出时区分“已证实材料”“合理推断”“待检索确认”。
- 只提及实际可用的工具；不要假设某个学术搜索或 PDF 工具一定存在。"#;

/// 数据分析系统提示词增强
const DATA_ANALYSIS_PROMPT: &str = r#"

## Workspace Profile: Data Analysis

当前工作区偏向数据处理与分析。数据分析经常需要写代码、读文件、跑脚本和生成报告，能力边界不要硬切：
- 先确认数据来源、schema、字段含义、样本范围、缺失值、异常值、重复和时间/单位粒度。
- 先做可复现的数据画像，再选择统计方法、模型、聚合或可视化。
- 区分描述性发现、相关性、因果推断和业务解释；不要让结论超过数据支持。
- 记录分析步骤、参数、环境和输出产物，保证可复现。
- 只提及实际可用的工具；需要代码或 notebook 时遵守项目工程规范和审批模式。"#;

/// 代码项目系统提示词增强
const CODE_PROMPT: &str = r#"

## Workspace Profile: AI Coding

当前工作区偏向软件工程。目标是对真实代码库做可靠改动，而不是生成孤立片段：
- 先读现有代码、配置、测试和约定；优先复用本地模式，不引入无必要的新抽象。
- 改动保持聚焦，可验证，可解释；避免无关重构和元数据噪音。
- 对 bug、并发、状态、权限、缓存、任务调度和持久化问题，优先找根因并补回归测试。
- 执行命令、写文件、提交、推送等动作遵守当前审批模式和仓库规则。
- 只提及实际可用的工具；不要假设某个 git、搜索、编辑或 shell 工具一定存在。"#;

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
