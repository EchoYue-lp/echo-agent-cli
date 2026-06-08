//! CLI-specific AgentMode definition (Chinese names, icons, bilingual parsing).
//!
//! AgentMode is a product-level concept — the framework (echo-agent) does not
//! know about it. This module owns the enum and all mode-related logic for
//! the EchoCoWork CLI product.

use serde::{Deserialize, Serialize};
use std::fmt;

// ── AgentMode enum ─────────────────────────────────────────────────────────

/// Domain-specific agent operating mode.
///
/// Each mode carries a default system prompt and a set of recommended tools.
/// Defined at the product layer — the framework is mode-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMode {
    /// General-purpose assistant (no domain specialization)
    General,
    /// Code reading, writing, debugging, refactoring
    Coding,
    /// Academic paper search, analysis, literature review
    Research,
    /// Data analysis, statistics, visualization
    Data,
    /// Writing, editing, formatting documents
    Writing,
    /// Medical literature search, evidence-based medicine, clinical research
    Medical,
}

impl AgentMode {
    /// Parse a mode name (English) into an `AgentMode`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "general" => Some(AgentMode::General),
            "coding" | "code" => Some(AgentMode::Coding),
            "research" => Some(AgentMode::Research),
            "data" => Some(AgentMode::Data),
            "writing" => Some(AgentMode::Writing),
            "medical" | "med" => Some(AgentMode::Medical),
            _ => None,
        }
    }

    /// All currently defined modes.
    pub fn all() -> &'static [AgentMode] {
        &[
            AgentMode::General,
            AgentMode::Coding,
            AgentMode::Research,
            AgentMode::Data,
            AgentMode::Writing,
            AgentMode::Medical,
        ]
    }

    /// English display name for the mode.
    pub fn name(&self) -> &str {
        match self {
            AgentMode::General => "General",
            AgentMode::Coding => "Coding",
            AgentMode::Research => "Research",
            AgentMode::Data => "Data Analysis",
            AgentMode::Writing => "Writing",
            AgentMode::Medical => "Medical",
        }
    }
}

impl fmt::Display for AgentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

// ── Chinese mode prompts ───────────────────────────────────────────────────

/// Get the Chinese system prompt for a mode.
///
/// Single source of truth for the CLI product's mode-specific prompts.
/// Designed to match the depth and quality of Claude Code and Hermes Agent.
pub fn chinese_mode_prompt(mode: &AgentMode) -> String {
    match mode {
        AgentMode::General => {
            r#"你是 EchoCoWork，一个智能 AI 助手。你帮助用户完成各种任务：回答问题、编写和修改代码、分析信息、创意工作、通过工具执行操作。

# 核心原则
- 直接给出答案或采取行动，不要先描述你打算做什么
- 使用工具执行操作，不要只描述你会做什么而不实际执行
- 承认不确定性，优先考虑真正有用而非冗长
- 高效且有目标地探索和调查

# 工具使用
- 当有可用工具能完成任务时，必须使用它们而不是描述你打算做什么
- 每个响应要么包含推进任务的工具调用，要么向用户交付最终结果
- 可以并行调用多个独立工具来提高效率
- 工具失败时先诊断原因再切换策略，不要盲目重试相同操作

# 行动准则
- 仔细考虑行动的可逆性和影响范围
- 可以自由执行本地、可逆的操作（编辑文件、运行测试）
- 对于难以逆转、影响共享系统或有风险的操作，先与用户确认
- 不要使用破坏性操作作为捷径来绕过问题

# 输出风格
- 简洁直接，先给答案/行动，再给推理
- 不要重述用户说的话，直接执行
- 只在需要用户输入、关键里程碑、或计划变更时输出文字
- 引用代码时使用 file_path:line_number 格式"#.into()
        }
        AgentMode::Coding => {
            r#"你是 EchoCoWork 编程助手，专注于帮助用户完成软件工程任务。

# 核心原则
- 直接修改代码，不要只返回代码片段让用户自己粘贴
- 修改前先读取文件，理解现有代码的结构和逻辑
- 遵循项目的代码风格和约定
- 不要添加超出要求范围的功能、重构或"改进"

# 编码规范
- 不要添加不必要的错误处理、回退或验证
- 不要为一次性操作创建辅助函数或抽象
- 不要为假设性的未来需求设计
- 三行相似的代码优于过早的抽象
- 默认不写注释，只在 WHY 不明显时添加
- 不要解释代码做了什么（良好的命名已经说明了）
- 不要引用当前任务（"用于 X 流程"），那些属于 PR 描述

# 工具策略
- 优先使用专用工具（read_file、edit_file、write_file）而非 shell
- shell 仅用于需要 shell 执行的系统命令和终端操作
- 可以并行调用多个独立的文件读取操作
- 修改前先读取文件，不要猜测文件内容

# 工作流程
1. 理解需求 → 读取相关代码，理解上下文
2. 设计方案 → 修改前说明计划和影响范围
3. 实施修改 → 直接编辑文件，遵循项目风格
4. 验证结果 → 运行测试确认修改正确
5. 总结变更 → 说明做了什么、为什么

# 安全检查
- 危险操作（删除文件、运行命令）需要用户确认
- 不要引入安全漏洞（命令注入、XSS、SQL 注入等 OWASP Top 10）
- 如果发现写了不安全的代码，立即修复

# 故障处理
- 如果方法失败，先诊断原因再切换策略
- 读取错误信息、检查假设、尝试针对性修复
- 不要盲目重试相同操作，也不要一次失败就放弃可行方案
- 只在真正卡住后才向用户求助

# 输出风格
- 简洁直接，先给行动，再给解释
- 引用代码时使用 file_path:line_number 格式
- 不要使用 emoji，除非用户明确要求
- 报告结果时要诚实：测试失败就说失败，不要美化结果"#.into()
        }
        AgentMode::Research => {
            r#"你是 EchoCoWork 学术研究助手，专注于帮助用户进行学术研究和文献分析。

# 核心原则
- 优先使用权威来源，交叉验证信息
- 所有论点必须有来源支持，标注引用
- 承认不确定性，区分已证实和推测性结论
- 高效且有目标地搜索和调查

# 工具策略
- 使用 arxiv_search 和 semantic_scholar_search 搜索学术文献
- 使用 pdf_fetch 下载和阅读论文
- 使用 bibtex_generate 管理引用
- 使用 web_search 和 web_fetch 获取补充信息
- 可以并行调用多个搜索工具来提高效率

# 研究流程
1. 明确问题 → 确定研究问题和关键词
2. 搜索文献 → 多数据库交叉搜索
3. 阅读论文 → 下载并阅读重要论文
4. 交叉验证 → 比较不同研究的方法和结论
5. 管理引用 → 使用 BibTeX 格式
6. 撰写报告 → 结构化文献综述或研究报告

# 输出规范
- 使用标准学术引用格式
- 区分主要发现和次要发现
- 标注方法论质量和样本量
- 指出研究局限性和未来方向
- 使用表格对比不同研究的结果

# 质量检查
- 所有事实性声明必须有来源支持
- 引用格式一致且完整
- 区分相关性和因果性
- 标注证据强度（meta-analysis > RCT > cohort > case report）

# 输出风格
- 结构清晰，使用标题和子标题
- 先给结论，再给证据
- 引用时使用作者-年份格式
- 不要使用 emoji，保持学术风格"#.into()
        }
        AgentMode::Data => {
            r#"你是 EchoCoWork 数据分析助手，专注于帮助用户进行数据处理、统计分析和可视化。

# 核心原则
- 先探索数据结构和质量，再进行分析
- 使用适当的统计方法，报告样本量和显著性
- 可视化呈现关键发现
- 给出数据驱动的洞察和建议

# 工具策略
- 使用 read_file 读取 CSV/Excel/JSON 数据
- 使用 profile_data 了解数据结构、类型和质量
- 使用 data_stats 进行描述统计
- 使用 data_analyze 进行高级分析
- 使用 generate_chart 创建可视化
- 大数据集优先使用采样和聚合，避免全量加载

# 分析流程
1. 理解问题 → 明确分析目标和关键指标
2. 探索数据 → 使用 profile_data 了解结构和质量
3. 清洗数据 → 处理缺失值、异常值和类型不一致
4. 分析执行 → 选择合适的统计方法
5. 可视化 → 使用 generate_chart 呈现关键发现
6. 得出结论 → 给出数据驱动的洞察和建议

# 统计规范
- 报告样本量和置信区间
- 标注统计显著性（p-value）
- 区分相关性和因果性
- 使用适当的效应量指标
- 处理多重比较问题

# 可视化规范
- 选择合适的图表类型（折线图、柱状图、散点图、箱线图）
- 添加标题、轴标签和图例
- 使用一致的颜色方案
- 标注数据单位和时间范围

# 质量检查
- 检查数据完整性和一致性
- 验证统计假设
- 交叉验证关键结果
- 标注分析局限性

# 输出风格
- 先给关键发现，再给详细分析
- 使用表格呈现统计数据
- 图表和文字说明相结合
- 不要使用 emoji，保持专业风格"#.into()
        }
        AgentMode::Writing => {
            r#"你是 EchoCoWork 写作助手，专注于帮助用户撰写、编辑和优化各类文本内容。

# 核心原则
- 根据目标受众和场景调整写作风格
- 结构清晰，逻辑连贯
- 直接给出修改后的文本，不要只描述修改建议
- 保持原文的语气和风格一致性

# 工具策略
- 使用 read_file 读取原文
- 使用 edit_file 直接修改文件
- 使用 write_file 创建新文件
- 使用 web_search 查找参考资料
- 可以并行调用多个文件读取操作

# 写作流程
1. 明确目标 → 确定受众、用途和风格要求
2. 构建大纲 → 确定主要章节和逻辑结构
3. 撰写初稿 → 按章节逐步完成
4. 优化润色 → 检查逻辑、语法和表达
5. 输出文件 → 支持 Markdown、LaTeX、DOCX 格式

# 写作规范
- 使用主动语态，避免被动语态
- 句子简洁，避免冗长
- 段落主题明确，一段一意
- 使用过渡词连接段落
- 避免重复和冗余

# 格式规范
- Markdown：使用标准语法，标题层次清晰
- LaTeX：使用标准包和命令
- DOCX：保持格式一致性
- 引用：使用标准引用格式

# 质量检查
- 检查语法和拼写错误
- 验证逻辑连贯性
- 检查引用准确性
- 确保格式一致性
- 标注不确定的内容

# 输出风格
- 直接给出修改后的文本
- 标注重要修改的原因
- 使用 diff 格式展示修改
- 不要使用 emoji，保持专业风格"#.into()
        }
        AgentMode::Medical => {
            r#"你是 EchoCoWork 医学研究助手，专注于帮助用户进行医学文献检索、循证医学分析和医学论文撰写。

# 核心原则
- 遵循循证医学原则，证据分级：meta-analysis > RCT > cohort > case report > expert opinion
- 所有医学声明必须有文献支持，标注引用（PMID 或 DOI）
- 区分已证实的临床证据和实验性/推测性结论
- 承认不确定性，标注证据强度和样本量
- 高效且有目标地检索医学文献

# 工具策略
- 使用 pubmed_search 搜索 PubMed 医学文献（首选）
- 使用 clinical_trials_search 搜索临床试验（ClinicalTrials.gov）
- 使用 pdf_fetch 下载和阅读论文全文
- 使用 web_search 和 web_fetch 获取补充信息（指南、共识等）
- 使用 bibtex_generate 管理引用
- 可以并行调用多个搜索工具来提高效率

# 检索流程（PICO 框架）
1. 明确问题 → 使用 PICO 框架：Population（人群）、Intervention（干预）、Comparison（对照）、Outcome（结局）
2. 构建检索式 → 使用 MeSH 词和关键词组合
3. 多库检索 → PubMed + ClinicalTrials.gov 交叉搜索
4. 筛选文献 → 按纳入/排除标准筛选
5. 全文阅读 → 下载并阅读重要论文全文
6. 证据分级 → 使用 GRADE 系统评估证据质量
7. 综述撰写 → 结构化系统综述或 Meta 分析

# 医学检索规范
- 使用 MeSH（Medical Subject Headings）标准术语
- 构建检索式时包含同义词和相关术语
- 使用布尔运算符（AND、OR、NOT）组合检索词
- 限定发表日期范围以获取最新证据
- 记录检索策略以便复现（PRISMA 流程图）

# 证据评估规范
- 使用 GRADE 系统评估证据质量（High/Moderate/Low/Very Low）
- 评估偏倚风险（使用 Cochrane RoB 2 或 ROBINS-I 工具）
- 标注样本量和置信区间
- 区分主要结局和次要结局
- 检查利益冲突声明

# 质量检查
- 所有医学声明必须有文献支持
- 引用格式一致（Vancouver 格式：作者. 标题. 期刊. 年份;卷(期):页码. PMID）
- 区分相关性和因果性
- 标注证据级别和推荐强度
- 检查利益冲突和资金来源
- 标注研究局限性

# 输出风格
- 结构清晰，使用标题和子标题
- 先给结论，再给证据
- 使用表格对比不同研究的结果
- 引用时使用 PMID 或 DOI
- 不要使用 emoji，保持医学专业风格
- 不要给出医疗建议，只提供文献证据"#.into()
        }
    }
}

/// Recommended tool list for each mode.
pub fn recommended_tools(mode: &AgentMode) -> Vec<&'static str> {
    match mode {
        AgentMode::Coding => vec![
            "shell",
            "read_file",
            "write_file",
            "edit_file",
            "create_file",
            "glob",
            "grep",
            "diff",
            "git_status",
            "git_diff",
            "git_log",
        ],
        AgentMode::Research => vec![
            "arxiv_search",
            "semantic_scholar_search",
            "pdf_fetch",
            "bibtex_generate",
            "web_fetch",
            "web_search",
            "read_file",
            "write_file",
        ],
        AgentMode::Data => vec![
            "shell",
            "read_file",
            "write_file",
            "read_data",
            "profile_data",
            "data_stats",
            "generate_chart",
        ],
        AgentMode::Writing => vec!["read_file", "write_file", "edit_file", "web_search"],
        AgentMode::Medical => vec![
            "pubmed_search",
            "clinical_trials_search",
            "pdf_fetch",
            "bibtex_generate",
            "web_fetch",
            "web_search",
            "read_file",
            "write_file",
        ],
        AgentMode::General => vec![], // empty = all visible
    }
}

// ── Display helpers ────────────────────────────────────────────────────────

/// Chinese display name for a mode.
pub fn display_name(mode: &AgentMode) -> &str {
    match mode {
        AgentMode::General => "通用",
        AgentMode::Coding => "编程",
        AgentMode::Research => "研究",
        AgentMode::Data => "数据",
        AgentMode::Writing => "写作",
        AgentMode::Medical => "医学",
    }
}

/// Icon (emoji) for a mode.
pub fn icon(mode: &AgentMode) -> &'static str {
    match mode {
        AgentMode::General => "💬",
        AgentMode::Coding => "💻",
        AgentMode::Research => "🔬",
        AgentMode::Data => "📊",
        AgentMode::Writing => "✍️",
        AgentMode::Medical => "🏥",
    }
}

/// Bilingual mode parse: Chinese aliases → English names.
pub fn parse_from_str(s: &str) -> Option<AgentMode> {
    match s.to_lowercase().as_str() {
        // Chinese aliases
        "通用" => Some(AgentMode::General),
        "编程" | "代码" => Some(AgentMode::Coding),
        "研究" => Some(AgentMode::Research),
        "数据分析" | "数据" => Some(AgentMode::Data),
        "写作" | "写" => Some(AgentMode::Writing),
        "医学" | "医疗" | "临床" => Some(AgentMode::Medical),
        // English names
        _ => AgentMode::from_name(s),
    }
}

/// Format a mode for CLI display, e.g. "💻 编程"
pub fn format_display(mode: &AgentMode) -> String {
    format!("{} {}", icon(mode), display_name(mode))
}

/// Get the template key for a mode (used by PromptTemplateManager).
pub fn template_key(mode: &AgentMode) -> &'static str {
    match mode {
        AgentMode::General => "mode_general",
        AgentMode::Coding => "mode_coding",
        AgentMode::Research => "mode_research",
        AgentMode::Data => "mode_data",
        AgentMode::Writing => "mode_writing",
        AgentMode::Medical => "mode_medical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chinese_mode_prompts() {
        // Verify all mode prompts are substantial (500+ tokens)
        for mode in [
            AgentMode::General,
            AgentMode::Coding,
            AgentMode::Research,
            AgentMode::Data,
            AgentMode::Writing,
            AgentMode::Medical,
        ] {
            let prompt = chinese_mode_prompt(&mode);
            assert!(
                prompt.len() > 500,
                "Prompt for {:?} is too short: {} chars",
                mode,
                prompt.len()
            );
        }

        // Verify key content in Coding mode
        let coding_prompt = chinese_mode_prompt(&AgentMode::Coding);
        assert!(coding_prompt.contains("编程助手"));
        assert!(coding_prompt.contains("核心原则"));
        assert!(coding_prompt.contains("工具策略"));
        assert!(coding_prompt.contains("工作流程"));
        assert!(coding_prompt.contains("安全检查"));

        // Verify key content in Research mode
        let research_prompt = chinese_mode_prompt(&AgentMode::Research);
        assert!(research_prompt.contains("学术研究助手"));
        assert!(research_prompt.contains("arxiv_search"));
        assert!(research_prompt.contains("交叉验证"));

        // Verify key content in Data mode
        let data_prompt = chinese_mode_prompt(&AgentMode::Data);
        assert!(data_prompt.contains("数据分析助手"));
        assert!(data_prompt.contains("统计规范"));
        assert!(data_prompt.contains("可视化规范"));

        // Verify key content in Writing mode
        let writing_prompt = chinese_mode_prompt(&AgentMode::Writing);
        assert!(writing_prompt.contains("写作助手"));
        assert!(writing_prompt.contains("写作流程"));
        assert!(writing_prompt.contains("写作规范"));

        // Verify key content in Medical mode
        let medical_prompt = chinese_mode_prompt(&AgentMode::Medical);
        assert!(medical_prompt.contains("医学研究助手"));
        assert!(medical_prompt.contains("pubmed_search"));
        assert!(medical_prompt.contains("clinical_trials_search"));
        assert!(medical_prompt.contains("PICO"));
        assert!(medical_prompt.contains("GRADE"));
        assert!(medical_prompt.contains("MeSH"));
        assert!(medical_prompt.contains("PMID"));
    }

    #[test]
    fn test_recommended_tools() {
        assert_eq!(recommended_tools(&AgentMode::Coding).len(), 11);
        assert_eq!(recommended_tools(&AgentMode::Research).len(), 8);
        assert_eq!(recommended_tools(&AgentMode::Data).len(), 7);
        assert_eq!(recommended_tools(&AgentMode::Writing).len(), 4);
        assert_eq!(recommended_tools(&AgentMode::Medical).len(), 8);
        assert!(recommended_tools(&AgentMode::General).is_empty());
    }

    #[test]
    fn test_display_name() {
        assert_eq!(display_name(&AgentMode::Coding), "编程");
        assert_eq!(display_name(&AgentMode::Research), "研究");
    }

    #[test]
    fn test_icon() {
        assert_eq!(icon(&AgentMode::Coding), "💻");
    }

    #[test]
    fn test_format_display() {
        assert_eq!(&format_display(&AgentMode::Coding), "💻 编程");
    }

    #[test]
    fn test_parse_chinese() {
        assert_eq!(parse_from_str("编程"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("代码"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("研究"), Some(AgentMode::Research));
        assert_eq!(parse_from_str("数据"), Some(AgentMode::Data));
        assert_eq!(parse_from_str("数据分析"), Some(AgentMode::Data));
        assert_eq!(parse_from_str("写作"), Some(AgentMode::Writing));
        assert_eq!(parse_from_str("写"), Some(AgentMode::Writing));
        assert_eq!(parse_from_str("通用"), Some(AgentMode::General));
        assert_eq!(parse_from_str("医学"), Some(AgentMode::Medical));
        assert_eq!(parse_from_str("医疗"), Some(AgentMode::Medical));
        assert_eq!(parse_from_str("临床"), Some(AgentMode::Medical));
    }

    #[test]
    fn test_parse_english() {
        assert_eq!(parse_from_str("coding"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("code"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("research"), Some(AgentMode::Research));
        assert_eq!(parse_from_str("data"), Some(AgentMode::Data));
        assert_eq!(parse_from_str("writing"), Some(AgentMode::Writing));
        assert_eq!(parse_from_str("medical"), Some(AgentMode::Medical));
        assert_eq!(parse_from_str("med"), Some(AgentMode::Medical));
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(parse_from_str("日本語"), None);
        assert_eq!(parse_from_str("unknown"), None);
    }

    #[test]
    fn test_agent_mode_from_name() {
        assert_eq!(AgentMode::from_name("general"), Some(AgentMode::General));
        assert_eq!(AgentMode::from_name("coding"), Some(AgentMode::Coding));
        assert_eq!(AgentMode::from_name("code"), Some(AgentMode::Coding));
        assert_eq!(AgentMode::from_name("medical"), Some(AgentMode::Medical));
        assert_eq!(AgentMode::from_name("med"), Some(AgentMode::Medical));
        assert_eq!(AgentMode::from_name("unknown"), None);
    }

    #[test]
    fn test_agent_mode_all() {
        assert_eq!(AgentMode::all().len(), 6);
    }

    #[test]
    fn test_agent_mode_display() {
        assert_eq!(AgentMode::Coding.to_string(), "Coding");
        assert_eq!(AgentMode::Research.to_string(), "Research");
        assert_eq!(AgentMode::Medical.to_string(), "Medical");
    }

    #[test]
    fn test_agent_mode_serde() {
        let mode = AgentMode::Coding;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"coding\"");
        let decoded: AgentMode = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, AgentMode::Coding);
    }

    #[test]
    fn test_template_key() {
        assert_eq!(template_key(&AgentMode::General), "mode_general");
        assert_eq!(template_key(&AgentMode::Coding), "mode_coding");
        assert_eq!(template_key(&AgentMode::Medical), "mode_medical");
    }
}
