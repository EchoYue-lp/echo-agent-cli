//! Deterministic fallback signals for TaskRuntime routing.
//!
//! LLM routing is the primary path. These signals exist so Auto mode still has
//! a conservative offline fallback when no model is available or JSON routing
//! fails. Keep signal tables centralized here; routing/planning code should
//! consume the derived capability areas instead of scattering keyword lists.

use super::types::DomainProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityArea {
    Coding,
    Data,
    Academic,
    Medical,
}

pub struct CapabilitySignal {
    pub area: CapabilityArea,
    pub terms: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct RoutingSignals {
    pub plan_only: bool,
    pub read_intent: bool,
    pub capability_areas: Vec<CapabilityArea>,
    pub labels: Vec<String>,
}

impl RoutingSignals {
    pub fn analyze(profile: DomainProfile, message: &str) -> Self {
        let lower = message.to_lowercase();
        let mut labels = Vec::new();
        let plan_only = contains_any(&lower, PLAN_ONLY_CUES);
        if plan_only {
            labels.push("plan_only".to_string());
        }

        let read_intent = contains_any(&lower, READ_INTENT_CUES);
        if read_intent {
            labels.push("read_intent".to_string());
        }

        let capability_areas = matched_capability_areas(profile, message);
        for area in &capability_areas {
            labels.push(format!("capability:{area}"));
        }

        Self {
            plan_only,
            read_intent,
            capability_areas,
            labels,
        }
    }

    pub fn supports_parallel_readonly(&self) -> bool {
        self.read_intent && !self.capability_areas.is_empty()
    }

    pub fn reason_suffix(&self) -> String {
        if self.labels.is_empty() {
            "routing_signals:none".to_string()
        } else {
            format!("routing_signals:{}", self.labels.join(","))
        }
    }
}

impl std::fmt::Display for CapabilityArea {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            CapabilityArea::Coding => "coding",
            CapabilityArea::Data => "data",
            CapabilityArea::Academic => "academic",
            CapabilityArea::Medical => "medical",
        };
        f.write_str(label)
    }
}

pub const PLAN_ONLY_CUES: &[&str] = &[
    "只给计划",
    "先给计划",
    "不要执行",
    "先不要执行",
    "plan only",
    "only make a plan",
];

pub const READ_INTENT_CUES: &[&str] = &[
    "分析",
    "review",
    "审查",
    "过一遍",
    "看看",
    "explore",
    "analyze",
    "investigate",
    "检索",
    "综述",
    "画像",
    "profiling",
];

pub const COMPLEX_TASK_CUES: &[&str] = &[
    "全面优化",
    "全面 review",
    "彻底 review",
    "深入排查",
    "architecture review",
    "analyze architecture",
    "analyze the architecture",
    "analyze project",
    "analyze the project",
    "项目 review",
    "项目分析",
    "分析项目",
    "项目架构",
    "分析架构",
    "架构分析",
    "代码库分析",
    "分析代码库",
    "codebase review",
    "codebase analysis",
    "full review",
    "refactor the",
    "重 构 整 个",
    "继续修复",
    "继续完善",
    "fix several",
    "fix multiple",
    "修复这些",
    "修复这几个",
    "这些 bug",
    "these bugs",
    "制定计划",
    "拆分任务",
    "分解任务",
    "make a plan",
    "break this down",
];

pub const MULTI_TARGET_CUES: &[&str] = &[
    "多个文件",
    "多个模块",
    "几个文件",
    "这几个",
    "all the files",
    "multiple files",
    "several modules",
];

pub const ACTION_VERB_CUES: &[&str] = &[
    "修改", "审查", "review", "fix", "update", "refactor", "实现",
];

pub const PROFILE_CUES: &[(&str, DomainProfile)] = &[
    ("arxiv", DomainProfile::AcademicResearch),
    ("pubmed", DomainProfile::MedicalResearch),
    ("literature review", DomainProfile::AcademicResearch),
    ("systematic review", DomainProfile::AcademicResearch),
    ("论文", DomainProfile::AcademicResearch),
    ("文献", DomainProfile::AcademicResearch),
    ("clinical", DomainProfile::MedicalResearch),
    ("medical", DomainProfile::MedicalResearch),
    ("guideline", DomainProfile::MedicalResearch),
    ("医学", DomainProfile::MedicalResearch),
    ("临床", DomainProfile::MedicalResearch),
    ("指南", DomainProfile::MedicalResearch),
    ("诊断", DomainProfile::MedicalResearch),
    ("循证", DomainProfile::MedicalResearch),
    ("数据集", DomainProfile::DataAnalysis),
    ("数据分析", DomainProfile::DataAnalysis),
    ("data analysis", DomainProfile::DataAnalysis),
    ("notebook", DomainProfile::DataAnalysis),
    ("eda", DomainProfile::DataAnalysis),
    ("cargo", DomainProfile::AiCoding),
    ("npm", DomainProfile::AiCoding),
    ("cargo check", DomainProfile::AiCoding),
    ("refactor", DomainProfile::AiCoding),
    ("代码", DomainProfile::AiCoding),
];

pub const CAPABILITY_SIGNALS: &[CapabilitySignal] = &[
    CapabilitySignal {
        area: CapabilityArea::Coding,
        terms: &[
            "代码",
            "仓库",
            "项目",
            "架构",
            "模块",
            "实现",
            "测试",
            "code",
            "repo",
            "repository",
            "codebase",
            "architecture",
            "module",
            "rust",
            "typescript",
            "python",
            "notebook",
            "script",
            "pipeline",
            "package",
        ],
    },
    CapabilitySignal {
        area: CapabilityArea::Data,
        terms: &[
            "数据",
            "数据集",
            "指标",
            "统计",
            "图表",
            "画像",
            "样本",
            "data",
            "dataset",
            "metric",
            "statistics",
            "statistical",
            "plot",
            "chart",
            "eda",
            "cohort",
            "table",
        ],
    },
    CapabilitySignal {
        area: CapabilityArea::Academic,
        terms: &[
            "论文",
            "文献",
            "研究",
            "综述",
            "引用",
            "证据",
            "literature",
            "paper",
            "papers",
            "research",
            "citation",
            "evidence",
            "arxiv",
        ],
    },
    CapabilitySignal {
        area: CapabilityArea::Medical,
        terms: &[
            "医学",
            "临床",
            "指南",
            "诊断",
            "治疗",
            "患者",
            "循证",
            "medical",
            "clinical",
            "guideline",
            "diagnosis",
            "treatment",
            "patient",
            "pubmed",
            "biomedical",
        ],
    },
];

pub fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

pub fn profile_seed_area(profile: DomainProfile) -> Option<CapabilityArea> {
    match profile {
        DomainProfile::AiCoding => Some(CapabilityArea::Coding),
        DomainProfile::DataAnalysis => Some(CapabilityArea::Data),
        DomainProfile::AcademicResearch => Some(CapabilityArea::Academic),
        DomainProfile::MedicalResearch => Some(CapabilityArea::Medical),
        DomainProfile::General => None,
    }
}

pub fn matched_capability_areas(profile: DomainProfile, message: &str) -> Vec<CapabilityArea> {
    let lower = message.to_lowercase();
    let mut out = Vec::new();
    if let Some(area) = profile_seed_area(profile) {
        push_area(&mut out, area);
    }
    for signal in CAPABILITY_SIGNALS {
        if contains_any(&lower, signal.terms) {
            push_area(&mut out, signal.area);
        }
    }
    out
}

fn push_area(out: &mut Vec<CapabilityArea>, area: CapabilityArea) {
    if !out.contains(&area) {
        out.push(area);
    }
}
