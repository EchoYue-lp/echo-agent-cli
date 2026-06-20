//! Task router for choosing chat vs first-class runtime orchestration.
//!
//! This is intentionally above the old complexity classifier: the classifier is
//! a cheap fallback signal, while the router decides the product path.

use std::sync::Arc;

use echo_agent::llm::{ChatRequest, LlmClient, ResponseFormat};
use echo_agent::prelude::Message;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::classify::{Classification, ComplexityLabel, HeuristicClassifier};
use super::profiles::ProfileTemplate;
use super::types::{DomainProfile, InteractionMode};

/// Runtime path selected for a user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRouteKind")]
pub enum TaskRouteKind {
    /// Normal streaming chat; no first-class runtime run is created.
    NormalChat,
    /// Generate a plan and stop for user review.
    PlanOnly,
    /// Generate a TaskRuntime plan and wait for explicit approval.
    ComplexRuntime,
    /// Generate a read-only parallel plan and auto-launch workers.
    ParallelReadonlyDelegation,
    /// Reserved for long-running detached agents.
    BackgroundTask,
    /// Reserved for direct small edits on the main agent path.
    DirectEdit,
}

impl TaskRouteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NormalChat => "normal_chat",
            Self::PlanOnly => "plan_only",
            Self::ComplexRuntime => "complex_runtime",
            Self::ParallelReadonlyDelegation => "parallel_readonly_delegation",
            Self::BackgroundTask => "background_task",
            Self::DirectEdit => "direct_edit",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "normal_chat" => Self::NormalChat,
            "plan_only" => Self::PlanOnly,
            "complex_runtime" => Self::ComplexRuntime,
            "parallel_readonly_delegation" => Self::ParallelReadonlyDelegation,
            "background_task" => Self::BackgroundTask,
            "direct_edit" => Self::DirectEdit,
            _ => return None,
        })
    }

    pub fn should_create_runtime_run(&self) -> bool {
        matches!(
            self,
            Self::PlanOnly | Self::ComplexRuntime | Self::ParallelReadonlyDelegation
        )
    }

    pub fn should_auto_execute(&self) -> bool {
        matches!(self, Self::ParallelReadonlyDelegation)
    }
}

/// Router verdict used by UI traces and the Tauri chat path.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRouteDecision")]
pub struct TaskRouteDecision {
    pub route: TaskRouteKind,
    pub confidence: f32,
    pub reason: String,
    pub suggested_workers: Vec<String>,
    pub classification: Classification,
}

impl TaskRouteDecision {
    pub fn normal(reason: impl Into<String>) -> Self {
        Self {
            route: TaskRouteKind::NormalChat,
            confidence: 1.0,
            reason: reason.into(),
            suggested_workers: Vec::new(),
            classification: HeuristicClassifier::new().classify(""),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRouteDecision {
    route: String,
    #[serde(default)]
    domain_profile: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    suggested_workers: Vec<String>,
}

/// Route with an LLM first, falling back to deterministic local signals.
pub async fn route_message(
    llm: Option<Arc<dyn LlmClient>>,
    message: &str,
    mode: InteractionMode,
) -> TaskRouteDecision {
    match mode {
        InteractionMode::Chat => {
            return forced_decision(TaskRouteKind::NormalChat, message, "forced chat mode");
        }
        InteractionMode::Plan => {
            return forced_decision(TaskRouteKind::PlanOnly, message, "forced plan mode");
        }
        InteractionMode::Auto => {}
    }

    if let Some(llm) = llm
        && let Ok(decision) = route_with_llm(&llm, message).await
    {
        return decision;
    }

    route_deterministically(message)
}

fn forced_decision(route: TaskRouteKind, message: &str, reason: &str) -> TaskRouteDecision {
    let classification = HeuristicClassifier::new().classify(message);
    TaskRouteDecision {
        route,
        confidence: 1.0,
        reason: reason.to_string(),
        suggested_workers: default_workers(route, classification.inferred_profile),
        classification,
    }
}

async fn route_with_llm(
    llm: &Arc<dyn LlmClient>,
    message: &str,
) -> Result<TaskRouteDecision, String> {
    let request = ChatRequest {
        messages: vec![
            Message::system(router_system_prompt()),
            Message::user(format!("User message:\n{message}")),
        ],
        response_format: Some(ResponseFormat::JsonObject),
        ..Default::default()
    };
    let response = llm.chat(request).await.map_err(|e| e.to_string())?;
    let content = response.content().unwrap_or_default();
    let raw: RawRouteDecision = serde_json::from_str(content.trim()).map_err(|e| e.to_string())?;
    let route = TaskRouteKind::from_str(raw.route.trim()).ok_or_else(|| {
        format!(
            "unknown route '{}'",
            raw.route.chars().take(80).collect::<String>()
        )
    })?;
    let confidence = raw.confidence.unwrap_or(0.65).clamp(0.0, 1.0);
    let mut classification = HeuristicClassifier::new().classify(message);
    if let Some(profile) = raw
        .domain_profile
        .as_deref()
        .and_then(DomainProfile::from_str)
    {
        classification.inferred_profile = profile;
    }
    Ok(TaskRouteDecision {
        route,
        confidence,
        reason: raw
            .reason
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "LLM router decision".to_string()),
        suggested_workers: normalize_workers(
            raw.suggested_workers,
            route,
            classification.inferred_profile,
        ),
        classification,
    })
}

fn router_system_prompt() -> String {
    r#"You route a user's message for a local AI coding assistant runtime.
Return ONLY JSON:
{
  "route": "normal_chat" | "plan_only" | "complex_runtime" | "parallel_readonly_delegation" | "background_task" | "direct_edit",
  "domain_profile": "general" | "ai_coding" | "academic_research" | "data_analysis" | "medical_research",
  "confidence": number between 0 and 1,
  "reason": string,
  "suggested_workers": string[]
}

EKO's main domains are AI coding, academic research, data processing/analysis, and medical research. Choose "parallel_readonly_delegation" for broad read-only work that benefits from multiple workers in any of these domains: project architecture analysis, codebase review, literature exploration, evidence review, dataset profiling, analysis-method review, clinical evidence review, safety review, or comparing several candidate root causes.

Suggested workers are not a fixed count. Pick the smallest useful set from:
- AI coding: project_explorer, code_reviewer, test_planner, summary_writer
- Academic research: literature_scout, evidence_reviewer, synthesis_planner, summary_writer
- Data analysis: data_profiler, analysis_reviewer, reproducibility_planner, summary_writer
- Medical research: medical_literature_scout, clinical_evidence_reviewer, safety_reviewer, summary_writer

Choose "plan_only" when the user explicitly asks for a plan or says not to execute.
Choose "complex_runtime" for multi-step work that may include edits, verification, or review gates.
Choose "normal_chat" for small questions, explanations, or single-file/simple answers.
Choose "direct_edit" only for tiny obvious edits.
Choose "background_task" for long detached tasks the user expects to run asynchronously."#
        .to_string()
}

fn route_deterministically(message: &str) -> TaskRouteDecision {
    let classification = HeuristicClassifier::new().classify(message);
    let route = if looks_like_plan_only(message) {
        TaskRouteKind::PlanOnly
    } else if looks_like_parallel_readonly(message) {
        TaskRouteKind::ParallelReadonlyDelegation
    } else if classification.complexity == ComplexityLabel::Complex {
        TaskRouteKind::ComplexRuntime
    } else {
        TaskRouteKind::NormalChat
    };
    TaskRouteDecision {
        route,
        confidence: if route == TaskRouteKind::NormalChat {
            0.7
        } else {
            0.78
        },
        reason: format!("deterministic fallback: {}", classification.reason),
        suggested_workers: default_workers(route, classification.inferred_profile),
        classification,
    }
}

fn looks_like_plan_only(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "只给计划",
        "先给计划",
        "不要执行",
        "先不要执行",
        "plan only",
        "only make a plan",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_parallel_readonly(message: &str) -> bool {
    let lower = message.to_lowercase();
    let read_intents = [
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
    let broad_targets = [
        "项目",
        "架构",
        "代码库",
        "仓库",
        "模块",
        "codebase",
        "architecture",
        "project",
        "repository",
        "repo",
        "论文",
        "文献",
        "研究",
        "证据",
        "literature",
        "paper",
        "papers",
        "evidence",
        "dataset",
        "数据集",
        "数据",
        "指标",
        "analysis",
        "医学",
        "临床",
        "指南",
        "medical",
        "clinical",
        "guideline",
    ];
    let has_read_intent = read_intents.iter().any(|needle| lower.contains(needle));
    let has_broad_target = broad_targets.iter().any(|needle| lower.contains(needle));
    has_read_intent && has_broad_target
}

fn normalize_workers(
    workers: Vec<String>,
    route: TaskRouteKind,
    profile: DomainProfile,
) -> Vec<String> {
    let template = ProfileTemplate::for_profile(profile);
    let mut out = Vec::new();
    for worker in workers {
        if template
            .default_worker_roles
            .iter()
            .any(|allowed| allowed == &worker.as_str())
            && !out.contains(&worker)
        {
            out.push(worker);
        }
    }
    if out.is_empty() {
        default_workers(route, profile)
    } else {
        out
    }
}

fn default_workers(route: TaskRouteKind, profile: DomainProfile) -> Vec<String> {
    match route {
        TaskRouteKind::ParallelReadonlyDelegation => ProfileTemplate::for_profile(profile)
            .default_worker_roles
            .iter()
            .map(|role| role.to_string())
            .collect(),
        TaskRouteKind::PlanOnly | TaskRouteKind::ComplexRuntime => {
            ProfileTemplate::for_profile(profile)
                .default_worker_roles
                .iter()
                .take(2)
                .map(|role| role.to_string())
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_router_detects_broad_readonly_project_analysis() {
        let decision = route_deterministically("请帮我分析一下这个项目架构");
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(
            decision
                .suggested_workers
                .iter()
                .any(|w| w == "project_explorer")
        );
    }

    #[test]
    fn deterministic_router_uses_academic_workers_for_literature_tasks() {
        let decision =
            route_deterministically("请做一个 arxiv literature review，分析相关论文证据");
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(
            decision
                .suggested_workers
                .iter()
                .any(|w| w == "literature_scout")
        );
        assert!(
            decision
                .suggested_workers
                .iter()
                .any(|w| w == "evidence_reviewer")
        );
    }

    #[test]
    fn deterministic_router_uses_data_workers_for_dataset_tasks() {
        let decision = route_deterministically("帮我分析这个数据集，先做数据画像和分析方法 review");
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(
            decision
                .suggested_workers
                .iter()
                .any(|w| w == "data_profiler")
        );
    }

    #[test]
    fn deterministic_router_keeps_simple_questions_in_chat() {
        let decision = route_deterministically("Rust 的 closure 是什么？");
        assert_eq!(decision.route, TaskRouteKind::NormalChat);
    }

    #[test]
    fn deterministic_router_respects_plan_only_language() {
        let decision = route_deterministically("先给计划，不要执行");
        assert_eq!(decision.route, TaskRouteKind::PlanOnly);
    }
}
