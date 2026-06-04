//! CLI-specific AgentMode definition (Chinese names, icons, bilingual parsing).
//!
//! AgentMode is a product-level concept — the framework (echo-agent) does not
//! know about it. This module owns the enum and all mode-related logic for
//! the EchoCoWork CLI product.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
pub fn chinese_mode_prompt(mode: &AgentMode) -> String {
    match mode {
        AgentMode::General => {
            "你是一个智能助手，可以回答各种问题并帮助用户完成任务。当需要时，你可以使用工具来获取信息或执行操作。".into()
        }
        AgentMode::Coding => {
            "你是一个专业的编程助手。你可以阅读、编写、调试和重构代码。在修改代码前，先理解现有代码的结构和逻辑。遵循项目的代码风格和约定。提供清晰、安全的代码修改，并解释你的变更。当执行危险操作（如删除文件、运行命令）时，需要获得用户确认。\n\n\
             工作流程：\n\
             1. 理解需求：先阅读相关代码，理解上下文\n\
             2. 设计方案：修改前说明计划和影响范围\n\
             3. 实施修改：编写代码，遵循项目风格\n\
             4. 验证结果：运行测试确认修改正确\n\
             5. 总结变更：说明做了什么、为什么".into()
        }
        AgentMode::Research => {
            "你是一个学术研究助手。你擅长搜索、分析和总结学术论文与研究信息。在进行研究时，你会：\n\
             1. 明确研究问题和关键词\n\
             2. 使用 arxiv_search 和 semantic_scholar_search 搜索多个学术数据库\n\
             3. 用 pdf_fetch 下载并阅读重要论文\n\
             4. 交叉验证信息，比较不同研究的方法和结论\n\
             5. 用 bibtex_generate 管理引用\n\
             6. 给出结构化的文献综述和研究报告\n\n\
             当撰写论文时，你会生成带完整引用的学术文本，确保每个论点都有来源支持。".into()
        }
        AgentMode::Data => {
            "你是一个数据分析助手。你可以读取和分析数据文件（CSV、Excel、JSON、Parquet 等），进行数据清洗和转换，生成统计摘要，创建可视化图表，并提供数据驱动的洞察。\n\n\
             分析流程：\n\
             1. 理解问题：明确分析目标和关键指标\n\
             2. 数据探索：用 profile_data 了解数据结构、类型和质量\n\
             3. 数据清洗：处理缺失值、异常值和类型不一致\n\
             4. 分析执行：选择合适的统计方法和工具\n\
             5. 可视化：用 generate_chart 呈现关键发现\n\
             6. 结论：给出数据驱动的洞察和建议，附带置信度和局限性说明\n\n\
             对大数据集优先使用采样和聚合，避免全量加载。始终报告样本量和统计显著性。".into()
        }
        AgentMode::Writing => {
            "你是一个写作助手。你擅长撰写、编辑和优化各类文本内容，包括技术文档、文章、报告、邮件等。你会根据目标受众和场景调整写作风格。\n\n\
             写作流程：\n\
             1. 明确目标：受众、用途、篇幅要求\n\
             2. 构建大纲：确定主要章节和逻辑结构\n\
             3. 撰写初稿：按章节逐步完成\n\
             4. 优化润色：检查逻辑、语法和表达\n\
             5. 输出文件：支持 Markdown、LaTeX、DOCX 格式".into()
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
            "git",
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
            "data_analyze",
            "chart",
            "excel_read",
            "csv_read",
        ],
        AgentMode::Writing => vec!["read_file", "write_file", "edit_file", "web_search"],
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chinese_mode_prompts() {
        let prompt = chinese_mode_prompt(&AgentMode::Coding);
        assert!(prompt.contains("编程助手"));
    }

    #[test]
    fn test_recommended_tools() {
        assert_eq!(recommended_tools(&AgentMode::Coding).len(), 9);
        assert_eq!(recommended_tools(&AgentMode::Research).len(), 8);
        assert_eq!(recommended_tools(&AgentMode::Data).len(), 7);
        assert_eq!(recommended_tools(&AgentMode::Writing).len(), 4);
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
    }

    #[test]
    fn test_parse_english() {
        assert_eq!(parse_from_str("coding"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("code"), Some(AgentMode::Coding));
        assert_eq!(parse_from_str("research"), Some(AgentMode::Research));
        assert_eq!(parse_from_str("data"), Some(AgentMode::Data));
        assert_eq!(parse_from_str("writing"), Some(AgentMode::Writing));
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
        assert_eq!(AgentMode::from_name("unknown"), None);
    }

    #[test]
    fn test_agent_mode_all() {
        assert_eq!(AgentMode::all().len(), 5);
    }

    #[test]
    fn test_agent_mode_display() {
        assert_eq!(AgentMode::Coding.to_string(), "Coding");
        assert_eq!(AgentMode::Research.to_string(), "Research");
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
    }
}
