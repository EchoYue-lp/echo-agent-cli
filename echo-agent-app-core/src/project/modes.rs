use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMode {
    General,
    Coding,
    Research,
    Data,
    Writing,
}

impl AgentMode {
    pub fn system_prompt(&self) -> &str {
        match self {
            AgentMode::General => {
                "你是一个智能助手，可以回答各种问题并帮助用户完成任务。当需要时，你可以使用工具来获取信息或执行操作。"
            }
            AgentMode::Coding => {
                "你是一个专业的编程助手。你可以阅读、编写、调试和重构代码。在修改代码前，先理解现有代码的结构和逻辑。遵循项目的代码风格和约定。提供清晰、安全的代码修改，并解释你的变更。当执行危险操作（如删除文件、运行命令）时，需要获得用户确认。"
            }
            AgentMode::Research => {
                "你是一个研究助手。你擅长搜索、分析和总结信息。在进行研究时，你会：1) 明确研究问题 2) 搜索多个来源 3) 交叉验证信息 4) 提供引用来源 5) 给出结构化的研究报告。"
            }
            AgentMode::Data => {
                "你是一个数据分析助手。你可以读取和分析数据文件（CSV、Excel、JSON、Parquet 等），进行数据清洗和转换，生成统计摘要，创建可视化图表，并提供数据驱动的洞察。"
            }
            AgentMode::Writing => {
                "你是一个写作助手。你擅长撰写、编辑和优化各类文本内容，包括技术文档、文章、报告、邮件等。你会根据目标受众和场景调整写作风格。"
            }
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            AgentMode::General => "通用",
            AgentMode::Coding => "编程",
            AgentMode::Research => "研究",
            AgentMode::Data => "数据",
            AgentMode::Writing => "写作",
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            AgentMode::General => "💬",
            AgentMode::Coding => "💻",
            AgentMode::Research => "🔬",
            AgentMode::Data => "📊",
            AgentMode::Writing => "✍️",
        }
    }

    pub fn recommended_tools(&self) -> &[&str] {
        match self {
            AgentMode::General => &[],
            AgentMode::Coding => &[
                "shell",
                "file_read",
                "file_write",
                "file_list",
                "file_delete",
                "git",
            ],
            AgentMode::Research => &["web_search", "web_fetch", "file_read"],
            AgentMode::Data => &["file_read", "data_analysis", "chart"],
            AgentMode::Writing => &["file_read", "file_write"],
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "general" | "通用" => Some(AgentMode::General),
            "coding" | "code" | "编程" | "代码" => Some(AgentMode::Coding),
            "research" | "研究" => Some(AgentMode::Research),
            "data" | "数据分析" | "数据" => Some(AgentMode::Data),
            "writing" | "写作" | "写" => Some(AgentMode::Writing),
            _ => None,
        }
    }

    pub fn all() -> &'static [AgentMode] {
        &[
            AgentMode::General,
            AgentMode::Coding,
            AgentMode::Research,
            AgentMode::Data,
            AgentMode::Writing,
        ]
    }
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}
