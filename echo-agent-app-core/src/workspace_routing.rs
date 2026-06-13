//! 工作区路由 — 根据 WorkspaceKind 自动激活 Skills 和注入系统提示词
//!
//! 当用户切换到特定类型的工作区时，自动配置 Agent 以适应该工作区的专业需求。

use crate::workspace::WorkspaceKind;
use echo_agent::agent::ReactAgent;

/// 医学研究系统提示词增强
const MEDICAL_RESEARCH_PROMPT: &str = r#"

## 医学研究模式已激活

你当前处于**医学研究**工作区，已自动激活以下专业技能：
- **evidence-medicine**: 循证医学研究、PICO 框架、系统评价、Meta 分析
- **paper-search**: 学术论文检索（PubMed、Cochrane Library、CNKI 等）
- **paper-reader**: 论文深度阅读与批判性分析

### 医学研究指导原则

1. **循证优先**: 所有医学声明必须有文献支持，使用 PMID/DOI 引用
2. **证据分级**: 遵循 GRADE 系统评估证据质量（High/Moderate/Low/Very Low）
3. **PICO 框架**: 构建临床问题时使用 Population-Intervention-Comparison-Outcome
4. **偏倚评估**: 使用 Cochrane RoB 2 或 ROBINS-I 工具评估研究偏倚风险
5. **透明报告**: 遵循 PRISMA 流程图报告系统评价过程

### 可用工具

- `pubmed_search`: PubMed 文献检索
- `clinical_trials_search`: 临床试验查询
- `web_search` / `web_fetch`: 网页搜索与抓取
- `pdf_extract`: PDF 论文全文提取
- `bibtex_generate`: BibTeX 引用生成

请使用上述工具和技能协助医学研究工作。"#;

/// 学术研究系统提示词增强
const RESEARCH_PROMPT: &str = r#"

## 学术研究模式已激活

你当前处于**学术研究**工作区，已自动激活以下专业技能：
- **paper-search**: 学术论文检索（arXiv、Semantic Scholar、Google Scholar 等）
- **paper-reader**: 论文深度阅读与结构化分析
- **doc-writing**: 学术文档写作（论文、综述、报告）

### 学术研究指导原则

1. **文献检索**: 使用多数据库交叉检索，确保文献覆盖全面
2. **批判性阅读**: 评估研究方法论、样本量、统计方法和结论可靠性
3. **引用规范**: 使用标准引用格式（APA、MLA、Chicago 等）
4. **学术写作**: 遵循学术写作规范，逻辑清晰、论证有力

### 可用工具

- `arxiv_search` / `semantic_scholar_search`: 学术论文检索
- `web_search` / `web_fetch`: 网页搜索与抓取
- `pdf_extract`: PDF 论文全文提取
- `bibtex_generate`: BibTeX 引用生成
- `write_file` / `edit_file`: 文档写作与编辑

请使用上述工具和技能协助学术研究工作。"#;

/// 数据分析系统提示词增强
const DATA_ANALYSIS_PROMPT: &str = r#"

## 数据分析模式已激活

你当前处于**数据分析**工作区，已自动激活以下专业技能：
- **data-wrangling**: 数据加载、清洗与探索性分析（EDA）
- **statistical-analysis**: 统计分析与假设检验
- **data-visualization**: 数据可视化与图表制作

### 数据分析指导原则

1. **数据质量**: 先检查数据质量（缺失值、异常值、一致性），再进行分析
2. **探索性分析**: 使用 EDA 理解数据分布、关系和模式
3. **统计严谨**: 选择合适的统计方法，报告 p 值、效应量和置信区间
4. **可视化**: 使用适当的图表展示数据洞察

### 可用工具

- `read_data` / `load_dataframe`: 数据加载（CSV/JSON/Parquet/Excel）
- `filter_data` / `aggregate_data`: 数据过滤与聚合
- `data_stats` / `profile_data`: 数据统计与画像
- `hypothesis_test` / `regression`: 假设检验与回归分析
- `generate_chart`: 数据可视化
- `read_excel` / `excel_to_csv`: Excel 文件处理

请使用上述工具和技能协助数据分析工作。"#;

/// 代码项目系统提示词增强
const CODE_PROMPT: &str = r#"

## 代码项目模式已激活

你当前处于**代码项目**工作区，已自动激活以下专业技能：
- **coding**: 代码生成、审查、重构、调试
- **git-workflow**: Git 版本控制工作流

### 代码项目指导原则

1. **代码质量**: 遵循项目现有代码风格和约定
2. **测试驱动**: 编写可测试的代码，提供单元测试
3. **Git 规范**: 使用规范的 commit message，遵循分支策略
4. **文档**: 为公共 API 提供清晰的文档注释

### 可用工具

- `read_file` / `write_file` / `edit_file`: 文件读写与编辑
- `shell`: 执行命令（构建、测试、运行）
- `grep` / `glob`: 代码搜索
- `git_*`: Git 操作（status、diff、commit、push 等）
- `code_search`: 语义代码搜索

请使用上述工具和技能协助代码开发工作。"#;

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
